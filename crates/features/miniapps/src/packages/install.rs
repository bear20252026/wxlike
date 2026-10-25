//! Crash-safe installation with a monotonic version watermark.
//!
//! Layout (all under the channel root, e.g. `.../miniapps/prod`):
//!
//! ```text
//! apps/<app_id>/v<version>-<digest12>/...   one directory per installed version
//! apps/<app_id>/v<version>-<digest12>/.complete
//! apps/<app_id>/pin.json                    optional operational rollback pin
//! staging/<unique>/                         in-flight install, never launched
//! watermarks/<app_id>/<version>             empty file per accepted version
//! ```
//!
//! The active version is the highest complete directory unless a pin names an
//! older one. Staging lives on the same filesystem, so activation is a single
//! rename; a crash before it leaves the previous version untouched and the
//! next [`Installer::recover`] deletes the leftover staging directory.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::error::{MiniError, Result};
use super::format::{verify, VerifiedPackage};
use super::manifest::{compare_versions, parse_semver, Channel, Manifest, MANIFEST_FILE};
use super::trust::{PackageSignature, TrustStore};

const APPS_DIR: &str = "apps";
const STAGING_DIR: &str = "staging";
const WATERMARKS_DIR: &str = "watermarks";
const COMPLETE_MARKER: &str = ".complete";
const PIN_FILE: &str = "pin.json";
const VERSION_PREFIX: &str = "v";
const DIGEST_PREFIX_LEN: usize = 12;
const MAX_KEPT_VERSIONS: usize = 3;

