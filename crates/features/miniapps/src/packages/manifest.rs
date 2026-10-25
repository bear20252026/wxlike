//! Signed mini-app manifest: identity, compatibility, declared capabilities
//! and one SHA-256 digest per resource.
//!
//! The manifest deliberately does **not** contain its own digest (that would
//! be a fixed-point problem). `mini.json` is covered by the signature; every
//! other file is covered by an entry in `resources`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::error::{MiniError, Result};

/// The only capabilities a manifest may declare. Host code decides what is
/// actually granted; an unknown capability is a manifest bug, not a request
/// to be silently dropped (a typo would otherwise look like a working app).
pub const CAPABILITIES: [&str; 6] = [
    "storage", "network", "camera", "location", "file", "payment",
];

pub const MANIFEST_FILE: &str = "mini.json";
pub const MAX_APP_ID_LEN: usize = 64;
pub const MAX_NAME_CHARS: usize = 64;
pub const MAX_RESOURCES: usize = 2048;
pub const SHA256_HEX_LEN: usize = 64;
const SEMVER_PARTS: usize = 3;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Dev,
    Prod,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Dev => "dev",
            Channel::Prod => "prod",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "dev" => Some(Channel::Dev),
            "prod" => Some(Channel::Prod),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub app_id: String,
    pub name: String,
    pub version: String,
    /// Oldest host version that can run this package.
    pub min_host: String,
    pub entry: String,
    pub channel: Channel,
    pub publisher: String,
    pub capabilities: Vec<String>,
    /// Relative path -> lowercase hex SHA-256 of the file contents.
    /// `mini.json` itself is covered by the signature, not by this map.
    pub resources: BTreeMap<String, String>,
}

impl Manifest {
    /// Check shape and compatibility without touching a package blob.
    pub fn validate(&self, host_version: &str) -> Result<()> {
        self.validate_identity()?;
        let host = parse_semver(host_version)
            .ok_or(MiniError::BadManifest("host version is not semver"))?;
        let min = parse_semver(&self.min_host).ok_or(MiniError::BadManifest("min_host"))?;
        if min > host {
            return Err(MiniError::HostTooOld(
                self.min_host.clone(),
                host_version.into(),
            ));
        }
        self.validate_resources()
    }

    fn validate_identity(&self) -> Result<()> {
        validate_app_id(&self.app_id)?;
        if self.name.trim().is_empty() || self.name.chars().count() > MAX_NAME_CHARS {
            return Err(MiniError::BadManifest("name"));
        }
        if parse_semver(&self.version).is_none() {
            return Err(MiniError::BadManifest("version"));
        }
        if parse_semver(&self.min_host).is_none() {
            return Err(MiniError::BadManifest("min_host"));
        }
        if !safe_relative(&self.entry) {
            return Err(MiniError::BadManifest("entry path"));
        }
        if self.publisher.trim().is_empty() || self.publisher.chars().count() > MAX_APP_ID_LEN {
            return Err(MiniError::BadManifest("publisher"));
        }
        if self
            .capabilities
            .iter()
            .any(|c| !CAPABILITIES.contains(&c.as_str()))
        {
            return Err(MiniError::BadManifest("unknown capability"));
        }
        Ok(())
    }

    fn validate_resources(&self) -> Result<()> {
        if self.resources.is_empty() || self.resources.len() > MAX_RESOURCES {
            return Err(MiniError::BadManifest("resources"));
        }
        for (path, digest) in &self.resources {
            if !safe_relative(path) || path.len() > wxlike_dpage::MAX_PATH_LEN {
                return Err(MiniError::BadManifest("resource path"));
            }
            if path == MANIFEST_FILE {
                return Err(MiniError::BadManifest("mini.json must not list itself"));
            }
            if digest.len() != SHA256_HEX_LEN || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(MiniError::BadManifest("resource digest"));
            }
        }
        if !self.resources.contains_key(&self.entry) {
            return Err(MiniError::BadManifest("entry has no digest"));
        }
        Ok(())
    }

    /// Capabilities added relative to an already installed manifest, in
    /// stable order. Used to decide whether an upgrade needs re-confirmation.
    pub fn added_capabilities(&self, installed: &Manifest) -> Vec<String> {
        CAPABILITIES
            .iter()
            .filter(|cap| {
                self.capabilities.iter().any(|c| c == *cap)
                    && !installed.capabilities.iter().any(|c| c == *cap)
            })
            .map(|cap| (*cap).to_string())
            .collect()
    }
}

