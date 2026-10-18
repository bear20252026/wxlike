//! WP-01 account endpoints: register / login / me / profile / logout.
//!
//! Response shapes are exactly the contract in `docs/WORK_BREAKDOWN.md` WP-01.
//! `GET /api/profile/{user_id}` returns the intentionally narrow public view
//! (`user_id`, `name`, `avatar`); `signature` is self-only and only appears on
//! `/api/me`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension as _};

use crate::auth::{self, Authed};
use crate::error::{ApiError, PlatformError, Result};
use crate::validate;
use crate::{Platform, PlatformState};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/register", post(register))
        .route("/api/login", post(login))
        .route("/api/me", get(me))
        .route("/api/profile", put(update_profile))
        .route("/api/profile/{user_id}", get(public_profile))
        .route("/api/logout", post(logout))
}

#[derive(serde::Deserialize)]
pub struct Credentials {
    pub name: String,
    pub password: String,
}

#[derive(serde::Serialize)]
pub struct AuthReply {
    pub user_id: String,
    pub token: String,
}

#[derive(serde::Deserialize, Default)]
pub struct ProfilePatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub signature: Option<String>,
}

#[derive(serde::Serialize)]
pub struct Profile {
    pub user_id: String,
    pub name: String,
    pub avatar: String,
    pub signature: String,
}

#[derive(serde::Serialize)]
pub struct PublicProfile {
    pub user_id: String,
    pub name: String,
    pub avatar: String,
}

/// rusqlite surfaces the UNIQUE index as this variant; the pre-check below
/// makes conflicts deterministic, this is the backstop under concurrency.
fn is_unique_violation(e: &rusqlite::Error) -> bool {
    match e {
        rusqlite::Error::SqliteFailure(_, Some(msg)) => msg.contains("UNIQUE"),
        _ => false,
    }
}

impl Platform {
    /// Creates the user row. All SQL for one logical operation runs under a
    /// single connection guard, so existence-check + insert cannot interleave
    /// with another registrant.
    pub fn register(&self, name: &str, password: &str) -> Result<String> {
        let name = validate::name(name)?;
        validate::password(password)?;
        let (salt, hash) = auth::hash_password(password, self.options().pbkdf2_rounds)?;
        let user_id = auth::random_hex(8);
        let now = wxlike_core::types::now_ms();
        let c = self.db().lock();
        let taken: Option<String> = c
            .query_row("SELECT id FROM users WHERE name = ?1", params![name], |r| {
                r.get::<_, String>(0)
            })
            .optional()?;
        if taken.is_some() {
            return Err(PlatformError::Conflict("name".into()));
        }
        c.execute(
            "INSERT INTO users(id, name, pw_salt, pw_hash, pw_rounds, avatar, signature, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, '', '', ?6)",
            params![user_id, name, salt, hash, self.options().pbkdf2_rounds as i64, now as i64],
        )
        .map_err(|e| {
            if is_unique_violation(&e) {
                PlatformError::Conflict("name".into())
            } else {
                PlatformError::Sqlite(e)
            }
        })?;
        drop(c); // audit re-locks the same non-reentrant mutex
        self.audit(&user_id, "user.register", &user_id, "");
        Ok(user_id)
    }

