//! G05-05 公开内容索引与检索（`/api/search/content`、`/api/search/trends`）。
//!
//! 三条不变式：
//!
//! 1. **先授权、后取正文。** 候选页只查 `doc_id/kind/owner` 等元数据，逐条
//!    回权威表判定可见性，通过后才读 `title/body` 生成 snippet。索引里的
//!    死行、撤权行不会以任何形式出现在响应里。
//! 2. **写同步、删同步。** 发布内容与写索引在同一个事务；删除内容同时删行。
//!    因此不存在"索引落后导致泄露/漏数据"的异步窗口（重建索引只是重建投影）。
//! 3. **私聊永不进索引。** 索引只接收朋友圈/文章/视频号/状态；消息正文
//!    仍只在客户端本地检索，服务端没有明文建索引的入口。
//!
//! 热搜是索引上的聚合：按**去重作者**计数（同一作者最多算 3 篇），只统计
//! `audience = 'public'` 的内容，因此好友可见/私密内容和个人私聊都不会出现。

use std::collections::{BTreeMap, BTreeSet};

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::page;
use crate::{Platform, PlatformState};

/// 单页上限：公开检索是扇出查询，别让它变成全表扫描的放大器。
pub const MAX_SEARCH_PAGE: i64 = 50;

pub const MAX_QUERY_CHARS: usize = 100;
const MAX_CURSOR_ID_LEN: usize = 64;
const DEFAULT_TREND_PAGE: usize = 20;
pub const MAX_SNIPPET_CHARS: usize = 120;
/// 热搜窗口：固定 7 天，客户端不能把它调成"全历史"。
pub const TREND_WINDOW_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const TREND_MAX_LIMIT: usize = 50;
/// 每个作者对单个词条最多贡献的文档数（防单人刷榜）。
const TREND_OWNER_CAP: usize = 3;

const KINDS: [&str; 4] = ["moment", "article", "video", "status"];

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/search/content", get(search))
        .route("/api/search/trends", get(trends))
}

/// 一条索引记录的写入形状。正文只存纯文本，HTML/结构留在原表。
pub(crate) struct IndexDoc<'a> {
    pub doc_id: &'a str,
    pub kind: &'a str,
    pub owner: &'a str,
    pub audience: &'a str,
    pub group_name: &'a str,
    pub title: &'a str,
    pub body: &'a str,
}

pub(crate) fn upsert(conn: &Connection, doc: &IndexDoc<'_>, now: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO content_index(doc_id, kind, owner, audience, group_name, title, body,
                                   created_at, updated_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
         ON CONFLICT(doc_id) DO UPDATE SET
           kind = excluded.kind, owner = excluded.owner, audience = excluded.audience,
           group_name = excluded.group_name, title = excluded.title, body = excluded.body,
           updated_at = excluded.updated_at",
        params![
            doc.doc_id,
            doc.kind,
            doc.owner,
            doc.audience,
            doc.group_name,
            doc.title,
            doc.body,
            now
        ],
    )?;
    Ok(())
}

/// 删除内容时同步调用：索引行消失后，检索不可能再命中已删内容。
pub(crate) fn remove(conn: &Connection, doc_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM content_index WHERE doc_id = ?1",
        params![doc_id],
    )?;
    Ok(())
}

pub(crate) fn add_media_refs(
    conn: &Connection,
    media_ids: &[String],
    ref_kind: &str,
    ref_id: &str,
    owner: &str,
    now: i64,
) -> Result<()> {
    for media_id in media_ids {
        conn.execute(
            "INSERT OR IGNORE INTO content_media_refs(media_id, ref_kind, ref_id, owner, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![media_id, ref_kind, ref_id, owner, now],
        )?;
    }
    Ok(())
}

pub(crate) fn remove_media_refs(conn: &Connection, ref_kind: &str, ref_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM content_media_refs WHERE ref_kind = ?1 AND ref_id = ?2",
        params![ref_kind, ref_id],
    )?;
    Ok(())
}

