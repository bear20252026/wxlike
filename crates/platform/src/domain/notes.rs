//! G05-03 收藏笔记（`/api/notes`）。
//!
//! **版本冲突是显式失败，不是静默覆盖。** 每次写入都带客户端读到的
//! `expected_version`；不匹配返回 409 并带上服务端当前版本，客户端选择
//! 合并或重写。`note_versions` 保留每个版本的全文（含自动保存），这样
//! "刷新后不丢"和"两端同时编辑"都能用真实数据验证。
//!
//! 自动保存与手工保存走同一条写路径，只是不额外产生审计噪声：两者都
//! 推进版本号并落版本行，所以自动保存后的冲突语义与手工保存完全一致。
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::notes`]。

use crate::error::{PlatformError, Result};
use crate::page::parse_cursor;
use crate::validate;
use crate::Platform;
use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};
use wxlike_core::tx::ConnectionTx as _;

use super::repo::notes::{note_map, note_row};

pub const MAX_NOTE_TITLE_CHARS: usize = 120;
pub const MAX_NOTE_BODY_CHARS: usize = 100_000;
pub const MAX_NOTE_PAGE: i64 = 50;

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
        c.with_tx(|tx| -> Result<Note> {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT user_id FROM notes WHERE id = ?1",
                    params![note_id],
                    |r| r.get(0),
                )
                .optional()?;
            if existing.is_some() {
                // 已存在：返回现有行，不覆盖（幂等）。空事务直接提交。
                let note = note_row(tx, user, &note_id)?.ok_or(PlatformError::NotFound)?;
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
            note_row(tx, user, &note_id)?.ok_or(PlatformError::NotFound)
        })
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
        c.with_tx(|tx| -> Result<Note> {
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
            note_row(tx, user, id)?.ok_or(PlatformError::NotFound)
        })
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
        crate::db::ensure_updated(changed)
    }
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
