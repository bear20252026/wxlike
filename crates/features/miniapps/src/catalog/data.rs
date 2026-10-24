//! Ports for cross-device install records and confirmed data removal.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::recent::SyncError;
use crate::packages::install::InstalledApp;

pub const MAX_DEVICE_ID_LEN: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WipedData {
    pub storage_keys: usize,
    pub bytes: u64,
}

/// Per-account install ledger (G02). Keeping the same app on two devices
/// must show up as two records, not two independent local truths.
pub trait InstallLedger {
    fn record(&mut self, device_id: &str, app: &InstalledApp) -> Result<(), SyncError>;
    fn remove(&mut self, device_id: &str, app_id: &str) -> Result<(), SyncError>;
    fn devices(&mut self, app_id: &str) -> Result<Vec<String>, SyncError>;
}

/// Everything a guest owns locally (storage rows, cached files). Uninstall
/// must call this before dropping the install directory.
pub trait DataWiper {
    fn wipe(&mut self, app_id: &str) -> Result<WipedData, String>;
}

#[derive(Clone, Default)]
pub struct MemoryInstallLedger {
    rows: Arc<Mutex<BTreeMap<String, BTreeMap<String, String>>>>,
}

impl MemoryInstallLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn version_on(&self, app_id: &str, device_id: &str) -> Option<String> {
        self.rows
            .lock()
            .unwrap()
            .get(app_id)
            .and_then(|devices| devices.get(device_id))
            .cloned()
    }
}

impl InstallLedger for MemoryInstallLedger {
    fn record(&mut self, device_id: &str, app: &InstalledApp) -> Result<(), SyncError> {
        if device_id.is_empty() || device_id.len() > MAX_DEVICE_ID_LEN {
            return Err(SyncError::Store("invalid device id".into()));
        }
        self.rows
            .lock()
            .unwrap()
            .entry(app.app_id.clone())
            .or_default()
            .insert(device_id.to_string(), app.version.clone());
        Ok(())
    }

    fn remove(&mut self, device_id: &str, app_id: &str) -> Result<(), SyncError> {
        let mut rows = self.rows.lock().unwrap();
        if let Some(devices) = rows.get_mut(app_id) {
            devices.remove(device_id);
            if devices.is_empty() {
                rows.remove(app_id);
            }
        }
        Ok(())
    }

    fn devices(&mut self, app_id: &str) -> Result<Vec<String>, SyncError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .get(app_id)
            .map(|devices| devices.keys().cloned().collect())
            .unwrap_or_default())
    }
}

/// In-memory guest data keyed by app. Other apps' rows must survive an
/// uninstall of one app.
#[derive(Clone, Default)]
pub struct MemoryData {
    rows: Arc<Mutex<BTreeMap<String, BTreeMap<String, usize>>>>,
}

impl MemoryData {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&self, app_id: &str, key: &str, bytes: usize) {
        self.rows
            .lock()
            .unwrap()
            .entry(app_id.to_string())
            .or_default()
            .insert(key.to_string(), bytes);
    }

    pub fn keys(&self, app_id: &str) -> Vec<String> {
        self.rows
            .lock()
            .unwrap()
            .get(app_id)
            .map(|keys| keys.keys().cloned().collect())
            .unwrap_or_default()
    }
}

impl DataWiper for MemoryData {
    fn wipe(&mut self, app_id: &str) -> Result<WipedData, String> {
        let rows = self.rows.lock().unwrap().remove(app_id).unwrap_or_default();
        Ok(WipedData {
            storage_keys: rows.len(),
            bytes: rows.values().map(|size| *size as u64).sum(),
        })
    }
}
