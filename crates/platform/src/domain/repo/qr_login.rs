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

/// 票据的创建者 + PKCE verifier（只有本 HTTP 面建出来的票才有登记）；
/// 不是这个面建出来的票对它一无所知，按不存在处理。
///
/// 两个一起回：轮询要 verifier 才能问 identity 状态机，兑换只要 creator。
/// 拆成两条查询只会让同一张表被查两次。
///
/// 返回顺序 `(creator, verifier)` 与 `SELECT` 的列序一致——两个都是 `String`，
/// 写反了编译期看不出来，只会变成 403。
pub(in crate::domain) fn creator_and_verifier(
    conn: &rusqlite::Connection,
    ticket_id: &str,
) -> Result<(String, String), ApiError> {
    conn.query_row(
        "SELECT creator, verifier FROM qr_ticket_creator WHERE ticket_id = ?1",
        params![ticket_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map_err(db_err)?
    .ok_or_else(|| ApiError::not_found("no such ticket"))
}

/// 建票时登记创建者 + PKCE verifier。
///
/// 平台自有表：identity 的状态机不知道 "creator" 是谁（它只认 secret），
/// 而轮询/兑换要判归属，所以这一行由 HTTP 面补。
pub(in crate::domain) fn register_creator(
    conn: &rusqlite::Connection,
    ticket_id: &str,
    creator: &str,
    verifier: &str,
    now: i64,
) -> Result<(), ApiError> {
    conn.execute(
        "INSERT INTO qr_ticket_creator(ticket_id, creator, verifier, created_at)
         VALUES(?1, ?2, ?3, ?4)",
        params![ticket_id, creator, verifier, now],
    )
    .map_err(db_err)?;
    Ok(())
}

/// 扫过码的账号线索（给创建端显示 "xxx 的手机扫了码"）；没人扫过 → `None`。
pub(in crate::domain) fn scan_hint(
    conn: &rusqlite::Connection,
    ticket_id: &str,
) -> Result<Option<String>, ApiError> {
    Ok(conn
        .query_row(
            "SELECT COALESCE(u.name, q.scanned_by) FROM identity_qr q
             LEFT JOIN users u ON u.id = q.scanned_by
             WHERE q.id = ?1 AND q.scanned_by <> ''",
            params![ticket_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_err)?
        .flatten())
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
