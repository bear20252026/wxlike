//! **仓储层**（`video_feed`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::{PlatformError, Result};
use crate::storage::{params, OptionalExt as _, StorageConnection};


use crate::domain::video_feed::{
    parse_score_cursor, video_view_from, VideoComment, VideoRow, VideoView, COL_DURATION_MS,
    COL_PLAYLIST_URL, COL_PLAY_URL, COL_THUMB_URL, HOT_WINDOW_MS,
};

/// 复核事务需要的四个字段；状态机仍在 domain。
pub(in crate::domain) struct ReviewRecord {
    pub author: String,
    pub state: String,
    pub caption: String,
    pub media_id: String,
}

/// 可见性判定只看作者与状态。
pub(in crate::domain) struct VideoAccess {
    pub author: String,
    pub state: String,
}

pub(in crate::domain) fn feed_by_seq(
    c: &StorageConnection<'_>,
    viewer: &str,
    cursor: Option<&str>,
    limit: i64,
) -> Result<(Vec<VideoView>, String, bool)> {
    let before = match cursor {
        Some(cur) if !cur.is_empty() => cur
            .parse::<i64>()
            .map_err(|_| PlatformError::BadInput("malformed cursor"))?,
        _ => i64::MAX,
    };
    let stmt = c.prepare(
        "SELECT v.seq, v.id FROM video_posts v
         WHERE v.state = 'published' AND v.seq < ?2
           AND (v.author = ?1
                OR v.author IN (SELECT creator FROM video_follows WHERE user_id = ?1))
           AND NOT EXISTS (SELECT 1 FROM blocks b
                WHERE (b.user_id = ?1 AND b.blocked_id = v.author)
                   OR (b.user_id = v.author AND b.blocked_id = ?1))
         ORDER BY v.seq DESC LIMIT ?3",
    )?;
    let ids = stmt
        .query_map(params![viewer, before, limit + 1], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    finish(
        c,
        viewer,
        ids,
        limit,
        |seq| seq.to_string(),
    )
}

/// 热门：7 天窗口内按点赞/评论加权；新近发的一条保底 1 分。
pub(in crate::domain) fn feed_hot(
    c: &StorageConnection<'_>,
    viewer: &str,
    cursor: Option<&str>,
    limit: i64,
) -> Result<(Vec<VideoView>, String, bool)> {
    let (before_score, before_seq) = parse_score_cursor(cursor)?;
    let cutoff = wxlike_core::types::now_ms() as i64 - HOT_WINDOW_MS;
    let stmt = c.prepare(
        "SELECT seq, id, score FROM (
           SELECT v.seq AS seq, v.id AS id,
                  (SELECT COUNT(*) FROM video_likes l WHERE l.video_id = v.id) * 3
                + (SELECT COUNT(*) FROM video_comments cm
                   WHERE cm.video_id = v.id AND cm.deleted = 0) * 2
                + CASE WHEN v.created_at >= ?2 THEN 1 ELSE 0 END AS score
           FROM video_posts v
           WHERE v.state = 'published'
             AND NOT EXISTS (SELECT 1 FROM blocks b
                  WHERE (b.user_id = ?1 AND b.blocked_id = v.author)
                     OR (b.user_id = v.author AND b.blocked_id = ?1))
             AND (v.created_at >= ?2
                  OR EXISTS (SELECT 1 FROM video_likes l WHERE l.video_id = v.id))
         )
         WHERE score < ?3 OR (score = ?3 AND seq < ?4)
         ORDER BY score DESC, seq DESC LIMIT ?5",
    )?;
    let rows = stmt
        .query_map(
            params![viewer, cutoff, before_score, before_seq, limit + 1],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    page_of_scored(
        c,
        viewer,
        rows,
        limit,
        "trending",
    )
}

/// 推荐：只推"好友看过的未关注作者作品"。没有社交痕迹就什么都不推——
/// 陌生人的热门视频只属于热门页，不冒充"为你推荐"。
pub(in crate::domain) fn feed_recommend(
    c: &StorageConnection<'_>,
    viewer: &str,
    cursor: Option<&str>,
    limit: i64,
) -> Result<(Vec<VideoView>, String, bool)> {
    let (before_score, before_seq) = parse_score_cursor(cursor)?;
    let stmt = c.prepare(
        "SELECT seq, id, score FROM (
           SELECT v.seq AS seq, v.id AS id,
                  (SELECT COUNT(*) FROM video_likes l
                   JOIN friends f ON f.user_id = ?1 AND f.friend_id = l.user_id
                   WHERE l.video_id = v.id) * 100
                + (SELECT COUNT(*) FROM video_likes l WHERE l.video_id = v.id) * 3
                + (SELECT COUNT(*) FROM video_comments cm
                   WHERE cm.video_id = v.id AND cm.deleted = 0) AS score
           FROM video_posts v
           WHERE v.state = 'published'
             AND v.author <> ?1
             AND v.author NOT IN (SELECT creator FROM video_follows WHERE user_id = ?1)
             AND EXISTS (SELECT 1 FROM video_likes l
                         JOIN friends f ON f.user_id = ?1
                                       AND f.friend_id = l.user_id
                         WHERE l.video_id = v.id)
             AND NOT EXISTS (SELECT 1 FROM blocks b
                  WHERE (b.user_id = ?1 AND b.blocked_id = v.author)
                     OR (b.user_id = v.author AND b.blocked_id = ?1))
         )
         WHERE score < ?2 OR (score = ?2 AND seq < ?3)
         ORDER BY score DESC, seq DESC LIMIT ?4",
    )?;
    let rows = stmt
        .query_map(params![viewer, before_score, before_seq, limit + 1], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    page_of_scored(
        c,
        viewer,
        rows,
        limit,
        "friends_liked",
    )
}

/// 打分流的公共尾部：多取一条判 has_more，游标取自页尾，逐条补视图。
pub(in crate::domain) fn page_of_scored(
    c: &StorageConnection<'_>,
    viewer: &str,
    rows: Vec<(i64, String, i64)>,
    limit: i64,
    reason: &str,
) -> Result<(Vec<VideoView>, String, bool)> {
    let has_more = rows.len() as i64 > limit;
    let take = if has_more {
        &rows[..rows.len() - 1]
    } else {
        &rows[..]
    };
    let next = take
        .last()
        .map(|(s, _, score)| format!("{score}:{s}"))
        .unwrap_or_default();
    let mut views = Vec::new();
    for (_, id, _) in take {
        if let Some(mut view) = video_row(
            c,
            viewer,
            id,
        )? {
            view.reason = reason.to_string();
            views.push(view);
        }
    }
    Ok((views, next, has_more))
}

pub(in crate::domain) fn finish(
    c: &StorageConnection<'_>,
    viewer: &str,
    ids: Vec<(i64, String)>,
    limit: i64,
    cursor_of: impl Fn(i64) -> String,
) -> Result<(Vec<VideoView>, String, bool)> {
    let has_more = ids.len() as i64 > limit;
    let take = if has_more {
        &ids[..ids.len() - 1]
    } else {
        &ids[..]
    };
    let next = take
        .last()
        .map(|(seq, _)| cursor_of(*seq))
        .unwrap_or_default();
    let mut views = Vec::with_capacity(take.len());
    for (_, id) in take {
        if let Some(view) = video_row(
            c,
            viewer,
            id,
        )? {
            views.push(view);
        }
    }
    Ok((views, next, has_more))
}

/// 索引 + 附件引用：只在内容真正可分发时写。审核通过走同一个函数，
/// 所以"放行"与"直接发布"的投影完全一致，不会有两套写法。
pub(in crate::domain) fn index_video(
    conn: &StorageConnection<'_>,
    id: &str,
    author: &str,
    caption: &str,
    media_id: &str,
    now: i64,
) -> Result<()> {
    crate::domain::repo::content_index::upsert(
        conn,
        &crate::domain::content_index::IndexDoc {
            doc_id: id,
            kind: "video",
            owner: author,
            audience: "public",
            group_name: "",
            title: caption,
            body: caption,
        },
        now,
    )?;
    crate::domain::repo::content_index::add_media_refs(
        conn,
        &[media_id.to_string()],
        "video",
        id,
        author,
        now,
    )?;
    Ok(())
}

/// 幂等发布查找：同作者同媒体、仍处于 published/pending 的既有行 id。
pub(in crate::domain) fn find_active_video_post(
    c: &StorageConnection<'_>,
    author: &str,
    media_id: &str,
) -> Result<Option<String>> {
    c.query_row(
        "SELECT id FROM video_posts
         WHERE author = ?1 AND media_id = ?2 AND state IN ('published', 'pending')
         ORDER BY seq LIMIT 1",
        params![author, media_id],
        |r| r.get(0),
    )
    .optional()
}

pub(in crate::domain) fn check_video_gate(
    c: &StorageConnection<'_>,
    author: &str,
    media_id: &str,
) -> Result<()> {
    let media: Option<String> = c
        .query_row(
            "SELECT uploader FROM media WHERE id = ?1",
            params![media_id],
            |r| r.get(0),
        )
        .optional()?;
    match media {
        Some(uploader) if uploader == author => {}
        Some(_) => return Err(PlatformError::Forbidden),
        None => return Err(PlatformError::BadInput("unknown media id")),
    }
    let transcode: Option<String> = c
        .query_row(
            "SELECT state FROM video_refs WHERE video_id = ?1",
            params![media_id],
            |r| r.get(0),
        )
        .optional()?;
    match transcode.as_deref() {
        Some("succeeded") => Ok(()),
        Some(_) => Err(PlatformError::Conflict("video is still transcoding".into())),
        None => Err(PlatformError::BadInput("video has not been transcoded")),
    }
}

pub(in crate::domain) fn video_row(
    c: &StorageConnection<'_>,
    viewer: &str,
    id: &str,
) -> Result<Option<VideoView>> {
    let row: Option<VideoRow> = c
        .query_row(
            "SELECT v.id, v.author, u.name, v.caption, v.state, v.created_at,
                    (SELECT COUNT(*) FROM video_likes l WHERE l.video_id = v.id),
                    (SELECT COUNT(*) FROM video_comments cm
                     WHERE cm.video_id = v.id AND cm.deleted = 0),
                    EXISTS(SELECT 1 FROM video_likes l
                           WHERE l.video_id = v.id AND l.user_id = ?2),
                    EXISTS(SELECT 1 FROM live_sessions ls
                           WHERE ls.host = v.author AND ls.state = 'live'),
                    COALESCE(vr.state = 'succeeded', 0),
                    COALESCE(vr.playlist_url, ''),
                    COALESCE(vr.play_url, ''),
                    COALESCE(vr.duration_ms, 0),
                    COALESCE(vr.thumb_url, '')
             FROM video_posts v JOIN users u ON u.id = v.author
             LEFT JOIN video_refs vr ON vr.video_id = v.media_id
             WHERE v.id = ?1 AND v.state IN ('published', 'pending')",
            params![id, viewer],
            |r| {
                Ok(VideoRow {
                    id: r.get(0)?,
                    author: r.get(1)?,
                    author_name: r.get(2)?,
                    caption: r.get(3)?,
                    state: r.get(4)?,
                    created_at: r.get(5)?,
                    like_count: r.get(6)?,
                    comment_count: r.get(7)?,
                    liked: r.get::<_, i64>(8)? != 0,
                    live: r.get::<_, i64>(9)? != 0,
                    playable: r.get::<_, i64>(10)? != 0,
                    playlist_url: r.get(COL_PLAYLIST_URL)?,
                    play_url: r.get(COL_PLAY_URL)?,
                    duration_ms: r.get(COL_DURATION_MS)?,
                    thumb_url: r.get(COL_THUMB_URL)?,
                })
            },
        )
        .optional()?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(video_view_from(row)))
}

