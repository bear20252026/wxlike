//! **仓储层**（`favorites`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use crate::storage::StorageConnection;
use rusqlite::{params, OptionalExtension as _};

use crate::domain::favorites::{
    kind_or_text, Favorite, FavoriteInput, COL_REVOKED, COL_SNAPSHOT_LABEL, COL_SORT_ORDER,
    COL_SOURCE_ID, COL_SOURCE_KIND,
};

/// 对象收藏的服务端快照原料；摘要裁剪仍在 domain。
pub(in crate::domain) struct SnapshotSource {
    pub(crate) text: String,
    pub(crate) label: String,
}

pub(in crate::domain) fn object_favorite_exists(
    c: &StorageConnection<'_>,
    user: &str,
    kind: &str,
    source_id: &str,
) -> Result<bool> {
    let c = c.sqlite()?;
    Ok(c.query_row(
        "SELECT 1 FROM favorites WHERE user_id = ?1 AND source_kind = ?2 AND source_id = ?3",
        params![user, kind, source_id],
        |_| Ok(()),
    )
    .optional()?
    .is_some())
}

#[allow(clippy::too_many_arguments)]
pub(in crate::domain) fn insert_object_favorite(
    c: &StorageConnection<'_>,
    fav_id: &str,
    user: &str,
    kind: &str,
    source_id: &str,
    text: &str,
    label: &str,
    now: i64,
) -> Result<()> {
    let c = c.sqlite()?;
    c.execute(
        "INSERT INTO favorites(id, user_id, source_conv, source_msg_id, kind, text,
                               media_id, sender, sent_at_ms, created_at,
                               source_kind, source_id, snapshot_label, sort_order, revoked)
         VALUES(?1, ?2, '', '', ?3, ?4, '', '', 0, ?5, ?3, ?6, ?7, 0, 0)
         ON CONFLICT DO NOTHING",
        params![fav_id, user, kind, text, now, source_id, label],
    )?;
    Ok(())
}

pub(in crate::domain) fn object_favorite_id(
    c: &StorageConnection<'_>,
    user: &str,
    kind: &str,
    source_id: &str,
) -> Result<Option<String>> {
    let c = c.sqlite()?;
    Ok(c.query_row(
        "SELECT id FROM favorites
         WHERE user_id = ?1 AND source_kind = ?2 AND source_id = ?3",
        params![user, kind, source_id],
        |r| r.get::<_, String>(0),
    )
    .optional()?)
}

/// 从权威表读一小段快照；对象不存在或未发布返回 `None`。
pub(in crate::domain) fn snapshot_source(
    c: &StorageConnection<'_>,
    kind: &str,
    id: &str,
) -> Result<Option<SnapshotSource>> {
    let c = c.sqlite()?;
    let row = match kind {
        "moment" => c
            .query_row(
                "SELECT text FROM moments WHERE id = ?1 AND deleted = 0",
                params![id],
                |r| {
                    Ok(SnapshotSource {
                        text: r.get(0)?,
                        label: String::new(),
                    })
                },
            )
            .optional()?,
        "article" => c
            .query_row(
                "SELECT title, body_text FROM articles WHERE id = ?1 AND state = 'published'",
                params![id],
                |r| {
                    Ok(SnapshotSource {
                        text: r.get(1)?,
                        label: r.get(0)?,
                    })
                },
            )
            .optional()?,
        "video" => c
            .query_row(
                "SELECT caption FROM video_posts WHERE id = ?1 AND state = 'published'",
                params![id],
                |r| {
                    Ok(SnapshotSource {
                        text: r.get(0)?,
                        label: String::new(),
                    })
                },
            )
            .optional()?,
        "status" => c
            .query_row(
                "SELECT text FROM statuses WHERE id = ?1 AND cleared_at IS NULL",
                params![id],
                |r| {
                    Ok(SnapshotSource {
                        text: r.get(0)?,
                        label: String::new(),
                    })
                },
            )
            .optional()?,
        _ => None,
    };
    Ok(row)
}

