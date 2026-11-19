//! 把支付/账本域（`crates/features/commerce`）装进运行中的 server：
//! 供应商回调（G06 / 第四轮审计 N7，`PAYMENT-CALLBACK-REPLAY`）+
//! 会话鉴权动作面 + admin 对账面（G06-01..05 的 ready-for-G00 集成请求）。
//!
//! commerce 的域侧早已就绪：`http::dispatch` 覆盖全部动作，带 `operation_id`
//! 的回调入口 `http::provider_callback` 有 13 个文件库重放用例
//! （`g06_06_provider_callback.rs`）。本模块只做组合根装配，与 media/discover/mini
//! 域同一个口径：
//!
//! 1. **无 actor 会话，只信签名**。回调来自供应商，不带用户令牌；鉴权是对
//!    **原始 body 字节**的 HMAC-SHA256（十六进制），密钥 `WXLIKE_PAYMENT_WEBHOOK_SECRET`。
//!    先按原始字节读 body 再验签，绝不 `serde_json::to_string` 重排后再签。
//! 2. **生产必须配密钥**：二进制 `check_secrets` 在缺 env 时拒绝启动
//!    （server/src/main.rs）；开发/CI 进程用本次启动的随机密钥（重启即失效，
//!    只够单进程），与 media 域同口径。
//! 3. **幂等只有一份实现**。重放/防重放/并发单一效果全在 commerce 域内
//!    （`store::replay_legs`），本模块只做 HTTP 形状与错误分类映射，不另写幂等表。
//! 4. **audience 在路由层收口**（G06-03/G06-05）：后台面（`reconcile.*`、
//!    `entitlement.rule.publish` / `entitlement.issue`）只认平台
//!    `x-admin-token`（没配 = 整面关闭，与 /metrics 同口径）。这是 G14 真实
//!    admin/merchant audience 落地前的失败关闭 interim，不伪造商户身份。
//! 5. **群成员是真实端口**（G06-04）：红包创建/领取的成员校验落平台
//!    `group_members` 表（`PlatformMembership`），不再用 DenyAll 兜底；
//!    StaticMembership 只存在于 commerce 的测试夹具。
//!
//! ADR-28：回调/动作/对账处理会碰 SQLite（同步 rusqlite），整段经
//! `spawn_blocking` 进阻塞池，async handler 不持连接锁；后台作业同理。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use wxlike_commerce::infrastructure::sandbox::SandboxProvider;
use wxlike_commerce::{Commerce, CommerceConfig, CommerceError, GroupMembership, Ports};
use wxlike_platform::PlatformState;

/// 回调验签密钥的环境变量。生产必配；多实例必须配成同一个值。
pub const SECRET_ENV: &str = "WXLIKE_PAYMENT_WEBHOOK_SECRET";

/// 签名头名。供应商对原始 body 字节算 HMAC-SHA256（十六进制）放这里。
/// 全仓此前未约定签名头名（第四轮审计 N7），由本挂载定名并写进契约。
pub const SIGNATURE_HEADER: &str = "x-payment-signature";

/// 装配 commerce 回调路由。返回 `(路由, 域句柄)`：路由进 `build_app`，
/// 域句柄交给组合根/测试创建意图与签名（与 media_mount 返回 service 同口径）。
///
/// 沙箱供应商 + 平台真实群成员端口（G06-04）：红包创建/领取的成员事实来自
/// 平台 `group_members` 表，与 hub 投递用的是同一份成员真相。真实供应商凭据
/// 下的回调/对账取证需要 live provider 挂载后由 G16 重跑（见
/// integration-requests/G06）；live 凭据不存在时本装配不伪造。
pub fn assemble(
    platform: &PlatformState,
    db_path: Option<&str>,
) -> anyhow::Result<(Router, Arc<Commerce>)> {
    assemble_with_provider(platform, db_path, Arc::new(SandboxProvider::new()))
}

