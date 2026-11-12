//! **仓储层**（`status`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::{PlatformError, Result};
use rusqlite::{params, Connection, OptionalExtension as _};

use crate::domain::status::{
    StatusInsert, StatusRow, StatusView, COL_STATUS_EXPIRES_AT, COL_STATUS_REACTIONS,
};

/// 事务内写状态行 + 内容索引 + 媒体引用（删状态/到期清扫用同一索引约定）。
pub(in crate::domain) fn insert_status_indexed(tx: &Connection, s: StatusInsert<'_>) -> Result<()> {
    tx.execute(
        "INSERT INTO statuses(id, owner, kind, text, media_id, background_media_id, audience,
                              version, created_at, updated_at, expires_at, cleared_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?8, ?9, NULL)",
        params![
            s.id,
            s.owner,
            s.kind,
            s.text,
            s.media_id,
            s.background,
            s.audience,
            s.now,
            s.expires_at
        ],
    )?;
    crate::domain::repo::content_index::upsert(
        tx,
        &crate::domain::content_index::IndexDoc {
            doc_id: s.id,
            kind: "status",
            owner: s.owner,
            audience: s.audience,
            group_name: "",
            title: "",
            body: s.text,
        },
        s.now,
    )?;
    let mut refs = Vec::new();
    if !s.media_id.is_empty() {
        refs.push(s.media_id.to_string());
    }
    if !s.background.is_empty() {
        refs.push(s.background.to_string());
    }
    crate::domain::repo::content_index::add_media_refs(tx, &refs, "status", s.id, s.owner, s.now)?;
    Ok(())
}

pub(in crate::domain) fn clear_owner(conn: &Connection, owner: &str, now: i64) -> Result<usize> {
    let ids: Vec<String> = {
        let mut stmt =
            conn.prepare("SELECT id FROM statuses WHERE owner = ?1 AND cleared_at IS NULL")?;
        let it = stmt.query_map(params![owner], |r| r.get::<_, String>(0))?;
        it.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for id in &ids {
        conn.execute(
            "UPDATE statuses SET cleared_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        crate::domain::repo::content_index::remove(conn, id)?;
        crate::domain::repo::content_index::remove_media_refs(conn, "status", id)?;
        crate::domain::repo::favorites::revoke_source(conn, "status", id)?;
    }
    Ok(ids.len())
}

pub(in crate::domain) fn check_media(
    c: &Connection,
    owner: &str,
    media_id: &str,
    kind: &str,
) -> Result<()> {
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT uploader, content_type FROM media WHERE id = ?1",
            params![media_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((uploader, content_type)) = row else {
        return Err(PlatformError::BadInput("unknown media id"));
    };
    if uploader != owner {
        return Err(PlatformError::Forbidden);
    }
    match kind {
        "image" => {
            if !content_type.starts_with("image/") {
                return Err(PlatformError::BadInput("background must be an image"));
            }
        }
        "video" => {
            let state: Option<String> = c
                .query_row(
                    "SELECT state FROM video_refs WHERE video_id = ?1",
                    params![media_id],
                    |r| r.get(0),
                )
                .optional()?;
            if state.as_deref() != Some("succeeded") {
                return Err(PlatformError::BadInput("status video is not playable"));
            }
        }
        _ => return Err(PlatformError::BadInput("unknown status kind")),
    }
    Ok(())
}

pub(in crate::domain) fn status_row(
    c: &Connection,
    viewer: &str,
    id: &str,
) -> Result<Option<StatusView>> {
    let now = wxlike_core::types::now_ms() as i64;
    let row: Option<StatusRow> = c
        .query_row(
            "SELECT s.id, s.owner, u.name, s.kind, s.text, s.media_id, s.background_media_id,
                    s.audience, s.version, s.created_at, s.updated_at, s.expires_at,
                    (SELECT COUNT(*) FROM status_reactions r WHERE r.status_id = s.id)
             FROM statuses s JOIN users u ON u.id = s.owner
             WHERE s.id = ?1",
            params![id],
            |r| {
                Ok(StatusRow {
                    id: r.get(0)?,
                    owner: r.get(1)?,
                    owner_name: r.get(2)?,
                    kind: r.get(3)?,
                    text: r.get(4)?,
                    media_id: r.get(5)?,
                    background_media_id: r.get(6)?,
                    audience: r.get(7)?,
                    version: r.get(8)?,
                    created_at: r.get(9)?,
                    updated_at: r.get(10)?,
                    expires_at: r.get(COL_STATUS_EXPIRES_AT)?,
                    reactions: r.get(COL_STATUS_REACTIONS)?,
                })
            },
        )
        .optional()?;
    let Some(row) = row else {
        return Ok(None);
    };
    let my_reaction: i64 = c.query_row(
        "SELECT COUNT(*) FROM status_reactions WHERE status_id = ?1 AND user_id = ?2",
        params![row.id, viewer],
        |r| r.get(0),
    )?;
    Ok(Some(StatusView {
        id: row.id,
        owner: row.owner,
        owner_name: row.owner_name,
        kind: row.kind,
        text: row.text,
        media_id: row.media_id,
        background_media_id: row.background_media_id,
        audience: row.audience,
        version: row.version,
        created_at: row.created_at,
        updated_at: row.updated_at,
        expires_at: row.expires_at,
        seconds_left: ((row.expires_at - now).max(0)) / 1000,
        reactions: row.reactions,
        my_reaction: my_reaction > 0,
    }))
}
