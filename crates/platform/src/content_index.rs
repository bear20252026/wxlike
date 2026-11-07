//! G05-05 公开内容索引与检索（`/api/search/content`、`/api/search/trends`、
//! `/api/search/scopes`）。
//!
//! 三条不变式（与 `crates/search/src/unified.rs` 的编排层同一套约定，
//! 见 integration-requests/G05/135-search-core-dep.yaml —— 那条依赖边由 G00
//! 挂上后，这里的多域归并会直接换成共用引擎，语义不变）：
//!
//! 1. **先授权、后取正文。** 候选页只查 `doc_id/kind/created_at`，逐条
//!    回权威表判定可见性，通过后才读 `title/body` 生成 snippet。索引里的
//!    死行、撤权行不会以任何形式出现在响应里。
//! 2. **写同步、删同步。** 发布内容与写索引在同一个事务；删除内容同时删行。
//!    因此不存在"索引落后导致泄露/漏数据"的异步窗口（重建索引只是重建投影）。
//! 3. **私聊永不进索引。** 索引只接收朋友圈/文章/视频号/状态；消息正文
//!    仍只在客户端本地检索，服务端没有明文建索引的入口。
//!
//! 分页正确性（#135 验收"多类别分页稳定 / 索引落后不漏数据"的那一条）：
//! 每个域各自持有一条 keyset 游标 `(created_at, doc_id)`，**游标跟着候选行
//! 推进，不跟着可见行推进**。老实现一次取 `limit + 1` 条候选、把不可见的滤掉
//! 之后就直接宣布 `has_more = false`——"最新两条是好友可见、第三条才公开"这种
//! 数据形态会让第三条及其后的公开内容永远检索不到。现在归并取满 `limit` 条
//! 或所有域翻到底才停。

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use crate::auth::Authed;
use crate::db::offload;
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

pub const KINDS: [&str; 4] = ["moment", "article", "video", "status"];

/// 单批候选下限：至少取这么多，免得归并每前进一步都打一次库。
const MIN_BATCH: i64 = 32;

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/search/content", get(search))
        .route("/api/search/trends", get(trends))
        .route("/api/search/scopes", get(scopes))
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

    /// 检索：每个域一条泳道，候选页（无正文）→ 逐条授权 → 只为通过者取正文，
    /// 归并到取满 `limit` 或所有泳道翻到底。
    ///
    /// `scopes` 为空 = 全部内置域（`KINDS`）；未知 scope 报错而不是退化成全量
    /// （退化会让"只搜文章"的调用变成对其它域的探测面）。
    pub fn search_content(
        &self,
        viewer: &str,
        q: &str,
        scopes: &[String],
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<(Vec<SearchItem>, String, bool)> {
        let needle = validate_query(q)?;
        let selected = select_scopes(scopes)?;
        let limit = limit.clamp(1, MAX_SEARCH_PAGE);
        let batch = (limit + 1).max(MIN_BATCH);
        let (before_at, before_id) = parse_cursor(cursor)?;
        let pattern = like_pattern(&needle);

        let mut lanes: Vec<Lane> = selected
            .into_iter()
            .map(|kind| Lane {
                kind,
                at: before_at,
                id: before_id.clone(),
                buf: VecDeque::new(),
                done: false,
            })
            .collect();

        let mut items: Vec<SearchItem> = Vec::new();
        let mut has_more = false;
        loop {
            if items.len() as i64 == limit {
                has_more = lanes.iter().any(|lane| !lane.buf.is_empty() || !lane.done);
                break;
            }
            for lane in lanes.iter_mut() {
                while lane.buf.is_empty() && !lane.done {
                    lane.fill(self, viewer, &pattern, &needle, batch)?;
                }
            }
            let Some(winner) = head_winner(&lanes) else {
                break;
            };
            match lanes[winner].buf.pop_front() {
                Some(head) => items.push(head),
                None => break,
            }
        }
        let next = items
            .last()
            .map(|i| format!("{}:{}", i.created_at, i.doc_id))
            .unwrap_or_default();
        Ok((items, next, has_more))
    }

    /// 某个域的一批候选：只有 id 与排序键，**没有**正文。
    fn search_candidates(
        &self,
        pattern: &str,
        kind: &str,
        before_at: i64,
        before_id: &str,
        limit: i64,
    ) -> Result<Vec<(String, i64)>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT doc_id, created_at FROM content_index
             WHERE kind = ?2 AND (title LIKE ?1 ESCAPE '\\' OR body LIKE ?1 ESCAPE '\\')
               AND (created_at < ?3 OR (created_at = ?3 AND doc_id < ?4))
             ORDER BY created_at DESC, doc_id DESC LIMIT ?5",
        )?;
        let rows = stmt.query_map(params![pattern, kind, before_at, before_id, limit], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
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

    /// 热搜：时间窗 + 去重作者 + 单人贡献上限 + 只用公开内容。
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
            doc_terms.extend(extract_terms(&title));
            doc_terms.extend(extract_terms(&body));
            for term in doc_terms {
                *contributions
                    .entry(term)
                    .or_default()
                    .entry(owner.clone())
                    .or_insert(0) += 1;
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
        // score DESC, term ASC：同分按词条字典序，结果与 map 迭代顺序无关
        terms.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.term.cmp(&b.term)));
        terms.truncate(limit);
        Ok(terms)
    }
}

/// 一个域在归并中的泳标与已授权缓冲。
struct Lane {
    kind: String,
    at: i64,
    id: String,
    buf: VecDeque<SearchItem>,
    done: bool,
}

