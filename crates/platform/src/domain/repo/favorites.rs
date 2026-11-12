//! **仓储层**（`favorites`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use rusqlite::{params, Connection, OptionalExtension as _};

use crate::domain::favorites::{
    kind_or_text, Favorite, FavoriteInput, COL_REVOKED, COL_SNAPSHOT_LABEL, COL_SORT_ORDER,
    COL_SOURCE_ID, COL_SOURCE_KIND,
};

/// 源对象删除/撤权/到期时调用（与删除同事务）。只标记，不删用户的行：
/// 用户仍能看到"这条收藏已不可用"，且排序位置保持不变。
pub(in crate::domain) fn revoke_source(
    conn: &Connection,
    source_kind: &str,
    source_id: &str,
) -> Result<()> {
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

/// 幂等语义是"同一载荷重复提交"：同会话同消息、同快照内容返回已有行；
/// 同消息不同快照（如重发后转发语不同）仍各成一行（P1 契约如此）。
pub(in crate::domain) fn find_message_snapshot(
    c: &Connection,
    user_id: &str,
    source_conv: &str,
    input: &FavoriteInput,
) -> Result<Option<String>> {
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
    c: &Connection,
    user_id: &str,
    source_conv: &str,
    input: &FavoriteInput,
    id: &str,
) -> Result<()> {
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
    c: &Connection,
    user: &str,
    id: &str,
) -> Result<Option<Favorite>> {
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
