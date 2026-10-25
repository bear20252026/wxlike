//! G05-03 收藏笔记（`/api/notes`）。
//!
//! **版本冲突是显式失败，不是静默覆盖。** 每次写入都带客户端读到的
//! `expected_version`；不匹配返回 409 并带上服务端当前版本，客户端选择
//! 合并或重写。`note_versions` 保留每个版本的全文（含自动保存），这样
//! "刷新后不丢"和"两端同时编辑"都能用真实数据验证。
//!
//! 自动保存与手工保存走同一条写路径，只是不额外产生审计噪声：两者都
//! 推进版本号并落版本行，所以自动保存后的冲突语义与手工保存完全一致。

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::extract::ApiJson;
use crate::page::{self, listing_response, parse_cursor};
use crate::validate;
use crate::{Platform, PlatformState};

pub const MAX_NOTE_TITLE_CHARS: usize = 120;
pub const MAX_NOTE_BODY_CHARS: usize = 100_000;
pub const MAX_NOTE_PAGE: i64 = 50;

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/notes", post(create))
        .route("/api/notes", get(list))
        .route("/api/notes/{id}", get(one))
        .route("/api/notes/{id}", put(update))
        .route("/api/notes/{id}/autosave", post(autosave))
        .route("/api/notes/{id}", delete(remove))
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Note {
    pub id: String,
    pub user_id: String,
    pub title: String,
    pub body: String,
    pub version: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Deserialize)]
pub struct CreateBody {
    /// 客户端生成的 id：重复提交同一 id 返回已存在的笔记，不覆盖内容。
    #[serde(default)]
    pub note_id: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
}

#[derive(Deserialize)]
pub struct UpdateBody {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub expected_version: i64,
}

#[derive(Deserialize)]
struct ListQuery {
    cursor: Option<String>,
    limit: Option<i64>,
}

impl Platform {
    pub fn create_note(
        &self,
        user: &str,
        note_id: Option<&str>,
        title: &str,
        body: &str,
    ) -> Result<Note> {
        let (title, body) = check_note(title, body)?;
        let note_id = match note_id {
            Some(id) => validate::id(id)?.to_string(),
            None => crate::auth::random_hex(8),
        };
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT user_id FROM notes WHERE id = ?1",
                params![note_id],
                |r| r.get(0),
            )
            .optional()?;
        if existing.is_some() {
            let note = note_row(&tx, user, &note_id)?.ok_or(PlatformError::NotFound)?;
            tx.commit()?;
            return Ok(note);
        }
        tx.execute(
            "INSERT INTO notes(id, user_id, title, body, version, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, 1, ?5, ?5)",
            params![note_id, user, title, body, now],
        )?;
        tx.execute(
            "INSERT INTO note_versions(note_id, version, title, body, saved_at)
             VALUES(?1, 1, ?2, ?3, ?4)",
            params![note_id, title, body, now],
        )?;
        let note = note_row(&tx, user, &note_id)?.ok_or(PlatformError::NotFound)?;
        tx.commit()?;
        Ok(note)
    }

    /// `expected_version` 必须等于服务端当前版本；不等就 409，绝不覆盖。
    pub fn update_note(
        &self,
        user: &str,
        id: &str,
        title: &str,
        body: &str,
        expected_version: i64,
    ) -> Result<Note> {
        let id = validate::id(id)?;
        let (title, body) = check_note(title, body)?;
        if expected_version < 1 {
            return Err(PlatformError::BadInput("expected_version must be >= 1"));
        }
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT version FROM notes WHERE id = ?1 AND user_id = ?2",
                params![id, user],
                |r| r.get(0),
            )
            .optional()?;
        let Some(current) = current else {
            return Err(PlatformError::NotFound);
        };
        if current != expected_version {
            return Err(PlatformError::Conflict(format!(
                "version mismatch: current {current}"
            )));
        }
        let next = current + 1;
        tx.execute(
            "UPDATE notes SET title = ?1, body = ?2, version = ?3, updated_at = ?4
             WHERE id = ?5 AND user_id = ?6",
            params![title, body, next, now, id, user],
        )?;
        tx.execute(
            "INSERT INTO note_versions(note_id, version, title, body, saved_at)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![id, next, title, body, now],
        )?;
        let note = note_row(&tx, user, id)?.ok_or(PlatformError::NotFound)?;
        tx.commit()?;
        Ok(note)
    }

    pub fn note(&self, user: &str, id: &str) -> Result<Note> {
        let id = validate::id(id)?;
        let c = self.db().lock();
        note_row(&c, user, id)?.ok_or(PlatformError::NotFound)
    }

    /// `updated_at DESC` 分页；`cursor` = `"{updated_at}:{id}"`，与检索游标同形。
    pub fn notes(
        &self,
        user: &str,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<(Vec<Note>, String, bool)> {
        let limit = limit.clamp(1, MAX_NOTE_PAGE);
        let (before_at, before_id) = parse_cursor(cursor)?;
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT id, user_id, title, body, version, created_at, updated_at
             FROM notes WHERE user_id = ?1
               AND (updated_at < ?2 OR (updated_at = ?2 AND id < ?3))
             ORDER BY updated_at DESC, id DESC LIMIT ?4",
        )?;
        let it = stmt.query_map(params![user, before_at, before_id, limit + 1], note_map)?;
        let mut rows = it.collect::<std::result::Result<Vec<_>, _>>()?;
        let has_more = rows.len() as i64 > limit;
        if has_more {
            rows.pop();
        }
        let next = rows
            .last()
            .map(|n| format!("{}:{}", n.updated_at, n.id))
            .unwrap_or_default();
        Ok((rows, next, has_more))
    }

    pub fn delete_note(&self, user: &str, id: &str) -> Result<()> {
        let id = validate::id(id)?;
        let changed = self.db().lock().execute(
            "DELETE FROM notes WHERE id = ?1 AND user_id = ?2",
            params![id, user],
        )?;
        page::ensure_updated(changed)
    }
}