pub(in crate::domain) fn list_page(
    c: &StorageConnection<'_>,
    user: &str,
    before: i64,
    limit: i64,
) -> Result<Vec<Favorite>> {
    let c = c.sqlite()?;
    let mut stmt = c.prepare(
        "SELECT id, user_id, source_conv, source_msg_id, kind, text, media_id,
                sender, sent_at_ms, created_at, source_kind, source_id,
                snapshot_label, sort_order, revoked
         FROM favorites WHERE user_id = ?1 AND created_at < ?2
         ORDER BY created_at DESC, id DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![user, before, limit], read_row)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub(in crate::domain) fn sorted_page(
    c: &StorageConnection<'_>,
    user: &str,
    sort: i64,
    created: i64,
    cursor_id: &str,
    limit: i64,
) -> Result<Vec<Favorite>> {
    let c = c.sqlite()?;
    let mut stmt = c.prepare(
        "SELECT id, user_id, source_conv, source_msg_id, kind, text, media_id,
                sender, sent_at_ms, created_at, source_kind, source_id,
                snapshot_label, sort_order, revoked
         FROM favorites
         WHERE user_id = ?1
           AND (sort_order > ?2
                OR (sort_order = ?2 AND (created_at < ?3
                    OR (created_at = ?3 AND id < ?4))))
         ORDER BY sort_order ASC, created_at DESC, id DESC LIMIT ?5",
    )?;
    let it = stmt.query_map(params![user, sort, created, cursor_id, limit], read_row)?;
    Ok(it.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub(in crate::domain) fn search_page(
    c: &StorageConnection<'_>,
    user: &str,
    pattern: &str,
    limit: i64,
) -> Result<Vec<Favorite>> {
    let c = c.sqlite()?;
    let mut stmt = c.prepare(
        "SELECT id, user_id, source_conv, source_msg_id, kind, text, media_id,
                sender, sent_at_ms, created_at, source_kind, source_id,
                snapshot_label, sort_order, revoked
         FROM favorites
         WHERE user_id = ?1 AND revoked = 0
           AND (text LIKE ?2 ESCAPE '\\' OR snapshot_label LIKE ?2 ESCAPE '\\'
                OR kind LIKE ?2 ESCAPE '\\')
         ORDER BY sort_order ASC, created_at DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![user, pattern, limit], read_row)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub(in crate::domain) fn update_fields(
    c: &StorageConnection<'_>,
    user: &str,
    id: &str,
    label: Option<&str>,
    sort_order: Option<i64>,
) -> Result<usize> {
    let c = c.sqlite()?;
    Ok(c.execute(
        "UPDATE favorites SET
           snapshot_label = COALESCE(?1, snapshot_label),
           sort_order = COALESCE(?2, sort_order)
         WHERE id = ?3 AND user_id = ?4",
        params![label, sort_order, id, user],
    )?)
}

pub(in crate::domain) fn owned_exists(
    c: &StorageConnection<'_>,
    user: &str,
    id: &str,
) -> Result<bool> {
    let c = c.sqlite()?;
    let owned: Option<i64> = c
        .query_row(
            "SELECT 1 FROM favorites WHERE id = ?1 AND user_id = ?2",
            params![id, user],
            |r| r.get(0),
        )
        .optional()?;
    Ok(owned.is_some())
}

pub(in crate::domain) fn set_sort_order(
    c: &StorageConnection<'_>,
    user: &str,
    id: &str,
    order: i64,
) -> Result<()> {
    let c = c.sqlite()?;
    c.execute(
        "UPDATE favorites SET sort_order = ?1 WHERE id = ?2 AND user_id = ?3",
        params![order, id, user],
    )?;
    Ok(())
}

pub(in crate::domain) fn delete_one(
    c: &StorageConnection<'_>,
    user: &str,
    id: &str,
) -> Result<usize> {
    let c = c.sqlite()?;
    Ok(c.execute(
        "DELETE FROM favorites WHERE id = ?1 AND user_id = ?2",
        params![id, user],
    )?)
}
/// 源对象删除/撤权/到期时调用（与删除同事务）。只标记，不删用户的行：
/// 用户仍能看到"这条收藏已不可用"，且排序位置保持不变。
pub(in crate::domain) fn revoke_source(
    conn: &StorageConnection<'_>,
    source_kind: &str,
    source_id: &str,
) -> Result<()> {
    let conn = conn.sqlite()?;
    if source_kind == "message" || source_id.is_empty() {
        return Ok(());
    }
    conn.execute(
        "UPDATE favorites SET revoked = 1, text = '', media_id = '', snapshot_label = ''
         WHERE source_kind = ?1 AND source_id = ?2",
        params![source_kind, source_id],
    )?;
    Ok(())
}

/// 事务内调用：`with_tx` 闭包只给 `&Transaction`，这里把它转成端口值。
pub(in crate::domain) fn revoke_source_tx(
    tx: &rusqlite::Transaction<'_>,
    source_kind: &str,
    source_id: &str,
) -> Result<()> {
    revoke_source(&StorageConnection::sqlite_tx(tx), source_kind, source_id)
}

/// 幂等语义是"同一载荷重复提交"：同会话同消息、同快照内容返回已有行；
/// 同消息不同快照（如重发后转发语不同）仍各成一行（P1 契约如此）。
pub(in crate::domain) fn find_message_snapshot(
    c: &StorageConnection<'_>,
    user_id: &str,
    source_conv: &str,
    input: &FavoriteInput,
) -> Result<Option<String>> {
    let c = c.sqlite()?;
    Ok(c.query_row(
        "SELECT id FROM favorites
         WHERE user_id = ?1 AND source_kind = 'message' AND source_msg_id = ?2
           AND source_conv = ?3 AND kind = ?4 AND text = ?5 AND media_id = ?6
         ORDER BY created_at LIMIT 1",
        params![
            user_id,
            input.source_msg_id,
            source_conv,
            kind_or_text(&input.kind),
            input.text,
            input.media_id
        ],
        |r| r.get::<_, String>(0),
    )
    .optional()?)
}

pub(in crate::domain) fn insert_message_favorite(
    c: &StorageConnection<'_>,
    user_id: &str,
    source_conv: &str,
    input: &FavoriteInput,
    id: &str,
) -> Result<()> {
    let c = c.sqlite()?;
    let now = wxlike_core::types::now_ms() as i64;
    c.execute(
        "INSERT INTO favorites(id, user_id, source_conv, source_msg_id, kind, text,
                               media_id, sender, sent_at_ms, created_at,
                               source_kind, source_id, snapshot_label, sort_order, revoked)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'message', '', ?5, 0, 0)",
        params![
            id,
            user_id,
            source_conv,
            input.source_msg_id,
            kind_or_text(&input.kind),
            input.text,
            input.media_id,
            input.sender,
            input.sent_at_ms,
            now
        ],
    )?;
    Ok(())
}

pub(in crate::domain) fn favorite_row(
    c: &StorageConnection<'_>,
    user: &str,
    id: &str,
) -> Result<Option<Favorite>> {
    let c = c.sqlite()?;
    Ok(c.query_row(
        "SELECT id, user_id, source_conv, source_msg_id, kind, text, media_id,
                sender, sent_at_ms, created_at, source_kind, source_id,
                snapshot_label, sort_order, revoked
         FROM favorites WHERE id = ?1 AND user_id = ?2",
        params![id, user],
        read_row,
    )
    .optional()?)
}

pub(in crate::domain) fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Favorite> {
    Ok(Favorite {
        id: r.get(0)?,
        user_id: r.get(1)?,
        source_conv: r.get(2)?,
        source_msg_id: r.get(3)?,
        kind: r.get(4)?,
        text: r.get(5)?,
        media_id: r.get(6)?,
        sender: r.get(7)?,
        sent_at_ms: r.get(8)?,
        created_at: r.get(9)?,
        source_kind: r.get(COL_SOURCE_KIND)?,
        source_id: r.get(COL_SOURCE_ID)?,
        snapshot_label: r.get(COL_SNAPSHOT_LABEL)?,
        sort_order: r.get(COL_SORT_ORDER)?,
        revoked: r.get::<_, i64>(COL_REVOKED)? != 0,
    })
}
