//! **仓储层**（`moments`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::{PlatformError, Result};
use crate::storage::StorageConnection;
use rusqlite::{params, OptionalExtension as _};

use crate::domain::moments::{Visibility, MAX_COMMENT_DEPTH, MAX_FANOUT_RECIPIENTS};

/// 动态主行 + 两个读时计数列；`seq` 只用于时间倒序翻页游标。
pub(in crate::domain) struct MomentRecord {
    pub seq: i64,
    pub moment_id: String,
    pub author: String,
    pub author_name: String,
    pub text: String,
    pub media_ids: Vec<String>,
    pub visibility: String,
    pub created_at: i64,
    pub like_count: i64,
    pub comment_count: i64,
}

/// 草稿主行；`media` 在仓储层解成 id 列表，领域层不再碰 JSON。
pub(in crate::domain) struct DraftRecord {
    pub id: String,
    pub author: String,
    pub text: String,
    pub media_ids: Vec<String>,
    pub visibility: String,
    pub audience_group: String,
    pub updated_at: i64,
}

/// 按 id 判定可见性所需的三个字段。
pub(in crate::domain) struct MomentAccess {
    pub author: String,
    pub visibility: String,
    pub audience_group: String,
}

/// 评论行；被回复者名字可能为空（顶层评论或数据缺失）。
pub(in crate::domain) struct CommentRecord {
    pub comment_id: String,
    pub author: String,
    pub author_name: String,
    pub text: String,
    pub reply_to: String,
    pub reply_to_name: String,
    pub created_at: i64,
}

fn map_moment(row: &rusqlite::Row<'_>) -> rusqlite::Result<MomentRecord> {
    let media: String = row.get(5)?;
    Ok(MomentRecord {
        seq: row.get(0)?,
        moment_id: row.get(1)?,
        author: row.get(2)?,
        author_name: row.get(3)?,
        text: row.get(4)?,
        media_ids: serde_json::from_str(&media).unwrap_or_default(),
        visibility: row.get(6)?,
        created_at: row.get(7)?,
        like_count: row.get(8)?,
        comment_count: row.get(9)?,
    })
}

/// 发布扇出：给**此刻有权看到这条动态的人**各插一条收件箱提醒。
///
/// 三条边界，都是 issue #126 明写的：
///
/// * 扇出是"提醒"，**不是聊天消息**：不产生 message 行、不进任何会话，
///   朋友圈不会冒充 IM。
/// * `private` 不扇出；`groups` 只扇给作者标在该分组的好友；
///   `public` / `friends` 扇给好友（feed 的受众集合本来就是"自 + 好友"）。
/// * 双向拉黑过滤与 feed 用同一条 SQL 规则，不另写一套判定。
///
/// 提醒正文是固定文案 + 作者 id，**不含动态内容**：原帖被删/被撤权后，
/// 收件箱里不会留着原文（最小权限与日志脱敏）。`dedupe_key` 让同一
/// 收件人对同一条动态只有一条提醒，重试发布不会刷屏。
pub(in crate::domain) fn fanout_new_moment(
    conn: &StorageConnection<'_>,
    author: &str,
    visibility: Visibility,
    audience_group: &str,
    moment_id: &str,
    now: i64,
) -> Result<i64> {
    if visibility == Visibility::Private {
        return Ok(0);
    }
    let c = conn.sqlite()?;
    // groups 之外的可见性不带分组条件（空串 = 不过滤分组）
    let group = match visibility {
        Visibility::Groups => audience_group,
        _ => "",
    };
    let mut stmt = c.prepare(
        "SELECT f.friend_id FROM friends f
         WHERE f.user_id = ?1
           AND (?2 = '' OR f.friend_group = ?2)
           AND NOT EXISTS (SELECT 1 FROM blocks b
                WHERE (b.user_id = ?1 AND b.blocked_id = f.friend_id)
                   OR (b.user_id = f.friend_id AND b.blocked_id = ?1))
         ORDER BY f.created_at DESC LIMIT ?3",
    )?;
    let recipients = stmt
        .query_map(params![author, group, MAX_FANOUT_RECIPIENTS], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);
    for recipient in &recipients {
        crate::domain::repo::inbox::notify(
            conn,
            recipient,
            "moment_published",
            author,
            "moment",
            moment_id,
            "好友发布了新动态",
            &format!("moment_published:{moment_id}:{recipient}"),
            now,
        )?;
    }
    Ok(recipients.len() as i64)
}

