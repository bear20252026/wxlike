//! G05-04 视频号（`/api/videos`、`/api/video-creators`）。
//!
//! **转码完成才可发布**：`video_refs.state == "succeeded"` 是唯一门禁。
//! 没有回调的源文件、失败/排队的作业都不能变成 feed 里的一条视频——否则
//! 客户端只能拿到一个打不开的引用。
//!
//! 三种 feed 是三个**可解释**的查询，不是"假装有推荐算法"：
//!
//! * `follow`：我关注的创作者 + 自己，时间倒序；
//! * `hot`：近 7 天互动（赞×3 + 评论×2）排序，同分按 seq；
//! * `recommend`：我朋友赞过的、我还没关注的作者，按"朋友赞数"优先；
//!   结果里带 `reason`，客户端能解释"为什么给我看这个"。
//!
//! 游标停在 `seq` 或 `"{score}:{seq}"` 上，删除/拉黑在下一次查询即生效
//! （没有异步索引窗口）。直播标记读 `live_sessions` 的当前状态，停播后
//! `live` 立即变回 false。

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use crate::auth::Authed;
use crate::content_index;
use crate::error::{ApiError, PlatformError, Result};
use crate::extract::ApiJson;
use crate::favorites;
use crate::inbox;
use crate::validate;
use crate::{Platform, PlatformState};

pub const MAX_VIDEO_PAGE: i64 = 50;
/// 未显式给 limit 时的默认页大小。
pub const DEFAULT_PAGE: i64 = 20;

pub const MAX_CAPTION_CHARS: usize = 500;
pub const MAX_VIDEO_COMMENT_CHARS: usize = 1_000;
/// 热榜窗口。
const HOT_WINDOW_MS: i64 = 7 * 24 * 60 * 60 * 1000;

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/videos", post(publish))
        .route("/api/videos/feed", get(feed))
        .route("/api/videos/{id}", get(one))
        .route("/api/videos/{id}", delete(remove))
        .route("/api/videos/{id}/like", post(like))
        .route("/api/videos/{id}/like", delete(unlike))
        .route("/api/videos/{id}/comments", get(comments))
        .route("/api/videos/{id}/comments", post(comment))
        .route(
            "/api/videos/{id}/comments/{comment_id}",
            delete(remove_comment),
        )
        .route("/api/videos/{id}/report", post(report))
        .route("/api/video-creators/{id}/follow", post(follow))
        .route("/api/video-creators/{id}/follow", delete(unfollow))
}

