//! G05-04 视频号（`/api/videos`、`/api/video-creators`）。
//!
//! **两道门禁，缺一不可**：
//!
//! 1. **转码完成才可发布**：`video_refs.state == "succeeded"`。
//!    没有回调的源文件、失败/排队的作业都不能变成 feed 里的一条视频——否则
//!    客户端只能拿到一个打不开的引用。
//! 2. **审核过才可分发**（issue #132 验收第 1 条"发布→审核→播放"）：正文走
//!    `wxlike_moderation` 判定。`Reject` 不落行；`Hold`（先审后发 + 软违规）
//!    落成 `state = 'pending'`——**不写索引、不写附件引用、不进任何 feed**，
//!    只有作者能看到自己的待审记录。运营通过 `/api/videos/{id}/review`
//!    放行或拒绝，放行才补索引与引用，拒绝才撤收藏。
//!
//! 判定与写行在同一次同步临界区里完成，因此不存在"先 published 再改 pending"
//! 的窗口。
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

use crate::auth::{AdminAuth, Authed};
use crate::content_index;
use crate::db::offload;
use crate::error::{ApiError, PlatformError, Result};
use crate::extract::ApiJson;
use crate::favorites;
use crate::inbox;
use crate::page;
use crate::validate;
use crate::{Platform, PlatformState};

pub const MAX_VIDEO_PAGE: i64 = 50;

pub const MAX_CAPTION_CHARS: usize = 500;
pub const MAX_VIDEO_COMMENT_CHARS: usize = 1_000;

