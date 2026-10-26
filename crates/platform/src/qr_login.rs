//! 扫码登录的 HTTP 传输层（G02-07，审计第 5 轮挂载）。
//!
//! 分层与 crate 约定一致：**规则在 `crates/features/identity/src/qr_login.rs`
//! 只实现一次**（状态机、PKCE、原子抢占），本模块只做参数解析、鉴权主体
//! 透传与 JSON 形状——identity 没有 axum 依赖，HTTP 面归平台。
//!
//! # 路由契约（客户端桥按这套写，改动即破坏）
//!
//! | 方法 | 路径 | 鉴权 | 语义 |
//! |------|------|------|------|
//! | POST | `/api/qr/tickets` | Bearer（创建者） | 建票，返回 `{ticket_id, secret, payload, expires_at_ms}`，`payload = wxlike-qr://{ticket_id}/{secret}` 是二维码内容 |
//! | GET  | `/api/qr/tickets/{id}` | Bearer 创建者 | 轮询 `{state, account_hint?}`，state ∈ CREATED \| SCANNED \| APPROVED \| EXPIRED \| REJECTED |
//! | POST | `/api/qr/tickets/{id}/scan` | Bearer 扫码者 | body `{secret}`：核验 secret、推进到 SCANNED、回设备线索 |
//! | POST | `/api/qr/tickets/{id}/approve` | Bearer 扫码者 | 批准 |
//! | POST | `/api/qr/tickets/{id}/reject` | Bearer 扫码者 | 拒绝 |
//! | POST | `/api/qr/tickets/{id}/exchange` | **匿名**（这就是要登录的那台设备）| body `{secret}`：APPROVED 后一次性兑换，返回创建者账号的真实会话 `{token, user_id, name}` |
//!
//! # 与 identity 状态机的两点契约性偏差（都记在这里，防止"改回去"）
//!
//! 1. **兑换发给创建者的账号**。`qr_login::exchange` 的语义是"新设备登录成
//!    扫码者的账号"（WeChat 流：桌面出码、手机扫码）。本契约相反：创建者
//!    已认证、想让**另一台设备**以自己身份登录，所以兑换发的是创建者会话。
//!    抢占式 `approved → consumed` CAS 与 `qr_login::exchange` 同一形状，
//!    只是会话签发对象不同——核心状态机没有被改写。
//! 2. **PKCE verifier 由服务端持有**（`qr_ticket_creator.verifier`）。
//!    契约的轮询/兑换不带 verifier，桌面端拿到的是 `secret`（进二维码）。
//!    verifier 留在服务端保证：轮询（GET）只对创建者开放；兑换仍以持有
//!    secret 为凭——这是契约接受的边界，别把它当漏洞修掉，改契约要先改桥。

use axum::body::Bytes;

/// QR 票据 secret 的十六进制长度（与 identity::qr_login 生成侧一致）。
const QR_SECRET_HEX_LEN: usize = 32;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension as _};
use wxlike_identity::qr_login::{self, PollState, ScanView};

use crate::auth::Authed;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::{Platform, PlatformState};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/qr/tickets", post(create_ticket))
        .route("/api/qr/tickets/{ticket_id}", get(ticket_state))
        .route("/api/qr/tickets/{ticket_id}/scan", post(scan_ticket))
        .route("/api/qr/tickets/{ticket_id}/approve", post(approve_ticket))
        .route("/api/qr/tickets/{ticket_id}/reject", post(reject_ticket))
        .route(
            "/api/qr/tickets/{ticket_id}/exchange",
            post(exchange_ticket),
        )
}

/// 建票时可带的设备线索（都可省，省了用默认值）。
#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct CreateBody {
    device_id: Option<String>,
    platform: Option<String>,
    label: Option<String>,
}

/// `{secret}` —— 扫码与兑换共用。
#[derive(serde::Deserialize)]
struct SecretBody {
    secret: String,
}

