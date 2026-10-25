//! The reviewed catalog is the only source of installable apps.
//!
//! Categories, search and recommendations come from the server-side
//! published catalog (G01/G02). This module defines the port, the query
//! shape and an in-memory double for tests and the offline demo; production
//! wiring replaces the double with the HTTP client.

use serde::{Deserialize, Serialize};

pub const DEFAULT_SEARCH_LIMIT: usize = 20;
pub const MAX_SEARCH_LIMIT: usize = 50;
pub const MAX_QUERY_CHARS: usize = 64;
pub const MAX_PUBLISHED_VERSIONS: usize = 32;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Productivity,
    Shopping,
    Games,
    Tools,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntry {
    pub app_id: String,
    pub name: String,
    /// Latest reviewed version, shown in the catalog list.
    pub version: String,
    /// Versions a user may still install (newest first). Empty is invalid.
    #[serde(default)]
    pub published_versions: Vec<String>,
    pub category: Category,
    pub summary: String,
    pub publisher: String,
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub featured: bool,
}

impl CatalogEntry {
    pub fn publishes(&self, version: &str) -> bool {
        if self.published_versions.is_empty() {
            self.version == version
        } else {
            self.published_versions.iter().any(|v| v == version)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchQuery {
    pub text: String,
    pub category: Option<Category>,
    pub limit: usize,
}

impl SearchQuery {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            category: None,
            limit: DEFAULT_SEARCH_LIMIT,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    #[error("query is invalid: {0}")]
    BadQuery(&'static str),
    #[error("catalog service is offline")]
    Offline,
    #[error("catalog service timed out")]
    Timeout,
    #[error("catalog request is not authorized")]
    Unauthorized,
    #[error("app {0:?} is not in the reviewed catalog")]
    NotFound(String),
}

pub trait CatalogSource {
    fn search(&mut self, query: &SearchQuery) -> Result<Vec<CatalogEntry>, SourceError>;
    fn recommend(&mut self, limit: usize) -> Result<Vec<CatalogEntry>, SourceError>;
    fn fetch(&mut self, app_id: &str) -> Result<CatalogEntry, SourceError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceFailure {
    Offline,
    Timeout,
    Unauthorized,
}

/// In-memory stand-in for the published catalog. `broken` injects a
/// transport fault; the catalog logic itself is the real code under test.
pub struct ReviewedCatalog {
    entries: Vec<CatalogEntry>,
    failure: Option<SourceFailure>,
}

impl ReviewedCatalog {
    pub fn new(entries: Vec<CatalogEntry>) -> Self {
        Self {
            entries,
            failure: None,
        }
    }

    pub fn broken(entries: Vec<CatalogEntry>, failure: SourceFailure) -> Self {
        Self {
            entries,
            failure: Some(failure),
        }
    }

    fn healthy(&self) -> Result<(), SourceError> {
        match self.failure {
            None => Ok(()),
            Some(SourceFailure::Offline) => Err(SourceError::Offline),
            Some(SourceFailure::Timeout) => Err(SourceError::Timeout),
            Some(SourceFailure::Unauthorized) => Err(SourceError::Unauthorized),
        }
    }

    fn sorted(mut hits: Vec<CatalogEntry>, limit: usize) -> Vec<CatalogEntry> {
        hits.sort_by(|a, b| {
            b.featured
                .cmp(&a.featured)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.app_id.cmp(&b.app_id))
        });
        hits.truncate(limit);
        hits
    }
}

impl CatalogSource for ReviewedCatalog {
    fn search(&mut self, query: &SearchQuery) -> Result<Vec<CatalogEntry>, SourceError> {
        self.healthy()?;
        let needle = query.text.to_lowercase();
        let hits = self
            .entries
            .iter()
            .filter(|entry| query.category.map(|c| entry.category == c).unwrap_or(true))
            .filter(|entry| {
                needle.is_empty()
                    || entry.name.to_lowercase().contains(&needle)
                    || entry.summary.to_lowercase().contains(&needle)
                    || entry.app_id.to_lowercase().contains(&needle)
            })
            .cloned()
            .collect();
        Ok(Self::sorted(hits, query.limit))
    }

    fn recommend(&mut self, limit: usize) -> Result<Vec<CatalogEntry>, SourceError> {
        self.healthy()?;
        Ok(Self::sorted(self.entries.clone(), limit))
    }

    fn fetch(&mut self, app_id: &str) -> Result<CatalogEntry, SourceError> {
        self.healthy()?;
        self.entries
            .iter()
            .find(|entry| entry.app_id == app_id)
            .cloned()
            .ok_or_else(|| SourceError::NotFound(app_id.into()))
    }
}
