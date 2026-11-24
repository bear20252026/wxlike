//! **仓储层**（`articles`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::{PlatformError, Result};
use crate::storage::StorageConnection;
use crate::validate;
use rusqlite::{params, OptionalExtension as _};

use crate::domain::articles::{
    check_href, check_text, escape, push_plain, render_spans, ArticleBlock, ArticleDoc,
    ArticleRevision, ArticleRow, ReactionCounts, COL_PUBLISHED_AT, MAX_ALT_CHARS,
    MAX_ARTICLE_BLOCKS, MAX_BODY_TEXT_CHARS,
};

/// 发布者主行；`following` / `unread` / 文章数是读模型，不在这里。
pub(in crate::domain) struct PublisherRecord {
    pub id: String,
    pub owner: String,
    pub name: String,
    pub bio: String,
    pub avatar_media_id: String,
    pub created_at: i64,
}

/// 新文章一次落库的全部字段；主行与首个版本历史共用。
pub(in crate::domain) struct ArticleCreate<'a> {
    pub id: &'a str,
    pub publisher_id: &'a str,
    pub author: &'a str,
    pub title: &'a str,
    pub doc_json: &'a str,
    pub plain: &'a str,
    pub html: &'a str,
    pub state: &'a str,
    pub now: i64,
    pub published_at: Option<i64>,
}

/// 列表页的原始行；`excerpt` 与 reaction 计数是领域读模型。
pub(in crate::domain) struct ArticleListRow {
    pub seq: i64,
    pub id: String,
    pub publisher_id: String,
    pub publisher_name: String,
    pub title: String,
    pub body_text: String,
    pub state: String,
    pub version: i64,
    pub created_at: i64,
    pub published_at: Option<i64>,
}

pub(in crate::domain) fn publisher_id_by_owner(
    c: &StorageConnection<'_>,
    owner: &str,
) -> Result<Option<String>> {
    let c = c.sqlite()?;
    let id = c
        .query_row(
            "SELECT id FROM publishers WHERE owner = ?1",
            params![owner],
            |r| r.get(0),
        )
        .optional()?;
    Ok(id)
}

