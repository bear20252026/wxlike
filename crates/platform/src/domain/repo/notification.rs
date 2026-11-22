//! **仓储层**（`notification`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use rusqlite::{params, Connection, OptionalExtension as _};

pub(in crate::domain) fn set(
    conn: &Connection,
    user: &str,
    conv: &str,
    level: &str,
    keywords: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO notification_settings(user_id, conv_id, level, keywords)
         VALUES(?1, ?2, ?3, ?4)
         ON CONFLICT(user_id, conv_id) DO UPDATE SET
           level = excluded.level, keywords = excluded.keywords",
        params![user, conv, level, keywords],
    )?;
    Ok(())
}

pub(in crate::domain) fn get(
    conn: &Connection,
    user: &str,
    conv: &str,
) -> Result<Option<(String, String)>> {
    Ok(conn
        .query_row(
            "SELECT level, keywords FROM notification_settings
             WHERE user_id = ?1 AND conv_id = ?2",
            params![user, conv],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

pub(in crate::domain) fn list(
    conn: &Connection,
    user: &str,
) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT conv_id, level, keywords FROM notification_settings
         WHERE user_id = ?1 ORDER BY conv_id",
    )?;
    let rows = stmt.query_map(params![user], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}
