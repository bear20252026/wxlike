//! 收藏（`/api/favorites`）：消息快照 + 内容对象引用。
//!
//! 两种来源，两种撤权策略，刻意不同：
//!
//! * **消息**存快照（P1 的既有契约）：消息被撤回/删除后，用户自己的副本
//!   仍然保留。收藏消息的语义就是"我要留一份"。
//! * **内容对象**（朋友圈/文章/视频号/状态）存受限引用 + 服务端生成的小
//!   快照。原对象删除、撤权或到期时，[`revoke_source`] 把相关收藏标记为
//!   `revoked`；读取面随后清空 `text/media_id/snapshot_label`，只留
//!   `revoked = true` 和来源类型。**撤权后不返回原文。**
//!
//! 排序：`sort_order` 由用户拖拽写入（[`Platform::reorder_favorites`]），
//! 存在服务端，因此跨端一致；同序时按创建时间倒序。

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::extract::ApiJson;
use crate::page;
use crate::validate;
use crate::{Platform, PlatformState};

/// Page cap; callers asking for more get this many.
pub const MAX_FAVORITE_PAGE: usize = 100;

/// A snapshot text longer than this is refused (same order as moments).
const MAX_SNAPSHOT_TEXT: usize = 5_000;
const MAX_SOURCE_MSG_ID: usize = 64;
const MAX_SNAPSHOT_FIELD: usize = 128;
/// `read_row` 里 G05 增量列的下标（与 SELECT 列序一致）。
const COL_SOURCE_KIND: usize = 10;
const COL_SOURCE_ID: usize = 11;
const COL_SNAPSHOT_LABEL: usize = 12;
const COL_SORT_ORDER: usize = 13;
const COL_REVOKED: usize = 14;
/// 列表标签（用户可改）与对象快照标签的上限。
const MAX_LABEL_CHARS: usize = 200;
const MAX_OBJECT_CAPTURE_CHARS: usize = 500;
const OBJECT_KINDS: [&str; 4] = ["moment", "article", "video", "status"];

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/favorites", post(create))
        .route("/api/favorites", get(list))
        .route("/api/favorites/objects", post(create_object))
        .route("/api/favorites/search", get(search))
        .route("/api/favorites/reorder", post(reorder))
        .route("/api/favorites/{id}", get(one))
        .route("/api/favorites/{id}", put(update))
        .route("/api/favorites/{id}", delete(remove))
}

/// The message snapshot a client sends when favoriting.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FavoriteInput {
    pub source_conv: String,
    pub source_msg_id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub media_id: String,
    #[serde(default)]
    pub sender: String,
    #[serde(default)]
    pub sent_at_ms: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Favorite {
    pub id: String,
    pub user_id: String,
    pub source_conv: String,
    pub source_msg_id: String,
    pub kind: String,
    pub text: String,
    pub media_id: String,
    pub sender: String,
    pub sent_at_ms: i64,
    pub created_at: i64,
    /// `message` 或对象类型（moment/article/video/status）。
    pub source_kind: String,
    /// 对象收藏指向的源 id；消息收藏为空。
    pub source_id: String,
    /// 用户可编辑的标签（收藏夹展示名）。
    pub snapshot_label: String,
    pub sort_order: i64,
    /// 源对象已撤权/删除/到期：读取面不再返回原文。
    pub revoked: bool,
}

/// 收藏一个内容对象（限制引用 + 服务端快照）。
#[derive(Deserialize)]
pub struct ObjectFavoriteBody {
    pub kind: String,
    pub id: String,
}

#[derive(Deserialize)]
pub struct FavoriteUpdateBody {
    #[serde(default)]
    pub snapshot_label: Option<String>,
    #[serde(default)]
    pub sort_order: Option<i64>,
}

#[derive(Deserialize)]
pub struct ReorderBody {
    pub ids: Vec<String>,
}