impl Platform {
    /// 公开内容是否对 `viewer` 可见。**唯一**的检索授权入口：索引只给候选，
    /// 这里回权威表作答。
    pub fn content_visible(&self, viewer: &str, kind: &str, doc_id: &str) -> Result<bool> {
        match kind {
            "moment" => self.moment_visible(viewer, doc_id),
            "article" => {
                let c = self.db().lock();
                Ok(c.query_row(
                    "SELECT 1 FROM articles WHERE id = ?1 AND state = 'published'",
                    params![doc_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some())
            }
            "video" => self.video_visible(viewer, doc_id),
            "status" => self.status_visible(viewer, doc_id),
            _ => Ok(false),
        }
    }

    pub fn media_referrers(&self, media_id: &str) -> Result<Vec<(String, String)>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT ref_kind, ref_id FROM content_media_refs WHERE media_id = ?1
             ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![media_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 检索：候选页（无正文）→ 逐条授权 → 只为通过者取正文。
    pub fn search_content(
        &self,
        viewer: &str,
        q: &str,
        kind: Option<&str>,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<(Vec<SearchItem>, String, bool)> {
        let needle = validate_query(q)?;
        let kind = match kind {
            Some(k) if !k.is_empty() => {
                if !KINDS.contains(&k) {
                    return Err(PlatformError::BadInput("unknown search kind"));
                }
                Some(k.to_string())
            }
            _ => None,
        };
        let (before_at, before_id) = parse_cursor(cursor)?;
        let limit = limit.clamp(1, MAX_SEARCH_PAGE);
        let pattern = like_pattern(&needle);

        let candidates =
            self.search_candidates(&pattern, kind.as_deref(), before_at, &before_id, limit)?;
        let mut items = Vec::new();
        let mut more = false;
        for (doc_id, kind, _owner, created_at) in candidates {
            if !self.content_visible(viewer, &kind, &doc_id)? {
                continue;
            }
            if items.len() as i64 == limit {
                more = true;
                break;
            }
            let Some((title, body)) = self.index_body(&doc_id)? else {
                continue;
            };
            items.push(SearchItem {
                doc_id,
                kind,
                title,
                snippet: snippet(&body, &needle),
                created_at,
            });
        }
        let next = items
            .last()
            .map(|i| format!("{}:{}", i.created_at, i.doc_id))
            .unwrap_or_default();
        Ok((items, next, more))
    }

    fn index_body(&self, doc_id: &str) -> Result<Option<(String, String)>> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT title, body FROM content_index WHERE doc_id = ?1",
            params![doc_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?)
    }

    /// 候选页只含元数据。`kind` 分支各用一套占位符编号，避免把 `None`
    /// 塞进 `? IS NOT NULL` 造成整页被过滤。
    fn search_candidates(
        &self,
        pattern: &str,
        kind: Option<&str>,
        before_at: i64,
        before_id: &str,
        limit: i64,
    ) -> Result<Vec<(String, String, String, i64)>> {
        let c = self.db().lock();
        let take = limit + 1;
        let mut rows = Vec::new();
        if let Some(kind) = kind {
            let mut stmt = c.prepare(
                "SELECT doc_id, kind, owner, created_at FROM content_index
                 WHERE (title LIKE ?1 ESCAPE '\\' OR body LIKE ?1 ESCAPE '\\') AND kind = ?2
                   AND (created_at < ?3 OR (created_at = ?3 AND doc_id < ?4))
                 ORDER BY created_at DESC, doc_id DESC LIMIT ?5",
            )?;
            let it = stmt.query_map(
                params![pattern, kind, before_at, before_id, take],
                candidate_row,
            )?;
            for row in it {
                rows.push(row?);
            }
        } else {
            let mut stmt = c.prepare(
                "SELECT doc_id, kind, owner, created_at FROM content_index
                 WHERE (title LIKE ?1 ESCAPE '\\' OR body LIKE ?1 ESCAPE '\\')
                   AND (created_at < ?2 OR (created_at = ?2 AND doc_id < ?3))
                 ORDER BY created_at DESC, doc_id DESC LIMIT ?4",
            )?;
            let it = stmt.query_map(params![pattern, before_at, before_id, take], candidate_row)?;
            for row in it {
                rows.push(row?);
            }
        }
        Ok(rows)
    }

    /// 热搜：去重作者 + 单人贡献上限 + 只用公开内容。
    pub fn content_trends(&self, limit: usize) -> Result<Vec<TrendTerm>> {
        let limit = limit.clamp(1, TREND_MAX_LIMIT);
        let cutoff = wxlike_core::types::now_ms() as i64 - TREND_WINDOW_MS;
        let rows = {
            let c = self.db().lock();
            let mut stmt = c.prepare(
                "SELECT owner, title, body FROM content_index
                 WHERE audience = 'public' AND created_at >= ?1",
            )?;
            let it = stmt.query_map(params![cutoff], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            it.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut contributions: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
        for (owner, title, body) in rows {
            let mut doc_terms = BTreeSet::new();
            for text in [title, body] {
                for term in extract_terms(&text) {
                    doc_terms.insert(term);
                }
            }
            for term in doc_terms {
                let entry = contributions.entry(term).or_default();
                let count = entry.entry(owner.clone()).or_insert(0);
                *count += 1;
            }
        }
        let mut terms: Vec<TrendTerm> = contributions
            .into_iter()
            .map(|(term, owners)| {
                let distinct_authors = owners.len() as i64;
                let doc_count: i64 = owners
                    .into_values()
                    .map(|n| n.min(TREND_OWNER_CAP) as i64)
                    .sum();
                TrendTerm {
                    term,
                    score: distinct_authors * 100 + doc_count,
                    doc_count,
                    distinct_authors,
                }
            })
            .collect();
        terms.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.term.cmp(&b.term)));
        terms.truncate(limit);
        Ok(terms)
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct SearchItem {
    pub doc_id: String,
    pub kind: String,
    pub title: String,
    pub snippet: String,
    pub created_at: i64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct TrendTerm {
    pub term: String,
    pub score: i64,
    pub doc_count: i64,
    pub distinct_authors: i64,
}

fn candidate_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(String, String, String, i64)> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
}

fn validate_query(q: &str) -> Result<String> {
    let q = q.trim();
    if q.is_empty() {
        return Err(PlatformError::BadInput("query must not be empty"));
    }
    if q.chars().count() > MAX_QUERY_CHARS {
        return Err(PlatformError::BadInput("query too long"));
    }
    if q.chars().any(|c| c.is_control()) {
        return Err(PlatformError::BadInput("query contains control characters"));
    }
    Ok(q.to_lowercase())
}

/// 转义 LIKE 的通配符，用户输入里的 `%`/`_` 只能当字面量。
fn like_pattern(q: &str) -> String {
    format!("%{}%", page::escape_like(q))
}

/// 在 page::parse_cursor 的通用校验之上，额外收紧游标 id 的形态：
/// 内容索引的 doc_id 全部由服务端生成（ASCII 字母数字、定长上限），
/// 不接受其他形态，防止把外部拼出的游标当检索入口。
fn parse_cursor(cursor: Option<&str>) -> Result<(i64, String)> {
    if let Some(c) = cursor.filter(|c| !c.is_empty()) {
        let id = c.split_once(':').map(|(_, id)| id).unwrap_or("");
        if id.is_empty()
            || id.len() > MAX_CURSOR_ID_LEN
            || !id.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Err(PlatformError::BadInput("malformed cursor"));
        }
    }
    page::parse_cursor(cursor)
}

/// 摘要只截取正文窗口，不复制整篇；找不到命中词时返回开头。
fn snippet(body: &str, needle: &str) -> String {
    // 用"小写串里的字符序"换算回原串：小写化可能改变字节长度（如 İ），
    // 直接拿字节下标切原串会在非边界处 panic。
    let lower = body.to_lowercase();
    let skip = lower
        .find(needle)
        .map(|byte| lower[..byte].chars().count())
        .unwrap_or(0);
    body.chars().skip(skip).take(MAX_SNIPPET_CHARS).collect()
}

/// 从标题/正文提取热搜词：ASCII 词（≥3）、CJK 二元组、#话题。
fn extract_terms(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut ascii = String::new();
    let mut cjk = Vec::new();
    let flush_ascii = |buf: &mut String, out: &mut Vec<String>| {
        if buf.chars().count() >= 3 && !STOPWORDS.contains(&buf.as_str()) {
            out.push(buf.clone());
        }
        buf.clear();
    };
    let flush_cjk = |buf: &mut Vec<char>, out: &mut Vec<String>| {
        if buf.len() >= 2 {
            for pair in buf.windows(2) {
                out.push(pair.iter().collect());
            }
        }
        buf.clear();
    };
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            flush_cjk(&mut cjk, &mut terms);
            ascii.push(ch.to_ascii_lowercase());
        } else if is_cjk(ch) {
            flush_ascii(&mut ascii, &mut terms);
            cjk.push(ch);
        } else {
            flush_ascii(&mut ascii, &mut terms);
            flush_cjk(&mut cjk, &mut terms);
        }
    }
    flush_ascii(&mut ascii, &mut terms);
    flush_cjk(&mut cjk, &mut terms);
    terms.sort();
    terms.dedup();
    terms
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF)
}

const STOPWORDS: [&str; 6] = ["the", "and", "for", "with", "this", "that"];

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    kind: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}