/// `video_posts.state` 的四个取值。`pending` / `rejected` 都不进检索索引，
/// 因此审核中的内容不可能出现在 `/api/search/content` 里。
pub const STATE_PUBLISHED: &str = "published";
pub const STATE_PENDING: &str = "pending";
pub const STATE_REJECTED: &str = "rejected";
pub const STATE_DELETED: &str = "deleted";
/// 运营复核动作白名单。
const REVIEW_ACTIONS: [&str; 2] = ["publish", "reject"];
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
        .route("/api/videos/{id}/review", post(review))
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
        check_video_caption(caption)?;
        {
            let c = self.db().lock();
            check_video_gate(&c, author, &media_id)?;
        }
        // 审核判定不碰库（只过词表），所以放在取连接之前：先定状态，再写行。
        let decision = self.moderate(caption);
        let state = match decision.outcome {
            // 硬违规：连行都不落，不制造一个"待审"的假象
            wxlike_moderation::Outcome::Reject => {
                return Err(PlatformError::Forbidden);
            }
            // 先审后发 + 软违规：落 pending，不给索引、不给引用、不进 feed
            wxlike_moderation::Outcome::Hold => STATE_PENDING,
            _ => STATE_PUBLISHED,
        };
        let labels = decision.labels.clone();
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        // 幂等：同一作者重复发布同一份字节，返回已有记录。
        let existing = find_active_video_post(&tx, author, &media_id)?;
        if let Some(id) = existing {
            let view = video_row(&tx, author, &id)?.ok_or(PlatformError::NotFound)?;
            tx.commit()?;
            return Ok(view);
        }
        let id = crate::auth::random_hex(8);
        tx.execute(
            "INSERT INTO video_posts(seq, id, author, media_id, caption, state, created_at)
             VALUES(NULL, ?1, ?2, ?3, ?4, ?6, ?5)",
            params![id, author, media_id, caption, now, state],
        )?;
        if state == STATE_PUBLISHED {
            index_video(&tx, &id, author, caption, &media_id, now)?;
        }
        let view = video_row(&tx, author, &id)?.ok_or(PlatformError::NotFound)?;
        tx.commit()?;
        drop(c);
        // 审计不能拿连接锁调用（`audit` 自己会 lock），所以放在事务提交之后。
        if state != STATE_PUBLISHED || !labels.is_empty() {
            self.audit(
                author,
                if state == STATE_PENDING {
                    "video.review_pending"
                } else {
                    "video.publish_flagged"
                },
                &id,
                &labels.join(","),
            );
        }
        Ok(view)
    }

    /// 运营复核待审视频。放行才补索引 + 附件引用 + 通知作者；拒绝撤收藏引用。
    /// 非 `pending` 的行一律 404（与"不存在"同形），复核端点因此不是探测面。
    ///
    /// 返回 [`ReviewResult`] 而不是 [`VideoView`]：驳回后这条内容**对外不存在**，
    /// 用视图当返回值会逼读一条本来就该读不到的行（`video_row` 只认
    /// published/pending），事务会被自己的善后语句带崩。
    pub fn review_video(&self, id: &str, actor: &str, action: &str) -> Result<ReviewResult> {
        if !REVIEW_ACTIONS.contains(&action) {
            return Err(PlatformError::BadInput("unknown review action"));
        }
        let id = validate::id(id)?.to_string();
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        let row: Option<(String, String, String, String)> = tx
            .query_row(
                "SELECT author, state, caption, media_id FROM video_posts WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let (author, state, caption, media_id) = row.ok_or(PlatformError::NotFound)?;
        if state != STATE_PENDING {
            return Err(PlatformError::NotFound);
        }
        let (next, note) = if action == "publish" {
            (STATE_PUBLISHED, "视频审核通过，已发布")
        } else {
            (STATE_REJECTED, "视频未通过审核")
        };
        tx.execute(
            "UPDATE video_posts SET state = ?2 WHERE id = ?1",
            params![id, next],
        )?;
        if action == "publish" {
            index_video(&tx, &id, &author, &caption, &media_id, now)?;
        } else {
            // pending 本来就没进索引，这里删 0 行：走的是与"直接发布"完全
            // 对称的撤权路径，不给驳回留一个"引用还在"的分支。
            content_index::remove(&tx, &id)?;
            content_index::remove_media_refs(&tx, "video", &id)?;
            favorites::revoke_source(&tx, "video", &id)?;
        }
        inbox::notify(
            &tx,
            &author,
            "video_review",
            &format!("admin:{actor}"),
            "video",
            &id,
            note,
            &format!("video_review:{id}"),
            now,
        )?;
        tx.commit()?;
        drop(c);
        self.audit(actor, &format!("video.review_{action}"), &id, "");
        Ok(ReviewResult {
            id,
            state: next.to_string(),
            author,
        })
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
        if author == viewer {
            // 待审记录对作者自己可见（否则客户端无法展示"审核中"）；
            // 已删/已驳回的一律按不存在处理。
            return Ok(state == STATE_PUBLISHED || state == STATE_PENDING);
        }
        if state != STATE_PUBLISHED {
            return Ok(false);
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
            "hot" => feed_hot(&c, viewer, cursor, limit),
            "recommend" => feed_recommend(&c, viewer, cursor, limit),
            _ => Err(PlatformError::BadInput("unknown feed mode")),
        }
    }

    pub fn delete_video(&self, author: &str, id: &str) -> Result<()> {
        let id = validate::id(id)?;
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        let changed = tx.execute(
            "UPDATE video_posts SET state = ?3
             WHERE id = ?1 AND author = ?2 AND state NOT IN ('deleted', 'rejected')",
            params![id, author, STATE_DELETED],
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

/// 热门：7 天窗口内按点赞/评论加权；新近发的一条保底 1 分。
fn feed_hot(
    c: &Connection,
    viewer: &str,
    cursor: Option<&str>,
    limit: i64,
) -> Result<(Vec<VideoView>, String, bool)> {
    let (before_score, before_seq) = parse_score_cursor(cursor)?;
    let cutoff = wxlike_core::types::now_ms() as i64 - HOT_WINDOW_MS;
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
    page_of_scored(c, viewer, rows, limit, "trending")
}

/// 推荐：只推"好友看过的未关注作者作品"。没有社交痕迹就什么都不推——
/// 陌生人的热门视频只属于热门页，不冒充"为你推荐"。
fn feed_recommend(
    c: &Connection,
    viewer: &str,
    cursor: Option<&str>,
    limit: i64,
) -> Result<(Vec<VideoView>, String, bool)> {
    let (before_score, before_seq) = parse_score_cursor(cursor)?;
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
    page_of_scored(c, viewer, rows, limit, "friends_liked")
}

/// `"{score}:{seq}"` 游标；空 = 从头。
fn parse_score_cursor(cursor: Option<&str>) -> Result<(i64, i64)> {
    match cursor {
        Some(cur) if !cur.is_empty() => {
            let (s, q) = cur
                .split_once(':')
                .ok_or(PlatformError::BadInput("malformed cursor"))?;
            Ok((
                s.parse::<i64>()
                    .map_err(|_| PlatformError::BadInput("malformed cursor"))?,
                q.parse::<i64>()
                    .map_err(|_| PlatformError::BadInput("malformed cursor"))?,
            ))
        }
        _ => Ok((i64::MAX, i64::MAX)),
    }
}

/// 打分流的公共尾部：多取一条判 has_more，游标取自页尾，逐条补视图。
fn page_of_scored(
    c: &Connection,
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
        if let Some(mut view) = video_row(c, viewer, id)? {
            view.reason = reason.to_string();
            views.push(view);
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

/// 索引 + 附件引用：只在内容真正可分发时写。审核通过走同一个函数，
/// 所以"放行"与"直接发布"的投影完全一致，不会有两套写法。
fn index_video(
    tx: &rusqlite::Connection,
    id: &str,
    author: &str,
    caption: &str,
    media_id: &str,
    now: i64,
) -> Result<()> {
    content_index::upsert(
        tx,
        &content_index::IndexDoc {
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
    content_index::add_media_refs(tx, &[media_id.to_string()], "video", id, author, now)?;
    Ok(())
}

/// 发布门禁：媒体必须是本人上传，且转码已 succeeded（唯一播放依据）。
/// 字幕输入校验（长度与控制字符）。发布路径入口用它，主体保持编排职责。
fn check_video_caption(caption: &str) -> Result<()> {
    if caption.chars().count() > MAX_CAPTION_CHARS {
        return Err(PlatformError::BadInput("caption too long"));
    }
    if caption.contains(|c: char| c.is_control() && c != '\n') {
        return Err(PlatformError::BadInput(
            "caption contains control characters",
        ));
    }
    Ok(())
}

/// 幂等发布查找：同作者同媒体、仍处于 published/pending 的既有行 id。
fn find_active_video_post(c: &Connection, author: &str, media_id: &str) -> Result<Option<String>> {
    c.query_row(
        "SELECT id FROM video_posts
         WHERE author = ?1 AND media_id = ?2 AND state IN ('published', 'pending')
         ORDER BY seq LIMIT 1",
        params![author, media_id],
        |r| r.get(0),
    )
    .optional()
}

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

/// `video_row` 里播放地址段的下标（与 SELECT 列序一致）。
const COL_PLAYLIST_URL: usize = 11;
const COL_PLAY_URL: usize = 12;
const COL_DURATION_MS: usize = 13;
const COL_THUMB_URL: usize = 14;

/// `video_row` 查出的裸行；字段名与 SELECT 列序一一对应。
struct VideoRow {
    id: String,
    author: String,
    author_name: String,
    caption: String,
    state: String,
    created_at: i64,
    like_count: i64,
    comment_count: i64,
    liked: bool,
    live: bool,
    playable: bool,
    playlist_url: String,
    play_url: String,
    duration_ms: i64,
    thumb_url: String,
}
/// 裸行 → 对外视图：纯字段搬运（reason 由调用方按门禁补写）。
fn video_view_from(row: VideoRow) -> VideoView {
    VideoView {
        id: row.id,
        author: row.author,
        author_name: row.author_name,
        caption: row.caption,
        state: row.state,
        created_at: row.created_at,
        like_count: row.like_count,
        comment_count: row.comment_count,
        liked: row.liked,
        live: row.live,
        playable: row.playable,
        playlist_url: row.playlist_url,
        play_url: row.play_url,
        thumb_url: row.thumb_url,
        duration_ms: row.duration_ms,
        reason: String::new(),
    }
}

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

// ---- HTTP handlers ----

async fn publish(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<PublishVideoBody>,
) -> std::result::Result<(StatusCode, Json<VideoView>), ApiError> {
    let author = authed.user_id.clone();
    let media_id = body.media_id.clone();
    let caption = body.caption.clone();
    Ok((
        StatusCode::CREATED,
        Json(
            offload(&platform, move |p| {
                p.publish_video(&author, &media_id, &caption)
            })
            .await?,
        ),
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
        q.limit.unwrap_or(page::DEFAULT_PAGE),
    )?;
    Ok(page::listing_response(items, next, has_more))
}

async fn one(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<VideoView>, ApiError> {
    let user = authed.user_id.clone();
    Ok(Json(
        offload(&platform, move |p| p.video(&user, &id)).await?,
    ))
}

async fn remove(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let user = authed.user_id.clone();
    offload(&platform, move |p| p.delete_video(&user, &id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn like(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let user = authed.user_id.clone();
    let (added, count) = offload(&platform, move |p| p.like_video(&id, &user)).await?;
    Ok(Json(
        serde_json::json!({ "liked": true, "added": added, "count": count }),
    ))
}

async fn unlike(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let user = authed.user_id.clone();
    let (removed, count) = offload(&platform, move |p| p.unlike_video(&id, &user)).await?;
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
    let user = authed.user_id.clone();
    let cursor = q.cursor;
    let limit = q.limit.unwrap_or(page::DEFAULT_PAGE);
    // 授权与取页在同一次 offload 里：分成两次会把"可见性判定"和"读评论"
    // 之间留出 await 间隙，删帖/拉黑正好能挤进来。
    let items = offload(&platform, move |p| {
        if !p.video_visible(&user, &id)? {
            return Err(PlatformError::NotFound);
        }
        p.video_comments(&id, cursor, limit)
    })
    .await?;
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
    let user = authed.user_id.clone();
    let text = body.text.clone();
    let id = offload(&platform, move |p| p.comment_video(&id, &user, &text)).await?;
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
    let user = authed.user_id.clone();
    offload(&platform, move |p| {
        p.delete_video_comment(&video_id, &comment_id, &user)
    })
    .await?;
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
    let id = validate::id(&id).map_err(ApiError::from)?.to_string();
    let reason = wxlike_report::Reason::parse(&body.reason)
        .ok_or_else(|| ApiError::from(PlatformError::BadInput("unknown report reason")))?;
    let user = authed.user_id.clone();
    let detail = body.detail.clone();
    let ticket = offload(&platform, move |p| {
        if !p.video_visible(&user, &id)? {
            return Err(PlatformError::NotFound);
        }
        p.create_report(&user, "video", &id, reason, &detail)
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "report_id": ticket.id })),
    ))
}

#[derive(Deserialize)]
struct ReviewBody {
    /// `publish` = 放行，`reject` = 驳回。
    pub action: String,
}

/// 复核结果：只回状态机事实，不回内容视图（驳回后内容对外不存在）。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ReviewResult {
    pub id: String,
    pub state: String,
    pub author: String,
}

/// 运营复核待审视频。`AdminAuth` 排在 body 之前：没令牌就是 403，
/// 不会先解析 body 而把动作枚举暴露出去。
async fn review(
    State(platform): State<PlatformState>,
    admin: AdminAuth,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ReviewBody>,
) -> std::result::Result<Json<ReviewResult>, ApiError> {
    let actor = admin.actor.clone();
    let action = body.action.clone();
    let result = offload(&platform, move |p| p.review_video(&id, &actor, &action)).await?;
    Ok(Json(result))
}

async fn follow(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let user = authed.user_id.clone();
    offload(&platform, move |p| p.follow_creator(&user, &id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unfollow(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let user = authed.user_id.clone();
    offload(&platform, move |p| p.unfollow_creator(&user, &id)).await?;
    Ok(StatusCode::NO_CONTENT)
}