impl Platform {
    pub fn add_favorite(&self, user_id: &str, input: &FavoriteInput) -> Result<Favorite> {
        let source_conv = validate::conv(&input.source_conv)?;
        if input.source_msg_id.len() > MAX_SOURCE_MSG_ID {
            return Err(PlatformError::BadInput("source_msg_id too long"));
        }
        if input.text.len() > MAX_SNAPSHOT_TEXT {
            return Err(PlatformError::BadInput("snapshot text too long"));
        }
        for field in [&input.kind, &input.media_id, &input.sender] {
            if field.len() > MAX_SNAPSHOT_FIELD {
                return Err(PlatformError::BadInput("snapshot field too long"));
            }
        }
        let c = self.db().lock();
        // 幂等语义是"同一载荷重复提交"：同会话同消息、同快照内容返回已有行；
        // 同消息不同快照（如重发后转发语不同）仍各成一行（P1 契约如此）。
        if let Some(existing) = c
            .query_row(
                "SELECT id FROM favorites
                 WHERE user_id = ?1 AND source_kind = 'message' AND source_msg_id = ?2
                   AND source_conv = ?3 AND kind = ?4 AND text = ?5 AND media_id = ?6
                 ORDER BY created_at LIMIT 1",
                params![
                    user_id,
                    input.source_msg_id,
                    source_conv,
                    if input.kind.is_empty() {
                        "text"
                    } else {
                        &input.kind
                    },
                    input.text,
                    input.media_id
                ],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return favorite_row(&c, user_id, &existing)?.ok_or(PlatformError::NotFound);
        }
        let id = crate::auth::random_hex(8);
        let now = wxlike_core::types::now_ms() as i64;
        c.execute(
            "INSERT INTO favorites(id, user_id, source_conv, source_msg_id, kind, text,
                                   media_id, sender, sent_at_ms, created_at,
                                   source_kind, source_id, snapshot_label, sort_order, revoked)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'message', '', ?5, 0, 0)",
            params![
                id,
                user_id,
                source_conv,
                input.source_msg_id,
                if input.kind.is_empty() {
                    "text"
                } else {
                    &input.kind
                },
                input.text,
                input.media_id,
                input.sender,
                input.sent_at_ms,
                now
            ],
        )?;
        favorite_row(&c, user_id, &id)?.ok_or(PlatformError::NotFound)
    }

    /// 收藏一个内容对象。授权在写入前完成；快照由服务端从权威表生成
    /// （不信任客户端提交的正文）。重复收藏同一对象返回已有行。
    pub fn add_object_favorite(&self, user: &str, kind: &str, id: &str) -> Result<Favorite> {
        if !OBJECT_KINDS.contains(&kind) {
            return Err(PlatformError::BadInput("unknown favorite kind"));
        }
        let id = validate::id(id)?.to_string();
        if self
            .db()
            .lock()
            .query_row(
                "SELECT 1 FROM favorites WHERE user_id = ?1 AND source_kind = ?2 AND source_id = ?3",
                params![user, kind, id],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return self.object_favorite(user, kind, &id);
        }
        if !self.content_visible(user, kind, &id)? {
            return Err(PlatformError::NotFound);
        }
        let snapshot = self.snapshot_of(kind, &id)?;
        let now = wxlike_core::types::now_ms() as i64;
        let fav_id = crate::auth::random_hex(8);
        let c = self.db().lock();
        c.execute(
            "INSERT INTO favorites(id, user_id, source_conv, source_msg_id, kind, text,
                                   media_id, sender, sent_at_ms, created_at,
                                   source_kind, source_id, snapshot_label, sort_order, revoked)
             VALUES(?1, ?2, '', '', ?3, ?4, '', '', 0, ?5, ?3, ?6, ?7, 0, 0)
             ON CONFLICT DO NOTHING",
            params![fav_id, user, kind, snapshot.0, now, id, snapshot.1],
        )?;
        drop(c);
        self.object_favorite(user, kind, &id)
    }

    fn object_favorite(&self, user: &str, kind: &str, id: &str) -> Result<Favorite> {
        let c = self.db().lock();
        let row = c
            .query_row(
                "SELECT id FROM favorites
                 WHERE user_id = ?1 AND source_kind = ?2 AND source_id = ?3",
                params![user, kind, id],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        let Some(id) = row else {
            return Err(PlatformError::NotFound);
        };
        favorite_row(&c, user, &id)?.ok_or(PlatformError::NotFound)
    }

    /// 服务端快照：`(正文摘要, 标签)`。只读权威表的一小段，不复制全文。
    fn snapshot_of(&self, kind: &str, id: &str) -> Result<(String, String)> {
        let c = self.db().lock();
        match kind {
            "moment" => c
                .query_row(
                    "SELECT text FROM moments WHERE id = ?1 AND deleted = 0",
                    params![id],
                    |r| {
                        let text: String = r.get(0)?;
                        Ok((capture(&text), String::new()))
                    },
                )
                .optional()?
                .ok_or(PlatformError::NotFound),
            "article" => c
                .query_row(
                    "SELECT title, body_text FROM articles WHERE id = ?1 AND state = 'published'",
                    params![id],
                    |r| {
                        let title: String = r.get(0)?;
                        let body: String = r.get(1)?;
                        Ok((capture(&body), title))
                    },
                )
                .optional()?
                .ok_or(PlatformError::NotFound),
            "video" => c
                .query_row(
                    "SELECT caption FROM video_posts WHERE id = ?1 AND state = 'published'",
                    params![id],
                    |r| {
                        let caption: String = r.get(0)?;
                        Ok((capture(&caption), String::new()))
                    },
                )
                .optional()?
                .ok_or(PlatformError::NotFound),
            "status" => c
                .query_row(
                    "SELECT text FROM statuses WHERE id = ?1 AND cleared_at IS NULL",
                    params![id],
                    |r| {
                        let text: String = r.get(0)?;
                        Ok((capture(&text), String::new()))
                    },
                )
                .optional()?
                .ok_or(PlatformError::NotFound),
            _ => Err(PlatformError::BadInput("unknown favorite kind")),
        }
    }

    /// Legacy 分页（按 created_at）。新客户端用 [`Platform::favorites_sorted`]。
    pub fn list_favorites(
        &self,
        user_id: &str,
        before_created: Option<i64>,
        limit: usize,
    ) -> Result<Vec<Favorite>> {
        let limit = limit.clamp(1, MAX_FAVORITE_PAGE) as i64;
        let before = before_created.unwrap_or(i64::MAX);
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT id, user_id, source_conv, source_msg_id, kind, text, media_id,
                    sender, sent_at_ms, created_at, source_kind, source_id,
                    snapshot_label, sort_order, revoked
             FROM favorites WHERE user_id = ?1 AND created_at < ?2
             ORDER BY created_at DESC, id DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![user_id, before, limit], read_row)?;
        Ok(rows
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(scrub)
            .collect())
    }

    /// 排序列表：`sort_order ASC, created_at DESC`，游标 `"{sort}:{created}:{id}"`。
    pub fn favorites_sorted(
        &self,
        user: &str,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<(Vec<Favorite>, String, bool)> {
        let limit = limit.clamp(1, MAX_FAVORITE_PAGE as i64);
        let (sort, created, id) = parse_cursor(cursor)?;
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT id, user_id, source_conv, source_msg_id, kind, text, media_id,
                    sender, sent_at_ms, created_at, source_kind, source_id,
                    snapshot_label, sort_order, revoked
             FROM favorites
             WHERE user_id = ?1
               AND (sort_order > ?2
                    OR (sort_order = ?2 AND (created_at < ?3
                        OR (created_at = ?3 AND id < ?4))))
             ORDER BY sort_order ASC, created_at DESC, id DESC LIMIT ?5",
        )?;
        let it = stmt.query_map(params![user, sort, created, id, limit + 1], read_row)?;
        let mut rows = it.collect::<std::result::Result<Vec<_>, _>>()?;
        let has_more = rows.len() as i64 > limit;
        if has_more {
            rows.pop();
        }
        let next = rows
            .last()
            .map(|f| format!("{}:{}:{}", f.sort_order, f.created_at, f.id))
            .unwrap_or_default();
        Ok((rows.into_iter().map(scrub).collect(), next, has_more))
    }

    pub fn favorite(&self, user_id: &str, id: &str) -> Result<Favorite> {
        let id = validate::id(id)?;
        let c = self.db().lock();
        let row = favorite_row(&c, user_id, id)?.ok_or(PlatformError::NotFound)?;
        Ok(scrub(row))
    }

    /// 收藏夹内搜索：只搜自己的行，撤权行不参与。
    pub fn search_favorites(&self, user: &str, q: &str, limit: i64) -> Result<Vec<Favorite>> {
        let q = q.trim();
        if q.is_empty() {
            return Err(PlatformError::BadInput("query must not be empty"));
        }
        if q.chars().count() > 100 || q.chars().any(char::is_control) {
            return Err(PlatformError::BadInput("query malformed"));
        }
        let limit = limit.clamp(1, MAX_FAVORITE_PAGE as i64);
        let pattern = format!("%{}%", page::escape_like(q));
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT id, user_id, source_conv, source_msg_id, kind, text, media_id,
                    sender, sent_at_ms, created_at, source_kind, source_id,
                    snapshot_label, sort_order, revoked
             FROM favorites
             WHERE user_id = ?1 AND revoked = 0
               AND (text LIKE ?2 ESCAPE '\\' OR snapshot_label LIKE ?2 ESCAPE '\\'
                    OR kind LIKE ?2 ESCAPE '\\')
             ORDER BY sort_order ASC, created_at DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![user, pattern, limit], read_row)?;
        Ok(rows
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(scrub)
            .collect())
    }

    pub fn update_favorite(
        &self,
        user: &str,
        id: &str,
        label: Option<&str>,
        sort_order: Option<i64>,
    ) -> Result<Favorite> {
        let id = validate::id(id)?;
        if label.is_none() && sort_order.is_none() {
            return Err(PlatformError::BadInput("nothing to update"));
        }
        if let Some(label) = label {
            if label.chars().count() > MAX_LABEL_CHARS || label.chars().any(char::is_control) {
                return Err(PlatformError::BadInput("label malformed"));
            }
        }
        if let Some(order) = sort_order {
            if !(0..=1_000_000).contains(&order) {
                return Err(PlatformError::BadInput("sort_order out of range"));
            }
        }
        let c = self.db().lock();
        let changed = c.execute(
            "UPDATE favorites SET
               snapshot_label = COALESCE(?1, snapshot_label),
               sort_order = COALESCE(?2, sort_order)
             WHERE id = ?3 AND user_id = ?4",
            params![label, sort_order, id, user],
        )?;
        if changed == 0 {
            return Err(PlatformError::NotFound);
        }
        let row = favorite_row(&c, user, id)?.ok_or(PlatformError::NotFound)?;
        Ok(scrub(row))
    }

    /// 拖拽排序：把 `ids` 依次写成 0..n。任一 id 不属于本人则整单拒绝。
    pub fn reorder_favorites(&self, user: &str, ids: &[String]) -> Result<usize> {
        if ids.len() > MAX_FAVORITE_PAGE {
            return Err(PlatformError::BadInput("too many favorites"));
        }
        let mut cleaned = Vec::with_capacity(ids.len());
        for id in ids {
            cleaned.push(validate::id(id)?.to_string());
        }
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        for id in &cleaned {
            let owned: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM favorites WHERE id = ?1 AND user_id = ?2",
                    params![id, user],
                    |r| r.get(0),
                )
                .optional()?;
            if owned.is_none() {
                return Err(PlatformError::NotFound);
            }
        }
        for (i, id) in cleaned.iter().enumerate() {
            tx.execute(
                "UPDATE favorites SET sort_order = ?1 WHERE id = ?2 AND user_id = ?3",
                params![i as i64, id, user],
            )?;
        }
        tx.commit()?;
        Ok(cleaned.len())
    }

    pub fn remove_favorite(&self, user_id: &str, id: &str) -> Result<()> {
        let id = validate::id(id)?;
        let changed = self.db().lock().execute(
            "DELETE FROM favorites WHERE id = ?1 AND user_id = ?2",
            params![id, user_id],
        )?;
        page::ensure_updated(changed)
    }
}

