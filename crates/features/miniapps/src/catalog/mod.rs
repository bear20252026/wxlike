//! Reviewed catalog: search, recents, favorites, install/upgrade/uninstall
//! and takedown, wired to the same signed-package installer as G08-01.
//!
//! The catalog never invents an app card: every card comes from the
//! server-reviewed source, and every install/upgrade must still verify the
//! package signature and match the reviewed capabilities. Personal data
//! (search text, recents) stays out of audit lines.

pub mod data;
pub mod recent;
pub mod source;
pub mod takedown;
pub mod upgrade;

use std::collections::{BTreeMap, BTreeSet};

use crate::packages::format::verify;
use crate::packages::install::{InstallOutcome, InstalledApp, Installer};
use crate::packages::manifest::{parse_semver, MAX_APP_ID_LEN};
use crate::packages::trust::PackageSignature;
use crate::packages::MiniError;

use data::{DataWiper, InstallLedger, WipedData, MAX_DEVICE_ID_LEN};
use recent::{RecentStore, RecentSync, RecentView, SyncError, SyncState, MAX_RECENT_LIMIT};
use source::{
    CatalogEntry, CatalogSource, SearchQuery, SourceError, MAX_QUERY_CHARS, MAX_SEARCH_LIMIT,
};
use takedown::{RuntimeAuthority, Takedown, TakedownReport};
use upgrade::UpgradePlan;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserDecision {
    Confirm,
    Deny,
    Cancel,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("invalid catalog request: {0}")]
    BadQuery(&'static str),
    #[error("catalog source: {0}")]
    Source(#[from] SourceError),
    #[error("version {1} of {0:?} is not published")]
    NotPublished(String, String),
    #[error("app {0:?} is withdrawn")]
    Revoked(String),
    #[error("app {0:?} is not installed")]
    NotInstalled(String),
    #[error("capability {1:?} of {0:?} was not reviewed")]
    CapabilityNotReviewed(String, String),
    #[error("operation on {0:?} was declined")]
    Denied(String),
    #[error("operation on {0:?} was cancelled")]
    Cancelled(String),
    #[error("recent sync failed: {0}")]
    Sync(#[from] SyncError),
    #[error("could not wipe app data: {0}")]
    DataWipe(String),
    #[error("package error: {0}")]
    Package(#[from] MiniError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchTicket {
    pub app_id: String,
    pub version: String,
    pub digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpgradeOutcome {
    pub plan: UpgradePlan,
    pub installed: InstalledApp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UninstallReport {
    pub app_id: String,
    pub version: String,
    pub wiped: WipedData,
}

/// Audit line: operation and app id only. Search text, takedown reasons and
/// storage keys are deliberately absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogAudit {
    pub operation: &'static str,
    pub app_id: Option<String>,
    pub outcome: &'static str,
}

pub struct Catalog {
    source: Box<dyn CatalogSource>,
    recents: Box<dyn RecentStore>,
    sync: Option<(String, Box<dyn RecentSync>)>,
    favorites: BTreeSet<String>,
    /// `None` value = every version withdrawn; `Some(set)` = those versions.
    withdrawn: BTreeMap<String, Option<BTreeSet<String>>>,
    audit: Vec<CatalogAudit>,
}

impl Catalog {
    pub fn new(source: Box<dyn CatalogSource>, recents: Box<dyn RecentStore>) -> Self {
        Self {
            source,
            recents,
            sync: None,
            favorites: BTreeSet::new(),
            withdrawn: BTreeMap::new(),
            audit: Vec::new(),
        }
    }

    pub fn enable_sync(
        &mut self,
        account: &str,
        sync: Box<dyn RecentSync>,
    ) -> Result<(), CatalogError> {
        if account.is_empty() || account.len() > MAX_DEVICE_ID_LEN {
            return Err(CatalogError::BadQuery("sync account"));
        }
        self.sync = Some((account.to_string(), sync));
        Ok(())
    }

    pub fn audit(&self) -> &[CatalogAudit] {
        &self.audit
    }

    pub fn search(&mut self, query: &SearchQuery) -> Result<Vec<CatalogEntry>, CatalogError> {
        validate_query(query)?;
        let hits = self.source.search(query)?;
        Ok(self.visible(hits))
    }

    pub fn recommend(&mut self, limit: usize) -> Result<Vec<CatalogEntry>, CatalogError> {
        validate_limit(limit)?;
        let hits = self.source.recommend(limit)?;
        Ok(self.visible(hits))
    }

    pub fn set_favorite(&mut self, app_id: &str, favorite: bool) -> Result<bool, CatalogError> {
        validate_app_id(app_id)?;
        let changed = if favorite {
            self.favorites.insert(app_id.to_string())
        } else {
            self.favorites.remove(app_id)
        };
        self.record("favorite", Some(app_id.to_string()), "ok");
        Ok(changed)
    }

    pub fn favorites(&self) -> Vec<String> {
        self.favorites.iter().cloned().collect()
    }

    pub fn recents(&mut self, limit: usize) -> Result<RecentView, CatalogError> {
        if !(1..=MAX_RECENT_LIMIT).contains(&limit) {
            return Err(CatalogError::BadQuery("recent limit"));
        }
        let pulled = self.sync.as_mut().map(|(account, sync)| sync.pull(account));
        let sync_state = match pulled {
            None => SyncState::LocalOnly,
            Some(Ok(entries)) => {
                for entry in entries {
                    self.recents.upsert(entry)?;
                }
                SyncState::Synced
            }
            Some(Err(err)) => SyncState::Degraded(err.as_str()),
        };
        let mut entries = self.recents.list(limit)?;
        entries.retain(|entry| !self.is_withdrawn(&entry.app_id, None));
        Ok(RecentView {
            entries,
            sync: sync_state,
        })
    }

    /// Record a successful launch. Sync failure degrades to local-only
    /// instead of losing the entry.
    pub fn record_launch(
        &mut self,
        installed: &InstalledApp,
        now: u64,
    ) -> Result<SyncState, CatalogError> {
        let entry = self.recents.touch(&installed.app_id, now)?;
        let state = match self.sync.as_mut() {
            None => SyncState::LocalOnly,
            Some((account, sync)) => match sync.push(account, &entry) {
                Ok(()) => SyncState::Synced,
                Err(err) => SyncState::Degraded(err.as_str()),
            },
        };
        self.record("launch", Some(installed.app_id.clone()), "ok");
        Ok(state)
    }

    /// Open an app from the recents list. The ticket names the installed
    /// version and is refused for a withdrawn app.
    pub fn restore(
        &mut self,
        installs: &Installer,
        app_id: &str,
        now: u64,
    ) -> Result<LaunchTicket, CatalogError> {
        validate_app_id(app_id)?;
        let installed = installs
            .current(app_id)
            .map_err(|_| CatalogError::NotInstalled(app_id.to_string()))?;
        let ticket = self.authorize_launch(&installed)?;
        self.record_launch(&installed, now)?;
        Ok(ticket)
    }

    pub fn authorize_launch(&self, installed: &InstalledApp) -> Result<LaunchTicket, CatalogError> {
        if self.is_withdrawn(&installed.app_id, Some(&installed.version)) {
            return Err(CatalogError::Revoked(installed.app_id.clone()));
        }
        Ok(LaunchTicket {
            app_id: installed.app_id.clone(),
            version: installed.version.clone(),
            digest: installed.digest.clone(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn install(
        &mut self,
        installs: &mut Installer,
        ledger: &mut dyn InstallLedger,
        device_id: &str,
        blob: &[u8],
        sig: &PackageSignature,
        now: u64,
    ) -> Result<InstallOutcome, CatalogError> {
        validate_device(device_id)?;
        let verified = verify(blob, sig, installs.trust(), now, installs.host_version())?;
        self.ensure_reviewed(&verified)?;
        let outcome = installs.install_verified(verified)?;
        let installed = installed_of(&outcome);
        ledger.record(device_id, installed)?;
        self.record("install", Some(installed.app_id.clone()), "ok");
        Ok(outcome)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upgrade(
        &mut self,
        installs: &mut Installer,
        ledger: &mut dyn InstallLedger,
        device_id: &str,
        blob: &[u8],
        sig: &PackageSignature,
        now: u64,
        decision: UserDecision,
    ) -> Result<UpgradeOutcome, CatalogError> {
        validate_device(device_id)?;
        let verified = verify(blob, sig, installs.trust(), now, installs.host_version())?;
        let app_id = verified.manifest.app_id.clone();
        let current = installs
            .current(&app_id)
            .map_err(|_| CatalogError::NotInstalled(app_id.clone()))?;
        let plan = UpgradePlan::between(&current, &verified.manifest);
        if plan.requires_confirmation() && decision != UserDecision::Confirm {
            let outcome = if decision == UserDecision::Cancel {
                "cancelled"
            } else {
                "denied"
            };
            self.record("upgrade", Some(app_id.clone()), outcome);
            return Err(match decision {
                UserDecision::Deny => CatalogError::Denied(app_id),
                _ => CatalogError::Cancelled(app_id),
            });
        }
        self.ensure_reviewed(&verified)?;
        let installed = installed_of(&installs.install_verified(verified)?).clone();
        ledger.record(device_id, &installed)?;
        self.record("upgrade", Some(app_id), "ok");
        Ok(UpgradeOutcome { plan, installed })
    }

    pub fn apply_takedown(
        &mut self,
        takedown: Takedown,
        authority: &mut dyn RuntimeAuthority,
    ) -> Result<TakedownReport, CatalogError> {
        if takedown.reason.trim().is_empty() {
            return Err(CatalogError::BadQuery("takedown reason"));
        }
        validate_app_id(&takedown.app_id)?;
        if let Some(version) = &takedown.version {
            if parse_semver(version).is_none() {
                return Err(CatalogError::BadQuery("takedown version"));
            }
        }
        let sessions = authority.revoke_app(&takedown.app_id);
        let version = takedown.version;
        self.withdraw(&takedown.app_id, version.as_deref());
        let _ = self.recents.remove(&takedown.app_id);
        self.favorites.remove(&takedown.app_id);
        self.record("takedown", Some(takedown.app_id.clone()), "ok");
        Ok(TakedownReport {
            app_id: takedown.app_id,
            version,
            sessions_revoked: sessions,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn uninstall(
        &mut self,
        installs: &mut Installer,
        ledger: &mut dyn InstallLedger,
        data: &mut dyn DataWiper,
        device_id: &str,
        app_id: &str,
        decision: UserDecision,
    ) -> Result<UninstallReport, CatalogError> {
        validate_device(device_id)?;
        validate_app_id(app_id)?;
        let installed = installs
            .current(app_id)
            .map_err(|_| CatalogError::NotInstalled(app_id.to_string()))?;
        if decision != UserDecision::Confirm {
            self.record("uninstall", Some(app_id.to_string()), "cancelled");
            return Err(match decision {
                UserDecision::Deny => CatalogError::Denied(app_id.to_string()),
                _ => CatalogError::Cancelled(app_id.to_string()),
            });
        }
        let wiped = data.wipe(app_id).map_err(CatalogError::DataWipe)?;
        if !installs.remove(app_id)? {
            return Err(CatalogError::NotInstalled(app_id.to_string()));
        }
        ledger.remove(device_id, app_id)?;
        let _ = self.recents.remove(app_id);
        self.favorites.remove(app_id);
        self.record("uninstall", Some(app_id.to_string()), "ok");
        Ok(UninstallReport {
            app_id: app_id.to_string(),
            version: installed.version,
            wiped,
        })
    }

    fn ensure_reviewed(
        &mut self,
        verified: &crate::packages::format::VerifiedPackage,
    ) -> Result<(), CatalogError> {
        let manifest = &verified.manifest;
        if self.is_withdrawn(&manifest.app_id, Some(&manifest.version)) {
            return Err(CatalogError::Revoked(manifest.app_id.clone()));
        }
        let entry = self.source.fetch(&manifest.app_id)?;
        if !entry.publishes(&manifest.version) {
            return Err(CatalogError::NotPublished(
                manifest.app_id.clone(),
                manifest.version.clone(),
            ));
        }
        for capability in &manifest.capabilities {
            if !entry.capabilities.contains(capability) {
                return Err(CatalogError::CapabilityNotReviewed(
                    manifest.app_id.clone(),
                    capability.clone(),
                ));
            }
        }
        Ok(())
    }

    fn visible(&self, hits: Vec<CatalogEntry>) -> Vec<CatalogEntry> {
        hits.into_iter()
            .filter(|entry| !self.is_withdrawn(&entry.app_id, Some(&entry.version)))
            .collect()
    }

    fn withdraw(&mut self, app_id: &str, version: Option<&str>) {
        let Some(version) = version else {
            self.withdrawn.insert(app_id.to_string(), None);
            return;
        };
        match self.withdrawn.get_mut(app_id) {
            Some(None) => {}
            Some(Some(versions)) => {
                versions.insert(version.to_string());
            }
            None => {
                self.withdrawn.insert(
                    app_id.to_string(),
                    Some(BTreeSet::from([version.to_string()])),
                );
            }
        }
    }

    fn is_withdrawn(&self, app_id: &str, version: Option<&str>) -> bool {
        match self.withdrawn.get(app_id) {
            None => false,
            Some(None) => true,
            Some(Some(versions)) => version
                .map(|candidate| versions.contains(candidate))
                .unwrap_or(false),
        }
    }

    fn record(&mut self, operation: &'static str, app_id: Option<String>, outcome: &'static str) {
        self.audit.push(CatalogAudit {
            operation,
            app_id,
            outcome,
        });
    }
}

fn installed_of(outcome: &InstallOutcome) -> &InstalledApp {
    match outcome {
        InstallOutcome::Installed(app) | InstallOutcome::AlreadyInstalled(app) => app,
    }
}

fn validate_query(query: &SearchQuery) -> Result<(), CatalogError> {
    if query.text.chars().count() > MAX_QUERY_CHARS {
        return Err(CatalogError::BadQuery("search text"));
    }
    validate_limit(query.limit)
}

fn validate_limit(limit: usize) -> Result<(), CatalogError> {
    if !(1..=MAX_SEARCH_LIMIT).contains(&limit) {
        return Err(CatalogError::BadQuery("limit"));
    }
    Ok(())
}

fn validate_device(device_id: &str) -> Result<(), CatalogError> {
    if device_id.is_empty() || device_id.len() > MAX_DEVICE_ID_LEN {
        return Err(CatalogError::BadQuery("device id"));
    }
    Ok(())
}

fn validate_app_id(app_id: &str) -> Result<(), CatalogError> {
    let valid = !app_id.is_empty()
        && app_id.len() <= MAX_APP_ID_LEN
        && app_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
        && !app_id.starts_with('.')
        && !app_id.starts_with('-');
    if valid {
        Ok(())
    } else {
        Err(CatalogError::BadQuery("app id"))
    }
}
