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

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use crate::auth::Authed;
use crate::content_index;
use crate::error::{ApiError, PlatformError, Result};
use crate::extract::ApiJson;
use crate::inbox;
use crate::page;
use crate::validate;
use crate::{Platform, PlatformState};

pub const MAX_ARTICLE_BLOCKS: usize = 200;
pub const MAX_TITLE_CHARS: usize = 120;
pub const MAX_BODY_TEXT_CHARS: usize = 50_000;
pub const MAX_ALT_CHARS: usize = 200;
pub const MAX_HREF_CHARS: usize = 2_048;
pub const MAX_PAGE: i64 = 50;
const MAX_PUBLISHER_NAME_CHARS: usize = 64;
const MAX_PUBLISHER_QUERY: usize = 64;
/// `article_row` 里 published_at 的下标（与 SELECT 列序一致）。
const COL_PUBLISHED_AT: usize = 11;
/// 列表页预览正文的截断长度。
const PREVIEW_MAX_CHARS: usize = 160;

const REACTION_KINDS: [&str; 3] = ["wow", "share", "favorite"];

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/publishers", post(set_publisher))
        .route("/api/publishers", get(list_publishers))
        .route("/api/publishers/{id}", get(one_publisher))
        .route("/api/publishers/{id}/follow", post(follow))
        .route("/api/publishers/{id}/follow", delete(unfollow))
        .route("/api/publishers/{id}/articles", get(publisher_articles))
        .route("/api/articles", post(create))
        .route("/api/articles/{id}", get(one))
        .route("/api/articles/{id}", put(update))
        .route("/api/articles/{id}", delete(remove))
        .route("/api/articles/{id}/read", post(mark_read))
        .route("/api/articles/{id}/react", post(react))
}

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

#[derive(Deserialize)]
pub struct PublisherBody {
    pub name: String,
    #[serde(default)]
    pub bio: String,
    #[serde(default)]
    pub avatar_media_id: String,
}

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

#[derive(Deserialize)]
struct ListQuery {
    q: Option<String>,
}

#[derive(Deserialize)]
struct ArticleListQuery {
    cursor: Option<i64>,
    limit: Option<i64>,
}

#[derive(Deserialize)]
struct ReactBody {
    pub kind: String,
}