/// 与 [`assemble`] 相同的装配，但允许注入供应商端口。生产走沙箱默认；
/// 集成测试用它注入可故障注入的沙箱句柄，以在**真实挂载 handler**上验证
/// PROVIDER_TIMEOUT / PROVIDER_UNAVAILABLE 的 HTTP 分类（不是另拼一个近似 app）。
pub fn assemble_with_provider(
    platform: &PlatformState,
    db_path: Option<&str>,
    provider: Arc<dyn wxlike_commerce::PaymentProvider>,
) -> anyhow::Result<(Router, Arc<Commerce>)> {
    let secret = match crate::non_empty_env(SECRET_ENV) {
        Some(secret) => secret.into_bytes(),
        None => {
            // 到这里的只有测试与 WXLIKE_DEV_INSECURE=1 的开发进程：二进制的
            // check_secrets 会在缺 env 时先拒绝启动（server/src/main.rs）。
            tracing::warn!(
                "{SECRET_ENV} 未设置：支付回调用本次启动的随机密钥验签，\
                 重启即失效、多实例互不认（只允许开发/CI 走到这里）"
            );
            wxlike_platform::random_hex(32).into_bytes()
        }
    };

    let path = crate::sidecar_dir(db_path, "commerce").join("commerce.db");
    let ports = Ports::new(provider, Arc::new(PlatformMembership(platform.clone())));
    let commerce = Arc::new(
        Commerce::open(path.to_str(), CommerceConfig::sandbox(secret), ports)
            .map_err(|err| anyhow::anyhow!("commerce open failed: {err}"))?,
    );

    // 回调面：无会话，只信原始 body 签名。两条入口共用同一套事件表与入账
    // 内核（G06-02）：`/api/payment/callback` 带 operation_id 台账，
    // `/api/v1/commerce/webhooks/payment` 是契约里的旧事件入口，只按
    // event_id 去重；幂等语义都只有 commerce 域内那一份实现。
    let callback = Router::new()
        .route("/api/payment/callback", post(provider_callback))
        .route("/api/v1/commerce/webhooks/payment", post(payment_webhook))
        .with_state(Arc::clone(&commerce));

    // 动作面：会话鉴权（平台 sessions 表，与 discover_mount 同口径）。
    // 创建意图/查询/回调台账排障都走这里；actor 来自令牌，不信请求体自报。
    let actions = Router::new()
        .route("/api/v1/commerce/actions/{action}", post(commerce_action))
        .route(
            "/api/v1/commerce/callbacks/{operation_id}",
            get(callback_lookup),
        )
        .with_state(CommerceActions {
            commerce: Arc::clone(&commerce),
            platform: platform.clone(),
        });

    Ok((callback.merge(actions), commerce))
}

/// 动作面的共享状态：域句柄 + 平台会话校验。
#[derive(Clone)]
struct CommerceActions {
    commerce: Arc<Commerce>,
    platform: PlatformState,
}

use crate::mount_common::actor_of;

/// `POST /api/v1/commerce/actions/{action}`：会话鉴权的命令分发。
async fn commerce_action(
    State(state): State<CommerceActions>,
    Path(action): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(actor) = actor_of(&state.platform, &headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "UNAUTHORIZED"})),
        )
            .into_response();
    };
    // audience 闸门在解析 body 之前：不给未授权调用方任何参数探测面。
    if let Some(response) = audience_gate(&action, &state.platform, &headers) {
        return response;
    }
    let payload: serde_json::Value = if body.is_empty() {
        serde_json::json!({})
    } else {
        match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "BAD_INPUT"})),
                )
                    .into_response()
            }
        }
    };
    let commerce = Arc::clone(&state.commerce);
    let ran = tokio::task::spawn_blocking(move || {
        wxlike_commerce::http::dispatch(&commerce, &actor, &action, &payload)
    })
    .await;
    match ran {
        Ok(Ok(value)) => (StatusCode::OK, Json(value)).into_response(),
        Ok(Err(err)) => {
            let (status, label) = classify(&err);
            (status, Json(serde_json::json!({"error": label}))).into_response()
        }
        Err(err) => {
            tracing::error!(%err, "commerce 动作阻塞任务异常结束");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "INTERNAL"})),
            )
                .into_response()
        }
    }
}

