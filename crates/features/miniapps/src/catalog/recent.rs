//! Recent apps and optional cross-device sync.
//!
//! Recents are personal data: the catalog stores only app id, last-used
//! timestamp and a launch counter. Sync is optional; a failed pull degrades
//! to the local list instead of blocking the bundle screen.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

pub const DEFAULT_RECENT_LIMIT: usize = 20;
pub const MAX_RECENT_LIMIT: usize = 50;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecentEntry {
    pub app_id: String,
    pub last_used: u64,
    pub launches: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SyncError {
    #[error("recent-sync request is not authorized")]
    Unauthorized,
    #[error("recent-sync request timed out")]
    Timeout,
    #[error("recent-sync is offline")]
    Offline,
    #[error("recent store failed: {0}")]
    Store(String),
}

impl SyncError {
    pub fn as_str(&self) -> &'static str {
        match self {
            SyncError::Unauthorized => "unauthorized",
            SyncError::Timeout => "timeout",
            SyncError::Offline => "offline",
            SyncError::Store(_) => "store",
        }
    }
}

pub trait RecentStore {
    fn touch(&mut self, app_id: &str, now: u64) -> Result<RecentEntry, SyncError>;
    fn upsert(&mut self, entry: RecentEntry) -> Result<(), SyncError>;
    fn list(&mut self, limit: usize) -> Result<Vec<RecentEntry>, SyncError>;
    fn remove(&mut self, app_id: &str) -> Result<(), SyncError>;
}

pub trait RecentSync {
    fn push(&mut self, account: &str, entry: &RecentEntry) -> Result<(), SyncError>;
    fn pull(&mut self, account: &str) -> Result<Vec<RecentEntry>, SyncError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncState {
    LocalOnly,
    Synced,
    Degraded(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentView {
    pub entries: Vec<RecentEntry>,
    pub sync: SyncState,
}

#[derive(Default)]
pub struct MemoryRecents {
    rows: BTreeMap<String, RecentEntry>,
}

impl RecentStore for MemoryRecents {
    fn touch(&mut self, app_id: &str, now: u64) -> Result<RecentEntry, SyncError> {
        let entry = self.rows.entry(app_id.to_string()).or_insert(RecentEntry {
            app_id: app_id.to_string(),
            last_used: 0,
            launches: 0,
        });
        entry.last_used = entry.last_used.max(now);
        entry.launches = entry.launches.saturating_add(1);
        Ok(entry.clone())
    }

    fn upsert(&mut self, incoming: RecentEntry) -> Result<(), SyncError> {
        let entry = self
            .rows
            .entry(incoming.app_id.clone())
            .or_insert_with(|| incoming.clone());
        if entry.app_id != incoming.app_id {
            entry.app_id = incoming.app_id.clone();
        }
        entry.last_used = entry.last_used.max(incoming.last_used);
        entry.launches = entry.launches.max(incoming.launches);
        Ok(())
    }

    fn list(&mut self, limit: usize) -> Result<Vec<RecentEntry>, SyncError> {
        let mut entries: Vec<_> = self.rows.values().cloned().collect();
        entries.sort_by(|a, b| {
            b.last_used
                .cmp(&a.last_used)
                .then_with(|| a.app_id.cmp(&b.app_id))
        });
        entries.truncate(limit);
        Ok(entries)
    }

    fn remove(&mut self, app_id: &str) -> Result<(), SyncError> {
        self.rows.remove(app_id);
        Ok(())
    }
}

/// Shared in-memory sync service. Two catalog instances in one test can
/// clone the same handle and observe each other's pushes.
#[derive(Clone, Default)]
pub struct SharedRecents {
    rows: Arc<Mutex<BTreeMap<String, BTreeMap<String, RecentEntry>>>>,
    failure: Option<SyncError>,
}

impl SharedRecents {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_failure(failure: SyncError) -> Self {
        Self {
            rows: Arc::default(),
            failure: Some(failure),
        }
    }

    fn healthy(&self) -> Result<(), SyncError> {
        match &self.failure {
            Some(err) => Err(err.clone()),
            None => Ok(()),
        }
    }
}

impl RecentSync for SharedRecents {
    fn push(&mut self, account: &str, entry: &RecentEntry) -> Result<(), SyncError> {
        self.healthy()?;
        let mut rows = self.rows.lock().unwrap();
        let slot = rows
            .entry(account.to_string())
            .or_default()
            .entry(entry.app_id.clone())
            .or_insert_with(|| entry.clone());
        slot.last_used = slot.last_used.max(entry.last_used);
        slot.launches = slot.launches.max(entry.launches);
        Ok(())
    }

    fn pull(&mut self, account: &str) -> Result<Vec<RecentEntry>, SyncError> {
        self.healthy()?;
        let rows = self.rows.lock().unwrap();
        let mut entries: Vec<_> = rows
            .get(account)
            .map(|apps| apps.values().cloned().collect())
            .unwrap_or_default();
        entries.sort_by(|a, b| {
            b.last_used
                .cmp(&a.last_used)
                .then_with(|| a.app_id.cmp(&b.app_id))
        });
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_is_monotonic_and_counted() {
        let mut store = MemoryRecents::default();
        let first = store.touch("com.demo.app", 100).unwrap();
        assert_eq!(first.launches, 1);
        let second = store.touch("com.demo.app", 50).unwrap();
        assert_eq!(second.launches, 2);
        assert_eq!(
            second.last_used, 100,
            "clock skew must not move recency back"
        );
    }
}
