//! 把支付/账本域（`crates/features/commerce`）的供应商回调装进运行中的 server
//! —— G06 / 第四轮审计 N7（`PAYMENT-CALLBACK-REPLAY` 唯一发布阻塞）。
//!
//! commerce 的域侧早已就绪：带 `operation_id` 的回调入口
//! `wxlike_commerce::http::provider_callback` + 13 个文件库重放用例
//! （`g06_06_provider_callback.rs`）。缺的只是把它挂成 HTTP 路由——本模块就是
//! 这一步组合根装配，与 media/discover/mini 域同一个口径：
//!
//! 1. **无 actor 会话，只信签名**。回调来自供应商，不带用户令牌；鉴权是对
//!    **原始 body 字节**的 HMAC-SHA256（十六进制），密钥 `WXLIKE_PAYMENT_WEBHOOK_SECRET`。
//!    先按原始字节读 body 再验签，绝不 `serde_json::to_string` 重排后再签。
//! 2. **没有密钥就诚实拒绝装配**（生产）；开发/CI 走 `WXLIKE_DEV_INSECURE=1`
//!    时用本次启动的随机密钥（重启即失效，只够单进程），与 media 域同口径。
//! 3. **幂等只有一份实现**。重放/防重放/并发单一效果全在 commerce 域内
//!    （`store::replay_legs`），本模块只做 HTTP 形状与错误分类映射，不另写幂等表。
//!
//! ADR-28：回调处理会碰 SQLite（同步 rusqlite），整段经 `spawn_blocking` 进
//! 阻塞池，async handler 不持连接锁。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use wxlike_commerce::infrastructure::sandbox::SandboxProvider;
use wxlike_commerce::{Commerce, CommerceConfig, CommerceError, DenyAllMembership, Ports};
use wxlike_platform::{Authed, PlatformState};

/// 回调验签密钥的环境变量。生产必配；多实例必须配成同一个值。
pub const SECRET_ENV: &str = "WXLIKE_PAYMENT_WEBHOOK_SECRET";

/// 签名头名。供应商对原始 body 字节算 HMAC-SHA256（十六进制）放这里。
/// 全仓此前未约定签名头名（第四轮审计 N7），由本挂载定名并写进契约。
pub const SIGNATURE_HEADER: &str = "x-payment-signature";

/// 装配 commerce 回调路由。返回 `(路由, 域句柄)`：路由进 `build_app`，
/// 域句柄交给组合根/测试创建意图与签名（与 media_mount 返回 service 同口径）。
///
/// 沙箱供应商 + 显式拒绝的群成员端口：回调入账不依赖群成员关系，红包等需要
/// 群成员的动作在本挂载下如实失败，不伪造。真实供应商凭据下的回调取证需要
/// live provider 挂载后由 G16 重跑（见 integration-requests/G06）。
pub fn assemble(
    platform: &PlatformState,
    db_path: Option<&str>,
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
    let ports = Ports::new(
        Arc::new(SandboxProvider::new()),
        Arc::new(DenyAllMembership),
    );
    let commerce = Arc::new(
        Commerce::open(path.to_str(), CommerceConfig::sandbox(secret), ports)
            .map_err(|err| anyhow::anyhow!("commerce open failed: {err}"))?,
    );

    // 回调面：无会话，只信原始 body 签名。
    let callback = Router::new()
        .route("/api/payment/callback", post(provider_callback))
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

/// 从请求头解析可信 actor（平台 sessions 表）。失败 → None → 401。
fn actor_of(platform: &PlatformState, headers: &HeaderMap) -> Option<String> {
    let token = Authed::bearer_from(headers)?;
    match platform.lookup_token(token) {
        Ok(Some(user_id)) => Some(user_id),
        _ => None,
    }
}

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

/// 错误 → HTTP 状态码 + 稳定分类。只回分类字符串，不回显供应商报文/SQL/密钥。
fn classify(err: &CommerceError) -> (StatusCode, &'static str) {
    match err {
        CommerceError::InvalidSignature => (StatusCode::UNAUTHORIZED, "INVALID_SIGNATURE"),
        CommerceError::BadInput(_) | CommerceError::Money(_) => {
            (StatusCode::BAD_REQUEST, "BAD_INPUT")
        }
        CommerceError::IdempotencyConflict => {
            (StatusCode::CONFLICT, "IDEMPOTENCY_CONFLICT")
        }
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
        CommerceError::ProviderUnavailable
        | CommerceError::ProviderTimeout
        | CommerceError::ProviderRejected
        | CommerceError::ProviderNotConfigured
        | CommerceError::MembershipUnavailable => {
            (StatusCode::BAD_GATEWAY, "PROVIDER_UNAVAILABLE")
        }
        CommerceError::Ledger(_) | CommerceError::Storage(_) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL")
        }
    }
}