impl Platform {
    pub fn set_publisher(
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
            require_image_media(&self.db().lock(), &id)?;
            id
        };
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        let existing: Option<String> = c
            .query_row(
                "SELECT id FROM publishers WHERE owner = ?1",
                params![owner],
                |r| r.get(0),
            )
            .optional()?;
        let id = match existing {
            Some(id) => {
                c.execute(
                    "UPDATE publishers SET name = ?1, bio = ?2, avatar_media_id = ?3
                     WHERE id = ?4",
                    params![name, bio, avatar, id],
                )?;
                id
            }
            None => {
                let id = crate::auth::random_hex(8);
                c.execute(
                    "INSERT INTO publishers(id, owner, name, bio, avatar_media_id, created_at)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, owner, name, bio, avatar, now],
                )?;
                id
            }
        };
        drop(c);
        self.publisher_by_id(&id, owner)
    }

    pub fn publisher_by_id(&self, id: &str, viewer: &str) -> Result<PublisherView> {
        let c = self.db().lock();
        let row = c
            .query_row(
                "SELECT id, owner, name, bio, avatar_media_id, created_at FROM publishers
                 WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, i64>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, owner, name, bio, avatar, created_at)) = row else {
            return Err(PlatformError::NotFound);
        };
        let following: bool = c
            .query_row(
                "SELECT 1 FROM publisher_follows WHERE user_id = ?1 AND publisher_id = ?2",
                params![viewer, id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        // 未读数属于"订阅者"；发布者看自己的号不应该显示成全部未读。
        let unread = if viewer == owner {
            0
        } else {
            unread_count(&c, viewer, &id)?
        };
        let published_articles: i64 = c.query_row(
            "SELECT COUNT(*) FROM articles WHERE publisher_id = ?1 AND state = 'published'",
            params![id],
            |r| r.get(0),
        )?;
        Ok(PublisherView {
            id,
            owner,
            name: name.to_string(),
            bio,
            avatar_media_id: avatar,
            created_at,
            following,
            unread,
            published_articles,
        })
    }

    pub fn publish_followers(&self, publisher_id: &str) -> Result<Vec<String>> {
        let c = self.db().lock();
        let mut stmt =
            c.prepare("SELECT user_id FROM publisher_follows WHERE publisher_id = ?1")?;
        let rows = stmt.query_map(params![publisher_id], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn follow_publisher(&self, user: &str, publisher_id: &str) -> Result<()> {
        let publisher_id = validate::id(publisher_id)?;
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        let exists: Option<i64> = c
            .query_row(
                "SELECT 1 FROM publishers WHERE id = ?1",
                params![publisher_id],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(PlatformError::NotFound);
        }
        c.execute(
            "INSERT OR IGNORE INTO publisher_follows(user_id, publisher_id, read_cursor, created_at)
             VALUES(?1, ?2, 0, ?3)",
            params![user, publisher_id, now],
        )?;
        Ok(())
    }

    pub fn unfollow_publisher(&self, user: &str, publisher_id: &str) -> Result<()> {
        let publisher_id = validate::id(publisher_id)?;
        self.db().lock().execute(
            "DELETE FROM publisher_follows WHERE user_id = ?1 AND publisher_id = ?2",
            params![user, publisher_id],
        )?;
        Ok(())
    }

    /// 我关注的 + 我自己的发布者。`q` 只做名称子串匹配（管理页搜索）。
    pub fn publishers(&self, user: &str, q: Option<&str>) -> Result<Vec<PublisherView>> {
        let ids: Vec<String> = {
            let c = self.db().lock();
            let pattern = match q.map(str::trim).filter(|s| !s.is_empty()) {
                Some(q) if q.chars().count() > MAX_PUBLISHER_QUERY => {
                    return Err(PlatformError::BadInput("query too long"))
                }
                Some(q) => Some(format!("%{}%", page::escape_like(q))),
                None => None,
            };
            let mut stmt = c.prepare(
                "SELECT DISTINCT p.id FROM publishers p
                 LEFT JOIN publisher_follows f
                   ON f.publisher_id = p.id AND f.user_id = ?1
                 WHERE f.user_id IS NOT NULL OR p.owner = ?1
                 ORDER BY p.created_at DESC, p.id",
            )?;
            let mut ids = stmt
                .query_map(params![user], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            if let Some(pattern) = pattern {
                ids.retain(|id| {
                    c.query_row(
                        "SELECT 1 FROM publishers WHERE id = ?1 AND name LIKE ?2 ESCAPE '\\'",
                        params![id, pattern],
                        |_| Ok(()),
                    )
                    .optional()
                    .ok()
                    .flatten()
                    .is_some()
                });
            }
            ids
        };
        ids.iter()
            .map(|id| self.publisher_by_id(id, user))
            .collect()
    }

    pub fn create_article(&self, author: &str, body: ArticleBody) -> Result<ArticleView> {
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
            let c = self.db().lock();
            render_doc(&c, &body.doc)?
        };
        let doc_json = serde_json::to_string(&body.doc)?;
        let state = if body.publish { "published" } else { "draft" };

        let mut c = self.db().lock();
        let tx = c.transaction()?;
        tx.execute(
            "INSERT INTO articles(seq, id, publisher_id, author, title, doc, body_text, body_html,
                                  state, version, created_at, updated_at, published_at)
             VALUES(NULL, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9, ?9, ?10)",
            params![
                id,
                publisher_id,
                author,
                title,
                doc_json,
                plain,
                html,
                state,
                now,
                body.publish.then_some(now)
            ],
        )?;
        tx.execute(
            "INSERT INTO article_versions(article_id, version, doc, saved_at) VALUES(?1, 1, ?2, ?3)",
            params![id, serde_json::to_string(&body.doc)?, now],
        )?;
        if body.publish {
            publish_side_effects(&tx, &id, &publisher_id, author, &title, &plain, now)?;
            content_index::add_media_refs(
                &tx,
                &doc_media_ids(&body.doc),
                "article",
                &id,
                author,
                now,
            )?;
        }
        tx.commit()?;
        drop(c);
        self.article_for(author, &id)
    }

    fn require_publisher_owner(&self, publisher_id: &str, owner: &str) -> Result<()> {
        let c = self.db().lock();
        let found: Option<String> = c
            .query_row(
                "SELECT owner FROM publishers WHERE id = ?1",
                params![publisher_id],
                |r| r.get(0),
            )
            .optional()?;
        match found {
            Some(found) if found == owner => Ok(()),
            Some(_) => Err(PlatformError::Forbidden),
            None => Err(PlatformError::NotFound),
        }
    }

    fn ensure_publisher(&self, owner: &str) -> Result<String> {
        let c = self.db().lock();
        if let Some(id) = c
            .query_row(
                "SELECT id FROM publishers WHERE owner = ?1",
                params![owner],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return Ok(id);
        }
        let name: String = c.query_row(
            "SELECT name FROM users WHERE id = ?1",
            params![owner],
            |r| r.get(0),
        )?;
        let id = crate::auth::random_hex(8);
        c.execute(
            "INSERT INTO publishers(id, owner, name, bio, avatar_media_id, created_at)
             VALUES(?1, ?2, ?3, '', '', ?4)",
            params![id, owner, name, wxlike_core::types::now_ms() as i64],
        )?;
        Ok(id)
    }

    pub fn article_for(&self, viewer: &str, id: &str) -> Result<ArticleView> {
        let id = validate::id(id)?;
        let row = {
            let c = self.db().lock();
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
        let c = self.db().lock();
        let doc: ArticleDoc =
            serde_json::from_str(&row.doc).unwrap_or(ArticleDoc { blocks: vec![] });
        let reactions = reaction_counts(&c, id)?;
        let mut my = Vec::new();
        for kind in REACTION_KINDS {
            if c.query_row(
                "SELECT 1 FROM article_reactions WHERE article_id = ?1 AND user_id = ?2 AND kind = ?3",
                params![id, viewer, kind],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
            {
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

    pub fn update_article(
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
            let c = self.db().lock();
            render_doc(&c, &body.doc)?
        };
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        let (publisher_id, state, version) =
            load_editable_article(&tx, id, author, body.expected_version)?;
        let next_version = version + 1;
        let next_state = if body.publish || state == "published" {
            "published"
        } else {
            "draft"
        };
        let publish_now = state != "published" && next_state == "published";
        write_article_revision(
            &tx,
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
            publish_side_effects(&tx, id, &publisher_id, author, &title, &plain, now)?;
            content_index::remove_media_refs(&tx, "article", id)?;
            content_index::add_media_refs(
                &tx,
                &doc_media_ids(&body.doc),
                "article",
                id,
                author,
                now,
            )?;
        }
        tx.commit()?;
        drop(c);
        self.article_for(author, id)
    }

    pub fn delete_article(&self, author: &str, id: &str) -> Result<()> {
        let id = validate::id(id)?;
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        let changed = tx.execute(
            "UPDATE articles SET state = 'deleted', updated_at = ?1
             WHERE id = ?2 AND author = ?3 AND state <> 'deleted'",
            params![wxlike_core::types::now_ms() as i64, id, author],
        )?;
        if changed == 0 {
            return Err(PlatformError::NotFound);
        }
        content_index::remove(&tx, id)?;
        content_index::remove_media_refs(&tx, "article", id)?;
        crate::favorites::revoke_source(&tx, "article", id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn articles_by_publisher(
        &self,
        publisher_id: &str,
        cursor: Option<i64>,
        limit: i64,
    ) -> Result<(Vec<ArticleSummary>, i64, bool)> {
        let publisher_id = validate::id(publisher_id)?;
        let limit = limit.clamp(1, MAX_PAGE);
        let before = cursor.unwrap_or(i64::MAX);
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT a.seq, a.id, a.publisher_id, p.name, a.title, a.body_text, a.state,
                    a.version, a.created_at, a.published_at
             FROM articles a JOIN publishers p ON p.id = a.publisher_id
             WHERE a.publisher_id = ?1 AND a.state = 'published' AND a.seq < ?2
             ORDER BY a.seq DESC LIMIT ?3",
        )?;
        let it = stmt.query_map(params![publisher_id, before, limit + 1], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                ArticleSummary {
                    id: r.get(1)?,
                    publisher_id: r.get(2)?,
                    publisher_name: r.get(3)?,
                    title: r.get(4)?,
                    excerpt: excerpt(&r.get::<_, String>(5)?),
                    state: r.get(6)?,
                    version: r.get(7)?,
                    created_at: r.get(8)?,
                    published_at: r.get(9)?,
                    reactions: ReactionCounts::default(),
                },
            ))
        })?;
        let mut rows = it.collect::<std::result::Result<Vec<_>, _>>()?;
        let has_more = rows.len() as i64 > limit;
        if has_more {
            rows.pop();
        }
        let next = rows.last().map(|(seq, _)| *seq).unwrap_or(0);
        let mut out = Vec::with_capacity(rows.len());
        for (_, mut summary) in rows {
            summary.reactions = reaction_counts(&c, &summary.id)?;
            out.push(summary);
        }
        Ok((out, next, has_more))
    }

    /// 打开全文：记阅读行并把订阅读游标推进到这篇文章的 seq。
    pub fn mark_article_read(&self, user: &str, id: &str) -> Result<i64> {
        let id = validate::id(id)?;
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        let (seq, publisher_id, state): (i64, String, String) = tx
            .query_row(
                "SELECT seq, publisher_id, state FROM articles WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .ok_or(PlatformError::NotFound)?;
        if state != "published" {
            return Err(PlatformError::NotFound);
        }
        tx.execute(
            "INSERT INTO article_reads(user_id, article_id, read_at) VALUES(?1, ?2, ?3)
             ON CONFLICT(user_id, article_id) DO UPDATE SET read_at = excluded.read_at",
            params![user, id, now],
        )?;
        tx.execute(
            "INSERT INTO publisher_follows(user_id, publisher_id, read_cursor, created_at)
             VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(user_id, publisher_id) DO UPDATE SET
               read_cursor = MAX(read_cursor, excluded.read_cursor)",
            params![user, publisher_id, seq, now],
        )?;
        tx.commit()?;
        let unread = unread_count(&c, user, &publisher_id)?;
        drop(c);
        Ok(unread)
    }

    pub fn react_article(
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
        let c = self.db().lock();
        let published: Option<i64> = c
            .query_row(
                "SELECT 1 FROM articles WHERE id = ?1 AND state = 'published'",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        if published.is_none() {
            return Err(PlatformError::NotFound);
        }
        let removed = c.execute(
            "DELETE FROM article_reactions WHERE article_id = ?1 AND user_id = ?2 AND kind = ?3",
            params![id, user, kind],
        )?;
        let active = if removed == 0 {
            c.execute(
                "INSERT INTO article_reactions(article_id, user_id, kind, created_at)
                 VALUES(?1, ?2, ?3, ?4)",
                params![id, user, kind, now],
            )?;
            true
        } else {
            false
        };
        Ok((active, reaction_counts(&c, id)?))
    }
}

struct ArticleRow {
    id: String,
    publisher_id: String,
    publisher_name: String,
    author: String,
    title: String,
    plain: String,
    html: String,
    doc: String,
    state: String,
    version: i64,
    created_at: i64,
    published_at: Option<i64>,
}

fn article_row(c: &Connection, id: &str) -> Result<Option<ArticleRow>> {
    Ok(c.query_row(
        "SELECT a.id, a.publisher_id, p.name, a.author, a.title, a.body_text, a.body_html,
                a.doc, a.state, a.version, a.created_at, a.published_at
         FROM articles a JOIN publishers p ON p.id = a.publisher_id
         WHERE a.id = ?1",
        params![id],
        |r| {
            Ok(ArticleRow {
                id: r.get(0)?,
                publisher_id: r.get(1)?,
                publisher_name: r.get(2)?,
                author: r.get(3)?,
                title: r.get(4)?,
                plain: r.get(5)?,
                html: r.get(6)?,
                doc: r.get(7)?,
                state: r.get(8)?,
                version: r.get(9)?,
                created_at: r.get(10)?,
                published_at: r.get(COL_PUBLISHED_AT)?,
            })
        },
    )
    .optional()?)
}

fn reaction_counts(c: &Connection, id: &str) -> Result<ReactionCounts> {
    let mut counts = ReactionCounts::default();
    let mut stmt = c.prepare(
        "SELECT kind, COUNT(*) FROM article_reactions WHERE article_id = ?1 GROUP BY kind",
    )?;
    let rows = stmt.query_map(params![id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (kind, n) = row?;
        match kind.as_str() {
            "wow" => counts.wow = n,
            "share" => counts.share = n,
            "favorite" => counts.favorite = n,
            _ => {}
        }
    }
    Ok(counts)
}

fn unread_count(c: &Connection, user: &str, publisher_id: &str) -> Result<i64> {
    let cursor: i64 = c
        .query_row(
            "SELECT read_cursor FROM publisher_follows WHERE user_id = ?1 AND publisher_id = ?2",
            params![user, publisher_id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    Ok(c.query_row(
        "SELECT COUNT(*) FROM articles
         WHERE publisher_id = ?1 AND state = 'published' AND seq > ?2",
        params![publisher_id, cursor],
        |r| r.get(0),
    )?)
}

/// 发布后的同事务副作用：索引 + 粉丝通知。重复发布同一篇用 dedupe 去重。
/// 载入一篇可编辑的文章：必须存在、作者是本人、未删、乐观锁版本匹配。
/// 返回 `(publisher_id, state, version)`。
fn load_editable_article(
    tx: &rusqlite::Connection,
    id: &str,
    author: &str,
    expected_version: i64,
) -> Result<(String, String, i64)> {
    let (owner, publisher_id, state, version): (String, String, String, i64) = tx
        .query_row(
            "SELECT author, publisher_id, state, version FROM articles WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .ok_or(PlatformError::NotFound)?;
    if owner != author {
        return Err(PlatformError::Forbidden);
    }
    if state == "deleted" {
        return Err(PlatformError::NotFound);
    }
    if version != expected_version {
        return Err(PlatformError::Conflict(format!(
            "version mismatch: current {version}"
        )));
    }
    Ok((publisher_id, state, version))
}

/// 一次保存要落库的全部字段；UPDATE 主行 + INSERT 版本历史共用。
struct ArticleRevision<'a> {
    title: &'a str,
    doc_json: &'a str,
    plain: &'a str,
    html: &'a str,
    next_state: &'a str,
    next_version: i64,
    now: i64,
    publish_now: bool,
}

fn write_article_revision(
    tx: &rusqlite::Connection,
    id: &str,
    r: &ArticleRevision<'_>,
) -> Result<()> {
    tx.execute(
        "UPDATE articles SET title = ?1, doc = ?2, body_text = ?3, body_html = ?4,
                             state = ?5, version = ?6, updated_at = ?7,
                             published_at = COALESCE(published_at, ?8)
         WHERE id = ?9",
        params![
            r.title,
            r.doc_json,
            r.plain,
            r.html,
            r.next_state,
            r.next_version,
            r.now,
            r.publish_now.then_some(r.now),
            id
        ],
    )?;
    tx.execute(
        "INSERT INTO article_versions(article_id, version, doc, saved_at) VALUES(?1, ?2, ?3, ?4)",
        params![id, r.next_version, r.doc_json, r.now],
    )?;
    Ok(())
}

fn publish_side_effects(
    tx: &Connection,
    id: &str,
    publisher_id: &str,
    author: &str,
    title: &str,
    plain: &str,
    now: i64,
) -> Result<()> {
    content_index::upsert(
        tx,
        &content_index::IndexDoc {
            doc_id: id,
            kind: "article",
            owner: author,
            audience: "public",
            group_name: "",
            title,
            body: plain,
        },
        now,
    )?;
    let mut stmt = tx.prepare("SELECT user_id FROM publisher_follows WHERE publisher_id = ?1")?;
    let followers: Vec<String> = stmt
        .query_map(params![publisher_id], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);
    for follower in followers {
        inbox::notify(
            tx,
            &follower,
            "article_published",
            author,
            "article",
            id,
            title,
            &format!("article:{id}"),
            now,
        )?;
    }
    Ok(())
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

/// 校验 + 渲染。返回 `(plain_text, html)`；HTML 只由白名单标签拼出。
fn render_doc(c: &Connection, doc: &ArticleDoc) -> Result<(String, String)> {
    if doc.blocks.is_empty() {
        return Err(PlatformError::BadInput("article is empty"));
    }
    if doc.blocks.len() > MAX_ARTICLE_BLOCKS {
        return Err(PlatformError::BadInput("too many blocks"));
    }
    let mut plain = String::new();
    let mut html = String::new();
    for block in &doc.blocks {
        match block {
            ArticleBlock::Paragraph { spans } => {
                let (text, inner) = render_spans(spans)?;
                push_plain(&mut plain, &text);
                html.push_str("<p>");
                html.push_str(&inner);
                html.push_str("</p>");
            }
            ArticleBlock::Heading { level, spans } => {
                if !(1..=3).contains(level) {
                    return Err(PlatformError::BadInput("heading level must be 1-3"));
                }
                let (text, inner) = render_spans(spans)?;
                push_plain(&mut plain, &text);
                html.push_str(&format!("<h{level}>{inner}</h{level}>"));
            }
            ArticleBlock::Quote { spans } => {
                let (text, inner) = render_spans(spans)?;
                push_plain(&mut plain, &text);
                html.push_str("<blockquote>");
                html.push_str(&inner);
                html.push_str("</blockquote>");
            }
            ArticleBlock::Image { media_id, alt } => {
                let media_id = validate::id(media_id)?;
                require_image_media(c, media_id)?;
                let alt = check_text("alt", alt, MAX_ALT_CHARS, false)?;
                html.push_str(&format!(
                    "<img src=\"/api/media/{}\" alt=\"{}\" />",
                    media_id,
                    escape(&alt)
                ));
            }
            ArticleBlock::Link { text, href } => {
                let text = check_text("link text", text, 500, false)?;
                let href = check_href(href)?;
                push_plain(&mut plain, &text);
                html.push_str(&format!(
                    "<a href=\"{}\" rel=\"noopener noreferrer\" target=\"_blank\">{}</a>",
                    escape(&href),
                    escape(&text)
                ));
            }
        }
    }
    if plain.chars().count() > MAX_BODY_TEXT_CHARS {
        return Err(PlatformError::BadInput("article body too long"));
    }
    Ok((plain, html))
}

fn render_spans(spans: &[ArticleSpan]) -> Result<(String, String)> {
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

fn push_plain(buf: &mut String, text: &str) {
    if !buf.is_empty() {
        buf.push('\n');
    }
    buf.push_str(text);
}

fn check_text(field: &'static str, s: &str, max: usize, allow_newline: bool) -> Result<String> {
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
fn check_href(href: &str) -> Result<String> {
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

fn require_image_media(c: &Connection, media_id: &str) -> Result<()> {
    let mime: Option<String> = c
        .query_row(
            "SELECT content_type FROM media WHERE id = ?1",
            params![media_id],
            |r| r.get(0),
        )
        .optional()?;
    match mime {
        Some(mime) if mime.starts_with("image/") => Ok(()),
        _ => Err(PlatformError::BadInput("unknown image media")),
    }
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

async fn set_publisher(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<PublisherBody>,
) -> std::result::Result<Json<PublisherView>, ApiError> {
    Ok(Json(platform.set_publisher(
        &authed.user_id,
        &body.name,
        &body.bio,
        &body.avatar_media_id,
    )?))
}

async fn list_publishers(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<ListQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let items = platform.publishers(&authed.user_id, q.q.as_deref())?;
    Ok(Json(
        serde_json::json!({ "items": items, "snapshot_version": wxlike_core::types::now_ms() }),
    ))
}

async fn one_publisher(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<PublisherView>, ApiError> {
    Ok(Json(platform.publisher_by_id(&id, &authed.user_id)?))
}

async fn follow(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<PublisherView>, ApiError> {
    platform.follow_publisher(&authed.user_id, &id)?;
    Ok(Json(platform.publisher_by_id(&id, &authed.user_id)?))
}

async fn unfollow(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<PublisherView>, ApiError> {
    platform.unfollow_publisher(&authed.user_id, &id)?;
    Ok(Json(platform.publisher_by_id(&id, &authed.user_id)?))
}

async fn publisher_articles(
    State(platform): State<PlatformState>,
    _authed: Authed,
    Path(id): Path<String>,
    Query(q): Query<ArticleListQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let (items, next, has_more) =
        platform.articles_by_publisher(&id, q.cursor, q.limit.unwrap_or(page::DEFAULT_PAGE))?;
    Ok(page::listing_response(items, next, has_more))
}

async fn create(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<ArticleBody>,
) -> std::result::Result<(StatusCode, Json<ArticleView>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(platform.create_article(&authed.user_id, body)?),
    ))
}

async fn one(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<ArticleView>, ApiError> {
    Ok(Json(platform.article_for(&authed.user_id, &id)?))
}

async fn update(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ArticleUpdateBody>,
) -> std::result::Result<Json<ArticleView>, ApiError> {
    Ok(Json(platform.update_article(&authed.user_id, &id, body)?))
}

async fn remove(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.delete_article(&authed.user_id, &id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn mark_read(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let unread = platform.mark_article_read(&authed.user_id, &id)?;
    Ok(Json(serde_json::json!({ "unread": unread })))
}

async fn react(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ReactBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let (active, counts) = platform.react_article(&authed.user_id, &id, &body.kind)?;
    Ok(Json(
        serde_json::json!({ "active": active, "counts": counts }),
    ))
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
