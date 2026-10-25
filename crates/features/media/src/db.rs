//! SQLite metadata plane for media.
//!
//! One connection behind a mutex, every statement parameterized. Callers drop
//! the guard before doing filesystem or child-process work, so a long transcode
//! never blocks the control plane.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::Connection;

use crate::error::Result;

/// The platform's proposal is the single source of truth for the schema; the
/// crate embeds it so an empty temp database migrates exactly like production.
pub const SCHEMA_SQL: &str = include_str!("../../../../schema/proposals/G04/0001_media.sql");
pub const BUSY_TIMEOUT_MS: u64 = 5000;

#[derive(Clone)]
pub struct MediaDb {
    inner: Arc<Mutex<Connection>>,
}

impl MediaDb {
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::from_connection(conn)
    }

    /// `None` means in-memory. A path creates parent directories first.
    pub fn open(path: Option<&str>) -> Result<Self> {
        let Some(path) = path else {
            return Self::open_memory();
        };
        if let Some(dir) = std::path::Path::new(path).parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        Self::from_connection(Connection::open(path)?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;",
        )?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn lock(&self) -> MutexGuard<'_, Connection> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub fn now_ms() -> i64 {
    wxlike_core::types::now_ms() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_idempotent_and_has_the_expected_tables() {
        let db = MediaDb::open_memory().unwrap();
        db.lock().execute_batch(SCHEMA_SQL).unwrap();
        let tables: Vec<String> = {
            let c = db.lock();
            let mut stmt = c
                .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
                .unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap()
        };
        for expected in [
            "media_object",
            "media_ref",
            "media_upload",
            "media_upload_chunk",
            "media_file",
            "media_share",
            "media_job",
            "media_backup",
            "media_backup_entry",
        ] {
            assert!(tables.iter().any(|t| t == expected), "missing {expected}");
        }
    }
}
