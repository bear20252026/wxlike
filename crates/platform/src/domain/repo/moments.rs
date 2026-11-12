//! **仓储层**（`moments`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::{PlatformError, Result};
use rusqlite::{params, OptionalExtension as _};

use crate::domain::moments::{Visibility, MAX_COMMENT_DEPTH, MAX_FANOUT_RECIPIENTS};

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
    conn: &rusqlite::Connection,
    author: &str,
    visibility: Visibility,
    audience_group: &str,
    moment_id: &str,
    now: i64,
) -> Result<i64> {
    if visibility == Visibility::Private {
        return Ok(0);
    }
    // groups 之外的可见性不带分组条件（空串 = 不过滤分组）
    let group = match visibility {
        Visibility::Groups => audience_group,
        _ => "",
    };
    let mut stmt = conn.prepare(
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
    conn: &rusqlite::Connection,
    moment_id: &str,
    reply_to: &str,
) -> Result<()> {
    let same_moment: Option<i64> = conn
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
    conn: &rusqlite::Connection,
    moment_id: &str,
    comment_id: &str,
) -> Result<usize> {
    let mut depth = 0usize;
    let mut cur = comment_id.to_string();
    for _ in 0..=(MAX_COMMENT_DEPTH + 1) {
        let parent: Option<String> = conn
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
    conn: &rusqlite::Connection,
    moment_id: &str,
    by: &str,
    reply_to: Option<&str>,
    now: i64,
) -> Result<()> {
    let (author, reply_author): (String, Option<String>) = conn.query_row(
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
    conn: &rusqlite::Connection,
    media_ids: &[String],
) -> Result<()> {
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