#[derive(Deserialize)]
pub struct PublishVideoBody {
    pub media_id: String,
    #[serde(default)]
    pub caption: String,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct VideoView {
    pub id: String,
    pub author: String,
    pub author_name: String,
    pub caption: String,
    pub state: String,
    pub created_at: i64,
    pub like_count: i64,
    pub comment_count: i64,
    pub liked: bool,
    /// 作者此刻正在直播（读 `live_sessions.state = 'live'`）。
    pub live: bool,
    pub playable: bool,
    pub playlist_url: String,
    pub play_url: String,
    pub thumb_url: String,
    pub duration_ms: i64,
    /// 仅 `recommend` 查询填充："friends_liked" / "trending"。
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct VideoComment {
    pub seq: i64,
    pub id: String,
    pub video_id: String,
    pub author: String,
    pub author_name: String,
    pub text: String,
    pub created_at: i64,
}

#[derive(Deserialize)]
pub struct CommentBody {
    pub text: String,
}

impl Platform {
    /// 发布一条视频号内容。转码未成功的媒体直接拒绝（422 语义的 BadInput）。
    pub fn publish_video(&self, author: &str, media_id: &str, caption: &str) -> Result<VideoView> {
        let media_id = validate::id(media_id)?.to_string();
        if caption.chars().count() > MAX_CAPTION_CHARS {
            return Err(PlatformError::BadInput("caption too long"));
        }
        if caption.contains(|c: char| c.is_control() && c != '\n') {
            return Err(PlatformError::BadInput(
                "caption contains control characters",
            ));
        }
        {
            let c = self.db().lock();
            check_video_gate(&c, author, &media_id)?;
        }
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        // 幂等：同一作者重复发布同一份字节，返回已有记录。
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM video_posts
                 WHERE author = ?1 AND media_id = ?2 AND state <> 'deleted'
                 ORDER BY seq LIMIT 1",
                params![author, media_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            let view = video_row(&tx, author, &id)?.ok_or(PlatformError::NotFound)?;
            tx.commit()?;
            return Ok(view);
        }
        let id = crate::auth::random_hex(8);
        tx.execute(
            "INSERT INTO video_posts(seq, id, author, media_id, caption, state, created_at)
             VALUES(NULL, ?1, ?2, ?3, ?4, 'published', ?5)",
            params![id, author, media_id, caption, now],
        )?;
        content_index::upsert(
            &tx,
            &content_index::IndexDoc {
                doc_id: &id,
                kind: "video",
                owner: author,
                audience: "public",
                group_name: "",
                title: caption,
                body: caption,
            },
            now,
        )?;
        content_index::add_media_refs(&tx, &[media_id], "video", &id, author, now)?;
        let view = video_row(&tx, author, &id)?.ok_or(PlatformError::NotFound)?;
        tx.commit()?;
        Ok(view)
    }

    pub fn video_visible(&self, viewer: &str, id: &str) -> Result<bool> {
        let row: Option<(String, String)> = {
            let c = self.db().lock();
            c.query_row(
                "SELECT author, state FROM video_posts WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
        };
        let Some((author, state)) = row else {
            return Ok(false);
        };
        if state != "published" {
            return Ok(false);
        }
        if author == viewer {
            return Ok(true);
        }
        // is_blocked 自己会拿锁：这里必须先放下上面的 guard（连接不可重入）。
        if self.is_blocked(viewer, &author)? || self.is_blocked(&author, viewer)? {
            return Ok(false);
        }
        Ok(true)
    }

    pub fn video(&self, viewer: &str, id: &str) -> Result<VideoView> {
        let id = validate::id(id)?;
        if !self.video_visible(viewer, id)? {
            return Err(PlatformError::NotFound);
        }
        let c = self.db().lock();
        video_row(&c, viewer, id)?.ok_or(PlatformError::NotFound)
    }

    /// `mode` = follow / hot / recommend。返回 `(items, next_cursor, has_more)`。
    pub fn video_feed(
        &self,
        viewer: &str,
        mode: &str,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<(Vec<VideoView>, String, bool)> {
        let limit = limit.clamp(1, MAX_VIDEO_PAGE);
        let c = self.db().lock();
        match mode {
            "follow" => feed_by_seq(&c, viewer, cursor, limit),
            "hot" => feed_by_score(&c, viewer, cursor, limit, ScoreScope::Hot),
            "recommend" => feed_by_score(&c, viewer, cursor, limit, ScoreScope::Recommend),
            _ => Err(PlatformError::BadInput("unknown feed mode")),
        }
    }

    pub fn delete_video(&self, author: &str, id: &str) -> Result<()> {
        let id = validate::id(id)?;
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        let changed = tx.execute(
            "UPDATE video_posts SET state = 'deleted'
             WHERE id = ?1 AND author = ?2 AND state <> 'deleted'",
            params![id, author],
        )?;
        if changed == 0 {
            return Err(PlatformError::NotFound);
        }
        content_index::remove(&tx, id)?;
        content_index::remove_media_refs(&tx, "video", id)?;
        favorites::revoke_source(&tx, "video", id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn like_video(&self, id: &str, by: &str) -> Result<(bool, i64)> {
        let id = validate::id(id)?;
        if !self.video_visible(by, id)? {
            return Err(PlatformError::NotFound);
        }
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        let added = tx.execute(
            "INSERT INTO video_likes(video_id, user_id, created_at) VALUES(?1, ?2, ?3)
             ON CONFLICT(video_id, user_id) DO NOTHING",
            params![id, by, now],
        )?;
        if added == 1 {
            let author: Option<String> = tx
                .query_row(
                    "SELECT author FROM video_posts WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(author) = author {
                inbox::notify(
                    &tx,
                    &author,
                    "video_like",
                    by,
                    "video",
                    id,
                    "赞了你的视频",
                    &format!("like:video:{id}:{by}"),
                    now,
                )?;
            }
        }
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM video_likes WHERE video_id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok((added == 1, count))
    }

    pub fn unlike_video(&self, id: &str, by: &str) -> Result<(bool, i64)> {
        let id = validate::id(id)?;
        let c = self.db().lock();
        let removed = c.execute(
            "DELETE FROM video_likes WHERE video_id = ?1 AND user_id = ?2",
            params![id, by],
        )?;
        let count: i64 = c.query_row(
            "SELECT COUNT(*) FROM video_likes WHERE video_id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        Ok((removed == 1, count))
    }

    pub fn comment_video(&self, id: &str, by: &str, text: &str) -> Result<String> {
        let id = validate::id(id)?;
        if !self.video_visible(by, id)? {
            return Err(PlatformError::NotFound);
        }
        if text.trim().is_empty() || text.chars().count() > MAX_VIDEO_COMMENT_CHARS {
            return Err(PlatformError::BadInput("comment length out of range"));
        }
        if text.contains(|c: char| c.is_control() && c != '\n') {
            return Err(PlatformError::BadInput(
                "comment contains control characters",
            ));
        }
        let now = wxlike_core::types::now_ms() as i64;
        let comment_id = crate::auth::random_hex(8);
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        tx.execute(
            "INSERT INTO video_comments(seq, id, video_id, author, text, created_at)
             VALUES(NULL, ?1, ?2, ?3, ?4, ?5)",
            params![comment_id, id, by, text, now],
        )?;
        let author: Option<String> = tx
            .query_row(
                "SELECT author FROM video_posts WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(author) = author {
            inbox::notify(
                &tx,
                &author,
                "video_comment",
                by,
                "video",
                id,
                "评论了你的视频",
                "",
                now,
            )?;
        }
        tx.commit()?;
        Ok(comment_id)
    }

    pub fn video_comments(
        &self,
        id: &str,
        cursor: Option<i64>,
        limit: i64,
    ) -> Result<Vec<VideoComment>> {
        let id = validate::id(id)?;
        let limit = limit.clamp(1, MAX_VIDEO_PAGE);
        let before = cursor.unwrap_or(i64::MAX);
        let c = self.db().lock();
        let mut stmt = c.prepare(
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
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 视频作者或评论作者可删；软删除保留分页/计数稳定。
    pub fn delete_video_comment(&self, video_id: &str, comment_id: &str, by: &str) -> Result<()> {
        let video_id = validate::id(video_id)?;
        let comment_id = validate::id(comment_id)?;
        let changed = self.db().lock().execute(
            "UPDATE video_comments SET deleted = 1
             WHERE id = ?1 AND video_id = ?2 AND deleted = 0
               AND (author = ?3 OR ?3 = (SELECT author FROM video_posts WHERE id = ?2))",
            params![comment_id, video_id, by],
        )?;
        if changed == 0 {
            return Err(PlatformError::NotFound);
        }
        Ok(())
    }

    pub fn follow_creator(&self, user: &str, creator: &str) -> Result<()> {
        let creator = validate::id(creator)?;
        if creator == user {
            return Err(PlatformError::BadInput("cannot follow yourself"));
        }
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        let exists: Option<i64> = c
            .query_row("SELECT 1 FROM users WHERE id = ?1", params![creator], |r| {
                r.get(0)
            })
            .optional()?;
        if exists.is_none() {
            return Err(PlatformError::NotFound);
        }
        c.execute(
            "INSERT OR IGNORE INTO video_follows(user_id, creator, created_at)
             VALUES(?1, ?2, ?3)",
            params![user, creator, now],
        )?;
        Ok(())
    }

    pub fn unfollow_creator(&self, user: &str, creator: &str) -> Result<()> {
        let creator = validate::id(creator)?;
        self.db().lock().execute(
            "DELETE FROM video_follows WHERE user_id = ?1 AND creator = ?2",
            params![user, creator],
        )?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum ScoreScope {
    Hot,
    Recommend,
}

fn feed_by_seq(
    c: &Connection,
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
    let mut stmt = c.prepare(
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
    finish(c, viewer, ids, limit, |seq| seq.to_string())
}

fn feed_by_score(
    c: &Connection,
    viewer: &str,
    cursor: Option<&str>,
    limit: i64,
    scope: ScoreScope,
) -> Result<(Vec<VideoView>, String, bool)> {
    let (before_score, before_seq) = match cursor {
        Some(cur) if !cur.is_empty() => {
            let (s, q) = cur
                .split_once(':')
                .ok_or(PlatformError::BadInput("malformed cursor"))?;
            (
                s.parse::<i64>()
                    .map_err(|_| PlatformError::BadInput("malformed cursor"))?,
                q.parse::<i64>()
                    .map_err(|_| PlatformError::BadInput("malformed cursor"))?,
            )
        }
        _ => (i64::MAX, i64::MAX),
    };
    let cutoff = wxlike_core::types::now_ms() as i64 - HOT_WINDOW_MS;
    let mut views = Vec::new();
    let has_more;
    let next;
    match scope {
        ScoreScope::Hot => {
            let mut stmt = c.prepare(
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
            let more = rows.len() as i64 > limit;
            let take = if more {
                &rows[..rows.len() - 1]
            } else {
                &rows[..]
            };
            next = take
                .last()
                .map(|(s, _, score)| format!("{score}:{s}"))
                .unwrap_or_default();
            has_more = more;
            for (_, id, _) in take {
                if let Some(view) = video_row(c, viewer, id)? {
                    views.push(view);
                }
            }
        }
        ScoreScope::Recommend => {
            let mut stmt = c.prepare(
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
            let more = rows.len() as i64 > limit;
            let take = if more {
                &rows[..rows.len() - 1]
            } else {
                &rows[..]
            };
            next = take
                .last()
                .map(|(s, _, score)| format!("{score}:{s}"))
                .unwrap_or_default();
            has_more = more;
            for (_, id, _) in take {
                if let Some(mut view) = video_row(c, viewer, id)? {
                    // 能进推荐页的前提就是有好友看过（上面的 EXISTS），reason 固定。
                    view.reason = "friends_liked".into();
                    views.push(view);
                }
            }
        }
    }
    Ok((views, next, has_more))
}

fn finish(
    c: &Connection,
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
        if let Some(view) = video_row(c, viewer, id)? {
            views.push(view);
        }
    }
    Ok((views, next, has_more))
}

/// `video_row` 里播放地址段的下标（与 SELECT 列序一致）。
const COL_PLAYLIST_URL: usize = 11;
const COL_PLAY_URL: usize = 12;
const COL_DURATION_MS: usize = 13;
const COL_THUMB_URL: usize = 14;

/// 发布门禁：媒体必须是本人上传，且转码已 succeeded（唯一播放依据）。
fn check_video_gate(c: &Connection, author: &str, media_id: &str) -> Result<()> {
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

/// 一行视频视图的裸列（与 SELECT 顺序一致，命名靠解构）。
type VideoRow = (
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    String,
    String,
    i64,
    String,
);

fn video_row(c: &Connection, viewer: &str, id: &str) -> Result<Option<VideoView>> {
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
             WHERE v.id = ?1 AND v.state = 'published'",
            params![id, viewer],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(COL_PLAYLIST_URL)?,
                    r.get(COL_PLAY_URL)?,
                    r.get(COL_DURATION_MS)?,
                    r.get(COL_THUMB_URL)?,
                ))
            },
        )
        .optional()?;
    let Some((
        id,
        author,
        author_name,
        caption,
        state,
        created_at,
        like_count,
        comment_count,
        liked,
        live,
        playable,
        playlist_url,
        play_url,
        duration_ms,
        thumb_url,
    )) = row
    else {
        return Ok(None);
    };
    Ok(Some(VideoView {
        id,
        author,
        author_name,
        caption,
        state,
        created_at,
        like_count,
        comment_count,
        liked: liked != 0,
        live: live != 0,
        playable: playable != 0,
        playlist_url,
        play_url,
        thumb_url,
        duration_ms,
        reason: String::new(),
    }))
}

// ---- HTTP handlers ----

async fn publish(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<PublishVideoBody>,
) -> std::result::Result<(StatusCode, Json<VideoView>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(platform.publish_video(&authed.user_id, &body.media_id, &body.caption)?),
    ))
}

#[derive(Deserialize)]
struct FeedQuery {
    #[serde(default = "default_mode")]
    mode: String,
    cursor: Option<String>,
    limit: Option<i64>,
}

fn default_mode() -> String {
    "follow".to_string()
}

async fn feed(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<FeedQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let (items, next, has_more) = platform.video_feed(
        &authed.user_id,
        &q.mode,
        q.cursor.as_deref(),
        q.limit.unwrap_or(DEFAULT_PAGE),
    )?;
    Ok(Json(serde_json::json!({
        "items": items,
        "next_cursor": next,
        "has_more": has_more,
        "snapshot_version": wxlike_core::types::now_ms(),
    })))
}

async fn one(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<VideoView>, ApiError> {
    Ok(Json(platform.video(&authed.user_id, &id)?))
}

async fn remove(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.delete_video(&authed.user_id, &id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn like(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let (added, count) = platform.like_video(&id, &authed.user_id)?;
    Ok(Json(
        serde_json::json!({ "liked": true, "added": added, "count": count }),
    ))
}

async fn unlike(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let (removed, count) = platform.unlike_video(&id, &authed.user_id)?;
    Ok(Json(
        serde_json::json!({ "liked": false, "removed": removed, "count": count }),
    ))
}

#[derive(Deserialize)]
struct CommentQuery {
    cursor: Option<i64>,
    limit: Option<i64>,
}

async fn comments(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    Query(q): Query<CommentQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    if !platform.video_visible(&authed.user_id, &id)? {
        return Err(ApiError::from(PlatformError::NotFound));
    }
    let items = platform.video_comments(&id, q.cursor, q.limit.unwrap_or(DEFAULT_PAGE))?;
    let next = items.last().map(|c| c.seq).unwrap_or(0);
    Ok(Json(
        serde_json::json!({ "items": items, "next_cursor": next }),
    ))
}

async fn comment(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<CommentBody>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let id = platform.comment_video(&id, &authed.user_id, &body.text)?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "comment_id": id })),
    ))
}

async fn remove_comment(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path((video_id, comment_id)): Path<(String, String)>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.delete_video_comment(&video_id, &comment_id, &authed.user_id)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ReportBody {
    reason: String,
    #[serde(default)]
    detail: String,
}

async fn report(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ReportBody>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    if !platform.video_visible(&authed.user_id, &id)? {
        return Err(ApiError::from(PlatformError::NotFound));
    }
    let reason = wxlike_report::Reason::parse(&body.reason)
        .ok_or_else(|| ApiError::from(PlatformError::BadInput("unknown report reason")))?;
    let ticket = platform.create_report(&authed.user_id, "video", &id, reason, &body.detail)?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "report_id": ticket.id })),
    ))
}

async fn follow(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.follow_creator(&authed.user_id, &id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unfollow(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.unfollow_creator(&authed.user_id, &id)?;
    Ok(StatusCode::NO_CONTENT)
}
