//! WP-07 moments: publish, timeline, likes, comments, and visibility.
//!
//! **Visibility is evaluated at read time**, not snapshotted at publish time.
//! A `Friends` post is visible to whoever is a mutual friend *now*, which means
//! removing a friend immediately removes their friends-only history from your
//! feed — the behaviour WP-07 A1 asks to see. The alternative (freezing an ACL
//! per post at publish) would need the audience materialised at write time and
//! would keep showing history after an unfriend; we deliberately chose read-time
//! evaluation and this paragraph is the disclosure the task book asks for.
//!
//! The feed itself is restricted to *self + friends* (contract), so a stranger's
//! `Public` post never appears in your timeline; `Public` only widens who may
//! open a specific moment by id.
//!
//! Paging: the feed is time-descending as required, so its cursor is a
//! **watermark**: pass `since` = the `next_cursor` from the previous page to get
//! strictly older entries. This is the opposite direction from the ascending
//! ledger / contact cursors, and that is inherent to newest-first ordering.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::validate;
use crate::{Platform, PlatformState};

/// Contract: text above this is refused (the task book allows either refusing
/// or accepting-with-documentation; we refuse).
pub const MAX_TEXT_CHARS: usize = 5_000;
pub const MAX_COMMENT_CHARS: usize = 1_000;
/// WeChat-style nine-grid.
pub const MAX_MEDIA_PER_MOMENT: usize = 9;
pub const MAX_FEED_PAGE: i64 = 50;

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/moments", post(publish))
        .route("/api/moments/feed", get(feed))
        .route("/api/moments/{moment_id}", get(detail))
        .route("/api/moments/{moment_id}", delete(remove))
        .route("/api/moments/{moment_id}/likes", post(like))
        .route("/api/moments/{moment_id}/likes", delete(unlike))
        .route("/api/moments/{moment_id}/comments", post(comment))
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Public,
    Friends,
}

impl Visibility {
    fn as_str(self) -> &'static str {
        match self {
            Visibility::Public => "public",
            Visibility::Friends => "friends",
        }
    }
}

#[derive(Deserialize)]
pub struct PublishBody {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub media_ids: Vec<String>,
    #[serde(default = "default_visibility")]
    pub visibility: Visibility,
}

fn default_visibility() -> Visibility {
    Visibility::Friends
}

#[derive(Deserialize)]
pub struct CommentBody {
    pub text: String,
    #[serde(default)]
    pub reply_to_comment_id: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct FeedQuery {
    #[serde(default)]
    since: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
}

#[derive(Serialize, Clone, Debug)]
pub struct MomentHead {
    pub moment_id: String,
    pub author: String,
    pub author_name: String,
    pub text: String,
    pub media_ids: Vec<String>,
    pub visibility: String,
    pub created_at: i64,
    pub like_count: i64,
    pub comment_count: i64,
}

#[derive(Serialize, Clone, Debug)]
pub struct CommentRow {
    pub comment_id: String,
    pub author: String,
    pub author_name: String,
    pub text: String,
    /// Empty for a top-level comment. The tree is the adjacency list: clients
    /// (and the acceptance script) reconstruct nesting from this link.
    pub reply_to: String,
    pub reply_to_name: String,
    pub created_at: i64,
}

#[derive(Serialize)]
pub struct MomentDetail {
    #[serde(flatten)]
    pub head: MomentHead,
    pub likes: Vec<String>,
    pub comments: Vec<CommentRow>,
}

impl Platform {
    /// Stores a new moment. Media ids must already exist (WP-05), so a moment
    /// cannot dangle a reference to an object nobody uploaded.
    pub fn publish_moment(
        &self,
        author: &str,
        text: &str,
        media_ids: &[String],
        visibility: Visibility,
    ) -> Result<String> {
        let author = validate::id(author)?.to_string();
        if text.chars().count() > MAX_TEXT_CHARS {
            return Err(PlatformError::BadInput("moment text too long"));
        }
        if text.contains(|c: char| c.is_control() && c != '\n') {
            return Err(PlatformError::BadInput(
                "moment text contains control characters",
            ));
        }
        if media_ids.len() > MAX_MEDIA_PER_MOMENT {
            return Err(PlatformError::BadInput("too many media items"));
        }
        if text.trim().is_empty() && media_ids.is_empty() {
            return Err(PlatformError::BadInput("moment is empty"));
        }
        let cleaned: Vec<String> = media_ids
            .iter()
            .map(|m| validate::id(m).map(|s| s.to_string()))
            .collect::<Result<Vec<_>>>()?;
        {
            let c = self.db().lock();
            for m in &cleaned {
                let known: Option<i64> = c
                    .query_row("SELECT 1 FROM media WHERE id = ?1", params![m], |r| {
                        r.get::<_, i64>(0)
                    })
                    .optional()?;
                if known.is_none() {
                    return Err(PlatformError::BadInput("unknown media id"));
                }
            }
        }

        let id = crate::auth::random_hex(8);
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        c.execute(
            "INSERT INTO moments(id, author, text, media, visibility, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                author,
                text,
                serde_json::to_string(&cleaned)?,
                visibility.as_str(),
                now
            ],
        )?;
        Ok(id)
    }

