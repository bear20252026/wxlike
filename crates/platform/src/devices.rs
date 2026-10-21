//! WP-06 device registry: which push token serves each platform a user runs.
//!
//! One row per `(user_id, platform)`: re-registering a platform replaces its
//! token (a fresh install must invalidate the old token — "多设备保留最新"),
//! while a phone and a tablet coexist and both get notified.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::params;

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::extract::ApiJson;
use crate::validate;
use crate::{Platform, PlatformState};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/devices", post(register))
        .route("/api/devices", get(list))
}

#[derive(serde::Deserialize)]
pub struct DeviceReg {
    /// e.g. "android" | "ios" | "desktop"
    pub platform: String,
    pub device_token: String,
}

#[derive(serde::Serialize, PartialEq, Debug)]
pub struct DeviceRow {
    pub platform: String,
    pub device_token: String,
}

impl Platform {
    /// Upserts the token for `(user, platform)`; returns whether it replaced
    /// an existing row.
    pub fn register_device(&self, user_id: &str, reg: &DeviceReg) -> Result<bool> {
        let platform = validate::device_platform(&reg.platform)?;
        if reg.device_token.is_empty() || reg.device_token.len() > 512 {
            return Err(PlatformError::BadInput("device token malformed"));
        }
        let replaced = {
            let c = self.db().lock();
            let existing: Option<String> = c
                .query_row(
                    "SELECT device_token FROM devices WHERE user_id = ?1 AND platform = ?2",
                    params![user_id, platform],
                    |r| r.get::<_, String>(0),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })?;
            c.execute(
                "INSERT INTO devices(user_id, platform, device_token, last_seen)
                 VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(user_id, platform) DO UPDATE SET
                   device_token = excluded.device_token,
                   last_seen = excluded.last_seen",
                params![
                    user_id,
                    platform,
                    reg.device_token,
                    wxlike_core::types::now_ms() as i64
                ],
            )?;
            existing
        };
        Ok(replaced.is_some())
    }

    /// All registered devices of a user (push dispatch reads this).
    pub fn devices_of(&self, user_id: &str) -> Result<Vec<DeviceRow>> {
        let c = self.db().lock();
        let mut stmt =
            c.prepare("SELECT platform, device_token FROM devices WHERE user_id = ?1")?;
        let rows = stmt
            .query_map(params![user_id], |r| {
                Ok(DeviceRow {
                    platform: r.get(0)?,
                    device_token: r.get(1)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

async fn register(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(reg): ApiJson<DeviceReg>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.register_device(&authed.user_id, &reg)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<Vec<DeviceRow>>, ApiError> {
    Ok(Json(platform.devices_of(&authed.user_id)?))
}