/// 哪些动作属于"后台面"（audience = admin / merchant），不能只凭普通会话调用。
///
/// 这些动作的域实现**不按角色收口**（`reconcile.run/list` 连 actor 都不收，
/// `rule.publish`/`issue` 任何 issuer 都能发），所以受众强制必须落在路由层：
/// * `reconcile.*`（G06-03，audience=admin）：跑对账 / 看差异 / 审批补账；
/// * `entitlement.rule.publish` / `entitlement.issue`（G06-05，audience=merchant）：
///   发布权益规则 / 发放权益。
///
/// 其余动作的权限语义（本人 / 收付双方 / 群成员 / 订单买卖双方）由 commerce
/// 域按 actor 强制，actor 只来自会话令牌，不信请求体自报，无需在此重复。
fn is_backstage_action(action: &str) -> bool {
    action.starts_with("reconcile.")
        || matches!(action, "entitlement.rule.publish" | "entitlement.issue")
}

/// 动作受众闸门（G06-03 / G06-05 的 audience 行，路由层是唯一强制点）。
///
/// 后台面统一要求平台 `x-admin-token`（与 /metrics、平台管理面同一口径：
/// **没配 = 整面关闭，不是放行**）。这是 G14 admin/merchant audience 落地前的
/// 诚实 interim：商户注册表与审批链路尚不存在，把任意登录用户当商户/管理员
/// 等于敞开"给自己发无限权益 / 随意补账"的口子，所以失败关闭。G14 落地后
/// 在这里换成真实的 admin/merchant 校验（见 integration-requests/G06）。
///
/// 非后台动作返回 `None`（放行给域内按 actor 鉴权）。
fn audience_gate(action: &str, platform: &PlatformState, headers: &HeaderMap) -> Option<Response> {
    if !is_backstage_action(action) {
        return None;
    }
    let presented = headers.get("x-admin-token").and_then(|v| v.to_str().ok());
    if platform.admin_ok(presented) {
        return None;
    }
    Some(
        (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "FORBIDDEN"})),
        )
            .into_response(),
    )
}

/// `GET /api/v1/commerce/callbacks/{operation_id}`：回调台账排障（会话鉴权）。
async fn callback_lookup(
    State(state): State<CommerceActions>,
    Path(operation_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if actor_of(&state.platform, &headers).is_none() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "UNAUTHORIZED"})),
        )
            .into_response();
    }
    let commerce = Arc::clone(&state.commerce);
    let ran = tokio::task::spawn_blocking(move || {
        wxlike_commerce::http::provider_callback_get(&commerce, &operation_id)
    })
    .await;
    match ran {
        Ok(Ok(value)) => (StatusCode::OK, Json(value)).into_response(),
        Ok(Err(err)) => {
            let (status, label) = classify(&err);
            (status, Json(serde_json::json!({"error": label}))).into_response()
        }
        Err(err) => {
            tracing::error!(%err, "commerce 台账查询阻塞任务异常结束");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "INTERNAL"})),
            )
                .into_response()
        }
    }
}

/// `POST /api/payment/callback`：带 `operation_id` 的供应商回调。
///
/// 原始 body 字节 → UTF-8 → 取签名头 → 域入口。错误只回稳定分类，不回显
/// 供应商报文（与 commerce 错误口径一致）。
async fn provider_callback(
    State(commerce): State<Arc<Commerce>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let raw = match String::from_utf8(body.to_vec()) {
        Ok(raw) => raw,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "BAD_INPUT"})),
            )
                .into_response()
        }
    };
    let signature = headers
        .get(SIGNATURE_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // ADR-28：域入口会碰 SQLite，整段进阻塞池；await 上下文不持连接锁。
    let ran = tokio::task::spawn_blocking(move || {
        wxlike_commerce::http::provider_callback(&commerce, &raw, &signature)
    })
    .await;
    let result = match ran {
        Ok(inner) => inner,
        Err(err) => {
            tracing::error!(%err, "commerce 回调阻塞任务异常结束");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "INTERNAL"})),
            )
                .into_response();
        }
    };
    match result {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(err) => {
            let (status, label) = classify(&err);
            if status.is_server_error() {
                tracing::warn!(%err, "commerce 回调失败");
            }
            (status, Json(serde_json::json!({"error": label}))).into_response()
        }
    }
}