async fn create_ticket(
    State(platform): State<PlatformState>,
    authed: Authed,
    body: Bytes,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    // 契约没规定建票 body；空 body 与 `{}` 等价（带线索则透传给 bind）。
    let body: CreateBody = if body.is_empty() {
        CreateBody::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|err| ApiError::bad_request(format!("invalid ticket body: {err}")))?
    };
    let device_id = body
        .device_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("qr-{}", crate::auth::random_hex(8)));
    let plat = body
        .platform
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("web");
    let label = body
        .label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("QR login");
    let now = wxlike_core::types::now_ms() as i64;
    // verifier 是服务端持有的 PKCE 秘密（见模块头第 2 点），64 hex 满足
    // qr_login 的长度/字符集校验。
    let verifier = crate::auth::random_hex(QR_SECRET_HEX_LEN);

    let ticket = {
        let conn = platform.db().lock();
        let ticket = qr_login::create(&conn, now).map_err(id_err)?;
        qr_login::bind(
            &conn, &ticket.id, &verifier, &device_id, plat, label, "", now,
        )
        .map_err(id_err)?;
        conn.execute(
            "INSERT INTO qr_ticket_creator(ticket_id, creator, verifier, created_at) \
             VALUES(?1, ?2, ?3, ?4)",
            params![ticket.id, authed.user_id, verifier, now],
        )
        .map_err(db_err)?;
        ticket
    };
    platform.audit(&authed.user_id, "qr.ticket.create", &ticket.id, "");
    Ok(Json(serde_json::json!({
        "ticket_id": ticket.id,
        "secret": ticket.secret,
        "payload": format!("wxlike-qr://{}/{}", ticket.id, ticket.secret),
        "expires_at_ms": ticket.expires_at,
    })))
}