/// 回复目标必须存在且属于同一条朋友圈，且嵌套不超上限。
pub(in crate::domain) fn check_reply_target(
    conn: &StorageConnection<'_>,
    moment_id: &str,
    reply_to: &str,
) -> Result<()> {
    let c = conn.sqlite()?;
    let same_moment: Option<i64> = c
        .query_row(
            "SELECT 1 FROM moment_comments WHERE id = ?1 AND moment_id = ?2 AND deleted = 0",
            params![reply_to, moment_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()?;
    if same_moment.is_none() {
        return Err(PlatformError::BadInput(
            "reply target is not a comment on this moment",
        ));
    }
    if comment_depth(conn, moment_id, reply_to)? >= MAX_COMMENT_DEPTH {
        return Err(PlatformError::BadInput("comment nesting too deep"));
    }
    Ok(())
}

pub(in crate::domain) fn comment_depth(
    conn: &StorageConnection<'_>,
    moment_id: &str,
    comment_id: &str,
) -> Result<usize> {
    let c = conn.sqlite()?;
    let mut depth = 0usize;
    let mut cur = comment_id.to_string();
    for _ in 0..=(MAX_COMMENT_DEPTH + 1) {
        let parent: Option<String> = c
            .query_row(
                "SELECT reply_to FROM moment_comments
                 WHERE id = ?1 AND moment_id = ?2 AND deleted = 0",
                params![cur, moment_id],
                |r| r.get(0),
            )
            .optional()?;
        match parent {
            Some(p) if !p.is_empty() => {
                depth += 1;
                cur = p;
            }
            _ => break,
        }
    }
    Ok(depth)
}

/// 评论/回复落评论者与被回复者两条事务内通知（楼中楼只补被回复者）。
pub(in crate::domain) fn notify_comment_inbox(
    conn: &StorageConnection<'_>,
    moment_id: &str,
    by: &str,
    reply_to: Option<&str>,
    now: i64,
) -> Result<()> {
    let c = conn.sqlite()?;
    let (author, reply_author): (String, Option<String>) = c.query_row(
        "SELECT m.author,
                (SELECT author FROM moment_comments WHERE id = ?2)
         FROM moments m WHERE m.id = ?1",
        params![moment_id, reply_to.unwrap_or("")],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    crate::domain::repo::inbox::notify(
        conn,
        &author,
        "moment_comment",
        by,
        "moment",
        moment_id,
        "评论了你的朋友圈",
        "",
        now,
    )?;
    if let Some(reply_author) = reply_author {
        crate::domain::repo::inbox::notify(
            conn,
            &reply_author,
            "moment_reply",
            by,
            "moment",
            moment_id,
            "回复了你的评论",
            "",
            now,
        )?;
    }
    Ok(())
}

pub(in crate::domain) fn require_media(
    conn: &StorageConnection<'_>,
    media_ids: &[String],
) -> Result<()> {
    let conn = conn.sqlite()?;
    for media_id in media_ids {
        let known: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM media WHERE id = ?1",
                params![media_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?;
        if known.is_none() {
            return Err(PlatformError::BadInput("unknown media id"));
        }
    }
    Ok(())
}

/// 动态主行与两个计数列（点赞数、未删评论数）在同一事务/连接上读。
pub(in crate::domain) fn moment_record(
    conn: &StorageConnection<'_>,
    moment_id: &str,
) -> Result<Option<MomentRecord>> {
    let conn = conn.sqlite()?;
    let row = conn
        .query_row(
            "SELECT m.seq, m.id, m.author, u.name, m.text, m.media, m.visibility, m.created_at,
                    (SELECT COUNT(*) FROM moment_likes l WHERE l.moment_id = m.id),
                    (SELECT COUNT(*) FROM moment_comments cm WHERE cm.moment_id = m.id AND cm.deleted = 0)
             FROM moments m JOIN users u ON u.id = m.author
             WHERE m.id = ?1 AND m.deleted = 0",
            params![moment_id],
            map_moment,
        )
        .optional()?;
    Ok(row)
}

pub(in crate::domain) fn insert_moment(
    conn: &StorageConnection<'_>,
    id: &str,
    author: &str,
    text: &str,
    media_json: &str,
    visibility: &str,
    audience_group: &str,
    now: i64,
) -> Result<()> {
    let conn = conn.sqlite()?;
    conn.execute(
        "INSERT INTO moments(id, author, text, media, visibility, audience_group, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            author,
            text,
            media_json,
            visibility,
            audience_group,
            now
        ],
    )?;
    Ok(())
}

/// 发布成功后消费掉草稿；返回 false 表示草稿不属于该作者（调用方拒绝）。
pub(in crate::domain) fn delete_draft_for_author(
    conn: &StorageConnection<'_>,
    draft_id: &str,
    author: &str,
) -> Result<bool> {
    let conn = conn.sqlite()?;
    let changed = conn.execute(
        "DELETE FROM moment_drafts WHERE id = ?1 AND author = ?2",
        params![draft_id, author],
    )?;
    Ok(changed == 1)
}

pub(in crate::domain) fn update_draft(
    conn: &StorageConnection<'_>,
    id: &str,
    author: &str,
    text: &str,
    media_json: &str,
    visibility: &str,
    audience_group: &str,
    now: i64,
) -> Result<bool> {
    let conn = conn.sqlite()?;
    let changed = conn.execute(
        "UPDATE moment_drafts SET text = ?1, media = ?2, visibility = ?3,
                              audience_group = ?4, updated_at = ?5
         WHERE id = ?6 AND author = ?7",
        params![
            text,
            media_json,
            visibility,
            audience_group,
            now,
            id,
            author
        ],
    )?;
    Ok(changed == 1)
}

pub(in crate::domain) fn insert_draft(
    conn: &StorageConnection<'_>,
    id: &str,
    author: &str,
    text: &str,
    media_json: &str,
    visibility: &str,
    audience_group: &str,
    now: i64,
) -> Result<()> {
    let conn = conn.sqlite()?;
    conn.execute(
        "INSERT INTO moment_drafts(id, author, text, media, visibility,
                               audience_group, updated_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            author,
            text,
            media_json,
            visibility,
            audience_group,
            now
        ],
    )?;
    Ok(())
}

pub(in crate::domain) fn drafts_by_author(
    conn: &StorageConnection<'_>,
    author: &str,
) -> Result<Vec<DraftRecord>> {
    let conn = conn.sqlite()?;
    let mut stmt = conn.prepare(
        "SELECT id, author, text, media, visibility, audience_group, updated_at
         FROM moment_drafts WHERE author = ?1 ORDER BY updated_at DESC",
    )?;
    let rows = stmt.query_map(params![author], |r| {
        let media: String = r.get(3)?;
        Ok(DraftRecord {
            id: r.get(0)?,
            author: r.get(1)?,
            text: r.get(2)?,
            media_ids: serde_json::from_str(&media).unwrap_or_default(),
            visibility: r.get(4)?,
            audience_group: r.get(5)?,
            updated_at: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub(in crate::domain) fn delete_draft(
    conn: &StorageConnection<'_>,
    id: &str,
    author: &str,
) -> Result<bool> {
    let conn = conn.sqlite()?;
    let changed = conn.execute(
        "DELETE FROM moment_drafts WHERE id = ?1 AND author = ?2",
        params![id, author],
    )?;
    Ok(changed == 1)
}

pub(in crate::domain) fn friend_group_contains(
    conn: &StorageConnection<'_>,
    author: &str,
    viewer: &str,
    group: &str,
) -> Result<bool> {
    let conn = conn.sqlite()?;
    let found = conn
        .query_row(
            "SELECT 1 FROM friends WHERE user_id = ?1 AND friend_id = ?2 AND friend_group = ?3",
            params![author, viewer, group],
            |_| Ok(()),
        )
        .optional()?;
    Ok(found.is_some())
}

pub(in crate::domain) fn moment_access(
    conn: &StorageConnection<'_>,
    moment_id: &str,
) -> Result<Option<MomentAccess>> {
    let conn = conn.sqlite()?;
    let row = conn
        .query_row(
            "SELECT author, visibility, audience_group FROM moments
             WHERE id = ?1 AND deleted = 0",
            params![moment_id],
            |r| {
                Ok(MomentAccess {
                    author: r.get(0)?,
                    visibility: r.get(1)?,
                    audience_group: r.get(2)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

pub(in crate::domain) fn moment_likes(
    conn: &StorageConnection<'_>,
    moment_id: &str,
) -> Result<Vec<(String, String)>> {
    let conn = conn.sqlite()?;
    let mut stmt = conn.prepare(
        "SELECT l.user_id, u.name FROM moment_likes l JOIN users u ON u.id = l.user_id
         WHERE l.moment_id = ?1 ORDER BY l.created_at",
    )?;
    let rows = stmt.query_map(params![moment_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub(in crate::domain) fn moment_comments(
    conn: &StorageConnection<'_>,
    moment_id: &str,
) -> Result<Vec<CommentRecord>> {
    let conn = conn.sqlite()?;
    let mut stmt = conn.prepare(
        "SELECT cm.id, cm.author, u.name, cm.text, cm.reply_to, rt.name, cm.created_at
         FROM moment_comments cm
         JOIN users u ON u.id = cm.author
         LEFT JOIN moment_comments rc ON rc.id = cm.reply_to
         LEFT JOIN users rt ON rt.id = rc.author
         WHERE cm.moment_id = ?1 AND cm.deleted = 0
         ORDER BY cm.seq",
    )?;
    let rows = stmt.query_map(params![moment_id], |r| {
        Ok(CommentRecord {
            comment_id: r.get(0)?,
            author: r.get(1)?,
            author_name: r.get(2)?,
            text: r.get(3)?,
            reply_to: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            reply_to_name: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
            created_at: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub(in crate::domain) fn soft_delete_moment(
    conn: &StorageConnection<'_>,
    moment_id: &str,
    by: &str,
) -> Result<bool> {
    let conn = conn.sqlite()?;
    let changed = conn.execute(
        "UPDATE moments SET deleted = 1 WHERE id = ?1 AND author = ?2 AND deleted = 0",
        params![moment_id, by],
    )?;
    Ok(changed == 1)
}

pub(in crate::domain) fn soft_delete_comment(
    conn: &StorageConnection<'_>,
    comment_id: &str,
    moment_id: &str,
    by: &str,
) -> Result<bool> {
    let conn = conn.sqlite()?;
    let changed = conn.execute(
        "UPDATE moment_comments SET deleted = 1
         WHERE id = ?1 AND moment_id = ?2 AND deleted = 0
           AND (author = ?3
                OR ?3 = (SELECT author FROM moments WHERE id = ?2))",
        params![comment_id, moment_id, by],
    )?;
    Ok(changed == 1)
}

pub(in crate::domain) fn insert_like(
    conn: &StorageConnection<'_>,
    moment_id: &str,
    by: &str,
    now: i64,
) -> Result<bool> {
    let conn = conn.sqlite()?;
    let changed = conn.execute(
        "INSERT INTO moment_likes(moment_id, user_id, created_at) VALUES(?1, ?2, ?3)
         ON CONFLICT(moment_id, user_id) DO NOTHING",
        params![moment_id, by, now],
    )?;
    Ok(changed == 1)
}

pub(in crate::domain) fn moment_author_if_live(
    conn: &StorageConnection<'_>,
    moment_id: &str,
) -> Result<Option<String>> {
    let conn = conn.sqlite()?;
    let author = conn
        .query_row(
            "SELECT author FROM moments WHERE id = ?1 AND deleted = 0",
            params![moment_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(author)
}

pub(in crate::domain) fn delete_like(
    conn: &StorageConnection<'_>,
    moment_id: &str,
    by: &str,
) -> Result<bool> {
    let conn = conn.sqlite()?;
    let changed = conn.execute(
        "DELETE FROM moment_likes WHERE moment_id = ?1 AND user_id = ?2",
        params![moment_id, by],
    )?;
    Ok(changed == 1)
}

pub(in crate::domain) fn insert_comment(
    conn: &StorageConnection<'_>,
    id: &str,
    moment_id: &str,
    by: &str,
    text: &str,
    reply_to: Option<&str>,
    now: i64,
) -> Result<()> {
    let conn = conn.sqlite()?;
    conn.execute(
        "INSERT INTO moment_comments(id, moment_id, author, text, reply_to, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, moment_id, by, text, reply_to.unwrap_or(""), now],
    )?;
    Ok(())
}

/// 相册页：某个作者对 `viewer` 读时可见的动态，时间倒序。
pub(in crate::domain) fn album_page(
    conn: &StorageConnection<'_>,
    viewer: &str,
    author: &str,
    upper: i64,
    limit: i64,
) -> Result<Vec<MomentRecord>> {
    let conn = conn.sqlite()?;
    let sql = "SELECT m.seq, m.id, m.author, u.name, m.text, m.media, m.visibility, m.created_at,
                    (SELECT COUNT(*) FROM moment_likes l WHERE l.moment_id = m.id),
                    (SELECT COUNT(*) FROM moment_comments cm WHERE cm.moment_id = m.id AND cm.deleted = 0)
             FROM moments m JOIN users u ON u.id = m.author
             WHERE m.deleted = 0 AND m.author = ?2
               AND (m.author = ?1 OR EXISTS (
                     SELECT 1 FROM friends f WHERE f.user_id = ?1 AND f.friend_id = m.author))
               AND (m.visibility <> 'private' OR m.author = ?1)
               AND (m.visibility <> 'groups' OR m.author = ?1 OR EXISTS (
                     SELECT 1 FROM friends f WHERE f.user_id = m.author AND f.friend_id = ?1
                       AND f.friend_group = m.audience_group))
               AND NOT EXISTS (
                     SELECT 1 FROM blocks b
                     WHERE (b.user_id = ?1 AND b.blocked_id = m.author)
                        OR (b.user_id = m.author AND b.blocked_id = ?1))
               AND m.seq < ?3
             ORDER BY m.seq DESC LIMIT ?4";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![viewer, author, upper, limit], map_moment)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Feed 页：自 + 好友，读时判定可见性、分组与双向拉黑。
pub(in crate::domain) fn feed_page(
    conn: &StorageConnection<'_>,
    me: &str,
    upper: i64,
    limit: i64,
) -> Result<Vec<MomentRecord>> {
    let conn = conn.sqlite()?;
    let sql = "SELECT m.seq, m.id, m.author, u.name, m.text, m.media, m.visibility, m.created_at,
                    (SELECT COUNT(*) FROM moment_likes l WHERE l.moment_id = m.id),
                    (SELECT COUNT(*) FROM moment_comments cm WHERE cm.moment_id = m.id AND cm.deleted = 0)
             FROM moments m JOIN users u ON u.id = m.author
             WHERE m.deleted = 0
               AND (m.author = ?1 OR m.author IN (SELECT friend_id FROM friends WHERE user_id = ?1))
               AND (m.visibility <> 'private' OR m.author = ?1)
               AND (m.visibility <> 'groups' OR m.author = ?1 OR EXISTS (
                     SELECT 1 FROM friends f WHERE f.user_id = m.author AND f.friend_id = ?1
                       AND f.friend_group = m.audience_group))
               AND NOT EXISTS (
                     SELECT 1 FROM blocks b
                     WHERE (b.user_id = ?1 AND b.blocked_id = m.author)
                        OR (b.user_id = m.author AND b.blocked_id = ?1))
               AND m.seq < ?2
             ORDER BY m.seq DESC LIMIT ?3";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![me, upper, limit], map_moment)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}
