//! **仓储层**（`secret`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use rusqlite::{params, Connection, OptionalExtension as _};

pub(in crate::domain) fn set_settings(
    conn: &Connection,
    conv_id: &str,
    enabled: bool,
    ttl_ms: i64,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO secret_settings(conv_id, enabled, ttl_ms, updated_at)
         VALUES(?1, ?2, ?3, ?4)
         ON CONFLICT(conv_id) DO UPDATE SET
           enabled = excluded.enabled,
           ttl_ms = excluded.ttl_ms,
           updated_at = excluded.updated_at",
        params![conv_id, i64::from(enabled), ttl_ms, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn settings(conn: &Connection, conv_id: &str) -> Result<Option<(i64, i64)>> {
    Ok(conn
        .query_row(
            "SELECT enabled, ttl_ms FROM secret_settings WHERE conv_id = ?1",
            params![conv_id],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?)
}

pub(in crate::domain) fn identity(conn: &Connection, user_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT identity FROM prekey_bundles WHERE user_id = ?1",
            params![user_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
}

pub(in crate::domain) fn save_verification(
    conn: &Connection,
    conv_id: &str,
    user_id: &str,
    peer_identity: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO secret_verifications(conv_id, user_id, peer_identity, verified_at)
         VALUES(?1, ?2, ?3, ?4)
         ON CONFLICT(conv_id, user_id) DO UPDATE SET
           peer_identity = excluded.peer_identity,
           verified_at = excluded.verified_at",
        params![conv_id, user_id, peer_identity, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn verification(
    conn: &Connection,
    conv_id: &str,
    user_id: &str,
) -> Result<Option<(String, i64)>> {
    Ok(conn
        .query_row(
            "SELECT peer_identity, verified_at FROM secret_verifications
             WHERE conv_id = ?1 AND user_id = ?2",
            params![conv_id, user_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?)
}