    /// `Ok(None)` = unknown user or wrong password (callers render 401 without
    /// distinguishing the two, so accounts cannot be enumerated by timing).
    pub fn verify_credentials(&self, name: &str, password: &str) -> Result<Option<String>> {
        let name = validate::name(name)?;
        let row: Option<(String, String, String, i64)> = {
            let c = self.db().lock();
            c.query_row(
                "SELECT id, pw_salt, pw_hash, pw_rounds FROM users WHERE name = ?1",
                params![name],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?
        };
        let Some((user_id, salt, hash, rounds)) = row else {
            return Ok(None);
        };
        let rounds = if rounds <= 0 {
            auth::DEFAULT_ROUNDS
        } else {
            rounds as u32
        };
        if auth::verify_password(password, &salt, &hash, rounds)? {
            Ok(Some(user_id))
        } else {
            Ok(None)
        }
    }

    pub fn profile_of(&self, user_id: &str) -> Result<Profile> {
        let c = self.db().lock();
        c.query_row(
            "SELECT name, avatar, signature FROM users WHERE id = ?1",
            params![user_id],
            |r| {
                Ok(Profile {
                    user_id: user_id.to_string(),
                    name: r.get(0)?,
                    avatar: r.get(1)?,
                    signature: r.get(2)?,
                })
            },
        )
        .optional()?
        .ok_or(PlatformError::NotFound)
    }

    /// Applies only the fields present in the patch.
    pub fn update_profile(&self, user_id: &str, patch: &ProfilePatch) -> Result<Profile> {
        let c = self.db().lock();
        let exists: Option<String> = c
            .query_row(
                "SELECT id FROM users WHERE id = ?1",
                params![user_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(PlatformError::NotFound);
        }
        if let Some(name) = &patch.name {
            let name = validate::name(name)?;
            let taken: Option<String> = c
                .query_row(
                    "SELECT id FROM users WHERE name = ?1 AND id <> ?2",
                    params![name, user_id],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;
            if taken.is_some() {
                return Err(PlatformError::Conflict("name".into()));
            }
            c.execute(
                "UPDATE users SET name = ?1 WHERE id = ?2",
                params![name, user_id],
            )?;
        }
        if let Some(avatar) = &patch.avatar {
            c.execute(
                "UPDATE users SET avatar = ?1 WHERE id = ?2",
                params![validate::avatar(avatar)?, user_id],
            )?;
        }
        if let Some(signature) = &patch.signature {
            c.execute(
                "UPDATE users SET signature = ?1 WHERE id = ?2",
                params![validate::signature(signature)?, user_id],
            )?;
        }
        drop(c);
        self.profile_of(user_id)
    }
}

async fn register(
    State(platform): State<PlatformState>,
    Json(req): Json<Credentials>,
) -> std::result::Result<(StatusCode, Json<AuthReply>), ApiError> {
    let user_id = platform.register(&req.name, &req.password)?;
    let token = platform.create_session(&user_id, platform.options().session_ttl_ms)?;
    Ok((StatusCode::OK, Json(AuthReply { user_id, token })))
}

async fn login(
    State(platform): State<PlatformState>,
    Json(req): Json<Credentials>,
) -> std::result::Result<(StatusCode, Json<AuthReply>), ApiError> {
    let user_id = platform
        .verify_credentials(&req.name, &req.password)?
        .ok_or_else(|| {
            platform.audit(&req.name, "user.login", "", "failed");
            ApiError::new(StatusCode::UNAUTHORIZED, "invalid credentials")
        })?;
    let token = platform.create_session(&user_id, platform.options().session_ttl_ms)?;
    platform.audit(&user_id, "user.login", &user_id, "ok");
    Ok((StatusCode::OK, Json(AuthReply { user_id, token })))
}

async fn me(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<Profile>, ApiError> {
    Ok(Json(platform.profile_of(&authed.user_id)?))
}

async fn update_profile(
    State(platform): State<PlatformState>,
    authed: Authed,
    Json(patch): Json<ProfilePatch>,
) -> std::result::Result<Json<Profile>, ApiError> {
    Ok(Json(platform.update_profile(&authed.user_id, &patch)?))
}

async fn public_profile(
    State(platform): State<PlatformState>,
    Path(user_id): Path<String>,
) -> std::result::Result<Json<PublicProfile>, ApiError> {
    let user_id = validate::id(&user_id).map_err(ApiError::from)?;
    let p = platform
        .profile_of(user_id)
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "no such user"))?;
    Ok(Json(PublicProfile {
        user_id: p.user_id,
        name: p.name,
        avatar: p.avatar,
    }))
}

async fn logout(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<StatusCode, ApiError> {
    platform.revoke_session(&authed.token)?;
    Ok(StatusCode::NO_CONTENT)
}