/// 源对象删除/撤权/到期时调用（与删除同事务）。只标记，不删用户的行：
/// 用户仍能看到"这条收藏已不可用"，且排序位置保持不变。
pub(crate) fn revoke_source(conn: &Connection, source_kind: &str, source_id: &str) -> Result<()> {
    if source_kind == "message" || source_id.is_empty() {
        return Ok(());
    }
    conn.execute(
        "UPDATE favorites SET revoked = 1, text = '', media_id = '', snapshot_label = ''
         WHERE source_kind = ?1 AND source_id = ?2",
        params![source_kind, source_id],
    )?;
    Ok(())
}

fn favorite_row(c: &Connection, user: &str, id: &str) -> Result<Option<Favorite>> {
    Ok(c.query_row(
        "SELECT id, user_id, source_conv, source_msg_id, kind, text, media_id,
                sender, sent_at_ms, created_at, source_kind, source_id,
                snapshot_label, sort_order, revoked
         FROM favorites WHERE id = ?1 AND user_id = ?2",
        params![id, user],
        read_row,
    )
    .optional()?)
}

fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Favorite> {
    Ok(Favorite {
        id: r.get(0)?,
        user_id: r.get(1)?,
        source_conv: r.get(2)?,
        source_msg_id: r.get(3)?,
        kind: r.get(4)?,
        text: r.get(5)?,
        media_id: r.get(6)?,
        sender: r.get(7)?,
        sent_at_ms: r.get(8)?,
        created_at: r.get(9)?,
        source_kind: r.get(COL_SOURCE_KIND)?,
        source_id: r.get(COL_SOURCE_ID)?,
        snapshot_label: r.get(COL_SNAPSHOT_LABEL)?,
        sort_order: r.get(COL_SORT_ORDER)?,
        revoked: r.get::<_, i64>(COL_REVOKED)? != 0,
    })
}

