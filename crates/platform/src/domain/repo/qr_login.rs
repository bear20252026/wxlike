//! **仓储层**（`qr_login`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::ApiError;
use axum::http::StatusCode;
use rusqlite::{params, OptionalExtension as _};

use crate::domain::qr_login::db_err;

/// 第一段（持锁）：定位、验期、抢占式消费 → `(creator, device_id, label)`。
/// 锁在这里必须放下：create_session_for_device 会再拿一次库锁。
/// 按 secret 哈希定位票据 id；不存在按 404 报。
pub(in crate::domain) fn locate_ticket_by_secret(
    conn: &rusqlite::Connection,
    secret: &str,
) -> Result<String, ApiError> {
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
pub(in crate::domain) fn ticket_creator(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<String, ApiError> {
    conn.query_row(
        "SELECT creator FROM qr_ticket_creator WHERE ticket_id = ?1",
        params![id],
        |r| r.get(0),
    )
    .optional()
    .map_err(db_err)?
    .ok_or_else(|| ApiError::not_found("no such ticket"))
}

/// 到期落 expired + 状态闸门：只有 `approved` 能被消费。
/// 与 qr_login::load 同一口径：到点的活跃票顺手落 expired。
pub(in crate::domain) fn check_qr_state(
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