#[derive(Serialize, Deserialize, Clone, Debug)]
struct CompleteMarker {
    version: String,
    digest: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Pin {
    version: String,
    digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledApp {
    pub app_id: String,
    pub version: String,
    pub digest: String,
    pub path: PathBuf,
    pub capabilities: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum InstallOutcome {
    Installed(InstalledApp),
    /// The exact same version and digest is already active; safe retry.
    AlreadyInstalled(InstalledApp),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultPoint {
    BeforeStaging,
    AfterStaging,
    BeforeActivate,
}

pub trait Faults {
    fn trip(&mut self, point: FaultPoint) -> Result<()>;
}

pub struct NoFaults;

impl Faults for NoFaults {
    fn trip(&mut self, _point: FaultPoint) -> Result<()> {
        Ok(())
    }
}

pub struct Installer {
    root: PathBuf,
    channel: Channel,
    trust: TrustStore,
    host_version: String,
    faults: Box<dyn Faults>,
}

impl Installer {
    pub fn new(
        base: &Path,
        channel: Channel,
        trust: TrustStore,
        host_version: impl Into<String>,
    ) -> Self {
        Self {
            root: base.join(channel.as_str()),
            channel,
            trust,
            host_version: host_version.into(),
            faults: Box::new(NoFaults),
        }
    }

    pub fn with_faults(mut self, faults: Box<dyn Faults>) -> Self {
        self.faults = faults;
        self
    }

    pub fn channel(&self) -> Channel {
        self.channel
    }

    pub fn trust(&self) -> &TrustStore {
        &self.trust
    }

    pub fn install(
        &mut self,
        blob: &[u8],
        sig: &PackageSignature,
        now: u64,
    ) -> Result<InstallOutcome> {
        let verified = verify(blob, sig, &self.trust, now, &self.host_version)?;
        if verified.manifest.channel != self.channel {
            return Err(MiniError::WrongChannel(
                verified.manifest.channel.as_str().into(),
                self.channel.as_str().into(),
            ));
        }
        if let Ok(current) = self.current(&verified.manifest.app_id) {
            if current.version == verified.manifest.version {
                if current.digest == verified.digest {
                    return Ok(InstallOutcome::AlreadyInstalled(current));
                }
                return Err(MiniError::VersionConflict(
                    verified.manifest.app_id.clone(),
                    verified.manifest.version.clone(),
                ));
            }
        }
        self.check_watermark(&verified)?;
        self.faults.trip(FaultPoint::BeforeStaging)?;
        let staging = self.stage(&verified)?;
        self.faults.trip(FaultPoint::AfterStaging)?;
        self.faults.trip(FaultPoint::BeforeActivate)?;
        let installed = self.activate(&staging, &verified)?;
        self.raise_watermark(&verified.manifest)?;
        self.clear_pin(&verified.manifest.app_id)?;
        self.prune(&verified.manifest.app_id)?;
        Ok(installed)
    }

    fn check_watermark(&self, verified: &VerifiedPackage) -> Result<()> {
        let manifest = &verified.manifest;
        let Some(floor) = self.watermark(&manifest.app_id)? else {
            return Ok(());
        };
        if compare_versions(&manifest.version, &floor) == Some(std::cmp::Ordering::Less) {
            return Err(MiniError::Downgrade(
                manifest.app_id.clone(),
                manifest.version.clone(),
                floor,
            ));
        }
        Ok(())
    }

    fn stage(&self, verified: &VerifiedPackage) -> Result<PathBuf> {
        let staging = self.root.join(STAGING_DIR).join(format!(
            "{}-{}",
            sanitize(&verified.manifest.app_id),
            unique_suffix()
        ));
        fs::create_dir_all(&staging)?;
        for (path, data) in &verified.files {
            let target = staging.join(path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = fs::File::create(&target)?;
            file.write_all(data)?;
            file.sync_all()?;
        }
        let marker = CompleteMarker {
            version: verified.manifest.version.clone(),
            digest: verified.digest.clone(),
        };
        write_synced(
            &staging.join(COMPLETE_MARKER),
            &serde_json::to_vec(&marker)?,
        )?;
        sync_dir(&staging);
        Ok(staging)
    }

    fn activate(&self, staging: &Path, verified: &VerifiedPackage) -> Result<InstallOutcome> {
        let app_dir = self.app_dir(&verified.manifest.app_id);
        let slot = app_dir.join(version_dir_name(&verified.manifest, &verified.digest));
        if slot.exists() {
            fs::remove_dir_all(staging)?;
            return self.current(&verified.manifest.app_id).map(|app| {
                if app.digest == verified.digest {
                    InstallOutcome::AlreadyInstalled(app)
                } else {
                    InstallOutcome::Installed(app)
                }
            });
        }
        fs::create_dir_all(&app_dir)?;
        fs::rename(staging, &slot)?;
        sync_dir(&app_dir);
        self.current(&verified.manifest.app_id)
            .map(InstallOutcome::Installed)
    }

    /// Point the app at the newest complete version older than the active one.
    /// The watermark is intentionally **not** lowered: reinstalling an old
    /// signed package is still a downgrade attack.
    pub fn rollback(&mut self, app_id: &str) -> Result<InstalledApp> {
        let current = self.current(app_id)?;
        let previous = self
            .complete_versions(app_id)?
            .into_iter()
            .filter(|candidate| {
                matches!(
                    compare_versions(&candidate.version, &current.version),
                    Some(std::cmp::Ordering::Less)
                )
            })
            .max_by(|a, b| {
                compare_versions(&a.version, &b.version).unwrap_or(std::cmp::Ordering::Equal)
            })
            .ok_or_else(|| MiniError::NotInstalled(app_id.into()))?;
        let pin = Pin {
            version: previous.version,
            digest: previous.digest,
        };
        write_atomic(
            &self.app_dir(app_id).join(PIN_FILE),
            &serde_json::to_vec(&pin)?,
        )?;
        self.current(app_id)
    }

    pub fn current(&self, app_id: &str) -> Result<InstalledApp> {
        let mut versions = self.complete_versions(app_id)?;
        if versions.is_empty() {
            return Err(MiniError::NotInstalled(app_id.into()));
        }
        if let Some(pin) = self.read_pin(app_id)? {
            if let Some(index) = versions
                .iter()
                .position(|v| v.version == pin.version && v.digest == pin.digest)
            {
                return self.installed_from(&versions.remove(index));
            }
        }
        versions.sort_by(|a, b| {
            compare_versions(&b.version, &a.version)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.digest.cmp(&a.digest))
        });
        self.installed_from(&versions[0])
    }

    pub fn remove(&mut self, app_id: &str) -> Result<bool> {
        let dir = self.app_dir(app_id);
        if !dir.exists() {
            return Ok(false);
        }
        fs::remove_dir_all(dir)?;
        Ok(true)
    }

    /// Clean leftovers from an interrupted install. Safe to call at startup.
    pub fn recover(&mut self) -> Result<usize> {
        let mut removed = 0;
        let staging = self.root.join(STAGING_DIR);
        if staging.exists() {
            for entry in fs::read_dir(&staging)? {
                fs::remove_dir_all(entry?.path())?;
                removed += 1;
            }
        }
        let apps = self.root.join(APPS_DIR);
        if apps.exists() {
            for app in fs::read_dir(apps)? {
                let dir = app?.path();
                if !dir.is_dir() {
                    continue;
                }
                for version in fs::read_dir(&dir)? {
                    let version = version?.path();
                    if version.is_dir() && !version.join(COMPLETE_MARKER).exists() {
                        fs::remove_dir_all(&version)?;
                        removed += 1;
                    }
                }
            }
        }
        Ok(removed)
    }

    pub fn watermark(&self, app_id: &str) -> Result<Option<String>> {
        let dir = self.watermarks_dir(app_id);
        if !dir.exists() {
            return Ok(None);
        }
        let mut best: Option<String> = None;
        for entry in fs::read_dir(&dir)? {
            let name = entry?.file_name().to_string_lossy().to_string();
            if parse_semver(&name).is_none() {
                continue;
            }
            if best
                .as_ref()
                .map(|current| {
                    compare_versions(&name, current) == Some(std::cmp::Ordering::Greater)
                })
                .unwrap_or(true)
            {
                best = Some(name);
            }
        }
        Ok(best)
    }

    fn raise_watermark(&self, manifest: &Manifest) -> Result<()> {
        let dir = self.watermarks_dir(&manifest.app_id);
        fs::create_dir_all(&dir)?;
        let path = dir.join(sanitize(&manifest.version));
        if !path.exists() {
            write_synced(&path, b"")?;
        }
        Ok(())
    }

    fn prune(&self, app_id: &str) -> Result<()> {
        let mut versions = self.complete_versions(app_id)?;
        versions.sort_by(|a, b| {
            compare_versions(&b.version, &a.version).unwrap_or(std::cmp::Ordering::Equal)
        });
        let pinned = self.read_pin(app_id)?;
        for candidate in versions.into_iter().skip(MAX_KEPT_VERSIONS) {
            let keep = pinned
                .as_ref()
                .map(|pin| pin.version == candidate.version && pin.digest == candidate.digest)
                .unwrap_or(false);
            if !keep {
                fs::remove_dir_all(candidate.path)?;
            }
        }
        Ok(())
    }

    fn installed_from(&self, complete: &CompleteVersion) -> Result<InstalledApp> {
        let manifest_bytes = fs::read(complete.path.join(MANIFEST_FILE))?;
        let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
        Ok(InstalledApp {
            app_id: manifest.app_id,
            version: complete.version.clone(),
            digest: complete.digest.clone(),
            path: complete.path.clone(),
            capabilities: manifest.capabilities,
        })
    }

    fn complete_versions(&self, app_id: &str) -> Result<Vec<CompleteVersion>> {
        let dir = self.app_dir(app_id);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if !path.is_dir() {
                continue;
            }
            let marker = path.join(COMPLETE_MARKER);
            if !marker.exists() {
                continue;
            }
            let marker: CompleteMarker = serde_json::from_slice(&fs::read(marker)?)?;
            out.push(CompleteVersion {
                version: marker.version,
                digest: marker.digest,
                path,
            });
        }
        Ok(out)
    }

    fn read_pin(&self, app_id: &str) -> Result<Option<Pin>> {
        let path = self.app_dir(app_id).join(PIN_FILE);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
    }

    fn clear_pin(&self, app_id: &str) -> Result<()> {
        let path = self.app_dir(app_id).join(PIN_FILE);
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    fn app_dir(&self, app_id: &str) -> PathBuf {
        self.root.join(APPS_DIR).join(sanitize(app_id))
    }

    fn watermarks_dir(&self, app_id: &str) -> PathBuf {
        self.root.join(WATERMARKS_DIR).join(sanitize(app_id))
    }
}

struct CompleteVersion {
    version: String,
    digest: String,
    path: PathBuf,
}

fn version_dir_name(manifest: &Manifest, digest: &str) -> String {
    format!(
        "{}{}-{}",
        VERSION_PREFIX,
        sanitize(&manifest.version),
        &digest[..DIGEST_PREFIX_LEN.min(digest.len())]
    )
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .replace("..", "__")
}

fn unique_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", unique_suffix()));
    write_synced(&tmp, bytes)?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(err) if path.exists() => {
            fs::remove_file(path).map_err(|_| MiniError::Io(err))?;
            fs::rename(&tmp, path)?;
            Ok(())
        }
        Err(err) => Err(MiniError::Io(err)),
    }
}

fn sync_dir(path: &Path) {
    // Directory fsync is a no-op on Windows; ignore errors on platforms where
    // opening a directory handle is not supported.
    if let Ok(handle) = fs::File::open(path) {
        let _ = handle.sync_all();
    }
}
