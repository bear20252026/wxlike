//! WP-06 device registry: which push token serves each platform a user runs.
//!
//! One row per `(user_id, platform)`: re-registering a platform replaces its
//! token (a fresh install must invalidate the old token — "多设备保留最新"),
//! while a phone and a tablet coexist and both get notified.
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::devices`]。

use crate::error::{PlatformError, Result};
use crate::validate;
use crate::Platform;

use super::repo::devices as devices_repo;

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
            let c = self.db().acquire();
            devices_repo::register(
                &c,
                user_id,
                platform,
                &reg.device_token,
                wxlike_core::types::now_ms() as i64,
            )?
        };
        Ok(replaced.is_some())
    }

    /// All registered devices of a user (push dispatch reads this).
    pub fn devices_of(&self, user_id: &str) -> Result<Vec<DeviceRow>> {
        let c = self.db().acquire();
        devices_repo::list(&c, user_id)
    }
}

#[derive(serde::Deserialize)]
pub struct DeviceReg {
    /// e.g. "android" | "ios" | "desktop"
    pub platform: String,
    pub device_token: String,
}
