//! Scopes, sessions and the per-app grant set.

use serde::{Deserialize, Serialize};

/// A capability the host can grant. The string form matches the capability
/// names in a signed manifest (`mini-package.v1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Perm {
    Storage,
    Network,
    Camera,
    Location,
    File,
    Payment,
}

impl Perm {
    pub const ALL: [Perm; 6] = [
        Perm::Storage,
        Perm::Network,
        Perm::Camera,
        Perm::Location,
        Perm::File,
        Perm::Payment,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Perm::Storage => "storage",
            Perm::Network => "network",
            Perm::Camera => "camera",
            Perm::Location => "location",
            Perm::File => "file",
            Perm::Payment => "payment",
        }
    }

    pub fn from_capability(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|perm| perm.as_str() == value)
    }

    /// Scopes that always prompt, even when granted at install time.
    pub fn needs_confirmation(self) -> bool {
        matches!(
            self,
            Perm::Camera | Perm::Location | Perm::File | Perm::Payment
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppSession {
    pub app_id: String,
    pub origin: String,
    pub session_id: String,
}

impl AppSession {
    pub fn new(
        app_id: impl Into<String>,
        origin: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            app_id: app_id.into(),
            origin: origin.into(),
            session_id: session_id.into(),
        }
    }
}

/// The identity a guest claims for one bridge call. The broker treats every
/// field as untrusted and compares it against the open session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestIdentity {
    pub app_id: String,
    pub origin: String,
    pub session_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_roundtrip_is_exact() {
        for perm in Perm::ALL {
            assert_eq!(Perm::from_capability(perm.as_str()), Some(perm));
        }
        assert_eq!(Perm::from_capability("root"), None);
        assert_eq!(Perm::from_capability("Storage"), None);
    }

    #[test]
    fn only_private_data_scopes_prompt() {
        assert!(!Perm::Storage.needs_confirmation());
        assert!(!Perm::Network.needs_confirmation());
        for perm in [Perm::Camera, Perm::Location, Perm::File, Perm::Payment] {
            assert!(perm.needs_confirmation());
        }
    }
}