/// `POST /api/v1/commerce/webhooks/payment`：契约里的旧事件回调入口
/// （G06-02）。与 `/api/payment/callback` 的区别只在幂等把手：这里按
/// `event_id` 去重（`Commerce::handle_webhook`），没有 operation_id 台账。
/// 验签口径完全相同：原始 body 字节的 HMAC，失败 401，不回显供应商报文。
async fn payment_webhook(
    State(commerce): State<Arc<Commerce>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let raw = match String::from_utf8(body.to_vec()) {
        Ok(raw) => raw,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "BAD_INPUT"})),
            )
                .into_response()
        }
    };
    let signature = headers
        .get(SIGNATURE_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // ADR-28：域入口会碰 SQLite，整段进阻塞池；await 上下文不持连接锁。
    let ran = tokio::task::spawn_blocking(move || {
        wxlike_commerce::http::webhook(&commerce, &raw, &signature)
    })
    .await;
    match ran {
        Ok(Ok(value)) => (StatusCode::OK, Json(value)).into_response(),
        Ok(Err(err)) => {
            let (status, label) = classify(&err);
            if status.is_server_error() {
                tracing::warn!(%err, "commerce webhook 失败");
            }
            (status, Json(serde_json::json!({"error": label}))).into_response()
        }
        Err(err) => {
            tracing::error!(%err, "commerce webhook 阻塞任务异常结束");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "INTERNAL"})),
            )
                .into_response()
        }
    }
}

/// 群成员端口 → 平台 `group_members` 表（G06-04 要求的真实成员事实）。
///
/// 平台的群会话 id 就是裸群 id（`create_group` 生成的 16 hex），与 hub 投递
/// 用的 `PlatformMembership` 同一份真相；这里只做同步查询（commerce 域在
/// 自己的阻塞上下文里调用它），查库失败如实报错 → 域内映射为
/// MEMBERSHIP_UNAVAILABLE，不猜"大概是成员"。
struct PlatformMembership(PlatformState);

impl GroupMembership for PlatformMembership {
    fn is_member(&self, conversation: &str, user: &str) -> wxlike_commerce::Result<bool> {
        // 查库失败的具体原因只落日志，不透给调用方；域内会统一映射为
        // MEMBERSHIP_UNAVAILABLE（失败关闭，不猜成员）。
        self.0.is_member(conversation, user).map_err(|err| {
            tracing::warn!(%err, "commerce 群成员查询失败");
            CommerceError::MembershipUnavailable
        })
    }
}

/// commerce 后台作业（G06-02/03/04 的 jobs 段）。返回句柄给调用方攥着，
/// 与 `spawn_background_workers` 同口径：循环活到进程结束。
///
/// * `commerce.payment.expire`（60s）：只把过期未支付意图置 expired，不退款、
///   不查询——未知状态由 `payment.refresh` / 对账收口；
/// * `commerce.transfer.expire` + `commerce.redpacket.expire`（60s）：到期
///   未确认转账解冻退回、到期红包未领份额退回发送者（24h TTL 是官方规则）；
/// * `commerce.withdrawal.refresh`（60s）：只重试 processing 提现，同一
///   payout 幂等键，不重复出款；
/// * `commerce.reconcile.daily`（24h 一轮，近似每日）：生成新 run + open
///   exceptions，不自动改账；审批补账仍走 admin 的 `reconcile.resolve`。
///
/// ADR-28：所有同步 SQL 段整段进 `spawn_blocking`；async 循环只负责节拍。
pub fn spawn_commerce_workers(commerce: Arc<Commerce>) -> Vec<tokio::task::JoinHandle<()>> {
    use std::time::Duration;
    let mut workers = vec![
        spawn_sweep(
            Arc::clone(&commerce),
            Duration::from_secs(60),
            "commerce.payment.expire",
            |c| c.expire_payment_intents(),
        ),
        spawn_sweep(
            Arc::clone(&commerce),
            Duration::from_secs(60),
            "commerce.transfer.expire",
            |c| c.expire_due_transfers(),
        ),
        spawn_sweep(
            Arc::clone(&commerce),
            Duration::from_secs(60),
            "commerce.redpacket.expire",
            |c| c.expire_due_red_packets(),
        ),
        spawn_sweep(
            Arc::clone(&commerce),
            Duration::from_secs(60),
            "commerce.withdrawal.refresh",
            |c| c.refresh_processing_withdrawals(),
        ),
    ];
    // 对账会生成新 run + open exceptions（不改账）。测试需要自己控制对账
    // 时机时用 WXLIKE_COMMERCE_RECONCILE_WORKER=0 关掉这条循环；默认开启。
    if crate::non_empty_env("WXLIKE_COMMERCE_RECONCILE_WORKER").as_deref() != Some("0") {
        workers.push(spawn_sweep(
            commerce,
            Duration::from_secs(24 * 60 * 60),
            "commerce.reconcile.daily",
            |c| c.run_reconciliation().map(|report| report.exceptions.len()),
        ));
    }
    workers
}

