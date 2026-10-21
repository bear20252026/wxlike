//! P1 message favorites (收藏): a per-user list of message snapshots.
//!
//! A favorite stores a **snapshot**, not a reference: messages can be
//! recalled or deleted, and the user's own copy must survive that. The
//! snapshot is deliberately structured columns rather than a JSON blob so the
//! list endpoint stays queryable and paging is a plain index walk.
//!
//! Authorization: everything is scoped to the token's user; a favorite of
//! another user is a 404 (not 403 — the row "does not exist" for you, the
//! same anti-probing rule the moments module uses).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::extract::ApiJson;
use crate::validate;
use crate::{Platform, PlatformState};

/// Page cap; callers asking for more get this many.
pub const MAX_FAVORITE_PAGE: usize = 100;
/// A snapshot text longer than this is refused (same order as moments).
const MAX_SNAPSHOT_TEXT: usize = 5_000;

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/favorites", post(create))
        .route("/api/favorites", get(list))
        .route("/api/favorites/{id}", get(one))
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
}

impl Platform {
    pub fn add_favorite(&self, user_id: &str, input: &FavoriteInput) -> Result<Favorite> {
        let source_conv = validate::conv(&input.source_conv)?;
        if input.source_msg_id.len() > 64 {
            return Err(PlatformError::BadInput("source_msg_id too long"));
        }
        if input.text.len() > MAX_SNAPSHOT_TEXT {
            return Err(PlatformError::BadInput("snapshot text too long"));
        }
        for field in [&input.kind, &input.media_id, &input.sender] {
            if field.len() > 128 {
                return Err(PlatformError::BadInput("snapshot field too long"));
            }
        }
        let id = crate::auth::random_hex(8);
        let now = wxlike_core::types::now_ms() as i64;
        let favorite = Favorite {
            id: id.clone(),
            user_id: user_id.to_string(),
            source_conv: source_conv.to_string(),
            source_msg_id: input.source_msg_id.clone(),
            kind: if input.kind.is_empty() {
                "text".to_string()
            } else {
                input.kind.clone()
            },
            text: input.text.clone(),
            media_id: input.media_id.clone(),
            sender: input.sender.clone(),
            sent_at_ms: input.sent_at_ms,
            created_at: now,
        };
        let c = self.db().lock();
        c.execute(
            "INSERT INTO favorites(id, user_id, source_conv, source_msg_id, kind, text,
                                   media_id, sender, sent_at_ms, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                favorite.id,
                favorite.user_id,
                favorite.source_conv,
                favorite.source_msg_id,
                favorite.kind,
                favorite.text,
                favorite.media_id,
                favorite.sender,
                favorite.sent_at_ms,
                favorite.created_at
            ],
        )?;
        Ok(favorite)
    }

    /// Newest first, `before_created` (exclusive) as the page cursor.
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
                    sender, sent_at_ms, created_at
             FROM favorites WHERE user_id = ?1 AND created_at < ?2
             ORDER BY created_at DESC, id DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![user_id, before, limit], row_to_favorite)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn favorite(&self, user_id: &str, id: &str) -> Result<Favorite> {
        let id = validate::id(id)?;
        let c = self.db().lock();
        let row = c
            .query_row(
                "SELECT id, user_id, source_conv, source_msg_id, kind, text, media_id,
                        sender, sent_at_ms, created_at
                 FROM favorites WHERE id = ?1 AND user_id = ?2",
                params![id, user_id],
                row_to_favorite,
            )
            .optional()?;
        row.ok_or(PlatformError::NotFound)
    }

    pub fn remove_favorite(&self, user_id: &str, id: &str) -> Result<()> {
        let id = validate::id(id)?;
        let changed = self.db().lock().execute(
            "DELETE FROM favorites WHERE id = ?1 AND user_id = ?2",
            params![id, user_id],
        )?;
        if changed == 0 {
            return Err(PlatformError::NotFound);
        }
        Ok(())
    }
}

fn row_to_favorite(r: &rusqlite::Row<'_>) -> rusqlite::Result<Favorite> {
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
    })
}

async fn create(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(input): ApiJson<FavoriteInput>,
) -> std::result::Result<(StatusCode, Json<Favorite>), ApiError> {
    let favorite = platform.add_favorite(&authed.user_id, &input)?;
    Ok((StatusCode::CREATED, Json(favorite)))
}

#[derive(Deserialize, Default)]
struct ListQuery {
    limit: Option<usize>,
    before_created: Option<i64>,
}

async fn list(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<ListQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let rows = platform.list_favorites(&authed.user_id, q.before_created, q.limit.unwrap_or(20))?;
    let next = rows.last().map(|f| f.created_at);
    Ok(Json(
        serde_json::json!({ "favorites": rows, "next_before_created": next }),
    ))
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
