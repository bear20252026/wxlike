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
use crate::validate;
use crate::Platform;
use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};
use wxlike_core::tx::ConnectionTx as _;

use super::repo::moments::{
    check_reply_target, fanout_new_moment, notify_comment_inbox, require_media,
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

impl Platform {
    /// Stores a new moment. Media ids must already exist (WP-05), so a moment
    /// cannot dangle a reference to an object nobody uploaded.
    pub fn publish_moment(
        &self,
        author: &str,
        text: &str,
        media_ids: &[String],
        visibility: Visibility,
    ) -> Result<String> {
        self.publish_moment_with(author, text, media_ids, visibility, "", None)
    }

    /// 发布（可带分组可见与草稿来源）。发布、索引、媒体引用、删草稿同事务。
    pub fn publish_moment_with(
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
            require_media(tx, &cleaned)?;
            tx.execute(
                "INSERT INTO moments(id, author, text, media, visibility, audience_group, created_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id,
                    author,
                    text,
                    serde_json::to_string(&cleaned)?,
                    visibility.as_str(),
                    group,
                    now
                ],
            )?;
            crate::domain::repo::content_index::upsert(
                tx,
                &moment_index_doc(&id, &author, visibility, &group, text),
                now,
            )?;
            crate::domain::repo::content_index::add_media_refs(tx, &cleaned, "moment", &id, &author, now)?;
            // 发布时扇出（issue #126 要求 4）：与插入同事务，所以“帖在提醒不在”
            // 或“提醒先于帖可见”的窗口都不存在。
            fanout_new_moment(tx, &author, visibility, &group, &id, now)?;
            if let Some(draft_id) = draft_id {
                let draft_id = validate::id(draft_id)?;
                let changed = tx.execute(
                    "DELETE FROM moment_drafts WHERE id = ?1 AND author = ?2",
                    params![draft_id, author],
                )?;
                if changed == 0 {
                    return Err(PlatformError::Forbidden);
                }
            }
            Ok(())
        })?;
        Ok(id)
    }

    /// 保存/覆盖草稿。草稿允许暂时引用不存在的媒体吗？不允许——"图片先上传
    /// 再发布"的同一道闸在草稿阶段就生效，客户端拿到明确错误后可重试上传。
    pub fn save_moment_draft(
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
        let id = c.with_tx(|tx| -> Result<String> {
            require_media(tx, &cleaned)?;
            let id = match draft_id {
                Some(id) => {
                    let id = validate::id(id)?.to_string();
                    let changed = tx.execute(
                        "UPDATE moment_drafts SET text = ?1, media = ?2, visibility = ?3,
                                              audience_group = ?4, updated_at = ?5
                         WHERE id = ?6 AND author = ?7",
                        params![text, media, visibility.as_str(), group, now, id, author],
                    )?;
                    if changed == 0 {
                        return Err(PlatformError::NotFound);
                    }
                    id
                }
                None => {
                    let id = crate::auth::random_hex(8);
                    tx.execute(
                        "INSERT INTO moment_drafts(id, author, text, media, visibility,
                                               audience_group, updated_at)
                         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![id, author, text, media, visibility.as_str(), group, now],
                    )?;
                    id
                }
            };
            Ok(id)
        })?;
        Ok(id)
    }

    pub fn moment_drafts(&self, author: &str) -> Result<Vec<MomentDraft>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT id, author, text, media, visibility, audience_group, updated_at
             FROM moment_drafts WHERE author = ?1 ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map(params![author], |r| {
            let media: String = r.get(3)?;
            Ok(MomentDraft {
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

    pub fn delete_moment_draft(&self, author: &str, draft_id: &str) -> Result<()> {
        let draft_id = validate::id(draft_id)?;
        let changed = self.db().lock().execute(
            "DELETE FROM moment_drafts WHERE id = ?1 AND author = ?2",
            params![draft_id, author],
        )?;
        if changed == 0 {
            return Err(PlatformError::NotFound);
        }
        Ok(())
    }

    pub(crate) fn moment_row(&self, moment_id: &str) -> Result<Option<(i64, MomentHead)>> {
        let c = self.db().lock();
        let row = c
            .query_row(
                "SELECT m.seq, m.id, m.author, u.name, m.text, m.media, m.visibility, m.created_at
                 FROM moments m JOIN users u ON u.id = m.author
                 WHERE m.id = ?1 AND m.deleted = 0",
                params![moment_id],
                |r| {
                    let media: String = r.get(5)?;
                    Ok((
                        r.get::<_, i64>(0)?,
                        MomentHead {
                            moment_id: r.get(1)?,
                            author: r.get(2)?,
                            author_name: r.get(3)?,
                            text: r.get(4)?,
                            media_ids: serde_json::from_str(&media).unwrap_or_default(),
                            visibility: r.get(6)?,
                            created_at: r.get(7)?,
                            like_count: 0,
                            comment_count: 0,
                        },
                    ))
                },
            )
            .optional()?;
        let Some((seq, mut head)) = row else {
            return Ok(None);
        };
        head.like_count = c.query_row(
            "SELECT COUNT(*) FROM moment_likes WHERE moment_id = ?1",
            params![moment_id],
            |r| r.get::<_, i64>(0),
        )?;
        head.comment_count = c.query_row(
            "SELECT COUNT(*) FROM moment_comments WHERE moment_id = ?1 AND deleted = 0",
            params![moment_id],
            |r| r.get::<_, i64>(0),
        )?;
        Ok(Some((seq, head)))
    }

    /// Whether `viewer` may see this moment. Authors always see their own;
    /// `public` is open to non-blocked viewers; `friends` requires the
    /// friendship row; `groups` additionally requires the author's friend
    /// group label to match `audience_group`.
    pub fn can_see_moment(&self, viewer: &str, author: &str, visibility: &str) -> Result<bool> {
        self.can_see_moment_full(viewer, author, visibility, "")
    }

    pub fn can_see_moment_full(
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
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT 1 FROM friends WHERE user_id = ?1 AND friend_id = ?2 AND friend_group = ?3",
            params![author, viewer, group],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
    }

    /// 按 id 判定可见性（检索/点赞/详情共用的授权入口）。
    pub fn moment_visible(&self, viewer: &str, moment_id: &str) -> Result<bool> {
        let row: Option<(String, String, String)> = {
            let c = self.db().lock();
            c.query_row(
                "SELECT author, visibility, audience_group FROM moments
                 WHERE id = ?1 AND deleted = 0",
                params![moment_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
        };
        let Some((author, visibility, group)) = row else {
            return Ok(false);
        };
        self.can_see_moment_full(viewer, &author, &visibility, &group)
    }

    pub fn moment_visitors_likes(&self, moment_id: &str) -> Result<Vec<(String, String)>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT l.user_id, u.name FROM moment_likes l JOIN users u ON u.id = l.user_id
             WHERE l.moment_id = ?1 ORDER BY l.created_at",
        )?;
        let rows = stmt.query_map(params![moment_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn moment_comments(&self, moment_id: &str) -> Result<Vec<CommentRow>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT cm.id, cm.author, u.name, cm.text, cm.reply_to, rt.name, cm.created_at
             FROM moment_comments cm
             JOIN users u ON u.id = cm.author
             LEFT JOIN moment_comments rc ON rc.id = cm.reply_to
             LEFT JOIN users rt ON rt.id = rc.author
             WHERE cm.moment_id = ?1 AND cm.deleted = 0
             ORDER BY cm.seq",
        )?;
        let rows = stmt.query_map(params![moment_id], |r| {
            Ok(CommentRow {
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

    /// Author-only removal. 索引与媒体引用同事务清理：检索同步不可见，
    /// 附件不会留下指向已删动态的孤儿引用。
    pub fn delete_moment(&self, moment_id: &str, by: &str) -> Result<bool> {
        let mut c = self.db().lock();
        c.with_tx(|tx| -> Result<bool> {
            let changed = tx.execute(
                "UPDATE moments SET deleted = 1 WHERE id = ?1 AND author = ?2 AND deleted = 0",
                params![moment_id, by],
            )?;
            if changed == 1 {
                crate::domain::repo::content_index::remove(tx, moment_id)?;
                crate::domain::repo::content_index::remove_media_refs(tx, "moment", moment_id)?;
                crate::domain::repo::favorites::revoke_source(tx, "moment", moment_id)?;
            }
            Ok(changed == 1)
        })
    }

    /// 删评论。**动态作者可以删任何人的评论，评论作者可以删自己的**——
    /// 这是微信的行为，也是唯一不会让作者"被挂在自己动态下面"的规则。
    /// 软删除（`deleted = 1`）：feed 的计数与详情都按 `deleted = 0` 过滤，
    /// 硬删会让分页游标与计数对不上。
    pub fn delete_comment(&self, moment_id: &str, comment_id: &str, by: &str) -> Result<bool> {
        let c = self.db().lock();
        let changed = c.execute(
            "UPDATE moment_comments SET deleted = 1
             WHERE id = ?1 AND moment_id = ?2 AND deleted = 0
               AND (author = ?3
                    OR ?3 = (SELECT author FROM moments WHERE id = ?2))",
            params![comment_id, moment_id, by],
        )?;
        Ok(changed == 1)
    }

    /// Idempotent like: re-liking reports success without a second row, and the
    /// dedupe key keeps it from generating a second notification.
    pub fn like_moment(&self, moment_id: &str, by: &str) -> Result<bool> {
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        c.with_tx(|tx| -> Result<bool> {
            let changed = tx.execute(
                "INSERT INTO moment_likes(moment_id, user_id, created_at) VALUES(?1, ?2, ?3)
                 ON CONFLICT(moment_id, user_id) DO NOTHING",
                params![moment_id, by, now],
            )?;
            if changed == 1 {
                let author: Option<String> = tx
                    .query_row(
                        "SELECT author FROM moments WHERE id = ?1 AND deleted = 0",
                        params![moment_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(author) = author {
                    crate::domain::repo::inbox::notify(
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
            Ok(changed == 1)
        })
    }

    pub fn unlike_moment(&self, moment_id: &str, by: &str) -> Result<bool> {
        let c = self.db().lock();
        let changed = c.execute(
            "DELETE FROM moment_likes WHERE moment_id = ?1 AND user_id = ?2",
            params![moment_id, by],
        )?;
        Ok(changed == 1)
    }

    /// Adds a comment, optionally as a reply to another comment **in the same
    /// moment** (a reply pointing elsewhere is refused rather than silently
    /// re-parented).
    pub fn comment_moment(
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
            if let Some(rt) = &reply_to {
                check_reply_target(tx, moment_id, rt)?;
            }
            tx.execute(
                "INSERT INTO moment_comments(id, moment_id, author, text, reply_to, created_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id,
                    moment_id,
                    by,
                    text,
                    reply_to.as_deref().unwrap_or(""),
                    now
                ],
            )?;
            notify_comment_inbox(tx, moment_id, &by, reply_to.as_deref(), now)?;
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
    pub fn moment_album(
        &self,
        viewer: &str,
        author: &str,
        since: Option<i64>,
        limit: i64,
    ) -> Result<(Vec<MomentHead>, i64, bool)> {
        let author = validate::id(author)?.to_string();
        let limit = limit.clamp(1, MAX_FEED_PAGE);
        let upper = since.unwrap_or(i64::MAX);
        let c = self.db().lock();
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
        let mut stmt = c.prepare(sql)?;
        let rows = stmt.query_map(params![viewer, author, upper, limit + 1], |r| {
            let media: String = r.get(5)?;
            Ok((
                r.get::<_, i64>(0)?,
                MomentHead {
                    moment_id: r.get(1)?,
                    author: r.get(2)?,
                    author_name: r.get(3)?,
                    text: r.get(4)?,
                    media_ids: serde_json::from_str(&media).unwrap_or_default(),
                    visibility: r.get(6)?,
                    created_at: r.get(7)?,
                    like_count: r.get(8)?,
                    comment_count: r.get(9)?,
                },
            ))
        })?;
        let mut all = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        let has_more = all.len() as i64 > limit;
        if has_more {
            all.pop();
        }
        let next = all.last().map(|(seq, _)| *seq).unwrap_or(upper);
        Ok((all.into_iter().map(|(_, h)| h).collect(), next, has_more))
    }

    /// Read fan-out: self + friends, newest first, at most `MAX_FEED_PAGE`.
    pub fn feed(
        &self,
        me: &str,
        since: Option<i64>,
        limit: i64,
    ) -> Result<(Vec<MomentHead>, i64, bool)> {
        let limit = limit.clamp(1, MAX_FEED_PAGE);
        // since is a watermark: strictly older than it; None = from the newest
        let upper = since.unwrap_or(i64::MAX);

        let c = self.db().lock();
        // 作者列表、可见性、拉黑全部在同一条 SQL 里判定：好友关系与分组是
        // 读时事实，删好友/改分组/拉黑立即反映到下一次 feed。
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
        let mut stmt = c.prepare(sql)?;
        // (seq, head) internally so the cursor needs no `seq` field in the API shape
        let rows = stmt.query_map(params![me, upper, limit + 1], |r| {
            let media: String = r.get(5)?;
            Ok((
                r.get::<_, i64>(0)?,
                MomentHead {
                    moment_id: r.get(1)?,
                    author: r.get(2)?,
                    author_name: r.get(3)?,
                    text: r.get(4)?,
                    media_ids: serde_json::from_str(&media).unwrap_or_default(),
                    visibility: r.get(6)?,
                    created_at: r.get(7)?,
                    like_count: r.get(8)?,
                    comment_count: r.get(9)?,
                },
            ))
        })?;
        let mut all = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        let has_more = all.len() as i64 > limit;
        if has_more {
            all.pop();
        }
        let next = all.last().map(|(seq, _)| *seq).unwrap_or(upper);
        Ok((all.into_iter().map(|(_, h)| h).collect(), next, has_more))
    }
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