impl Lane {
    fn fill(
        &mut self,
        platform: &Platform,
        viewer: &str,
        pattern: &str,
        needle: &str,
        batch: i64,
    ) -> Result<()> {
        let page = platform.search_candidates(pattern, &self.kind, self.at, &self.id, batch)?;
        if let Some((doc_id, created_at)) = page.last() {
            // 游标跟着**候选**走：一整批不可见的候选也必须把游标推过去，
            // 否则要么死循环重取，要么被当成"到底了"而漏数据。
            self.at = *created_at;
            self.id = doc_id.clone();
        }
        if (page.len() as i64) < batch {
            self.done = true;
        }
        for (doc_id, created_at) in page {
            if !platform.content_visible(viewer, &self.kind, &doc_id)? {
                continue;
            }
            // 索引行没了（同事务删行的极端情况）就丢弃，不返回半个对象
            let Some((title, body)) = platform.index_body(&doc_id)? else {
                continue;
            };
            self.buf.push_back(SearchItem {
                doc_id,
                kind: self.kind.clone(),
                title,
                snippet: snippet(&body, needle),
                created_at,
            });
        }
        Ok(())
    }
}

/// 归并冠军：队头排序键 `(created_at, doc_id)` 最大的泳道下标；
/// 严格大于才替换，键相等时保留更靠前的泳道（与检索内核语义一致）。
fn head_winner(lanes: &[Lane]) -> Option<usize> {
    lanes.iter().enumerate().fold(None, |best, (idx, lane)| {
        let Some(head) = lane.buf.front() else {
            return best;
        };
        match best.and_then(|b| lanes[b].buf.front()) {
            None => Some(idx),
            Some(cur)
                if (head.created_at, head.doc_id.as_str())
                    > (cur.created_at, cur.doc_id.as_str()) =>
            {
                Some(idx)
            }
            _ => best,
        }
    })
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

fn select_scopes(scopes: &[String]) -> Result<Vec<String>> {
    if scopes.is_empty() {
        return Ok(KINDS.iter().map(|k| (*k).to_string()).collect());
    }
    let mut out: Vec<String> = Vec::with_capacity(scopes.len());
    for want in scopes {
        let trimmed = want.trim().to_lowercase();
        if trimmed.is_empty() {
            continue;
        }
        if !KINDS.contains(&trimmed.as_str()) {
            return Err(PlatformError::BadInput("unknown search scope"));
        }
        if !out.iter().any(|held| held == &trimmed) {
            out.push(trimmed);
        }
    }
    Ok(out)
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

/// 从标题/正文提取热搜词：ASCII 词（≥3）、CJK 二元组。
fn extract_terms(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut ascii = String::new();
    let mut cjk = Vec::new();
    let flush_ascii = |buf: &mut String, out: &mut Vec<String>| {
        if buf.chars().count() >= 3 && !STOPWORDS.contains(&buf.as_str()) {
            out.push(std::mem::take(buf));
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

/// 英文高频词：进热搜等于把榜单让给 "the"。
const STOPWORDS: [&str; 6] = ["the", "and", "for", "with", "this", "that"];

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    /// 单域过滤（P3 起的既有参数，保留）。
    kind: Option<String>,
    /// 多域过滤，逗号分隔；与 `kind` 同时给出时取并集。
    scope: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}

/// 把 `kind` + `scope` 合成一个 scope 列表。
fn requested_scopes(kind: Option<&str>, scope: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for piece in kind
        .into_iter()
        .chain(scope.into_iter().flat_map(|s| s.split(',')))
        .flat_map(|s| s.split(','))
    {
        let trimmed = piece.trim().to_lowercase();
        if !trimmed.is_empty() && !out.contains(&trimmed) {
            out.push(trimmed);
        }
    }
    out
}

async fn search(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<SearchQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let viewer = authed.user_id.clone();
    let input = q.q.clone();
    let scopes = requested_scopes(q.kind.as_deref(), q.scope.as_deref());
    let cursor = q.cursor.clone();
    let limit = q.limit.unwrap_or(page::DEFAULT_PAGE);
    // ADR-28：检索是多轮 keyset 查询，整段搬到阻塞线程池，handler 只 await。
    let (items, next_cursor, has_more) = offload(&platform, move |p| {
        p.search_content(&viewer, &input, &scopes, cursor.as_deref(), limit)
    })
    .await?;
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
    let limit = q.limit.unwrap_or(DEFAULT_TREND_PAGE);
    let terms = offload(&platform, move |p| p.content_trends(limit)).await?;
    Ok(Json(serde_json::json!({
        "terms": terms,
        "window_ms": TREND_WINDOW_MS,
        "snapshot_version": wxlike_core::types::now_ms(),
    })))
}

/// 自省：这个统一入口现在能搜哪些域、上限是多少。客户端据此渲染 tab，不用猜，
/// 也不用把"支持哪些 scope"硬编码到三端。纯常量读取，不碰数据库。
async fn scopes(_authed: Authed) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(serde_json::json!({
        "scopes": KINDS,
        "max_page": MAX_SEARCH_PAGE,
        "max_query_chars": MAX_QUERY_CHARS,
        "trend_window_ms": TREND_WINDOW_MS,
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

    #[test]
    fn scope_selection_never_widens_on_unknown_input() {
        assert_eq!(requested_scopes(None, None).len(), 0);
        assert_eq!(
            requested_scopes(Some("Article"), Some("video, moment ,video")),
            ["article", "video", "moment"]
        );
        // 空 scope 列表 = 全部域；未知域在 select_scopes 里报错，不静默忽略
        assert_eq!(
            select_scopes(&[]).unwrap(),
            ["moment", "article", "video", "status"]
        );
        assert!(select_scopes(&["wallet".to_string()]).is_err());
        assert_eq!(
            select_scopes(&["Video".to_string(), "video".to_string()]).unwrap(),
            ["video"]
        );
    }
}