/// 撤权后的读取面：内容字段一律清空（数据库里也已清），防止旧客户端读到。
fn scrub(mut f: Favorite) -> Favorite {
    if f.revoked {
        f.text.clear();
        f.media_id.clear();
        f.snapshot_label.clear();
    }
    f
}

fn capture(text: &str) -> String {
    text.chars().take(MAX_OBJECT_CAPTURE_CHARS).collect()
}

fn parse_cursor(cursor: Option<&str>) -> Result<(i64, i64, String)> {
    match cursor {
        None | Some("") => Ok((0, i64::MAX, "\u{10ffff}".to_string())),
        Some(c) => {
            let mut parts = c.split(':');
            let (Some(sort), Some(created), Some(id), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return Err(PlatformError::BadInput("malformed cursor"));
            };
            let sort: i64 = sort
                .parse()
                .map_err(|_| PlatformError::BadInput("malformed cursor"))?;
            let created: i64 = created
                .parse()
                .map_err(|_| PlatformError::BadInput("malformed cursor"))?;
            let id = validate::id(id)?;
            Ok((sort, created, id.to_string()))
        }
    }
}

async fn create(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(input): ApiJson<FavoriteInput>,
) -> std::result::Result<(StatusCode, Json<Favorite>), ApiError> {
    let favorite = platform.add_favorite(&authed.user_id, &input)?;
    Ok((StatusCode::CREATED, Json(favorite)))
}