async fn search(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<SearchQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let (items, next_cursor, has_more) = platform.search_content(
        &authed.user_id,
        &q.q,
        q.kind.as_deref(),
        q.cursor.as_deref(),
        q.limit.unwrap_or(page::DEFAULT_PAGE),
    )?;
    Ok(Json(serde_json::json!({
        "items": items,
        "next_cursor": next_cursor,
        "has_more": has_more,
        "snapshot_version": wxlike_core::types::now_ms(),
    })))
}

#[derive(Deserialize)]
struct TrendQuery {
    limit: Option<usize>,
}

async fn trends(
    State(platform): State<PlatformState>,
    _authed: Authed,
    Query(q): Query<TrendQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let terms = platform.content_trends(q.limit.unwrap_or(DEFAULT_TREND_PAGE))?;
    Ok(Json(serde_json::json!({
        "terms": terms,
        "window_ms": TREND_WINDOW_MS,
        "snapshot_version": wxlike_core::types::now_ms(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_pattern_escapes_wildcards() {
        assert_eq!(like_pattern("100%_x"), "%100\\%\\_x%");
    }

    #[test]
    fn cursor_roundtrip_and_rejection() {
        assert_eq!(
            parse_cursor(None).unwrap(),
            (i64::MAX, "\u{10ffff}".to_string())
        );
        assert_eq!(parse_cursor(Some("9:abc")).unwrap(), (9, "abc".to_string()));
        assert!(parse_cursor(Some("9")).is_err());
        assert!(parse_cursor(Some("x:abc")).is_err());
        assert!(parse_cursor(Some("9:a b")).is_err());
    }

    #[test]
    fn snippet_is_bounded_and_centered() {
        let body = "前言".repeat(200) + "慢速模式" + &"后续".repeat(200);
        let s = snippet(&body, "慢速模式");
        assert!(s.chars().count() <= MAX_SNIPPET_CHARS);
        assert!(s.contains("慢速模式"));
    }

    #[test]
    fn terms_include_cjk_bigrams_and_ascii() {
        let terms = extract_terms("慢速模式 rust language");
        assert!(terms.contains(&"慢速".to_string()));
        assert!(terms.contains(&"rust".to_string()));
        assert!(!terms.contains(&"the".to_string()));
    }
}
