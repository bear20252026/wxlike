//! G05-02 自主公众号与文章订阅（`/api/publishers`、`/api/articles`）。
//!
//! **不承诺导入真实微信后台**：这里的 publisher 是 wxlike 自己的账号，文章
//! 只存在于本服务。UI/文案不能把它描述成"同步微信公众号"。
//!
//! 富文本边界：客户端只能提交 [`ArticleDoc`] 的受限块结构，服务端逐块校验
//! 并**自己渲染 HTML**（全量转义 + 只允许 http/https 链接）。不接受 HTML
//! 字符串，因此不存在"净化器漏了一个标签"的绕过面；XSS 用例在渲染层就失败。
//! 外链预览是占位字段：抓取器属于 G04/安全抓取服务，尚未合入时返回
//! `preview_state = "unavailable"`，不伪造标题/图片。
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::articles`]。

use crate::error::{PlatformError, Result};
use crate::page;
use crate::validate;
use crate::Platform;
use serde::{Deserialize, Serialize};
use wxlike_core::tx::ConnectionTx as _;

use super::repo::articles::{
    advance_follow_cursor, article_is_published, article_list, article_read_target, article_row,
    delete_follow, insert_article, insert_article_version, insert_follow_ignore, insert_publisher,
    insert_reaction, load_editable_article, publish_side_effects, publisher_exists,
    publisher_followed, publisher_id_by_owner, publisher_ids_for_user, publisher_name_matches,
    publisher_owner, publisher_published_count, publisher_record, reaction_active, reaction_counts,
    record_article_read, remove_reaction, render_doc, require_image_media, soft_delete_article,
    unread_count, update_publisher, user_name, write_article_revision, ArticleCreate,
    PublisherRecord,
};

pub const MAX_ARTICLE_BLOCKS: usize = 200;
pub const MAX_TITLE_CHARS: usize = 120;
pub const MAX_BODY_TEXT_CHARS: usize = 50_000;
pub const MAX_ALT_CHARS: usize = 200;
pub const MAX_HREF_CHARS: usize = 2_048;
pub const MAX_PAGE: i64 = 50;
const MAX_PUBLISHER_NAME_CHARS: usize = 64;
const MAX_PUBLISHER_QUERY: usize = 64;

/// `article_row` 里 published_at 的下标（与 SELECT 列序一致）。
pub(in crate::domain) const COL_PUBLISHED_AT: usize = 11;

/// 列表页预览正文的截断长度。
const PREVIEW_MAX_CHARS: usize = 160;
const REACTION_KINDS: [&str; 3] = ["wow", "share", "favorite"];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ArticleSpan {
    pub text: String,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ArticleBlock {
    Paragraph {
        spans: Vec<ArticleSpan>,
    },
    Heading {
        level: u8,
        spans: Vec<ArticleSpan>,
    },
    Quote {
        spans: Vec<ArticleSpan>,
    },
    Image {
        media_id: String,
        #[serde(default)]
        alt: String,
    },
    Link {
        text: String,
        href: String,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ArticleDoc {
    pub blocks: Vec<ArticleBlock>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct PublisherView {
    pub id: String,
    pub owner: String,
    pub name: String,
    pub bio: String,
    pub avatar_media_id: String,
    pub created_at: i64,
    pub following: bool,
    pub unread: i64,
    pub published_articles: i64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ArticleSummary {
    pub id: String,
    pub publisher_id: String,
    pub publisher_name: String,
    pub title: String,
    pub excerpt: String,
    pub state: String,
    pub version: i64,
    pub created_at: i64,
    pub published_at: Option<i64>,
    pub reactions: ReactionCounts,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
pub struct ReactionCounts {
    pub wow: i64,
    pub share: i64,
    pub favorite: i64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ArticleView {
    #[serde(flatten)]
    pub summary: ArticleSummary,
    pub doc: ArticleDoc,
    pub html: String,
    pub my_reactions: Vec<String>,
}

impl Platform {
    pub(crate) fn set_publisher(
        &self,
        owner: &str,
        name: &str,
        bio: &str,
        avatar_media_id: &str,
    ) -> Result<PublisherView> {
        let name = check_text("publisher name", name, MAX_PUBLISHER_NAME_CHARS, false)?;
        if name.is_empty() {
            return Err(PlatformError::BadInput("publisher name is required"));
        }
        let bio = check_text("bio", bio, 200, true)?;
        let avatar = if avatar_media_id.is_empty() {
            String::new()
        } else {
            let id = validate::id(avatar_media_id)?.to_string();
            require_image_media(&self.db().acquire(), &id)?;
            id
        };
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().acquire();
        let existing = publisher_id_by_owner(&c, owner)?;
        let id = match existing {
            Some(id) => {
                update_publisher(&c, &id, &name, &bio, &avatar)?;
                id
            }
            None => {
                let id = crate::auth::random_hex(8);
                insert_publisher(&c, &id, owner, &name, &bio, &avatar, now)?;
                id
            }
        };
        drop(c);
        self.publisher_by_id(&id, owner)
    }

    pub(crate) fn publisher_by_id(&self, id: &str, viewer: &str) -> Result<PublisherView> {
        let c = self.db().acquire();
        let Some(PublisherRecord {
            id,
            owner,
            name,
            bio,
            avatar_media_id: avatar,
            created_at,
        }) = publisher_record(&c, id)?
        else {
            return Err(PlatformError::NotFound);
        };
        let following = publisher_followed(&c, viewer, &id)?;
        // 未读数属于"订阅者"；发布者看自己的号不应该显示成全部未读。
        let unread = if viewer == owner.as_str() {
            0
        } else {
            unread_count(&c, viewer, &id)?
        };
        let published_articles = publisher_published_count(&c, &id)?;
        Ok(PublisherView {
            id,
            owner,
            name,
            bio,
            avatar_media_id: avatar,
            created_at,
            following,
            unread,
            published_articles,
        })
    }

    pub(crate) fn follow_publisher(&self, user: &str, publisher_id: &str) -> Result<()> {
        let publisher_id = validate::id(publisher_id)?;
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().acquire();
        if !publisher_exists(&c, publisher_id)? {
            return Err(PlatformError::NotFound);
        }
        insert_follow_ignore(&c, user, publisher_id, now)
    }

    pub(crate) fn unfollow_publisher(&self, user: &str, publisher_id: &str) -> Result<()> {
        let publisher_id = validate::id(publisher_id)?;
        let c = self.db().acquire();
        delete_follow(&c, user, publisher_id)
    }

    /// 我关注的 + 我自己的发布者。`q` 只做名称子串匹配（管理页搜索）。
    pub(crate) fn publishers(&self, user: &str, q: Option<&str>) -> Result<Vec<PublisherView>> {
        let ids: Vec<String> = {
            let c = self.db().acquire();
            let pattern = match q.map(str::trim).filter(|s| !s.is_empty()) {
                Some(q) if q.chars().count() > MAX_PUBLISHER_QUERY => {
                    return Err(PlatformError::BadInput("query too long"))
                }
                Some(q) => Some(format!("%{}%", page::escape_like(q))),
                None => None,
            };
            let mut ids = publisher_ids_for_user(&c, user)?;
            if let Some(pattern) = pattern.as_deref() {
                ids.retain(|id| publisher_name_matches(&c, id, pattern).unwrap_or(false));
            }
            ids
        };
        ids.iter()
            .map(|id| self.publisher_by_id(id, user))
            .collect()
    }

    pub(crate) fn create_article(&self, author: &str, body: ArticleBody) -> Result<ArticleView> {
        let title = check_text("title", &body.title, MAX_TITLE_CHARS, false)?;
        if title.trim().is_empty() {
            return Err(PlatformError::BadInput("title is required"));
        }
        let publisher_id = match body.publisher_id {
            Some(id) => {
                let id = validate::id(&id)?.to_string();
                self.require_publisher_owner(&id, author)?;
                id
            }
            None => self.ensure_publisher(author)?,
        };
        let now = wxlike_core::types::now_ms() as i64;
        let id = crate::auth::random_hex(8);
        let (plain, html) = {
            let c = self.db().acquire();
            render_doc(&c, &body.doc)?
        };
        let doc_json = serde_json::to_string(&body.doc)?;
        let state = if body.publish { "published" } else { "draft" };

        let mut c = self.db().lock()?;
        c.with_tx(|tx| -> Result<()> {
            let port = crate::storage::StorageConnection::sqlite_tx(tx);
            insert_article(
                &port,
                &ArticleCreate {
                    id: &id,
                    publisher_id: &publisher_id,
                    author,
                    title: &title,
                    doc_json: &doc_json,
                    plain: &plain,
                    html: &html,
                    state,
                    now,
                    published_at: body.publish.then_some(now),
                },
            )?;
            insert_article_version(&port, &id, &doc_json, now)?;
            if body.publish {
                publish_side_effects(&port, &id, &publisher_id, author, &title, &plain, now)?;
                crate::domain::repo::content_index::add_media_refs_tx(
                    tx,
                    &doc_media_ids(&body.doc),
                    "article",
                    &id,
                    author,
                    now,
                )?;
            }
            Ok(())
        })?;
        drop(c);
        self.article_for(author, &id)
    }

    fn require_publisher_owner(&self, publisher_id: &str, owner: &str) -> Result<()> {
        let c = self.db().acquire();
        match publisher_owner(&c, publisher_id)? {
            Some(found) if found.as_str() == owner => Ok(()),
            Some(_) => Err(PlatformError::Forbidden),
            None => Err(PlatformError::NotFound),
        }
    }

    fn ensure_publisher(&self, owner: &str) -> Result<String> {
        let c = self.db().acquire();
        if let Some(id) = publisher_id_by_owner(&c, owner)? {
            return Ok(id);
        }
        let name = user_name(&c, owner)?;
        let id = crate::auth::random_hex(8);
        insert_publisher(
            &c,
            &id,
            owner,
            &name,
            "",
            "",
            wxlike_core::types::now_ms() as i64,
        )?;
        Ok(id)
    }

    pub(crate) fn article_for(&self, viewer: &str, id: &str) -> Result<ArticleView> {
        let id = validate::id(id)?;
        let row = {
            let c = self.db().acquire();
            article_row(&c, id)?
        };
        let Some(row) = row else {
            return Err(PlatformError::NotFound);
        };
        if row.state == "deleted" {
            return Err(PlatformError::NotFound);
        }
        if row.state != "published" && row.author != viewer {
            // 未发布稿只有授权编辑者（这里即作者/发布者 owner）可见。
            return Err(PlatformError::NotFound);
        }
        let c = self.db().acquire();
        let doc: ArticleDoc =
            serde_json::from_str(&row.doc).unwrap_or(ArticleDoc { blocks: vec![] });
        let reactions = reaction_counts(&c, id)?;
        let mut my = Vec::new();
        for kind in REACTION_KINDS {
            if reaction_active(&c, id, viewer, kind)? {
                my.push(kind.to_string());
            }
        }
        Ok(ArticleView {
            summary: ArticleSummary {
                id: row.id,
                publisher_id: row.publisher_id,
                publisher_name: row.publisher_name,
                title: row.title,
                excerpt: excerpt(&row.plain),
                state: row.state,
                version: row.version,
                created_at: row.created_at,
                published_at: row.published_at,
                reactions,
            },
            doc,
            html: row.html,
            my_reactions: my,
        })
    }

    pub(crate) fn update_article(
        &self,
        author: &str,
        id: &str,
        body: ArticleUpdateBody,
    ) -> Result<ArticleView> {
        let id = validate::id(id)?;
        let title = check_text("title", &body.title, MAX_TITLE_CHARS, false)?;
        if title.trim().is_empty() {
            return Err(PlatformError::BadInput("title is required"));
        }
        let (plain, html) = {
            let c = self.db().acquire();
            render_doc(&c, &body.doc)?
        };
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock()?;
        c.with_tx(|tx| -> Result<()> {
            let port = crate::storage::StorageConnection::sqlite_tx(tx);
            let (publisher_id, state, version) =
                load_editable_article(&port, id, author, body.expected_version)?;
            let next_version = version + 1;
            let next_state = if body.publish || state == "published" {
                "published"
            } else {
                "draft"
            };
            let publish_now = state != "published" && next_state == "published";
            write_article_revision(
                &port,
                id,
                &ArticleRevision {
                    title: &title,
                    doc_json: &serde_json::to_string(&body.doc)?,
                    plain: &plain,
                    html: &html,
                    next_state,
                    next_version,
                    now,
                    publish_now,
                },
            )?;
            if next_state == "published" {
                publish_side_effects(&port, id, &publisher_id, author, &title, &plain, now)?;
                crate::domain::repo::content_index::remove_media_refs_tx(tx, "article", id)?;
                crate::domain::repo::content_index::add_media_refs_tx(
                    tx,
                    &doc_media_ids(&body.doc),
                    "article",
                    id,
                    author,
                    now,
                )?;
            }
            Ok(())
        })?;
        drop(c);
        self.article_for(author, id)
    }

    pub(crate) fn delete_article(&self, author: &str, id: &str) -> Result<()> {
        let id = validate::id(id)?;
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock()?;
        c.with_tx(|tx| -> Result<()> {
            let port = crate::storage::StorageConnection::sqlite_tx(tx);
            let changed = soft_delete_article(&port, id, author, now)?;
            crate::db::ensure_updated(changed)?;
            crate::domain::repo::content_index::remove_tx(tx, id)?;
            crate::domain::repo::content_index::remove_media_refs_tx(tx, "article", id)?;
            crate::domain::repo::favorites::revoke_source_tx(tx, "article", id)?;
            Ok(())
        })
    }

    pub(crate) fn articles_by_publisher(
        &self,
        publisher_id: &str,
        cursor: Option<i64>,
        limit: i64,
    ) -> Result<(Vec<ArticleSummary>, i64, bool)> {
        let publisher_id = validate::id(publisher_id)?;
        let limit = limit.clamp(1, MAX_PAGE);
        let before = cursor.unwrap_or(i64::MAX);
        let c = self.db().acquire();
        let mut rows = article_list(&c, publisher_id, before, limit)?;
        let has_more = rows.len() as i64 > limit;
        if has_more {
            rows.pop();
        }
        let next = rows.last().map(|row| row.seq).unwrap_or(0);
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let reactions = reaction_counts(&c, &row.id)?;
            out.push(ArticleSummary {
                id: row.id,
                publisher_id: row.publisher_id,
                publisher_name: row.publisher_name,
                title: row.title,
                excerpt: excerpt(&row.body_text),
                state: row.state,
                version: row.version,
                created_at: row.created_at,
                published_at: row.published_at,
                reactions,
            });
        }
        Ok((out, next, has_more))
    }

    /// 打开全文：记阅读行并把订阅读游标推进到这篇文章的 seq。
    pub(crate) fn mark_article_read(&self, user: &str, id: &str) -> Result<i64> {
        let id = validate::id(id)?;
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock()?;
        let publisher_id = c.with_tx(|tx| -> Result<String> {
            let port = crate::storage::StorageConnection::sqlite_tx(tx);
            let (seq, publisher_id, state) =
                article_read_target(&port, id)?.ok_or(PlatformError::NotFound)?;
            if state != "published" {
                return Err(PlatformError::NotFound);
            }
            record_article_read(&port, user, id, now)?;
            advance_follow_cursor(&port, user, &publisher_id, seq, now)?;
            Ok(publisher_id)
        })?;
        let unread = unread_count(
            &crate::storage::StorageConnection::sqlite_borrow(&c),
            user,
            &publisher_id,
        )?;
        drop(c);
        Ok(unread)
    }

    pub(crate) fn react_article(
        &self,
        user: &str,
        id: &str,
        kind: &str,
    ) -> Result<(bool, ReactionCounts)> {
        if !REACTION_KINDS.contains(&kind) {
            return Err(PlatformError::BadInput("unknown reaction kind"));
        }
        let id = validate::id(id)?;
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().acquire();
        if !article_is_published(&c, id)? {
            return Err(PlatformError::NotFound);
        }
        let removed = remove_reaction(&c, id, user, kind)?;
        let active = if removed == 0 {
            insert_reaction(&c, id, user, kind, now)?;
            true
        } else {
            false
        };
        Ok((active, reaction_counts(&c, id)?))
    }
}

pub(in crate::domain) struct ArticleRow {
    pub(crate) id: String,
    pub(crate) publisher_id: String,
    pub(crate) publisher_name: String,
    pub(crate) author: String,
    pub(crate) title: String,
    pub(crate) plain: String,
    pub(crate) html: String,
    pub(crate) doc: String,
    pub(crate) state: String,
    pub(crate) version: i64,
    pub(crate) created_at: i64,
    pub(crate) published_at: Option<i64>,
}

/// 一次保存要落库的全部字段；UPDATE 主行 + INSERT 版本历史共用。
pub(in crate::domain) struct ArticleRevision<'a> {
    pub(crate) title: &'a str,
    pub(crate) doc_json: &'a str,
    pub(crate) plain: &'a str,
    pub(crate) html: &'a str,
    pub(crate) next_state: &'a str,
    pub(crate) next_version: i64,
    pub(crate) now: i64,
    pub(crate) publish_now: bool,
}

fn excerpt(text: &str) -> String {
    text.chars().take(PREVIEW_MAX_CHARS).collect()
}

/// 文章引用到的图片媒体 id（去重）。删除/更新时据此维护附件引用表。
fn doc_media_ids(doc: &ArticleDoc) -> Vec<String> {
    let mut ids: Vec<String> = doc
        .blocks
        .iter()
        .filter_map(|b| match b {
            ArticleBlock::Image { media_id, .. } => Some(media_id.clone()),
            _ => None,
        })
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

pub(in crate::domain) fn render_spans(spans: &[ArticleSpan]) -> Result<(String, String)> {
    let mut text = String::new();
    let mut html = String::new();
    for span in spans {
        let piece = check_text("span", &span.text, 2_000, true)?;
        text.push_str(&piece);
        let escaped = escape(&piece);
        if span.bold && span.italic {
            html.push_str(&format!("<strong><em>{escaped}</em></strong>"));
        } else if span.bold {
            html.push_str(&format!("<strong>{escaped}</strong>"));
        } else if span.italic {
            html.push_str(&format!("<em>{escaped}</em>"));
        } else {
            html.push_str(&escaped);
        }
    }
    Ok((text, html))
}

pub(in crate::domain) fn push_plain(buf: &mut String, text: &str) {
    if !buf.is_empty() {
        buf.push('\n');
    }
    buf.push_str(text);
}

pub(in crate::domain) fn check_text(
    field: &'static str,
    s: &str,
    max: usize,
    allow_newline: bool,
) -> Result<String> {
    if s.chars().count() > max {
        return Err(PlatformError::BadInput(field));
    }
    if s.chars()
        .any(|c| c.is_control() && !(allow_newline && matches!(c, '\n' | '\t')))
    {
        return Err(PlatformError::BadInput(field));
    }
    Ok(s.to_string())
}

/// 外链只允许 http/https；`javascript:`、`data:`、`file:` 与协议相对地址
/// 一律拒绝。没有这一步，净化过的正文仍能把用户送进任意脚本上下文。
pub(in crate::domain) fn check_href(href: &str) -> Result<String> {
    let href = href.trim();
    if href.is_empty() || href.chars().count() > MAX_HREF_CHARS {
        return Err(PlatformError::BadInput("link href malformed"));
    }
    if href.chars().any(char::is_control) {
        return Err(PlatformError::BadInput("link href malformed"));
    }
    let lower = href.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return Err(PlatformError::BadInput("link scheme not allowed"));
    }
    if lower.contains('@') && lower.split("://").nth(1).unwrap_or("").contains('@') {
        // userinfo 容易伪装目标域名；文章外链不需要它。
        return Err(PlatformError::BadInput("link userinfo not allowed"));
    }
    Ok(href.to_string())
}

pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

// ---- HTTP handlers ----

#[derive(Deserialize)]
pub struct ArticleBody {
    #[serde(default)]
    pub publisher_id: Option<String>,
    pub title: String,
    pub doc: ArticleDoc,
    #[serde(default)]
    pub publish: bool,
}

#[derive(Deserialize)]
pub struct ArticleUpdateBody {
    pub title: String,
    pub doc: ArticleDoc,
    /// 乐观锁：客户端带上读到的版本，服务端不匹配就 409，不静默覆盖。
    pub expected_version: i64,
    #[serde(default)]
    pub publish: bool,
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_covers_html_metacharacters() {
        assert_eq!(
            escape(r#"<script>alert("x&y")</script>"#),
            "&lt;script&gt;alert(&quot;x&amp;y&quot;)&lt;/script&gt;"
        );
    }

    #[test]
    fn href_scheme_is_whitelisted() {
        assert!(check_href("https://example.com/a?b=1").is_ok());
        assert!(check_href("http://example.com").is_ok());
        for bad in [
            "javascript:alert(1)",
            "data:text/html,x",
            "file:///etc/passwd",
            "//example.com",
            "https://user:pass@example.com",
        ] {
            assert!(check_href(bad).is_err(), "{bad}");
        }
    }
}
