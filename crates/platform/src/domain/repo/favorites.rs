//! **仓储层**（`favorites`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use crate::storage::{params, OptionalExt as _, StorageConnection};

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
    c.query_row(
        "SELECT id FROM favorites
         WHERE user_id = ?1 AND source_kind = ?2 AND source_id = ?3",
        params![user, kind, source_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
}

/// 从权威表读一小段快照；对象不存在或未发布返回 `None`。
pub(in crate::domain) fn snapshot_source(
    c: &StorageConnection<'_>,
    kind: &str,
    id: &str,
) -> Result<Option<SnapshotSource>> {
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
    let stmt = c.prepare(
        "SELECT id, user_id, source_conv, source_msg_id, kind, text, media_id,
                sender, sent_at_ms, created_at, source_kind, source_id,
                snapshot_label, sort_order, revoked
         FROM favorites WHERE user_id = ?1 AND created_at < ?2
         ORDER BY created_at DESC, id DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![user, before, limit], read_row)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
}

pub(in crate::domain) fn sorted_page(
    c: &StorageConnection<'_>,
    user: &str,
    sort: i64,
    created: i64,
    cursor_id: &str,
    limit: i64,
) -> Result<Vec<Favorite>> {
    let stmt = c.prepare(
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
    it.collect::<std::result::Result<Vec<_>, _>>()
}

pub(in crate::domain) fn search_page(
    c: &StorageConnection<'_>,
    user: &str,
    pattern: &str,
    limit: i64,
) -> Result<Vec<Favorite>> {
    let stmt = c.prepare(
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
    rows.collect::<std::result::Result<Vec<_>, _>>()
}

pub(in crate::domain) fn update_fields(
    c: &StorageConnection<'_>,
    user: &str,
    id: &str,
    label: Option<&str>,
    sort_order: Option<i64>,
) -> Result<usize> {
    c.execute(
        "UPDATE favorites SET
           snapshot_label = COALESCE(?1, snapshot_label),
           sort_order = COALESCE(?2, sort_order)
         WHERE id = ?3 AND user_id = ?4",
        params![label, sort_order, id, user],
    )
}

/// 一次 UPDATE 完成整批排序：`CASE id` 写目标序号，`WHERE` 里的
/// `COUNT(*) = n` 保证所有 id 都属于本人且都存在，缺一即 0 行受影响。
/// 这样不依赖 SQLite 事务，PG 单语句也是原子的。
pub(in crate::domain) fn reorder(
    c: &StorageConnection<'_>,
    user: &str,
    ids: &[String],
) -> Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    let mut sql = String::from("UPDATE favorites SET sort_order = CASE id");
    let mut params = Vec::with_capacity(ids.len() * 2 + 1);
    for (i, id) in ids.iter().enumerate() {
        let when = i * 2 + 1;
        sql.push_str(&format!(" WHEN ?{when} THEN ?{}", when + 1));
        params.push(crate::storage::port::Param::Text(id.clone()));
        params.push(crate::storage::port::Param::Int(i as i64));
    }
    let user_pos = ids.len() * 2 + 1;
    let in_list = (1..=ids.len())
        .map(|i| format!("?{}", i * 2 - 1))
        .collect::<Vec<_>>()
        .join(", ");
    sql.push_str(&format!(
        " END WHERE user_id = ?{user_pos} AND id IN ({in_list})
          AND (SELECT COUNT(*) FROM favorites WHERE user_id = ?{user_pos} AND id IN ({in_list})) = {}",
        ids.len()
    ));
    params.push(crate::storage::port::Param::Text(user.to_string()));
    let updated = c.execute(&sql, crate::storage::port::Params::new(params))?;
    if updated == ids.len() {
        Ok(updated)
    } else {
        Err(crate::error::PlatformError::NotFound)
    }
}

delete_owned_row!("favorites");
/// 源对象删除/撤权/到期时调用（与删除同事务）。只标记，不删用户的行：
/// 用户仍能看到"这条收藏已不可用"，且排序位置保持不变。
pub(in crate::domain) fn revoke_source(
    conn: &StorageConnection<'_>,
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
    c: &StorageConnection<'_>,
    user_id: &str,
    source_conv: &str,
    input: &FavoriteInput,
) -> Result<Option<String>> {
    c.query_row(
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
    .optional()
}

pub(in crate::domain) fn insert_message_favorite(
    c: &StorageConnection<'_>,
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
    c: &StorageConnection<'_>,
    user: &str,
    id: &str,
) -> Result<Option<Favorite>> {
    c.query_row(
        "SELECT id, user_id, source_conv, source_msg_id, kind, text, media_id,
                sender, sent_at_ms, created_at, source_kind, source_id,
                snapshot_label, sort_order, revoked
         FROM favorites WHERE id = ?1 AND user_id = ?2",
        params![id, user],
        read_row,
    )
    .optional()
}

pub(in crate::domain) fn read_row(r: &crate::storage::StorageRow<'_>) -> Result<Favorite> {
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

#[cfg(test)]
mod tests {
    use super::{insert_object_favorite, list_page, reorder};
    use crate::Platform;

    #[test]
    fn reorder_is_single_statement_and_rejects_unknown_ids() {
        let platform = Platform::in_memory().expect("in-memory platform");
        let user = platform.register("favreorder", "pw123456").expect("register");
        let c = platform.db().acquire();
        for (id, source) in [("f1", "s1"), ("f2", "s2"), ("f3", "s3")] {
            insert_object_favorite(&c, id, &user, "moment", source, "t", "l", 1)
                .expect("seed favorite");
        }
        let ids = vec!["f3".to_string(), "f1".to_string(), "f2".to_string()];
        assert_eq!(reorder(&c, &user, &ids).expect("reorder"), 3);
        let rows = list_page(&c, &user, i64::MAX, 10).expect("list");
        let mut order = rows
            .into_iter()
            .map(|f| (f.id, f.sort_order))
            .collect::<Vec<_>>();
        order.sort_by_key(|(_, n)| *n);
        assert_eq!(order, vec![
            ("f3".to_string(), 0),
            ("f1".to_string(), 1),
            ("f2".to_string(), 2)
        ]);

        let bad = vec![
            "f1".to_string(),
            "f3".to_string(),
            "missing".to_string(),
        ];
        assert!(reorder(&c, &user, &bad).is_err());
        let rows = list_page(&c, &user, i64::MAX, 10).expect("list after bad reorder");
        let f1 = rows.iter().find(|f| f.id == "f1").expect("f1");
        assert_eq!(f1.sort_order, 1, "失败批次不得改掉已有顺序");
    }
}
