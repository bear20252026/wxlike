//! **仓储层**（`articles`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::{PlatformError, Result};
use crate::validate;
use rusqlite::{params, Connection, OptionalExtension as _};

use crate::domain::articles::{
    check_href, check_text, escape, push_plain, render_spans, ArticleBlock, ArticleDoc,
    ArticleRevision, ArticleRow, ReactionCounts, COL_PUBLISHED_AT, MAX_ALT_CHARS,
    MAX_ARTICLE_BLOCKS, MAX_BODY_TEXT_CHARS,
};

pub(in crate::domain) fn article_row(c: &Connection, id: &str) -> Result<Option<ArticleRow>> {
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

pub(in crate::domain) fn reaction_counts(c: &Connection, id: &str) -> Result<ReactionCounts> {
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
    c: &Connection,
    user: &str,
    publisher_id: &str,
) -> Result<i64> {
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

pub(in crate::domain) fn publish_side_effects(
    tx: &Connection,
    id: &str,
    publisher_id: &str,
    author: &str,
    title: &str,
    plain: &str,
    now: i64,
) -> Result<()> {
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
    let mut stmt = tx.prepare("SELECT user_id FROM publisher_follows WHERE publisher_id = ?1")?;
    let followers: Vec<String> = stmt
        .query_map(params![publisher_id], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);
    for follower in followers {
        crate::domain::repo::inbox::notify(
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

/// 校验 + 渲染。返回 `(plain_text, html)`；HTML 只由白名单标签拼出。
/// 渲染单个内容块并追加到 plain/html 累积串（图片块只进 html 不进 plain）。
pub(in crate::domain) fn render_block(
    c: &Connection,
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

pub(in crate::domain) fn render_doc(c: &Connection, doc: &ArticleDoc) -> Result<(String, String)> {
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

pub(in crate::domain) fn require_image_media(c: &Connection, media_id: &str) -> Result<()> {
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