pub(in crate::domain) fn publisher_record(
    c: &StorageConnection<'_>,
    id: &str,
) -> Result<Option<PublisherRecord>> {
    let c = c.sqlite()?;
    let row = c
        .query_row(
            "SELECT id, owner, name, bio, avatar_media_id, created_at FROM publishers
             WHERE id = ?1",
            params![id],
            |r| {
                Ok(PublisherRecord {
                    id: r.get(0)?,
                    owner: r.get(1)?,
                    name: r.get(2)?,
                    bio: r.get(3)?,
                    avatar_media_id: r.get(4)?,
                    created_at: r.get(5)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

pub(in crate::domain) fn publisher_followed(
    c: &StorageConnection<'_>,
    user: &str,
    publisher_id: &str,
) -> Result<bool> {
    let c = c.sqlite()?;
    let found = c
        .query_row(
            "SELECT 1 FROM publisher_follows WHERE user_id = ?1 AND publisher_id = ?2",
            params![user, publisher_id],
            |_| Ok(1i64),
        )
        .optional()?;
    Ok(found.is_some())
}

pub(in crate::domain) fn publisher_published_count(
    c: &StorageConnection<'_>,
    publisher_id: &str,
) -> Result<i64> {
    let c = c.sqlite()?;
    let count = c.query_row(
        "SELECT COUNT(*) FROM articles WHERE publisher_id = ?1 AND state = 'published'",
        params![publisher_id],
        |r| r.get(0),
    )?;
    Ok(count)
}

pub(in crate::domain) fn update_publisher(
    c: &StorageConnection<'_>,
    id: &str,
    name: &str,
    bio: &str,
    avatar_media_id: &str,
) -> Result<()> {
    let c = c.sqlite()?;
    c.execute(
        "UPDATE publishers SET name = ?1, bio = ?2, avatar_media_id = ?3
         WHERE id = ?4",
        params![name, bio, avatar_media_id, id],
    )?;
    Ok(())
}

pub(in crate::domain) fn insert_publisher(
    c: &StorageConnection<'_>,
    id: &str,
    owner: &str,
    name: &str,
    bio: &str,
    avatar_media_id: &str,
    now: i64,
) -> Result<()> {
    let c = c.sqlite()?;
    c.execute(
        "INSERT INTO publishers(id, owner, name, bio, avatar_media_id, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, owner, name, bio, avatar_media_id, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn publisher_exists(c: &StorageConnection<'_>, id: &str) -> Result<bool> {
    let c = c.sqlite()?;
    let found = c
        .query_row(
            "SELECT 1 FROM publishers WHERE id = ?1",
            params![id],
            |_| Ok(1i64),
        )
        .optional()?;
    Ok(found.is_some())
}

pub(in crate::domain) fn insert_follow_ignore(
    c: &StorageConnection<'_>,
    user: &str,
    publisher_id: &str,
    now: i64,
) -> Result<()> {
    let c = c.sqlite()?;
    c.execute(
        "INSERT OR IGNORE INTO publisher_follows(user_id, publisher_id, read_cursor, created_at)
         VALUES(?1, ?2, 0, ?3)",
        params![user, publisher_id, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn delete_follow(
    c: &StorageConnection<'_>,
    user: &str,
    publisher_id: &str,
) -> Result<()> {
    let c = c.sqlite()?;
    c.execute(
        "DELETE FROM publisher_follows WHERE user_id = ?1 AND publisher_id = ?2",
        params![user, publisher_id],
    )?;
    Ok(())
}

pub(in crate::domain) fn publisher_ids_for_user(
    c: &StorageConnection<'_>,
    user: &str,
) -> Result<Vec<String>> {
    let c = c.sqlite()?;
    let mut stmt = c.prepare(
        "SELECT DISTINCT p.id FROM publishers p
         LEFT JOIN publisher_follows f
           ON f.publisher_id = p.id AND f.user_id = ?1
         WHERE f.user_id IS NOT NULL OR p.owner = ?1
         ORDER BY p.created_at DESC, p.id",
    )?;
    let ids = stmt
        .query_map(params![user], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(ids)
}

pub(in crate::domain) fn publisher_name_matches(
    c: &StorageConnection<'_>,
    id: &str,
    pattern: &str,
) -> Result<bool> {
    let c = c.sqlite()?;
    let found = c
        .query_row(
            "SELECT 1 FROM publishers WHERE id = ?1 AND name LIKE ?2 ESCAPE '\\'",
            params![id, pattern],
            |_| Ok(1i64),
        )
        .optional()?;
    Ok(found.is_some())
}

pub(in crate::domain) fn insert_article(
    c: &StorageConnection<'_>,
    a: &ArticleCreate<'_>,
) -> Result<()> {
    let c = c.sqlite()?;
    c.execute(
        "INSERT INTO articles(seq, id, publisher_id, author, title, doc, body_text, body_html,
                          state, version, created_at, updated_at, published_at)
         VALUES(NULL, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9, ?9, ?10)",
        params![
            a.id,
            a.publisher_id,
            a.author,
            a.title,
            a.doc_json,
            a.plain,
            a.html,
            a.state,
            a.now,
            a.published_at
        ],
    )?;
    Ok(())
}

pub(in crate::domain) fn insert_article_version(
    c: &StorageConnection<'_>,
    id: &str,
    doc_json: &str,
    now: i64,
) -> Result<()> {
    let c = c.sqlite()?;
    c.execute(
        "INSERT INTO article_versions(article_id, version, doc, saved_at) VALUES(?1, 1, ?2, ?3)",
        params![id, doc_json, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn publisher_owner(
    c: &StorageConnection<'_>,
    id: &str,
) -> Result<Option<String>> {
    let c = c.sqlite()?;
    let owner = c
        .query_row(
            "SELECT owner FROM publishers WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(owner)
}

pub(in crate::domain) fn user_name(c: &StorageConnection<'_>, id: &str) -> Result<String> {
    let c = c.sqlite()?;
    let name = c.query_row("SELECT name FROM users WHERE id = ?1", params![id], |r| {
        r.get(0)
    })?;
    Ok(name)
}

pub(in crate::domain) fn reaction_active(
    c: &StorageConnection<'_>,
    article_id: &str,
    user: &str,
    kind: &str,
) -> Result<bool> {
    let c = c.sqlite()?;
    let found = c
        .query_row(
            "SELECT 1 FROM article_reactions WHERE article_id = ?1 AND user_id = ?2 AND kind = ?3",
            params![article_id, user, kind],
            |_| Ok(1i64),
        )
        .optional()?;
    Ok(found.is_some())
}

pub(in crate::domain) fn article_list(
    c: &StorageConnection<'_>,
    publisher_id: &str,
    before: i64,
    limit: i64,
) -> Result<Vec<ArticleListRow>> {
    let c = c.sqlite()?;
    let mut stmt = c.prepare(
        "SELECT a.seq, a.id, a.publisher_id, p.name, a.title, a.body_text, a.state,
                a.version, a.created_at, a.published_at
         FROM articles a JOIN publishers p ON p.id = a.publisher_id
         WHERE a.publisher_id = ?1 AND a.state = 'published' AND a.seq < ?2
         ORDER BY a.seq DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![publisher_id, before, limit + 1], |r| {
        Ok(ArticleListRow {
            seq: r.get(0)?,
            id: r.get(1)?,
            publisher_id: r.get(2)?,
            publisher_name: r.get(3)?,
            title: r.get(4)?,
            body_text: r.get(5)?,
            state: r.get(6)?,
            version: r.get(7)?,
            created_at: r.get(8)?,
            published_at: r.get(9)?,
        })
    })?;
    let list = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(list)
}

pub(in crate::domain) fn article_read_target(
    c: &StorageConnection<'_>,
    id: &str,
) -> Result<Option<(i64, String, String)>> {
    let c = c.sqlite()?;
    let row = c
        .query_row(
            "SELECT seq, publisher_id, state FROM articles WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(row)
}

pub(in crate::domain) fn record_article_read(
    c: &StorageConnection<'_>,
    user: &str,
    article_id: &str,
    now: i64,
) -> Result<()> {
    let c = c.sqlite()?;
    c.execute(
        "INSERT INTO article_reads(user_id, article_id, read_at) VALUES(?1, ?2, ?3)
         ON CONFLICT(user_id, article_id) DO UPDATE SET read_at = excluded.read_at",
        params![user, article_id, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn advance_follow_cursor(
    c: &StorageConnection<'_>,
    user: &str,
    publisher_id: &str,
    cursor: i64,
    now: i64,
) -> Result<()> {
    let c = c.sqlite()?;
    c.execute(
        "INSERT INTO publisher_follows(user_id, publisher_id, read_cursor, created_at)
         VALUES(?1, ?2, ?3, ?4)
         ON CONFLICT(user_id, publisher_id) DO UPDATE SET
           read_cursor = MAX(read_cursor, excluded.read_cursor)",
        params![user, publisher_id, cursor, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn article_is_published(c: &StorageConnection<'_>, id: &str) -> Result<bool> {
    let c = c.sqlite()?;
    let published = c
        .query_row(
            "SELECT 1 FROM articles WHERE id = ?1 AND state = 'published'",
            params![id],
            |_| Ok(1i64),
        )
        .optional()?;
    Ok(published.is_some())
}

pub(in crate::domain) fn remove_reaction(
    c: &StorageConnection<'_>,
    article_id: &str,
    user: &str,
    kind: &str,
) -> Result<usize> {
    let c = c.sqlite()?;
    let removed = c.execute(
        "DELETE FROM article_reactions WHERE article_id = ?1 AND user_id = ?2 AND kind = ?3",
        params![article_id, user, kind],
    )?;
    Ok(removed)
}

pub(in crate::domain) fn insert_reaction(
    c: &StorageConnection<'_>,
    article_id: &str,
    user: &str,
    kind: &str,
    now: i64,
) -> Result<()> {
    let c = c.sqlite()?;
    c.execute(
        "INSERT INTO article_reactions(article_id, user_id, kind, created_at)
         VALUES(?1, ?2, ?3, ?4)",
        params![article_id, user, kind, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn soft_delete_article(
    c: &StorageConnection<'_>,
    id: &str,
    author: &str,
    now: i64,
) -> Result<usize> {
    let c = c.sqlite()?;
    let changed = c.execute(
        "UPDATE articles SET state = 'deleted', updated_at = ?1
         WHERE id = ?2 AND author = ?3 AND state <> 'deleted'",
        params![now, id, author],
    )?;
    Ok(changed)
}

pub(in crate::domain) fn article_row(
    c: &StorageConnection<'_>,
    id: &str,
) -> Result<Option<ArticleRow>> {
    let c = c.sqlite()?;
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

pub(in crate::domain) fn reaction_counts(
    c: &StorageConnection<'_>,
    id: &str,
) -> Result<ReactionCounts> {
    let c = c.sqlite()?;
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

pub(in crate::domain) fn unread_count(
    c: &StorageConnection<'_>,
    user: &str,
    publisher_id: &str,
) -> Result<i64> {
    let c = c.sqlite()?;
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
pub(in crate::domain) fn load_editable_article(
    tx: &StorageConnection<'_>,
    id: &str,
    author: &str,
    expected_version: i64,
) -> Result<(String, String, i64)> {
    let tx = tx.sqlite()?;
    let (owner, publisher_id, state, version): (String, String, String, i64) = tx
        .query_row(
            "SELECT author, publisher_id, state, version FROM articles WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .ok_or(PlatformError::NotFound)?;
    if owner != author {
        // 别人的稿子（尤其是未发布草稿）与不存在的稿子必须同形：403 会把
        // "这个 id 是一篇别人的草稿"泄露给探测者。
        return Err(PlatformError::NotFound);
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

pub(in crate::domain) fn write_article_revision(
    tx: &StorageConnection<'_>,
    id: &str,
    r: &ArticleRevision<'_>,
) -> Result<()> {
    let tx = tx.sqlite()?;
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

pub(in crate::domain) fn publish_side_effects(
    tx: &StorageConnection<'_>,
    id: &str,
    publisher_id: &str,
    author: &str,
    title: &str,
    plain: &str,
    now: i64,
) -> Result<()> {
    let conn = tx.sqlite()?;
    crate::domain::repo::content_index::upsert(
        tx,
        &crate::domain::content_index::IndexDoc {
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
    let mut stmt = conn.prepare("SELECT user_id FROM publisher_follows WHERE publisher_id = ?1")?;
    let followers: Vec<String> = stmt
        .query_map(params![publisher_id], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);
    for follower in followers {
        crate::domain::repo::inbox::notify_conn(
            conn,
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

/// 校验 + 渲染。返回 `(plain_text, html)`；HTML 只由白名单标签拼出。
/// 渲染单个内容块并追加到 plain/html 累积串（图片块只进 html 不进 plain）。
pub(in crate::domain) fn render_block(
    c: &StorageConnection<'_>,
    block: &ArticleBlock,
    plain: &mut String,
    html: &mut String,
) -> Result<()> {
    match block {
        ArticleBlock::Paragraph { spans } => {
            let (text, inner) = render_spans(spans)?;
            push_plain(plain, &text);
            html.push_str("<p>");
            html.push_str(&inner);
            html.push_str("</p>");
        }
        ArticleBlock::Heading { level, spans } => {
            if !(1..=3).contains(level) {
                return Err(PlatformError::BadInput("heading level must be 1-3"));
            }
            let (text, inner) = render_spans(spans)?;
            push_plain(plain, &text);
            html.push_str(&format!("<h{level}>{inner}</h{level}>"));
        }
        ArticleBlock::Quote { spans } => {
            let (text, inner) = render_spans(spans)?;
            push_plain(plain, &text);
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
            push_plain(plain, &text);
            html.push_str(&format!(
                "<a href=\"{}\" rel=\"noopener noreferrer\" target=\"_blank\">{}</a>",
                escape(&href),
                escape(&text)
            ));
        }
    }
    Ok(())
}

pub(in crate::domain) fn render_doc(
    c: &StorageConnection<'_>,
    doc: &ArticleDoc,
) -> Result<(String, String)> {
    if doc.blocks.is_empty() {
        return Err(PlatformError::BadInput("article is empty"));
    }
    if doc.blocks.len() > MAX_ARTICLE_BLOCKS {
        return Err(PlatformError::BadInput("too many blocks"));
    }
    let mut plain = String::new();
    let mut html = String::new();
    for block in &doc.blocks {
        render_block(c, block, &mut plain, &mut html)?;
    }
    if plain.chars().count() > MAX_BODY_TEXT_CHARS {
        return Err(PlatformError::BadInput("article body too long"));
    }
    Ok((plain, html))
}

pub(in crate::domain) fn require_image_media(
    c: &StorageConnection<'_>,
    media_id: &str,
) -> Result<()> {
    let c = c.sqlite()?;
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
