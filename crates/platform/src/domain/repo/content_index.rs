//! **仓储层**（`content_index`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use crate::storage::StorageConnection;
use rusqlite::{params, OptionalExtension as _};

use crate::domain::content_index::IndexDoc;

pub(in crate::domain) fn upsert(
    conn: &StorageConnection<'_>,
    doc: &IndexDoc<'_>,
    now: i64,
) -> Result<()> {
    let conn = conn.sqlite()?;
    conn.execute(
        "INSERT INTO content_index(doc_id, kind, owner, audience, group_name, title, body,
                                   created_at, updated_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
         ON CONFLICT(doc_id) DO UPDATE SET
           kind = excluded.kind, owner = excluded.owner, audience = excluded.audience,
           group_name = excluded.group_name, title = excluded.title, body = excluded.body,
           updated_at = excluded.updated_at",
        params![
            doc.doc_id,
            doc.kind,
            doc.owner,
            doc.audience,
            doc.group_name,
            doc.title,
            doc.body,
            now
        ],
    )?;
    Ok(())
}

/// 删除内容时同步调用：索引行消失后，检索不可能再命中已删内容。
pub(in crate::domain) fn remove(conn: &StorageConnection<'_>, doc_id: &str) -> Result<()> {
    let conn = conn.sqlite()?;
    conn.execute(
        "DELETE FROM content_index WHERE doc_id = ?1",
        params![doc_id],
    )?;
    Ok(())
}

pub(in crate::domain) fn add_media_refs(
    conn: &StorageConnection<'_>,
    media_ids: &[String],
    ref_kind: &str,
    ref_id: &str,
    owner: &str,
    now: i64,
) -> Result<()> {
    let conn = conn.sqlite()?;
    for media_id in media_ids {
        conn.execute(
            "INSERT OR IGNORE INTO content_media_refs(media_id, ref_kind, ref_id, owner, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![media_id, ref_kind, ref_id, owner, now],
        )?;
    }
    Ok(())
}

pub(in crate::domain) fn remove_media_refs(
    conn: &StorageConnection<'_>,
    ref_kind: &str,
    ref_id: &str,
) -> Result<()> {
    let conn = conn.sqlite()?;
    conn.execute(
        "DELETE FROM content_media_refs WHERE ref_kind = ?1 AND ref_id = ?2",
        params![ref_kind, ref_id],
    )?;
    Ok(())
}

/// 文章类候选的授权回源：只认已发布状态。
pub(in crate::domain) fn article_published(
    conn: &StorageConnection<'_>,
    doc_id: &str,
) -> Result<bool> {
    let conn = conn.sqlite()?;
    Ok(conn
        .query_row(
            "SELECT 1 FROM articles WHERE id = ?1 AND state = 'published'",
            params![doc_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// 单个域的一批候选，只有 id 与排序键，正文另外按条取。
pub(in crate::domain) fn candidates(
    conn: &StorageConnection<'_>,
    pattern: &str,
    kind: &str,
    before_at: i64,
    before_id: &str,
    limit: i64,
) -> Result<Vec<(String, i64)>> {
    let conn = conn.sqlite()?;
    let mut stmt = conn.prepare(
        "SELECT doc_id, created_at FROM content_index
         WHERE kind = ?2 AND (title LIKE ?1 ESCAPE '\\' OR body LIKE ?1 ESCAPE '\\')
           AND (created_at < ?3 OR (created_at = ?3 AND doc_id < ?4))
         ORDER BY created_at DESC, doc_id DESC LIMIT ?5",
    )?;
    let rows = stmt.query_map(params![pattern, kind, before_at, before_id, limit], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub(in crate::domain) fn body(
    conn: &StorageConnection<'_>,
    doc_id: &str,
) -> Result<Option<(String, String)>> {
    let conn = conn.sqlite()?;
    Ok(conn
        .query_row(
            "SELECT title, body FROM content_index WHERE doc_id = ?1",
            params![doc_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?)
}

/// 热搜窗口内的公开语料，排序与计数逻辑留在 domain。
pub(in crate::domain) fn trend_rows(
    conn: &StorageConnection<'_>,
    cutoff: i64,
) -> Result<Vec<(String, String, String)>> {
    let conn = conn.sqlite()?;
    let mut stmt = conn.prepare(
        "SELECT owner, title, body FROM content_index
         WHERE audience = 'public' AND created_at >= ?1",
    )?;
    let it = stmt.query_map(params![cutoff], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    Ok(it.collect::<std::result::Result<Vec<_>, _>>()?)
}

// ── 事务内桥接 ─────────────────────────────────────────────────────────────
//
// 尚未迁端口的内容域（articles / moments / status / video_feed）仍在
// `with_tx` 闭包里持 `&Transaction`。这里只把事务借成端口，SQL 仍只有一份。

pub(in crate::domain) fn upsert_tx(
    tx: &rusqlite::Transaction<'_>,
    doc: &IndexDoc<'_>,
    now: i64,
) -> Result<()> {
    upsert(&StorageConnection::sqlite_borrow(tx), doc, now)
}

pub(in crate::domain) fn remove_tx(tx: &rusqlite::Transaction<'_>, doc_id: &str) -> Result<()> {
    remove(&StorageConnection::sqlite_borrow(tx), doc_id)
}

pub(in crate::domain) fn add_media_refs_tx(
    tx: &rusqlite::Transaction<'_>,
    media_ids: &[String],
    ref_kind: &str,
    ref_id: &str,
    owner: &str,
    now: i64,
) -> Result<()> {
    add_media_refs(
        &StorageConnection::sqlite_borrow(tx),
        media_ids,
        ref_kind,
        ref_id,
        owner,
        now,
    )
}

pub(in crate::domain) fn remove_media_refs_tx(
    tx: &rusqlite::Transaction<'_>,
    ref_kind: &str,
    ref_id: &str,
) -> Result<()> {
    remove_media_refs(&StorageConnection::sqlite_borrow(tx), ref_kind, ref_id)
}