    fn moment_row(&self, moment_id: &str) -> Result<Option<(i64, MomentHead)>> {
        let c = self.db().lock();
        let row = c
            .query_row(
                "SELECT m.seq, m.id, m.author, u.name, m.text, m.media, m.visibility, m.created_at
                 FROM moments m JOIN users u ON u.id = m.author
                 WHERE m.id = ?1 AND m.deleted = 0",
                params![moment_id],
                |r| {
                    let media: String = r.get(5)?;
                    Ok((
                        r.get::<_, i64>(0)?,
                        MomentHead {
                            moment_id: r.get(1)?,
                            author: r.get(2)?,
                            author_name: r.get(3)?,
                            text: r.get(4)?,
                            media_ids: serde_json::from_str(&media).unwrap_or_default(),
                            visibility: r.get(6)?,
                            created_at: r.get(7)?,
                            like_count: 0,
                            comment_count: 0,
                        },
                    ))
                },
            )
            .optional()?;
        let Some((seq, mut head)) = row else {
            return Ok(None);
        };
        head.like_count = c.query_row(
            "SELECT COUNT(*) FROM moment_likes WHERE moment_id = ?1",
            params![moment_id],
            |r| r.get::<_, i64>(0),
        )?;
        head.comment_count = c.query_row(
            "SELECT COUNT(*) FROM moment_comments WHERE moment_id = ?1 AND deleted = 0",
            params![moment_id],
            |r| r.get::<_, i64>(0),
        )?;
        Ok(Some((seq, head)))
    }

    /// Whether `viewer` may see this moment. Authors always see their own;
    /// `public` is open; `friends` requires the mutual-friendship row.
    pub fn can_see_moment(&self, viewer: &str, author: &str, visibility: &str) -> Result<bool> {
        if viewer == author {
            return Ok(true);
        }
        if visibility == "public" {
            return Ok(true);
        }
        self.are_friends(author, viewer)
    }

