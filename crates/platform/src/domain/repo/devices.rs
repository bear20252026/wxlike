//! **仓储层**（`devices`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::domain::devices::DeviceRow;
use crate::error::Result;
use rusqlite::{params, Connection, OptionalExtension as _};

pub(in crate::domain) fn token(
    conn: &Connection,
    user_id: &str,
    platform: &str,
) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT device_token FROM devices WHERE user_id = ?1 AND platform = ?2",
            params![user_id, platform],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
}

/// 注册同时返回旧 token（`Some` = 替换了已有行），调用方据此给出 replaced。
pub(in crate::domain) fn register(
    conn: &Connection,
    user_id: &str,
    platform: &str,
    device_token: &str,
    now: i64,
) -> Result<Option<String>> {
    let existing = token(conn, user_id, platform)?;
    conn.execute(
        "INSERT INTO devices(user_id, platform, device_token, last_seen)
         VALUES(?1, ?2, ?3, ?4)
         ON CONFLICT(user_id, platform) DO UPDATE SET
           device_token = excluded.device_token,
           last_seen = excluded.last_seen",
        params![user_id, platform, device_token, now],
    )?;
    Ok(existing)
}

pub(in crate::domain) fn list(conn: &Connection, user_id: &str) -> Result<Vec<DeviceRow>> {
    let mut stmt = conn.prepare("SELECT platform, device_token FROM devices WHERE user_id = ?1")?;
    let rows = stmt.query_map(params![user_id], |r| {
        Ok(DeviceRow {
            platform: r.get(0)?,
            device_token: r.get(1)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}
