//! **handler 层**（`qr_login`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::PlatformState;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension as _};
use wxlike_identity::qr_login::{self};

use crate::domain::qr_login::{
    consume_qr_ticket, db_err, id_err, parse_create_body, scan_json, secret_must_match, state_str,
    QR_SECRET_HEX_LEN,
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
    let now = wxlike_core::types::now_ms() as i64;
    // verifier 是服务端持有的 PKCE 秘密（见模块头第 2 点），64 hex 满足
    // qr_login 的长度/字符集校验。
    let verifier = crate::auth::random_hex(QR_SECRET_HEX_LEN);

    let ticket = {
        // TODO(ADR-28): 同类点位（非本卡三条样板热点）——建票 + 绑 verifier 的
        // 两条写语句在 await 上下文里上锁。handler 手里有 `PlatformState`，改法
        // 与 friends.rs 一样：`offload(&platform, move |p| { .. })` 整段搬走。
        let conn = platform.db().lock();
        let ticket = qr_login::create(&conn, now).map_err(id_err)?;
        qr_login::bind(
            &conn,
            &ticket.id,
            &verifier,
            &prefs.device_id,
            &prefs.platform,
            &prefs.label,
            "",
            now,
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
        // TODO(ADR-28): 同类点位——票状态这条读在 await 上下文里上锁（改法同 `create_ticket`）。
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
        // TODO(ADR-28): 同类点位——扫码那次上锁跑在 await 上下文里（改法同 `create_ticket`）。
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
    let now = wxlike_core::types::now_ms() as i64;
    {
        // TODO(ADR-28): 同类点位——approve/reject 这两条写在 await 上下文里上锁
        // （`decide` 收的是 `PlatformState`，改造直接套 friends.rs 的 `offload` 样板）。
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
