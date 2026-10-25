//! Takedown: a reviewed app can be withdrawn after publication.
//!
//! Withdrawal must do more than hide a store card: active sessions lose
//! their authorization, and no new install or update of the affected
//! version is accepted. Local data is left alone until the user uninstalls.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Takedown {
    pub app_id: String,
    /// `None` withdraws every version of the app.
    #[serde(default)]
    pub version: Option<String>,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TakedownReport {
    pub app_id: String,
    pub version: Option<String>,
    /// Sessions the host authority terminated for this app.
    pub sessions_revoked: usize,
}

pub trait RuntimeAuthority {
    fn revoke_app(&mut self, app_id: &str) -> usize;
}

#[derive(Default)]
pub struct RecordingAuthority {
    revoked: Vec<String>,
}

impl RuntimeAuthority for RecordingAuthority {
    fn revoke_app(&mut self, app_id: &str) -> usize {
        self.revoked.push(app_id.to_string());
        1
    }
}

impl RecordingAuthority {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn revoked(&self) -> &[String] {
        &self.revoked
    }
}
