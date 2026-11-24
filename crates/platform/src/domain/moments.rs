//! WP-07 moments: publish, timeline, likes, comments, and visibility.
//!
//! **Visibility is evaluated at read time**, not snapshotted at publish time.
//! A `Friends` post is visible to whoever is a mutual friend *now*, which means
//! removing a friend immediately removes their friends-only history from your
//! feed — the behaviour WP-07 A1 asks to see. The alternative (freezing an ACL
//! per post at publish) would need the audience materialised at write time and
//! would keep showing history after an unfriend; we deliberately chose read-time
//! evaluation and this paragraph is the disclosure the task book asks for.
//!
//! The feed itself is restricted to *self + friends* (contract), so a stranger's
//! `Public` post never appears in your timeline; `Public` only widens who may
//! open a specific moment by id.
//!
//! Paging: the feed is time-descending as required, so its cursor is a
//! **watermark**: pass `since` = the `next_cursor` from the previous page to get
//! strictly older entries. This is the opposite direction from the ascending
//! ledger / contact cursors, and that is inherent to newest-first ordering.
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::moments`]。

use crate::error::{PlatformError, Result};
use crate::storage::StorageConnection;
use crate::validate;
use crate::Platform;
use serde::{Deserialize, Serialize};
use wxlike_core::tx::ConnectionTx as _;

use super::repo::moments::{
    album_page, check_reply_target, delete_draft, delete_draft_for_author, delete_like,
    drafts_by_author, fanout_new_moment, feed_page, friend_group_contains, insert_comment,
    insert_draft, insert_like, insert_moment, moment_access, moment_author_if_live,
    moment_comments, moment_likes, moment_record, notify_comment_inbox, require_media,
    soft_delete_comment, soft_delete_moment, update_draft, MomentRecord,
};

/// Contract: text above this is refused (the task book allows either refusing
/// or accepting-with-documentation; we refuse).
pub const MAX_TEXT_CHARS: usize = 5_000;
pub const MAX_COMMENT_CHARS: usize = 1_000;

/// WeChat-style nine-grid.
pub const MAX_MEDIA_PER_MOMENT: usize = 9;
pub const MAX_FEED_PAGE: i64 = 50;

/// 评论层级上限：顶层评论 depth=0，直接回复=1，再往下=2；更深的回复拒绝。
/// 微信没有无限嵌套线程，链表式的 `reply_to` 也容易被用来构造超深树。
pub const MAX_COMMENT_DEPTH: usize = 2;

/// 分组名是作者好友分组（`friends.friend_group`）的字符串，不是新对象。
pub const MAX_GROUP_CHARS: usize = 32;

/// 一次发布最多扇出多少条提醒。大 V 式的好友规模不应该让一次 POST 变成
/// 万行插入；超出的部分靠 feed 的读时查询兜住（提醒不是真相）。
pub const MAX_FANOUT_RECIPIENTS: i64 = 500;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Public,
    Friends,
    /// 仅作者自己一个好友分组可见（`audience_group` 必填）。
    Groups,
    /// 仅自己可见（微信的"私密"）。**不进任何人的 feed**，包括好友的。
    Private,
}

impl Visibility {
    fn as_str(self) -> &'static str {
        match self {
            Visibility::Public => "public",
            Visibility::Friends => "friends",
            Visibility::Groups => "groups",
            Visibility::Private => "private",
        }
    }
}

pub(crate) fn default_visibility() -> Visibility {
    Visibility::Friends
}

#[derive(Serialize, Clone, Debug)]
pub struct MomentHead {
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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MomentDraft {
    pub id: String,
    pub author: String,
    pub text: String,
    pub media_ids: Vec<String>,
    pub visibility: String,
    pub audience_group: String,
    pub updated_at: i64,
}

fn check_moment_input(
    text: &str,
    media_ids: &[String],
    visibility: Visibility,
    audience_group: &str,
    allow_empty: bool,
) -> Result<(String, Vec<String>)> {
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(PlatformError::BadInput("moment text too long"));
    }
    if text.contains(|c: char| c.is_control() && c != '\n') {
        return Err(PlatformError::BadInput(
            "moment text contains control characters",
        ));
    }
    if media_ids.len() > MAX_MEDIA_PER_MOMENT {
        return Err(PlatformError::BadInput("too many media items"));
    }
    if !allow_empty && text.trim().is_empty() && media_ids.is_empty() {
        return Err(PlatformError::BadInput("moment is empty"));
    }
    let group = check_group(visibility, audience_group)?;
    Ok((group, validate_media_ids(media_ids)?))
}

fn check_group(visibility: Visibility, group: &str) -> Result<String> {
    let group = group.trim();
    if visibility != Visibility::Groups {
        return Ok(String::new());
    }
    if group.is_empty() {
        return Err(PlatformError::BadInput("audience_group is required"));
    }
    if group.chars().count() > MAX_GROUP_CHARS || group.chars().any(char::is_control) {
        return Err(PlatformError::BadInput("audience_group malformed"));
    }
    Ok(group.to_string())
}

fn validate_media_ids(media_ids: &[String]) -> Result<Vec<String>> {
    media_ids
        .iter()
        .map(|m| validate::id(m).map(|s| s.to_string()))
        .collect()
}

/// 自底向上数 `reply_to` 链；循环上限防止损坏数据里的环把请求卡死。
/// 评论正文的三道闸：非空、限长、无控制字符（换行放行）。
fn check_comment_text(text: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Err(PlatformError::BadInput("comment must not be empty"));
    }
    if text.chars().count() > MAX_COMMENT_CHARS {
        return Err(PlatformError::BadInput("comment too long"));
    }
    if text.contains(|c: char| c.is_control() && c != '\n') {
        return Err(PlatformError::BadInput(
            "comment contains control characters",
        ));
    }
    Ok(())
}

/// 动态进统一检索索引的文档形状：可见性字段与落库行同源，
/// 避免"行已更新、索引口径还手写一遍"的两处漂移。
fn moment_index_doc<'a>(
    id: &'a str,
    author: &'a str,
    visibility: Visibility,
    group: &'a str,
    text: &'a str,
) -> crate::domain::content_index::IndexDoc<'a> {
    crate::domain::content_index::IndexDoc {
        doc_id: id,
        kind: "moment",
        owner: author,
        audience: visibility.as_str(),
        group_name: group,
        title: "",
        body: text,
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct CommentRow {
    pub comment_id: String,
    pub author: String,
    pub author_name: String,
    pub text: String,
    /// Empty for a top-level comment. The tree is the adjacency list: clients
    /// (and the acceptance script) reconstruct nesting from this link.
    pub reply_to: String,
    pub reply_to_name: String,
    pub created_at: i64,
}

#[derive(Serialize)]
pub struct MomentDetail {
    #[serde(flatten)]
    pub head: MomentHead,
    pub likes: Vec<String>,
    pub comments: Vec<CommentRow>,
}

impl MomentRecord {
    fn into_head(self) -> MomentHead {
        MomentHead {
            moment_id: self.moment_id,
            author: self.author,
            author_name: self.author_name,
            text: self.text,
            media_ids: self.media_ids,
            visibility: self.visibility,
            created_at: self.created_at,
            like_count: self.like_count,
            comment_count: self.comment_count,
        }
    }
}

fn page_from_rows(rows: Vec<MomentRecord>, limit: i64, upper: i64) -> (Vec<MomentHead>, i64, bool) {
    let has_more = rows.len() as i64 > limit;
    let next = if has_more {
        rows.get(limit as usize - 1).map(|r| r.seq).unwrap_or(upper)
    } else {
        rows.last().map(|r| r.seq).unwrap_or(upper)
    };
    let heads = rows
        .into_iter()
        .take(limit as usize)
        .map(MomentRecord::into_head)
        .collect();
    (heads, next, has_more)
}

/// 点赞/评论/举报/删评论共用的可见性闸。
/// 同步函数：调用方已经在 `offload` 里，不能再 `await`（ADR-28）。
pub(crate) fn require_visible(platform: &Platform, moment_id: &str, viewer: &str) -> Result<()> {
    platform.moment_row(moment_id)?;
    if !platform.moment_visible(viewer, moment_id)? {
        return Err(PlatformError::NotFound);
    }
    Ok(())
}

impl Platform {
    /// 发布（可带分组可见与草稿来源）。发布、索引、媒体引用、删草稿同事务。
    pub(crate) fn publish_moment_with(
        &self,
        author: &str,
        text: &str,
        media_ids: &[String],
        visibility: Visibility,
        audience_group: &str,
        draft_id: Option<&str>,
    ) -> Result<String> {
        let author = validate::id(author)?.to_string();
        let (group, cleaned) =
            check_moment_input(text, media_ids, visibility, audience_group, false)?;
        let id = crate::auth::random_hex(8);
        let now = wxlike_core::types::now_ms() as i64;

        let mut c = self.db().lock();
        c.with_tx(|tx| -> Result<()> {
            let port = StorageConnection::sqlite_tx(tx);
            require_media(&port, &cleaned)?;
            insert_moment(
                &port,
                &id,
                &author,
                text,
                &serde_json::to_string(&cleaned)?,
                visibility.as_str(),
                &group,
                now,
            )?;
            crate::domain::repo::content_index::upsert_tx(
                tx,
                &moment_index_doc(&id, &author, visibility, &group, text),
                now,
            )?;
            crate::domain::repo::content_index::add_media_refs_tx(
                tx, &cleaned, "moment", &id, &author, now,
            )?;
            // 发布时扇出（issue #126 要求 4）：与插入同事务，所以“帖在提醒不在”
            // 或“提醒先于帖可见”的窗口都不存在。
            fanout_new_moment(&port, &author, visibility, &group, &id, now)?;
            if let Some(draft_id) = draft_id {
                let draft_id = validate::id(draft_id)?;
                if !delete_draft_for_author(&port, draft_id, &author)? {
                    return Err(PlatformError::Forbidden);
                }
            }
            Ok(())
        })?;
        Ok(id)
    }

    /// 保存/覆盖草稿。草稿允许暂时引用不存在的媒体吗？不允许——"图片先上传
    /// 再发布"的同一道闸在草稿阶段就生效，客户端拿到明确错误后可重试上传。
    pub(crate) fn save_moment_draft(
        &self,
        author: &str,
        draft_id: Option<&str>,
        text: &str,
        media_ids: &[String],
        visibility: Visibility,
        audience_group: &str,
    ) -> Result<String> {
        let author = validate::id(author)?.to_string();
        let (group, cleaned) =
            check_moment_input(text, media_ids, visibility, audience_group, true)?;
        let now = wxlike_core::types::now_ms() as i64;
        let media = serde_json::to_string(&cleaned)?;
        let mut c = self.db().lock();
        c.with_tx(|tx| -> Result<String> {
            let port = StorageConnection::sqlite_tx(tx);
            require_media(&port, &cleaned)?;
            let fields = DraftFields {
                author: &author,
                text,
                media: &media,
                visibility: visibility.as_str(),
                group: &group,
                now,
            };
            write_draft(&port, draft_id, &fields)
        })
    }

    pub(crate) fn moment_drafts(&self, author: &str) -> Result<Vec<MomentDraft>> {
        let c = self.db().acquire();
        Ok(drafts_by_author(&c, author)?
            .into_iter()
            .map(|d| MomentDraft {
                id: d.id,
                author: d.author,
                text: d.text,
                media_ids: d.media_ids,
                visibility: d.visibility,
                audience_group: d.audience_group,
                updated_at: d.updated_at,
            })
            .collect())
    }

    pub(crate) fn delete_moment_draft(&self, author: &str, draft_id: &str) -> Result<()> {
        let draft_id = validate::id(draft_id)?;
        if !delete_draft(&self.db().acquire(), draft_id, author)? {
            return Err(PlatformError::NotFound);
        }
        Ok(())
    }

    pub(crate) fn moment_row(&self, moment_id: &str) -> Result<Option<(i64, MomentHead)>> {
        let c = self.db().acquire();
        let row = moment_record(&c, moment_id)?;
        Ok(row.map(|r| (r.seq, r.into_head())))
    }

    pub(crate) fn can_see_moment_full(
        &self,
        viewer: &str,
        author: &str,
        visibility: &str,
        audience_group: &str,
    ) -> Result<bool> {
        if viewer == author {
            return Ok(true);
        }
        // 拉黑是双向失效：任一方拉黑后，公开/好友/分组内容都不再互相可见。
        if self.is_blocked(viewer, author)? || self.is_blocked(author, viewer)? {
            return Ok(false);
        }
        match visibility {
            "public" => Ok(true),
            "friends" => self.are_friends(author, viewer),
            "groups" => self.in_author_group(author, viewer, audience_group),
            // 私密：连好友也看不到（viewer == author 已在上面返回）
            _ => Ok(false),
        }
    }

    fn in_author_group(&self, author: &str, viewer: &str, group: &str) -> Result<bool> {
        if group.is_empty() {
            return Ok(false);
        }
        friend_group_contains(&self.db().acquire(), author, viewer, group)
    }

    /// 按 id 判定可见性（检索/点赞/详情共用的授权入口）。
    pub(crate) fn moment_visible(&self, viewer: &str, moment_id: &str) -> Result<bool> {
        let row = {
            let c = self.db().acquire();
            moment_access(&c, moment_id)?
        };
        let Some(access) = row else {
            return Ok(false);
        };
        self.can_see_moment_full(
            viewer,
            &access.author,
            &access.visibility,
            &access.audience_group,
        )
    }

    pub(crate) fn moment_visitors_likes(&self, moment_id: &str) -> Result<Vec<(String, String)>> {
        moment_likes(&self.db().acquire(), moment_id)
    }

    pub(crate) fn moment_comments(&self, moment_id: &str) -> Result<Vec<CommentRow>> {
        Ok(moment_comments(&self.db().acquire(), moment_id)?
            .into_iter()
            .map(|c| CommentRow {
                comment_id: c.comment_id,
                author: c.author,
                author_name: c.author_name,
                text: c.text,
                reply_to: c.reply_to,
                reply_to_name: c.reply_to_name,
                created_at: c.created_at,
            })
            .collect())
    }

    /// Author-only removal. 索引与媒体引用同事务清理：检索同步不可见，
    /// 附件不会留下指向已删动态的孤儿引用。
    pub(crate) fn delete_moment(&self, moment_id: &str, by: &str) -> Result<bool> {
        let mut c = self.db().lock();
        c.with_tx(|tx| -> Result<bool> {
            let port = StorageConnection::sqlite_tx(tx);
            let changed = soft_delete_moment(&port, moment_id, by)?;
            if changed {
                crate::domain::repo::content_index::remove_tx(tx, moment_id)?;
                crate::domain::repo::content_index::remove_media_refs_tx(tx, "moment", moment_id)?;
                crate::domain::repo::favorites::revoke_source_tx(tx, "moment", moment_id)?;
            }
            Ok(changed)
        })
    }

    /// 删评论。**动态作者可以删任何人的评论，评论作者可以删自己的**——
    /// 这是微信的行为，也是唯一不会让作者"被挂在自己动态下面"的规则。
    /// 软删除（`deleted = 1`）：feed 的计数与详情都按 `deleted = 0` 过滤，
    /// 硬删会让分页游标与计数对不上。
    pub(crate) fn delete_comment(
        &self,
        moment_id: &str,
        comment_id: &str,
        by: &str,
    ) -> Result<bool> {
        soft_delete_comment(&self.db().acquire(), comment_id, moment_id, by)
    }

    /// Idempotent like: re-liking reports success without a second row, and the
    /// dedupe key keeps it from generating a second notification.
    pub(crate) fn like_moment(&self, moment_id: &str, by: &str) -> Result<bool> {
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        c.with_tx(|tx| -> Result<bool> {
            let port = StorageConnection::sqlite_tx(tx);
            let changed = insert_like(&port, moment_id, by, now)?;
            if changed {
                if let Some(author) = moment_author_if_live(&port, moment_id)? {
                    crate::domain::repo::inbox::notify_tx(
                        tx,
                        &author,
                        "moment_like",
                        by,
                        "moment",
                        moment_id,
                        "赞了你的朋友圈",
                        &format!("like:moment:{moment_id}:{by}"),
                        now,
                    )?;
                }
            }
            Ok(changed)
        })
    }

    pub(crate) fn unlike_moment(&self, moment_id: &str, by: &str) -> Result<bool> {
        delete_like(&self.db().acquire(), moment_id, by)
    }

    /// Adds a comment, optionally as a reply to another comment **in the same
    /// moment** (a reply pointing elsewhere is refused rather than silently
    /// re-parented).
    pub(crate) fn comment_moment(
        &self,
        moment_id: &str,
        by: &str,
        text: &str,
        reply_to: Option<&str>,
    ) -> Result<String> {
        let by = validate::id(by)?.to_string();
        check_comment_text(text)?;
        let id = crate::auth::random_hex(8);
        let now = wxlike_core::types::now_ms() as i64;
        let reply_to = match reply_to {
            Some(r) if !r.is_empty() => Some(validate::id(r)?.to_string()),
            _ => None,
        };
        let mut c = self.db().lock();
        c.with_tx(|tx| -> Result<()> {
            let port = StorageConnection::sqlite_tx(tx);
            if let Some(rt) = &reply_to {
                check_reply_target(&port, moment_id, rt)?;
            }
            insert_comment(&port, &id, moment_id, &by, text, reply_to.as_deref(), now)?;
            notify_comment_inbox(&port, moment_id, &by, reply_to.as_deref(), now)?;
            Ok(())
        })?;
        Ok(id)
    }

    /// 相册（个人主页九宫格）：某个作者对 `viewer` 可见的动态，时间倒序。
    ///
    /// 与 [`Self::feed`] 共用同一套**读时**可见性 SQL（好友关系、分组标签、
    /// 双向拉黑、private 只给自己看），差别只在受众条件从"自+好友"换成
    /// "指定作者"。
    ///
    /// **不存在的用户 / 不是好友 / 被拉黑 → 同一个形状**（200 + 空页）。
    /// 相册是"看某人发过什么"的入口，最容易变成用户 enumeration 探针，
    /// 所以这里刻意不返回 403/404 之类的区分信号。
    pub(crate) fn moment_album(
        &self,
        viewer: &str,
        author: &str,
        since: Option<i64>,
        limit: i64,
    ) -> Result<(Vec<MomentHead>, i64, bool)> {
        let author = validate::id(author)?.to_string();
        let limit = limit.clamp(1, MAX_FEED_PAGE);
        let upper = since.unwrap_or(i64::MAX);
        let c = self.db().acquire();
        let rows = album_page(&c, viewer, &author, upper, limit + 1)?;
        Ok(page_from_rows(rows, limit, upper))
    }

    /// Read fan-out: self + friends, newest first, at most `MAX_FEED_PAGE`.
    pub(crate) fn feed(
        &self,
        me: &str,
        since: Option<i64>,
        limit: i64,
    ) -> Result<(Vec<MomentHead>, i64, bool)> {
        let limit = limit.clamp(1, MAX_FEED_PAGE);
        // since is a watermark: strictly older than it; None = from the newest
        let upper = since.unwrap_or(i64::MAX);

        let c = self.db().acquire();
        // 作者列表、可见性、拉黑全部在同一条 SQL 里判定：好友关系与分组是
        // 读时事实，删好友/改分组/拉黑立即反映到下一次 feed。
        let rows = feed_page(&c, me, upper, limit + 1)?;
        Ok(page_from_rows(rows, limit, upper))
    }
}

/// 草稿写入的字段集合（参数超过七个，收进结构体后读写更清楚）。
struct DraftFields<'a> {
    author: &'a str,
    text: &'a str,
    media: &'a str,
    visibility: &'a str,
    group: &'a str,
    now: i64,
}

/// 覆盖已有草稿或新建一条，返回草稿 id。
fn write_draft(
    port: &StorageConnection<'_>,
    draft_id: Option<&str>,
    fields: &DraftFields<'_>,
) -> Result<String> {
    let Some(draft_id) = draft_id else {
        let id = crate::auth::random_hex(8);
        insert_draft(
            port,
            &id,
            fields.author,
            fields.text,
            fields.media,
            fields.visibility,
            fields.group,
            fields.now,
        )?;
        return Ok(id);
    };
    let id = validate::id(draft_id)?.to_string();
    if !update_draft(
        port,
        &id,
        fields.author,
        fields.text,
        fields.media,
        fields.visibility,
        fields.group,
        fields.now,
    )? {
        return Err(PlatformError::NotFound);
    }
    Ok(id)
}