/// Fill `resources` from a directory tree. The returned file map still needs
/// `mini.json` added by the package builder.
pub fn seal_dir(
    dir: &std::path::Path,
    template: &Manifest,
    host_version: &str,
) -> Result<(Manifest, wxlike_dpage::FileMap)> {
    let files = wxlike_dpage::collect_dir(dir)?;
    let mut manifest = template.clone();
    manifest.resources.clear();
    for entry in &files.0 {
        if entry.path == MANIFEST_FILE {
            return Err(MiniError::BadManifest(
                "source dir contains reserved mini.json",
            ));
        }
        manifest
            .resources
            .insert(entry.path.clone(), digest_hex(&entry.data));
    }
    manifest.validate(host_version)?;
    Ok((manifest, files))
}

pub fn digest_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn safe_relative(path: &str) -> bool {
    !path.is_empty() && wxlike_dpage::safe_rel_path(path)
}

fn validate_app_id(app_id: &str) -> Result<()> {
    if app_id.is_empty() || app_id.len() > MAX_APP_ID_LEN {
        return Err(MiniError::BadManifest("app_id"));
    }
    let valid = app_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-');
    if !valid || app_id.starts_with('.') || app_id.starts_with('-') {
        return Err(MiniError::BadManifest("app_id charset"));
    }
    Ok(())
}

/// Parse `major.minor.patch`, ignoring a `-prerelease` / `+build` suffix.
pub fn parse_semver(value: &str) -> Option<(u64, u64, u64)> {
    let core = value.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let mut numbers = [0u64; SEMVER_PARTS];
    for slot in &mut numbers {
        let raw = parts.next()?;
        if raw.is_empty() {
            return None;
        }
        *slot = raw.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some((numbers[0], numbers[1], numbers[2]))
}

pub fn compare_versions(left: &str, right: &str) -> Option<std::cmp::Ordering> {
    Some(parse_semver(left)?.cmp(&parse_semver(right)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest {
            app_id: "com.wxlike.notes".into(),
            name: "笔记".into(),
            version: "1.0.0".into(),
            min_host: "0.1.0".into(),
            entry: "index.js".into(),
            channel: Channel::Prod,
            publisher: "wxlike-labs".into(),
            capabilities: vec!["storage".into()],
            resources: BTreeMap::from([("index.js".into(), digest_hex(b"x"))]),
        }
    }

    #[test]
    fn validates_a_minimal_manifest() {
        manifest().validate("0.1.0").unwrap();
    }

    #[test]
    fn rejects_unknown_capability_and_missing_entry_digest() {
        let mut m = manifest();
        m.capabilities.push("root".into());
        assert!(matches!(
            m.validate("0.1.0"),
            Err(MiniError::BadManifest("unknown capability"))
        ));

        let mut m = manifest();
        m.resources.clear();
        assert!(m.validate("0.1.0").is_err());
    }

    #[test]
    fn rejects_host_too_old_and_self_referential_resource() {
        let mut m = manifest();
        m.min_host = "9.0.0".into();
        assert!(matches!(
            m.validate("0.1.0"),
            Err(MiniError::HostTooOld(_, _))
        ));

        let mut m = manifest();
        m.resources.insert(MANIFEST_FILE.into(), digest_hex(b"x"));
        assert!(m.validate("0.1.0").is_err());
    }

    #[test]
    fn added_capabilities_are_stable_and_ordered() {
        let old = manifest();
        let mut new = manifest();
        new.capabilities.push("payment".into());
        new.capabilities.push("camera".into());
        // CAPABILITIES order: camera before payment
        assert_eq!(
            new.added_capabilities(&old),
            vec!["camera".to_string(), "payment".to_string()]
        );
    }

    #[test]
    fn semver_parsing_is_strict() {
        assert_eq!(parse_semver("1.2.3-beta+7"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2"), None);
        assert_eq!(parse_semver("1.2.3.4"), None);
        assert_eq!(parse_semver("1..3"), None);
    }
}