    pub fn moment_visitors_likes(&self, moment_id: &str) -> Result<Vec<(String, String)>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT l.user_id, u.name FROM moment_likes l JOIN users u ON u.id = l.user_id
             WHERE l.moment_id = ?1 ORDER BY l.created_at",
        )?;
        let rows = stmt.query_map(params![moment_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn moment_comments(&self, moment_id: &str) -> Result<Vec<CommentRow>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT cm.id, cm.author, u.name, cm.text, cm.reply_to, rt.name, cm.created_at
             FROM moment_comments cm
             JOIN users u ON u.id = cm.author
             LEFT JOIN moment_comments rc ON rc.id = cm.reply_to
             LEFT JOIN users rt ON rt.id = rc.author
             WHERE cm.moment_id = ?1 AND cm.deleted = 0
             ORDER BY cm.seq",
        )?;
        let rows = stmt.query_map(params![moment_id], |r| {
            Ok(CommentRow {
                comment_id: r.get(0)?,
                author: r.get(1)?,
                author_name: r.get(2)?,
                text: r.get(3)?,
                reply_to: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                reply_to_name: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                created_at: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Author-only removal. Returns false when the moment is not theirs or gone.
    pub fn delete_moment(&self, moment_id: &str, by: &str) -> Result<bool> {
        let c = self.db().lock();
        let changed = c.execute(
            "UPDATE moments SET deleted = 1 WHERE id = ?1 AND author = ?2 AND deleted = 0",
            params![moment_id, by],
        )?;
        Ok(changed == 1)
    }

    /// Idempotent like: re-liking reports success without a second row.
    pub fn like_moment(&self, moment_id: &str, by: &str) -> Result<bool> {
        let c = self.db().lock();
        let changed = c.execute(
            "INSERT INTO moment_likes(moment_id, user_id, created_at) VALUES(?1, ?2, ?3)
             ON CONFLICT(moment_id, user_id) DO NOTHING",
            params![moment_id, by, wxlike_core::types::now_ms() as i64],
        )?;
        Ok(changed == 1)
    }

    pub fn unlike_moment(&self, moment_id: &str, by: &str) -> Result<bool> {
        let c = self.db().lock();
        let changed = c.execute(
            "DELETE FROM moment_likes WHERE moment_id = ?1 AND user_id = ?2",
            params![moment_id, by],
        )?;
        Ok(changed == 1)
    }

    /// Adds a comment, optionally as a reply to another comment **in the same
    /// moment** (a reply pointing elsewhere is refused rather than silently
    /// re-parented).
    pub fn comment_moment(
        &self,
        moment_id: &str,
        by: &str,
        text: &str,
        reply_to: Option<&str>,
    ) -> Result<String> {
        let by = validate::id(by)?.to_string();
        if text.trim().is_empty() {
            return Err(PlatformError::BadInput("comment must not be empty"));
        }
        if text.chars().count() > MAX_COMMENT_CHARS {
            return Err(PlatformError::BadInput("comment too long"));
        }
        if text.contains(|c: char| c.is_control() && c != '\n') {
            return Err(PlatformError::BadInput(
                "comment contains control characters",
            ));
        }
        let id = crate::auth::random_hex(8);
        let now = wxlike_core::types::now_ms() as i64;
        let reply_to = match reply_to {
            Some(r) if !r.is_empty() => Some(validate::id(r)?.to_string()),
            _ => None,
        };
        let c = self.db().lock();
        if let Some(rt) = &reply_to {
            let same_moment: Option<i64> = c
                .query_row(
                    "SELECT 1 FROM moment_comments WHERE id = ?1 AND moment_id = ?2 AND deleted = 0",
                    params![rt, moment_id],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?;
            if same_moment.is_none() {
                return Err(PlatformError::BadInput(
                    "reply target is not a comment on this moment",
                ));
            }
        }
        c.execute(
            "INSERT INTO moment_comments(id, moment_id, author, text, reply_to, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, moment_id, by, text, reply_to.unwrap_or_default(), now],
        )?;
        Ok(id)
    }

    /// Read fan-out: self + friends, newest first, at most `MAX_FEED_PAGE`.
    pub fn feed(
        &self,
        me: &str,
        since: Option<i64>,
        limit: i64,
    ) -> Result<(Vec<MomentHead>, i64, bool)> {
        let friends: Vec<String> = self.friends(me)?.into_iter().map(|f| f.friend_id).collect();
        let limit = limit.clamp(1, MAX_FEED_PAGE);
        // since is a watermark: strictly older than it; None = from the newest
        let upper = since.unwrap_or(i64::MAX);

        let c = self.db().lock();
        let authors: Vec<String> = friends
            .into_iter()
            .chain(std::iter::once(me.to_string()))
            .collect();
        let placeholders = vec!["?"; authors.len()].join(",");
        let sql = format!(
            "SELECT m.seq, m.id, m.author, u.name, m.text, m.media, m.visibility, m.created_at,
                    (SELECT COUNT(*) FROM moment_likes l WHERE l.moment_id = m.id),
                    (SELECT COUNT(*) FROM moment_comments cm WHERE cm.moment_id = m.id AND cm.deleted = 0)
             FROM moments m JOIN users u ON u.id = m.author
             WHERE m.deleted = 0 AND m.author IN ({placeholders}) AND m.seq < ?{}
             ORDER BY m.seq DESC LIMIT ?{}",
            authors.len() + 1,
            authors.len() + 2
        );
        let mut stmt = c.prepare(&sql)?;
        let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        for a in &authors {
            params_vec.push(Box::new(a.clone()));
        }
        params_vec.push(Box::new(upper));
        params_vec.push(Box::new(limit + 1)); // one extra row tells us if there is more
        let refs: Vec<&dyn rusqlite::types::ToSql> = params_vec.iter().map(|p| &**p).collect();
        // (seq, head) internally so the cursor needs no `seq` field in the API shape
        let rows = stmt.query_map(refs.as_slice(), |r| {
            let media: String = r.get(5)?;
            Ok((
                r.get::<_, i64>(0)?,
                MomentHead {
                    moment_id: r.get(1)?,
                    author: r.get(2)?,
                    author_name: r.get(3)?,
                    text: r.get(4)?,
                    media_ids: serde_json::from_str(&media).unwrap_or_default(),
                    visibility: r.get(6)?,
                    created_at: r.get(7)?,
                    like_count: r.get(8)?,
                    comment_count: r.get(9)?,
                },
            ))
        })?;
        let mut all = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        let has_more = all.len() as i64 > limit;
        if has_more {
            all.pop();
        }
        let next = all.last().map(|(seq, _)| *seq).unwrap_or(upper);
        Ok((all.into_iter().map(|(_, h)| h).collect(), next, has_more))
    }
}

async fn publish(
    State(platform): State<PlatformState>,
    authed: Authed,
    Json(body): Json<PublishBody>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let id = platform.publish_moment(
        &authed.user_id,
        &body.text,
        &body.media_ids,
        body.visibility,
    )?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "moment_id": id })),
    ))
}