pub(in crate::domain) fn insert_video_post(
    c: &StorageConnection<'_>,
    id: &str,
    author: &str,
    media_id: &str,
    caption: &str,
    state: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO video_posts(seq, id, author, media_id, caption, state, created_at)
         VALUES(NULL, ?1, ?2, ?3, ?4, ?6, ?5)",
        params![id, author, media_id, caption, now, state],
    )?;
    Ok(())
}

pub(in crate::domain) fn review_record(
    c: &StorageConnection<'_>,
    id: &str,
) -> Result<Option<ReviewRecord>> {
    let row = c
        .query_row(
            "SELECT author, state, caption, media_id FROM video_posts WHERE id = ?1",
            params![id],
            |r| {
                Ok(ReviewRecord {
                    author: r.get(0)?,
                    state: r.get(1)?,
                    caption: r.get(2)?,
                    media_id: r.get(3)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

pub(in crate::domain) fn set_video_state(
    c: &StorageConnection<'_>,
    id: &str,
    state: &str,
) -> Result<()> {
    c.execute(
        "UPDATE video_posts SET state = ?2 WHERE id = ?1",
        params![id, state],
    )?;
    Ok(())
}

pub(in crate::domain) fn video_access(
    c: &StorageConnection<'_>,
    id: &str,
) -> Result<Option<VideoAccess>> {
    let row = c
        .query_row(
            "SELECT author, state FROM video_posts WHERE id = ?1",
            params![id],
            |r| {
                Ok(VideoAccess {
                    author: r.get(0)?,
                    state: r.get(1)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

pub(in crate::domain) fn soft_delete_video(
    c: &StorageConnection<'_>,
    id: &str,
    author: &str,
    deleted_state: &str,
) -> Result<bool> {
    let changed = c.execute(
        "UPDATE video_posts SET state = ?3
         WHERE id = ?1 AND author = ?2 AND state NOT IN ('deleted', 'rejected')",
        params![id, author, deleted_state],
    )?;
    Ok(changed == 1)
}

pub(in crate::domain) fn insert_video_like(
    c: &StorageConnection<'_>,
    id: &str,
    by: &str,
    now: i64,
) -> Result<bool> {
    let added = c.execute(
        "INSERT INTO video_likes(video_id, user_id, created_at) VALUES(?1, ?2, ?3)
         ON CONFLICT(video_id, user_id) DO NOTHING",
        params![id, by, now],
    )?;
    Ok(added == 1)
}

pub(in crate::domain) fn video_author(
    c: &StorageConnection<'_>,
    id: &str,
) -> Result<Option<String>> {
    let author = c
        .query_row(
            "SELECT author FROM video_posts WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(author)
}

pub(in crate::domain) fn video_like_count(c: &StorageConnection<'_>, id: &str) -> Result<i64> {
    let count = c.query_row(
        "SELECT COUNT(*) FROM video_likes WHERE video_id = ?1",
        params![id],
        |r| r.get(0),
    )?;
    Ok(count)
}

pub(in crate::domain) fn delete_video_like(
    c: &StorageConnection<'_>,
    id: &str,
    by: &str,
) -> Result<bool> {
    let removed = c.execute(
        "DELETE FROM video_likes WHERE video_id = ?1 AND user_id = ?2",
        params![id, by],
    )?;
    Ok(removed == 1)
}

pub(in crate::domain) fn insert_video_comment(
    c: &StorageConnection<'_>,
    comment_id: &str,
    video_id: &str,
    author: &str,
    text: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO video_comments(seq, id, video_id, author, text, created_at)
         VALUES(NULL, ?1, ?2, ?3, ?4, ?5)",
        params![comment_id, video_id, author, text, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn video_comments(
    c: &StorageConnection<'_>,
    id: &str,
    before: i64,
    limit: i64,
) -> Result<Vec<VideoComment>> {
    let stmt = c.prepare(
        "SELECT cm.id, cm.video_id, cm.author, u.name, cm.text, cm.created_at, cm.seq
         FROM video_comments cm JOIN users u ON u.id = cm.author
         WHERE cm.video_id = ?1 AND cm.deleted = 0 AND cm.seq < ?2
         ORDER BY cm.seq DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![id, before, limit], |r| {
        Ok(VideoComment {
            id: r.get(0)?,
            video_id: r.get(1)?,
            author: r.get(2)?,
            author_name: r.get(3)?,
            text: r.get(4)?,
            created_at: r.get(5)?,
            seq: r.get(6)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
}

pub(in crate::domain) fn soft_delete_video_comment(
    c: &StorageConnection<'_>,
    video_id: &str,
    comment_id: &str,
    by: &str,
) -> Result<bool> {
    let changed = c.execute(
        "UPDATE video_comments SET deleted = 1
         WHERE id = ?1 AND video_id = ?2 AND deleted = 0
           AND (author = ?3 OR ?3 = (SELECT author FROM video_posts WHERE id = ?2))",
        params![comment_id, video_id, by],
    )?;
    Ok(changed == 1)
}

pub(in crate::domain) fn creator_exists(c: &StorageConnection<'_>, creator: &str) -> Result<bool> {
    let exists = c
        .query_row("SELECT 1 FROM users WHERE id = ?1", params![creator], |r| {
            r.get::<_, i64>(0)
        })
        .optional()?;
    Ok(exists.is_some())
}

pub(in crate::domain) fn follow_creator(
    c: &StorageConnection<'_>,
    user: &str,
    creator: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT OR IGNORE INTO video_follows(user_id, creator, created_at)
         VALUES(?1, ?2, ?3)",
        params![user, creator, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn unfollow_creator(
    c: &StorageConnection<'_>,
    user: &str,
    creator: &str,
) -> Result<()> {
    c.execute(
        "DELETE FROM video_follows WHERE user_id = ?1 AND creator = ?2",
        params![user, creator],
    )?;
    Ok(())
}
