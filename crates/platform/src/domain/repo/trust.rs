//! **仓储层**（`trust`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::domain::trust::QueuedItem;
use crate::error::Result;
use crate::storage::StorageConnection;
use rusqlite::{params, OptionalExtension as _};

#[allow(clippy::too_many_arguments)]
pub(in crate::domain) fn enqueue(
    conn: &StorageConnection<'_>,
    id: &str,
    conv_id: &str,
    sender: &str,
    text: &str,
    labels: &str,
    decision: &str,
    now: i64,
) -> Result<()> {
    let conn = conn.sqlite()?;
    conn.execute(
        "INSERT INTO moderation_queue(id, conv_id, sender, text, labels, decision, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, conv_id, sender, text, labels, decision, now],
    )?;
    Ok(())
}

/// 旧签名调用点（`Db::blocking` 闭包给的是 `&mut Connection`）的桥。
#[allow(clippy::too_many_arguments)]
pub(in crate::domain) fn enqueue_conn(
    conn: &rusqlite::Connection,
    id: &str,
    conv_id: &str,
    sender: &str,
    text: &str,
    labels: &str,
    decision: &str,
    now: i64,
) -> Result<()> {
    enqueue(
        &StorageConnection::sqlite_borrow(conn),
        id,
        conv_id,
        sender,
        text,
        labels,
        decision,
        now,
    )
}

pub(in crate::domain) fn queue(
    conn: &StorageConnection<'_>,
    limit: i64,
) -> Result<Vec<QueuedItem>> {
    let conn = conn.sqlite()?;
    let mut stmt = conn.prepare(
        "SELECT id, conv_id, sender, text, labels, created_at
         FROM moderation_queue ORDER BY created_at ASC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit], item_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(in crate::domain) fn item(
    conn: &StorageConnection<'_>,
    id: &str,
) -> Result<Option<QueuedItem>> {
    let conn = conn.sqlite()?;
    Ok(conn
        .query_row(
            "SELECT id, conv_id, sender, text, labels, created_at
             FROM moderation_queue WHERE id = ?1",
            params![id],
            item_from_row,
        )
        .optional()?)
}

/// 放行与拒绝共用：返回被删行数，调用方据此决定是否审计。
pub(in crate::domain) fn delete(conn: &StorageConnection<'_>, id: &str) -> Result<usize> {
    let conn = conn.sqlite()?;
    Ok(conn.execute("DELETE FROM moderation_queue WHERE id = ?1", params![id])?)
}

fn item_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<QueuedItem> {
    let labels: String = r.get(4)?;
    Ok(QueuedItem {
        id: r.get(0)?,
        conv_id: r.get(1)?,
        sender: r.get(2)?,
        text: r.get(3)?,
        labels: serde_json::from_str(&labels).unwrap_or_default(),
        created_at: r.get(5)?,
    })
}
