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
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::video_feed`]。

use crate::error::{PlatformError, Result};
use crate::validate;
use crate::Platform;
use serde::Serialize;
use wxlike_core::tx::ConnectionTx as _;

use super::repo::video_feed::{
    check_video_gate, creator_exists, delete_video_like, feed_by_seq, feed_hot, feed_recommend,
    find_active_video_post, follow_creator, index_video, insert_video_comment, insert_video_like,
    insert_video_post, review_record, set_video_state, soft_delete_video,
    soft_delete_video_comment, unfollow_creator, video_access, video_author, video_comments,
    video_like_count, video_row,
};

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
pub(in crate::domain) const HOT_WINDOW_MS: i64 = 7 * 24 * 60 * 60 * 1000;

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

impl Platform {
    /// 发布一条视频号内容。转码未成功的媒体直接拒绝（422 语义的 BadInput）。
    pub(crate) fn publish_video(
        &self,
        author: &str,
        media_id: &str,
        caption: &str,
    ) -> Result<VideoView> {
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
        let (id, view) = c.with_tx(|tx| -> Result<(String, VideoView)> {
            // 幂等：同一作者重复发布同一份字节，返回已有记录。
            if let Some(existing) = find_active_video_post(tx, author, &media_id)? {
                let view = video_row(tx, author, &existing)?.ok_or(PlatformError::NotFound)?;
                return Ok((existing, view));
            }
            let id = crate::auth::random_hex(8);
            insert_video_post(tx, &id, author, &media_id, caption, state, now)?;
            if state == STATE_PUBLISHED {
                index_video(tx, &id, author, caption, &media_id, now)?;
            }
            let view = video_row(tx, author, &id)?.ok_or(PlatformError::NotFound)?;
            Ok((id, view))
        })?;
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
    /// 返回 `ReviewResult` 而不是 `VideoView`：驳回后这条内容**对外不存在**，
    /// 用视图当返回值会逼读一条本来就该读不到的行（`video_row` 只认
    /// published/pending），事务会被自己的善后语句带崩。
    pub(crate) fn review_video(&self, id: &str, actor: &str, action: &str) -> Result<ReviewResult> {
        if !REVIEW_ACTIONS.contains(&action) {
            return Err(PlatformError::BadInput("unknown review action"));
        }
        let id = validate::id(id)?.to_string();
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        let result = c.with_tx(|tx| -> Result<ReviewResult> {
            let row = review_record(tx, &id)?.ok_or(PlatformError::NotFound)?;
            let (author, state, caption, media_id) =
                (row.author, row.state, row.caption, row.media_id);
            if state != STATE_PENDING {
                return Err(PlatformError::NotFound);
            }
            let (next, note) = if action == "publish" {
                (STATE_PUBLISHED, "视频审核通过，已发布")
            } else {
                (STATE_REJECTED, "视频未通过审核")
            };
            set_video_state(tx, &id, next)?;
            if action == "publish" {
                index_video(tx, &id, &author, &caption, &media_id, now)?;
            } else {
                // pending 本来就没进索引，这里删 0 行：走的是与“直接发布”完全
                // 对称的撤权路径，不给驳回留一个“引用还在”的分支。
                crate::domain::repo::content_index::remove_tx(tx, &id)?;
                crate::domain::repo::content_index::remove_media_refs_tx(tx, "video", &id)?;
                crate::domain::repo::favorites::revoke_source_tx(tx, "video", &id)?;
            }
            crate::domain::repo::inbox::notify_tx(
                tx,
                &author,
                "video_review",
                &format!("admin:{actor}"),
                "video",
                &id,
                note,
                &format!("video_review:{id}"),
                now,
            )?;
            Ok(ReviewResult {
                id: id.clone(),
                state: next.to_string(),
                author,
            })
        })?;
        drop(c);
        self.audit(actor, &format!("video.review_{action}"), &id, "");
        Ok(result)
    }

    pub(crate) fn video_visible(&self, viewer: &str, id: &str) -> Result<bool> {
        let access = {
            let c = self.db().lock();
            video_access(&c, id)?
        };
        let Some(access) = access else {
            return Ok(false);
        };
        let (author, state) = (access.author, access.state);
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

    pub(crate) fn video(&self, viewer: &str, id: &str) -> Result<VideoView> {
        let id = validate::id(id)?;
        if !self.video_visible(viewer, id)? {
            return Err(PlatformError::NotFound);
        }
        let c = self.db().lock();
        video_row(&c, viewer, id)?.ok_or(PlatformError::NotFound)
    }

    /// `mode` = follow / hot / recommend。返回 `(items, next_cursor, has_more)`。
    pub(crate) fn video_feed(
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

    pub(crate) fn delete_video(&self, author: &str, id: &str) -> Result<()> {
        let id = validate::id(id)?;
        let mut c = self.db().lock();
        c.with_tx(|tx| -> Result<()> {
            if !soft_delete_video(tx, id, author, STATE_DELETED)? {
                return Err(PlatformError::NotFound);
            }
            crate::domain::repo::content_index::remove_tx(tx, id)?;
            crate::domain::repo::content_index::remove_media_refs_tx(tx, "video", id)?;
            crate::domain::repo::favorites::revoke_source_tx(tx, "video", id)?;
            Ok(())
        })
    }

    pub(crate) fn like_video(&self, id: &str, by: &str) -> Result<(bool, i64)> {
        let id = validate::id(id)?;
        if !self.video_visible(by, id)? {
            return Err(PlatformError::NotFound);
        }
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        c.with_tx(|tx| -> Result<(bool, i64)> {
            let added = insert_video_like(tx, id, by, now)?;
            if added {
                if let Some(author) = video_author(tx, id)? {
                    crate::domain::repo::inbox::notify_tx(
                        tx,
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
            let count = video_like_count(tx, id)?;
            Ok((added, count))
        })
    }

    pub(crate) fn unlike_video(&self, id: &str, by: &str) -> Result<(bool, i64)> {
        let id = validate::id(id)?;
        let c = self.db().lock();
        let removed = delete_video_like(&c, id, by)?;
        let count = video_like_count(&c, id)?;
        Ok((removed, count))
    }

    pub(crate) fn comment_video(&self, id: &str, by: &str, text: &str) -> Result<String> {
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
        c.with_tx(|tx| -> Result<()> {
            insert_video_comment(tx, &comment_id, id, by, text, now)?;
            if let Some(author) = video_author(tx, id)? {
                crate::domain::repo::inbox::notify_tx(
                    tx,
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
            Ok(())
        })?;
        Ok(comment_id)
    }

    pub(crate) fn video_comments(
        &self,
        id: &str,
        cursor: Option<i64>,
        limit: i64,
    ) -> Result<Vec<VideoComment>> {
        let id = validate::id(id)?;
        let limit = limit.clamp(1, MAX_VIDEO_PAGE);
        let before = cursor.unwrap_or(i64::MAX);
        let c = self.db().lock();
        Ok(video_comments(&c, id, before, limit)?
            .into_iter()
            .map(|cm| VideoComment {
                seq: cm.seq,
                id: cm.id,
                video_id: cm.video_id,
                author: cm.author,
                author_name: cm.author_name,
                text: cm.text,
                created_at: cm.created_at,
            })
            .collect())
    }

    /// 视频作者或评论作者可删；软删除保留分页/计数稳定。
    pub(crate) fn delete_video_comment(
        &self,
        video_id: &str,
        comment_id: &str,
        by: &str,
    ) -> Result<()> {
        let video_id = validate::id(video_id)?;
        let comment_id = validate::id(comment_id)?;
        if !soft_delete_video_comment(&self.db().lock(), video_id, comment_id, by)? {
            return Err(PlatformError::NotFound);
        }
        Ok(())
    }

    pub(crate) fn follow_creator(&self, user: &str, creator: &str) -> Result<()> {
        let creator = validate::id(creator)?;
        if creator == user {
            return Err(PlatformError::BadInput("cannot follow yourself"));
        }
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        if !creator_exists(&c, creator)? {
            return Err(PlatformError::NotFound);
        }
        follow_creator(&c, user, creator, now)
    }

    pub(crate) fn unfollow_creator(&self, user: &str, creator: &str) -> Result<()> {
        let creator = validate::id(creator)?;
        unfollow_creator(&self.db().lock(), user, creator)
    }
}

/// `"{score}:{seq}"` 游标；空 = 从头。
pub(in crate::domain) fn parse_score_cursor(cursor: Option<&str>) -> Result<(i64, i64)> {
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

/// `video_row` 里播放地址段的下标（与 SELECT 列序一致）。
pub(in crate::domain) const COL_PLAYLIST_URL: usize = 11;
pub(in crate::domain) const COL_PLAY_URL: usize = 12;
pub(in crate::domain) const COL_DURATION_MS: usize = 13;
pub(in crate::domain) const COL_THUMB_URL: usize = 14;

/// `video_row` 查出的裸行；字段名与 SELECT 列序一一对应。
pub(in crate::domain) struct VideoRow {
    pub(crate) id: String,
    pub(crate) author: String,
    pub(crate) author_name: String,
    pub(crate) caption: String,
    pub(crate) state: String,
    pub(crate) created_at: i64,
    pub(crate) like_count: i64,
    pub(crate) comment_count: i64,
    pub(crate) liked: bool,
    pub(crate) live: bool,
    pub(crate) playable: bool,
    pub(crate) playlist_url: String,
    pub(crate) play_url: String,
    pub(crate) duration_ms: i64,
    pub(crate) thumb_url: String,
}

/// 裸行 → 对外视图：纯字段搬运（reason 由调用方按门禁补写）。
pub(in crate::domain) fn video_view_from(row: VideoRow) -> VideoView {
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

// ---- HTTP handlers ----
pub(crate) fn default_mode() -> String {
    "follow".to_string()
}

/// 复核结果：只回状态机事实，不回内容视图（驳回后内容对外不存在）。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ReviewResult {
    pub id: String,
    pub state: String,
    pub author: String,
}
