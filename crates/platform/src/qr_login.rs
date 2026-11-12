//! **handler 层**（`qr_login`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。
//!
//! 这个模块原来有三段裸 SQL（建票登记、轮询取 verifier、扫到的人是谁），
//! 还有四处 `platform.db().lock()` 直接写在 `async fn` 里。两层问题同源：
//! handler 自己写 SQL，就必然自己在 await 上下文里上锁。现在两件事一起收：
//! SQL 全部下沉到 [`crate::domain::qr_login`] / `domain::repo::qr_login`，
//! 那四处上锁搬进 [`crate::db::offload_api`]。
//!
//! `offload_api` 而不是 `offload`：本模块的领域方法返回 `ApiError`（票过期
//! 要回 410 GONE，`PlatformError` 没有对应变体），而 `offload` 的类型钉在
//! `PlatformError` 上。
//!
//! `exchange_ticket` 的两段结构**刻意不动**：它是匿名端点，"消费票"与"签发
//! 会话"必须分开——消费那段的锁在 [`crate::domain::qr_login::consume_qr_ticket`]
//! 里，本来就不跨 await。

use crate::auth::Authed;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::PlatformState;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::domain::qr_login::{
    consume_qr_ticket, parse_create_body, scan_json, secret_must_match, state_str,
};

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
    let prefs = parse_create_body(&body)?;
    let creator = authed.user_id.clone();
    let ticket =
        crate::db::offload_api(&platform, move |p| p.create_qr_ticket(&creator, &prefs)).await?;
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
    let viewer = authed.user_id.clone();
    let (state, account_hint) =
        crate::db::offload_api(&platform, move |p| p.qr_ticket_state(&ticket_id, &viewer)).await?;
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
    let scanner = authed.user_id.clone();
    // secret 是二维码里唯一能对上的东西，identity 状态机按它定位票据。
    let secret = body.secret.trim().to_string();
    let view =
        crate::db::offload_api(&platform, move |p| p.scan_qr_ticket(&secret, &scanner)).await?;
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

async fn decide(
    platform: PlatformState,
    authed: Authed,
    ticket_id: String,
    approve: bool,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let scanner = authed.user_id.clone();
    // `ticket_id` 之后还要进审计与回包，闭包只能拿走一份 clone。
    let target = ticket_id.clone();
    crate::db::offload_api(&platform, move |p| {
        p.decide_qr_ticket(&target, &scanner, approve)
    })
    .await?;
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