async fn create_object(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<ObjectFavoriteBody>,
) -> std::result::Result<(StatusCode, Json<Favorite>), ApiError> {
    let favorite = platform.add_object_favorite(&authed.user_id, &body.kind, &body.id)?;
    Ok((StatusCode::CREATED, Json(favorite)))
}

#[derive(Deserialize, Default)]
struct ListQuery {
    limit: Option<i64>,
    before_created: Option<i64>,
    cursor: Option<String>,
}

async fn list(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<ListQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    if q.cursor.is_none() && q.before_created.is_some() {
        // 兼容旧客户端：created_at 水位线，仍按时间倒序。
        let rows = platform.list_favorites(
            &authed.user_id,
            q.before_created,
            q.limit.unwrap_or(page::DEFAULT_PAGE) as usize,
        )?;
        let next = rows.last().map(|f| f.created_at);
        return Ok(Json(
            serde_json::json!({ "favorites": rows, "next_before_created": next }),
        ));
    }
    let (rows, next, has_more) = platform.favorites_sorted(
        &authed.user_id,
        q.cursor.as_deref(),
        q.limit.unwrap_or(page::DEFAULT_PAGE),
    )?;
    Ok(Json(serde_json::json!({
        "favorites": rows,
        "next_cursor": next,
        "has_more": has_more,
    })))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    limit: Option<i64>,
}