async fn ticket_state(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(ticket_id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let now = wxlike_core::types::now_ms() as i64;
    let (state, account_hint) = {
        let conn = platform.db().lock();
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT verifier, creator FROM qr_ticket_creator WHERE ticket_id = ?1",
                params![ticket_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(db_err)?;
        let Some((verifier, creator)) = row else {
            return Err(ApiError::not_found("no such ticket"));
        };
        if creator != authed.user_id {
            return Err(ApiError::forbidden("not your ticket"));
        }
        let state = qr_login::poll(&conn, &ticket_id, &verifier, now).map_err(id_err)?;
        // 扫过码才给提示：让创建端能显示"xxx 的手机扫了码"。
        let hint: Option<String> = conn
            .query_row(
                "SELECT COALESCE(u.name, q.scanned_by) FROM identity_qr q \
                 LEFT JOIN users u ON u.id = q.scanned_by \
                 WHERE q.id = ?1 AND q.scanned_by <> ''",
                params![ticket_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err)?;
        (state, hint)
    };
    let mut v = serde_json::json!({ "state": state_str(state) });
    if let Some(hint) = account_hint {
        v["account_hint"] = serde_json::Value::String(hint);
    }
    Ok(Json(v))
}

async fn scan_ticket(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(ticket_id): Path<String>,
    ApiJson(body): ApiJson<SecretBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let now = wxlike_core::types::now_ms() as i64;
    let view = {
        let conn = platform.db().lock();
        // qr_login::scan 按 secret 定位（secret 是二维码里唯一能对上的东西）
        qr_login::scan(&conn, body.secret.trim(), &authed.user_id, now).map_err(id_err)?
    };
    secret_must_match(&ticket_id, &view.challenge_id)?;
    Ok(scan_json(&view))
}

async fn approve_ticket(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(ticket_id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    decide(platform, authed, ticket_id, true).await
}

async fn reject_ticket(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(ticket_id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    decide(platform, authed, ticket_id, false).await
}

async fn decide(
    platform: PlatformState,
    authed: Authed,
    ticket_id: String,
    approve: bool,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let now = wxlike_core::types::now_ms() as i64;
    {
        let conn = platform.db().lock();
        let outcome = if approve {
            qr_login::approve(&conn, &ticket_id, &authed.user_id, now)
        } else {
            qr_login::reject(&conn, &ticket_id, &authed.user_id, now)
        };
        outcome.map_err(id_err)?;
    }
    platform.audit(
        &authed.user_id,
        if approve {
            "qr.ticket.approve"
        } else {
            "qr.ticket.reject"
        },
        &ticket_id,
        "",
    );
    Ok(Json(serde_json::json!({
        "ticket_id": ticket_id,
        "state": if approve { "APPROVED" } else { "REJECTED" },
    })))
}

/// 兑换：**匿名**（要登录的那台设备还没有凭据），凭 `secret` 一次性换取
/// 创建者账号的会话。`approved → consumed` 的 CAS 保证两台设备同时兑换只有
/// 一台成功。
async fn exchange_ticket(
    State(platform): State<PlatformState>,
    Path(ticket_id): Path<String>,
    ApiJson(body): ApiJson<SecretBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let secret = body.secret.trim();
    if secret.is_empty() {
        return Err(ApiError::unprocessable("secret is required"));
    }
    let now = wxlike_core::types::now_ms() as i64;

    // 第一段（持锁）：定位、验期、抢占式消费。锁在函数里放下：
    // create_session_for_device 会再拿一次库锁。
    let (creator, device_id, label) = consume_qr_ticket(&platform, &ticket_id, secret, now)?;

    // 第二段（无锁）：签发创建者的真实会话——与 /api/login 同一条路径。
    let token = platform
        .create_session_for_device(
            &creator,
            platform.options().session_ttl_ms,
            &device_id,
            &label,
            "",
        )
        .map_err(ApiError::from)?;
    let name = platform.display_name_of(&creator).map_err(ApiError::from)?;
    platform.audit(&creator, "qr.ticket.exchange", &ticket_id, "session issued");
    Ok(Json(serde_json::json!({
        "token": token,
        "user_id": creator,
        "name": name,
    })))
}

/// 第一段（持锁）：定位、验期、抢占式消费 → `(creator, device_id, label)`。
/// 锁在这里必须放下：create_session_for_device 会再拿一次库锁。
/// 按 secret 哈希定位票据 id；不存在按 404 报。
fn locate_ticket_by_secret(conn: &rusqlite::Connection, secret: &str) -> Result<String, ApiError> {
    conn.query_row(
        "SELECT id FROM identity_qr WHERE secret_hash = ?1",
        params![wxlike_identity::digest(secret)],
        |r| r.get(0),
    )
    .optional()
    .map_err(db_err)?
    .ok_or_else(|| ApiError::not_found("no such ticket"))
}

/// 票据的创建者（只有本 HTTP 面建出来的票才有登记）；
/// 不是这个面建出来的票对它一无所知，按不存在处理。
fn ticket_creator(conn: &rusqlite::Connection, id: &str) -> Result<String, ApiError> {
    conn.query_row(
        "SELECT creator FROM qr_ticket_creator WHERE ticket_id = ?1",
        params![id],
        |r| r.get(0),
    )
    .optional()
    .map_err(db_err)?
    .ok_or_else(|| ApiError::not_found("no such ticket"))
}

fn consume_qr_ticket(
    platform: &Platform,
    ticket_id: &str,
    secret: &str,
    now: i64,
) -> std::result::Result<(String, String, String), ApiError> {
    let conn = platform.db().lock();
    let id = locate_ticket_by_secret(&conn, secret)?;
    secret_must_match(ticket_id, &id)?;
    let creator = ticket_creator(&conn, &id)?;

    let (state, expires_at, device_id, label): (String, i64, String, String) = conn
        .query_row(
            "SELECT state, expires_at, device_id, label FROM identity_qr WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(db_err)?
        .ok_or_else(|| ApiError::not_found("no such ticket"))?;
    check_qr_state(&conn, &id, &state, expires_at, now)?;

    let consumed = conn
        .execute(
            "UPDATE identity_qr SET state = 'consumed', consumed_at = ?2 \
             WHERE id = ?1 AND state = 'approved'",
            params![id, now],
        )
        .map_err(db_err)?;
    if consumed != 1 {
        return Err(ApiError::conflict("ticket already used"));
    }
    Ok((creator, device_id, label))
}

/// 到期落 expired + 状态闸门：只有 `approved` 能被消费。
/// 与 qr_login::load 同一口径：到点的活跃票顺手落 expired。
fn check_qr_state(
    conn: &rusqlite::Connection,
    id: &str,
    state: &str,
    expires_at: i64,
    now: i64,
) -> std::result::Result<(), ApiError> {
    if expires_at <= now && matches!(state, "pending" | "scanned" | "approved") {
        conn.execute(
            "UPDATE identity_qr SET state = 'expired' \
             WHERE id = ?1 AND state IN ('pending','scanned','approved')",
            params![id],
        )
        .map_err(db_err)?;
        return Err(ApiError::new(StatusCode::GONE, "ticket expired"));
    }
    match state {
        "approved" => Ok(()),
        "consumed" => Err(ApiError::conflict("ticket already used")),
        "rejected" => Err(ApiError::conflict("ticket was rejected")),
        "expired" => Err(ApiError::new(StatusCode::GONE, "ticket expired")),
        _ => Err(ApiError::conflict("ticket not approved yet")),
    }
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

fn secret_must_match(path_id: &str, secret_id: &str) -> std::result::Result<(), ApiError> {
    if path_id == secret_id {
        Ok(())
    } else {
        // 路径与 secret 指向不同的票 = 调用方拼错了；宁可 400 也不猜
        Err(ApiError::bad_request("secret does not match this ticket"))
    }
}

fn scan_json(view: &ScanView) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ticket_id": view.challenge_id,
        "state": "SCANNED",
        "platform": view.platform,
        "label": view.label,
        "ip_hint": view.ip_hint,
    }))
}

/// 契约的 state 名 ←→ `qr_login::PollState`。
///
/// `WAITING_APPROVAL` 在 identity 的状态机里没有独立态（scanned 即等待批准），
/// 契约集合里保留这个名字，映射取更直接的 `SCANNED`。
fn state_str(state: PollState) -> &'static str {
    match state {
        PollState::Pending => "CREATED",
        PollState::Scanned => "SCANNED",
        PollState::Approved => "APPROVED",
        PollState::Rejected => "REJECTED",
        PollState::Expired => "EXPIRED",
    }
}

/// identity 错误 → HTTP。`Display` 不含凭据（identity 的约定），可以进消息；
/// 只有 Db 分支换 generic 并落日志。
fn id_err(err: wxlike_identity::IdError) -> ApiError {
    use wxlike_identity::IdError;
    match err {
        IdError::BadInput(what) => ApiError::unprocessable(format!("bad input: {what}")),
        IdError::Unauthorized(what) => ApiError::unauthorized(format!("unauthorized: {what}")),
        IdError::Forbidden(what) => ApiError::forbidden(format!("forbidden: {what}")),
        IdError::NotFound(what) => ApiError::not_found(format!("no such {what}")),
        IdError::Conflict(what) => ApiError::conflict(what),
        IdError::Expired => ApiError::new(StatusCode::GONE, "ticket expired"),
        IdError::RateLimited { retry_after_ms } => ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            format!("rate limited, retry after {retry_after_ms} ms"),
        ),
        IdError::Db(err) => db_err(err),
        IdError::Json(err) => {
            tracing::error!(error = %err, "qr login: encoding failure");
            ApiError::internal("encoding failure")
        }
    }
}

fn db_err(err: rusqlite::Error) -> ApiError {
    tracing::error!(error = %err, "qr login: database failure");
    ApiError::internal("database failure")
}

impl Platform {
    /// 用户显示名（兑换回包 `{name}` 用；用户不存在时退回 id 本身）。
    pub fn display_name_of(&self, user_id: &str) -> crate::error::Result<String> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT name FROM users WHERE id = ?1",
            params![user_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .unwrap_or_else(|| user_id.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_state_names_match_the_contract() {
        assert_eq!(state_str(PollState::Pending), "CREATED");
        assert_eq!(state_str(PollState::Scanned), "SCANNED");
        assert_eq!(state_str(PollState::Approved), "APPROVED");
        assert_eq!(state_str(PollState::Rejected), "REJECTED");
        assert_eq!(state_str(PollState::Expired), "EXPIRED");
    }
}
