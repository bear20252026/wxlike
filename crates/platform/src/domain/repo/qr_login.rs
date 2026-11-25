//! **仓储层**（`qr_login`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::ApiError;
use crate::storage::{params, OptionalExt as _, StorageConnection, StorageRow};
use axum::http::StatusCode;

use crate::domain::qr_login::db_err;

/// 第一段（持锁）：定位、验期、抢占式消费 → `(creator, device_id, label)`。
/// 锁在这里必须放下：create_session_for_device 会再拿一次库锁。
/// 按 secret 哈希定位票据 id；不存在按 404 报。
pub(in crate::domain) fn locate_ticket_by_secret(
    conn: &StorageConnection<'_>,
    secret: &str,
) -> Result<String, ApiError> {
    conn.query_row(
        "SELECT id FROM identity_qr WHERE secret_hash = ?1",
        params![wxlike_identity::digest(secret)],
        |r: &StorageRow<'_>| r.get::<_, String>(0),
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
    conn: &StorageConnection<'_>,
    ticket_id: &str,
) -> Result<(String, String), ApiError> {
    conn.query_row(
        "SELECT creator, verifier FROM qr_ticket_creator WHERE ticket_id = ?1",
        params![ticket_id],
        |r: &StorageRow<'_>| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    )
    .optional()
    .map_err(db_err)?
    .ok_or_else(|| ApiError::not_found("no such ticket"))
}

/// 扫过码的账号线索（给创建端显示 "xxx 的手机扫了码"）；没人扫过 → `None`。
pub(in crate::domain) fn scan_hint(
    conn: &StorageConnection<'_>,
    ticket_id: &str,
) -> Result<Option<String>, ApiError> {
    Ok(conn
        .query_row(
            "SELECT COALESCE(u.name, q.scanned_by) FROM identity_qr q
             LEFT JOIN users u ON u.id = q.scanned_by
             WHERE q.id = ?1 AND q.scanned_by <> ''",
            params![ticket_id],
            |r: &StorageRow<'_>| r.get(0),
        )
        .optional()
        .map_err(db_err)?
        .flatten())
}

/// 到期落 expired + 状态闸门：只有 `approved` 能被消费。
/// 与 qr_login::load 同一口径：到点的活跃票顺手落 expired。
pub(in crate::domain) fn check_qr_state(
    conn: &StorageConnection<'_>,
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

/// 兑换所需的票据字段：`(state, expires_at, device_id, label)`。
pub(in crate::domain) fn ticket_row(
    conn: &StorageConnection<'_>,
    id: &str,
) -> std::result::Result<(String, i64, String, String), ApiError> {
    conn.query_row(
        "SELECT state, expires_at, device_id, label FROM identity_qr WHERE id = ?1",
        params![id],
        |r: &StorageRow<'_>| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        },
    )
    .optional()
    .map_err(db_err)?
    .ok_or_else(|| ApiError::not_found("no such ticket"))
}

/// 抢占式消费：只有 `approved` 能推进到 `consumed`，返回受影响行数。
pub(in crate::domain) fn consume(
    conn: &StorageConnection<'_>,
    id: &str,
    now: i64,
) -> std::result::Result<usize, ApiError> {
    conn.execute(
        "UPDATE identity_qr SET state = 'consumed', consumed_at = ?2 \
         WHERE id = ?1 AND state = 'approved'",
        params![id, now],
    )
    .map_err(db_err)
}

/// 用户显示名；行不存在时返回 `None`，由调用方回落到 id 本身。
pub(in crate::domain) fn display_name(
    conn: &StorageConnection<'_>,
    user_id: &str,
) -> crate::error::Result<Option<String>> {
    conn.query_row(
        "SELECT name FROM users WHERE id = ?1",
        params![user_id],
        |r: &StorageRow<'_>| r.get::<_, String>(0),
    )
    .optional()
}

#[cfg(test)]
mod pg_tests {
    use super::{consume, creator_and_verifier, locate_ticket_by_secret, ticket_row};
    use crate::storage::params;
    use crate::storage::StorageProfile;
    use crate::Db;

    #[test]
    #[ignore = "requires WXLIKE_TEST_PG_URL and M014 applied"]
    fn postgres_qr_ticket_roundtrip_and_consume() {
        let Ok(url) = std::env::var("WXLIKE_TEST_PG_URL") else {
            return;
        };
        let db = Db::open_profile(StorageProfile::Postgres { url }, None).expect("pg db");
        let conn = db.acquire();
        let id = "pg-qr-1";
        let secret = "pg-qr-secret";
        conn.execute("DELETE FROM identity_qr WHERE id = ?1", params![id])
            .expect("clean qr");
        conn.execute(
            "INSERT INTO identity_qr(id, secret_hash, state, expires_at, created_at) \
             VALUES(?1, ?2, 'pending', ?3, ?4)",
            params![
                id,
                wxlike_identity::digest(secret),
                10_000_i64,
                1_i64
            ],
        )
        .expect("insert qr");
        conn.execute(
            "INSERT INTO qr_ticket_creator(ticket_id, creator, verifier, created_at) \
             VALUES(?1, ?2, ?3, ?4) ON CONFLICT(ticket_id) DO UPDATE SET creator = excluded.creator",
            params![id, "creator-1", "verifier-1", 1_i64],
        )
        .expect("insert creator");
        let found = locate_ticket_by_secret(&conn, secret).expect("locate");
        assert_eq!(found, id);
        let (creator, verifier) = creator_and_verifier(&conn, id).expect("creator");
        assert_eq!(creator, "creator-1");
        assert_eq!(verifier, "verifier-1");
        let (state, expires_at, device, label) = ticket_row(&conn, id).expect("row");
        assert_eq!(state, "pending");
        assert_eq!(expires_at, 10_000);
        assert!(device.is_empty());
        assert!(label.is_empty());
        assert_eq!(consume(&conn, id, 5).expect("consume"), 0);
    }
}
