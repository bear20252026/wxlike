//! **仓储层**（`content_index`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use rusqlite::{params, Connection};

use crate::domain::content_index::IndexDoc;

pub(in crate::domain) fn upsert(conn: &Connection, doc: &IndexDoc<'_>, now: i64) -> Result<()> {
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
pub(in crate::domain) fn remove(conn: &Connection, doc_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM content_index WHERE doc_id = ?1",
        params![doc_id],
    )?;
    Ok(())
}

pub(in crate::domain) fn add_media_refs(
    conn: &Connection,
    media_ids: &[String],
    ref_kind: &str,
    ref_id: &str,
    owner: &str,
    now: i64,
) -> Result<()> {
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
    conn: &Connection,
    ref_kind: &str,
    ref_id: &str,
) -> Result<()> {
    conn.execute(
        "DELETE FROM content_media_refs WHERE ref_kind = ?1 AND ref_id = ?2",
        params![ref_kind, ref_id],
    )?;
    Ok(())
}