async fn detail(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(moment_id): Path<String>,
) -> std::result::Result<Json<MomentDetail>, ApiError> {
    let moment_id = validate::id(&moment_id)
        .map_err(ApiError::from)?
        .to_string();
    let (_, head) = platform
        .moment_row(&moment_id)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such moment"))?;
    if !platform.can_see_moment(&authed.user_id, &head.author, &head.visibility)? {
        // a friends-only moment is simply not there for a stranger
        return Err(ApiError::new(StatusCode::NOT_FOUND, "no such moment"));
    }
    let likes = platform
        .moment_visitors_likes(&moment_id)?
        .into_iter()
        .map(|(id, _name)| id)
        .collect();
    Ok(Json(MomentDetail {
        head,
        likes,
        comments: platform.moment_comments(&moment_id)?,
    }))
}

async fn remove(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(moment_id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let moment_id = validate::id(&moment_id)
        .map_err(ApiError::from)?
        .to_string();
    let (_, head) = platform
        .moment_row(&moment_id)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such moment"))?;
    if head.author != authed.user_id {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "only the author may delete",
        ));
    }
    if !platform.delete_moment(&moment_id, &authed.user_id)? {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "no such moment"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Both like handlers need the moment to be visible to the caller first.
async fn require_visible(platform: &Platform, moment_id: &str, viewer: &str) -> Result<()> {
    let (_, head) = platform
        .moment_row(moment_id)?
        .ok_or(PlatformError::NotFound)?;
    if !platform.can_see_moment(viewer, &head.author, &head.visibility)? {
        return Err(PlatformError::NotFound);
    }
    Ok(())
}

async fn like(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(moment_id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let moment_id = validate::id(&moment_id)
        .map_err(ApiError::from)?
        .to_string();
    require_visible(&platform, &moment_id, &authed.user_id)
        .await
        .map_err(ApiError::from)?;
    let created = platform.like_moment(&moment_id, &authed.user_id)?;
    // idempotent: a repeat like is a 200 that added nothing
    Ok(Json(serde_json::json!({ "liked": true, "added": created })))
}

async fn unlike(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(moment_id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let moment_id = validate::id(&moment_id)
        .map_err(ApiError::from)?
        .to_string();
    require_visible(&platform, &moment_id, &authed.user_id)
        .await
        .map_err(ApiError::from)?;
    let removed = platform.unlike_moment(&moment_id, &authed.user_id)?;
    Ok(Json(
        serde_json::json!({ "liked": false, "removed": removed }),
    ))
}

async fn comment(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(moment_id): Path<String>,
    Json(body): Json<CommentBody>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let moment_id = validate::id(&moment_id)
        .map_err(ApiError::from)?
        .to_string();
    require_visible(&platform, &moment_id, &authed.user_id)
        .await
        .map_err(ApiError::from)?;
    let id = platform.comment_moment(
        &moment_id,
        &authed.user_id,
        &body.text,
        body.reply_to_comment_id.as_deref(),
    )?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "comment_id": id })),
    ))
}

async fn feed(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<FeedQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let (rows, next, has_more) = platform.feed(
        &authed.user_id,
        q.since.filter(|s| *s > 0),
        q.limit.unwrap_or(20),
    )?;
    Ok(Json(serde_json::json!({
        "moments": rows,
        "next_cursor": next,
        "has_more": has_more,
    })))
}