/// 一条节拍循环：每 `every` 把同步域方法整段丢进阻塞池跑一轮。
fn spawn_sweep(
    commerce: Arc<Commerce>,
    every: std::time::Duration,
    name: &'static str,
    sweep: fn(&Commerce) -> wxlike_commerce::Result<usize>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(every).await;
            let commerce = Arc::clone(&commerce);
            // ADR-28：sweep 是同步 rusqlite 段，整段进阻塞池。
            let ran = tokio::task::spawn_blocking(move || sweep(&commerce)).await;
            match ran {
                Ok(Ok(n)) if n > 0 => tracing::info!(job = name, affected = n, "commerce 作业推进"),
                Ok(Ok(_)) => {}
                Ok(Err(err)) => tracing::warn!(job = name, %err, "commerce 作业本轮失败"),
                Err(err) => tracing::error!(job = name, %err, "commerce 作业阻塞任务异常结束"),
            }
        }
    })
}

/// 错误 → HTTP 状态码 + 稳定分类。只回分类字符串，不回显供应商报文/SQL/密钥。
fn classify(err: &CommerceError) -> (StatusCode, &'static str) {
    match err {
        CommerceError::InvalidSignature => (StatusCode::UNAUTHORIZED, "INVALID_SIGNATURE"),
        CommerceError::BadInput(_) | CommerceError::Money(_) => {
            (StatusCode::BAD_REQUEST, "BAD_INPUT")
        }
        CommerceError::IdempotencyConflict => (StatusCode::CONFLICT, "IDEMPOTENCY_CONFLICT"),
        CommerceError::Replay => (StatusCode::CONFLICT, "REPLAY"),
        CommerceError::NotFound => (StatusCode::NOT_FOUND, "NOT_FOUND"),
        CommerceError::Forbidden | CommerceError::Unauthorized => {
            (StatusCode::FORBIDDEN, "FORBIDDEN")
        }
        CommerceError::Expired => (StatusCode::GONE, "EXPIRED"),
        CommerceError::IllegalTransition(_) | CommerceError::Conflict(_) => {
            (StatusCode::CONFLICT, "CONFLICT")
        }
        CommerceError::InsufficientFunds => (StatusCode::CONFLICT, "INSUFFICIENT_FUNDS"),
        CommerceError::AmountTooLarge => (StatusCode::BAD_REQUEST, "AMOUNT_TOO_LARGE"),
        // 供应商错误按契约 errors 清单逐类回稳定标签（G06-02-payment-refresh
        // 要求超时与不可达可区分）：超时=未知状态，调用方应 refresh 而不是重试扣款。
        CommerceError::ProviderTimeout => (StatusCode::GATEWAY_TIMEOUT, "PROVIDER_TIMEOUT"),
        CommerceError::ProviderRejected => (StatusCode::BAD_GATEWAY, "PROVIDER_REJECTED"),
        CommerceError::ProviderNotConfigured => {
            (StatusCode::BAD_GATEWAY, "PROVIDER_NOT_CONFIGURED")
        }
        CommerceError::MembershipUnavailable => (StatusCode::BAD_GATEWAY, "MEMBERSHIP_UNAVAILABLE"),
        CommerceError::ProviderUnavailable => (StatusCode::BAD_GATEWAY, "PROVIDER_UNAVAILABLE"),
        CommerceError::Ledger(_) | CommerceError::Storage(_) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL")
        }
    }
}