async fn search(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<SearchQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let items =
        platform.search_favorites(&authed.user_id, &q.q, q.limit.unwrap_or(page::DEFAULT_PAGE))?;
    Ok(Json(serde_json::json!({ "items": items })))
}

async fn update(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<FavoriteUpdateBody>,
) -> std::result::Result<Json<Favorite>, ApiError> {
    Ok(Json(platform.update_favorite(
        &authed.user_id,
        &id,
        body.snapshot_label.as_deref(),
        body.sort_order,
    )?))
}

async fn reorder(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<ReorderBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let updated = platform.reorder_favorites(&authed.user_id, &body.ids)?;
    Ok(Json(serde_json::json!({ "updated": updated })))
}

async fn one(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<Favorite>, ApiError> {
    Ok(Json(platform.favorite(&authed.user_id, &id)?))
}

async fn remove(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.remove_favorite(&authed.user_id, &id)?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_shape_is_strict() {
        assert_eq!(
            parse_cursor(None).unwrap(),
            (0, i64::MAX, "\u{10ffff}".to_string())
        );
        assert_eq!(
            parse_cursor(Some("2:10:abc")).unwrap(),
            (2, 10, "abc".to_string())
        );
        assert!(parse_cursor(Some("2:10")).is_err());
        assert!(parse_cursor(Some("2:10:abc:9")).is_err());
        assert!(parse_cursor(Some("x:10:abc")).is_err());
    }

    #[test]
    fn capture_and_like_escape_are_bounded() {
        assert_eq!(
            capture(&"x".repeat(10_000)).chars().count(),
            MAX_OBJECT_CAPTURE_CHARS
        );
        assert_eq!(page::escape_like("a%b_c"), "a\\%b\\_c");
    }

    #[test]
    fn revoked_rows_do_not_leak_content() {
        let f = Favorite {
            id: "x".into(),
            user_id: "u".into(),
            source_conv: String::new(),
            source_msg_id: String::new(),
            kind: "moment".into(),
            text: "secret".into(),
            media_id: "m".into(),
            sender: String::new(),
            sent_at_ms: 0,
            created_at: 1,
            source_kind: "moment".into(),
            source_id: "p".into(),
            snapshot_label: "label".into(),
            sort_order: 0,
            revoked: true,
        };
        let clean = scrub(f);
        assert!(clean.text.is_empty());
        assert!(clean.media_id.is_empty());
        assert!(clean.snapshot_label.is_empty());
        assert!(clean.revoked);
    }
}
