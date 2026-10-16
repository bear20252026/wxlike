//! SQLite access for the platform data plane.
//!
//! One `Connection` behind a `Mutex` (the task book's required shape): callers
//! lock, run fully-synchronous SQL, and drop the guard before any `await`, so
//! the guard is never held across a suspension point. Every statement is
//! parameterized — string-built SQL is banned by the architecture rules.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::Connection;

use crate::error::Result;

#[derive(Clone)]
pub struct Db {
    inner: Arc<Mutex<Connection>>,
}

impl Db {
    /// `None` opens an in-memory database (tests, single-process demos).
    pub fn open(path: Option<&str>) -> Result<Self> {
        let conn = match path {
            Some(p) => {
                if let Some(dir) = std::path::Path::new(p).parent() {
                    if !dir.as_os_str().is_empty() {
                        std::fs::create_dir_all(dir)?;
                    }
                }
                Connection::open(p)
            }
            None => Connection::open_in_memory(),
        }?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 5000;
            "#,
        )?;
        let db = Self {
            inner: Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        let c = self.lock();
        c.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS users (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL UNIQUE,
                pw_salt     TEXT NOT NULL,
                pw_hash     TEXT NOT NULL,
                pw_rounds   INTEGER NOT NULL,
                avatar      TEXT NOT NULL DEFAULT '',
                signature   TEXT NOT NULL DEFAULT '',
                created_at  INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sessions (
                token       TEXT PRIMARY KEY,
                user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                created_at  INTEGER NOT NULL,
                expires_at  INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_user ON sessions(user_id);
            "#,
        )?;
        Ok(())
    }

    /// A poisoned lock only happens if SQL panicked mid-statement; SQLite
    /// connections survive that, so recovering the guard beats crashing.
    pub fn lock(&self) -> MutexGuard<'_, Connection> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
