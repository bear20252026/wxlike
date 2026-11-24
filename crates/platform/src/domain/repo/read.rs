//! **仓储层**（`read`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use crate::storage::StorageConnection;
use rusqlite::{params, OptionalExtension as _};

pub(in crate::domain) fn owner_kind(
    conn: &StorageConnection<'_>,
    conv: &str,
) -> Result<Option<String>> {
    let conn = conn.sqlite()?;
    Ok(conn
        .query_row(
            "SELECT owner_type FROM conv_owner WHERE conv_id = ?1",
            params![conv],
            |r| r.get(0),
        )
        .optional()?)
}

/// 登记会话属主（建群 / 建频道时调用）。幂等。
pub(in crate::domain) fn set_owner_kind(
    conn: &StorageConnection<'_>,
    conv: &str,
    name: &str,
) -> Result<()> {
    let conn = conn.sqlite()?;
    conn.execute(
        "INSERT INTO conv_owner(conv_id, owner_type) VALUES(?1, ?2)
         ON CONFLICT(conv_id) DO UPDATE SET owner_type = excluded.owner_type",
        params![conv, name],
    )?;
    Ok(())
}
