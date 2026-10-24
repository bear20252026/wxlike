//! Upgrade consent: every newly declared capability is shown before the
//! new version is installed. Location and payment are called out separately
//! so a UI cannot fold them into a generic "continue".

use crate::packages::install::InstalledApp;
use crate::packages::manifest::{Manifest, CAPABILITIES};

pub const SENSITIVE_CAPABILITIES: [&str; 4] = ["camera", "location", "file", "payment"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpgradePlan {
    pub app_id: String,
    pub from_version: String,
    pub to_version: String,
    pub added_capabilities: Vec<String>,
    pub sensitive_added: Vec<String>,
}

impl UpgradePlan {
    pub fn between(installed: &InstalledApp, incoming: &Manifest) -> Self {
        let added: Vec<String> = CAPABILITIES
            .iter()
            .filter(|cap| {
                incoming.capabilities.iter().any(|c| c == *cap)
                    && !installed.capabilities.iter().any(|c| c == *cap)
            })
            .map(|cap| (*cap).to_string())
            .collect();
        let sensitive = added
            .iter()
            .filter(|cap| {
                SENSITIVE_CAPABILITIES
                    .iter()
                    .any(|sensitive| sensitive == cap)
            })
            .cloned()
            .collect();
        Self {
            app_id: incoming.app_id.clone(),
            from_version: installed.version.clone(),
            to_version: incoming.version.clone(),
            added_capabilities: added,
            sensitive_added: sensitive,
        }
    }

    /// Any new capability needs a fresh user decision, not just the
    /// sensitive ones: the manifest the user approved has changed.
    pub fn requires_confirmation(&self) -> bool {
        !self.added_capabilities.is_empty()
    }

    pub fn touches_sensitive_data(&self) -> bool {
        !self.sensitive_added.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packages::manifest::{digest_hex, Channel};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn installed(caps: &[&str]) -> InstalledApp {
        InstalledApp {
            app_id: "com.demo.app".into(),
            version: "1.0.0".into(),
            digest: "d".into(),
            path: PathBuf::from("x"),
            capabilities: caps.iter().map(|c| (*c).to_string()).collect(),
        }
    }

    fn manifest(version: &str, caps: &[&str]) -> Manifest {
        Manifest {
            app_id: "com.demo.app".into(),
            name: "Demo".into(),
            version: version.into(),
            min_host: "0.1.0".into(),
            entry: "index.js".into(),
            channel: Channel::Prod,
            publisher: "wxlike-labs".into(),
            capabilities: caps.iter().map(|c| (*c).to_string()).collect(),
            resources: BTreeMap::from([("index.js".into(), digest_hex(b"x"))]),
        }
    }

    #[test]
    fn added_sensitive_scopes_are_listed_explicitly() {
        let plan = UpgradePlan::between(
            &installed(&["storage"]),
            &manifest("2.0.0", &["storage", "location", "payment"]),
        );
        assert!(plan.requires_confirmation());
        assert!(plan.touches_sensitive_data());
        assert_eq!(
            plan.added_capabilities,
            vec!["location".to_string(), "payment".to_string()]
        );
        assert_eq!(plan.sensitive_added, plan.added_capabilities);
    }

    #[test]
    fn unchanged_capabilities_do_not_prompt() {
        let plan = UpgradePlan::between(
            &installed(&["storage", "network"]),
            &manifest("2.0.0", &["storage", "network"]),
        );
        assert!(!plan.requires_confirmation());
        assert!(!plan.touches_sensitive_data());
    }
}