fn note_row(c: &rusqlite::Connection, user: &str, id: &str) -> Result<Option<Note>> {
    Ok(c.query_row(
        "SELECT id, user_id, title, body, version, created_at, updated_at
         FROM notes WHERE id = ?1 AND user_id = ?2",
        params![id, user],
        note_map,
    )
    .optional()?)
}

fn note_map(r: &rusqlite::Row<'_>) -> rusqlite::Result<Note> {
    Ok(Note {
        id: r.get(0)?,
        user_id: r.get(1)?,
        title: r.get(2)?,
        body: r.get(3)?,
        version: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
    })
}

fn check_note<'a>(title: &'a str, body: &'a str) -> Result<(&'a str, &'a str)> {
    if title.chars().count() > MAX_NOTE_TITLE_CHARS {
        return Err(PlatformError::BadInput("note title too long"));
    }
    if body.chars().count() > MAX_NOTE_BODY_CHARS {
        return Err(PlatformError::BadInput("note body too long"));
    }
    for field in [title, body] {
        if field.contains(|c: char| c.is_control() && !matches!(c, '\n' | '\t')) {
            return Err(PlatformError::BadInput("note contains control characters"));
        }
    }
    if title.trim().is_empty() && body.trim().is_empty() {
        return Err(PlatformError::BadInput("note is empty"));
    }
    Ok((title, body))
}

async fn create(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<CreateBody>,
) -> std::result::Result<(StatusCode, Json<Note>), ApiError> {
    let note = platform.create_note(
        &authed.user_id,
        body.note_id.as_deref(),
        &body.title,
        &body.body,
    )?;
    Ok((StatusCode::CREATED, Json(note)))
}

async fn list(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<ListQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let (items, next, has_more) = platform.notes(
        &authed.user_id,
        q.cursor.as_deref(),
        q.limit.unwrap_or(page::DEFAULT_PAGE),
    )?;
    Ok(listing_response(items, next, has_more))
}

async fn one(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<Note>, ApiError> {
    Ok(Json(platform.note(&authed.user_id, &id)?))
}

async fn update(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<UpdateBody>,
) -> std::result::Result<Json<Note>, ApiError> {
    Ok(Json(platform.update_note(
        &authed.user_id,
        &id,
        &body.title,
        &body.body,
        body.expected_version,
    )?))
}

async fn autosave(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<UpdateBody>,
) -> std::result::Result<Json<Note>, ApiError> {
    Ok(Json(platform.update_note(
        &authed.user_id,
        &id,
        &body.title,
        &body.body,
        body.expected_version,
    )?))
}

async fn remove(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.delete_note(&authed.user_id, &id)?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_requires_datetime_and_id() {
        assert_eq!(
            parse_cursor(None).unwrap(),
            (i64::MAX, "\u{10ffff}".to_string())
        );
        assert_eq!(parse_cursor(Some("7:abc")).unwrap(), (7, "abc".to_string()));
        assert!(parse_cursor(Some("7")).is_err());
        assert!(parse_cursor(Some("x:abc")).is_err());
        assert!(parse_cursor(Some("7:a/b")).is_err());
    }

    #[test]
    fn note_input_is_bounded() {
        assert!(check_note("t", "b").is_ok());
        assert!(check_note("", "  ").is_err());
        assert!(check_note(&"x".repeat(MAX_NOTE_TITLE_CHARS + 1), "b").is_err());
        assert!(check_note("t", &"x".repeat(MAX_NOTE_BODY_CHARS + 1)).is_err());
        assert!(check_note("t", "a\u{0}b").is_err());
        assert!(check_note("t", "a\nb").is_ok());
    }
}
