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

            CREATE TABLE IF NOT EXISTS friend_requests (
                id          TEXT PRIMARY KEY,
                from_uid    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                to_uid      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                message     TEXT NOT NULL DEFAULT '',
                status      TEXT NOT NULL DEFAULT 'pending',
                created_at  INTEGER NOT NULL,
                UNIQUE(from_uid, to_uid)
            );
            CREATE INDEX IF NOT EXISTS idx_fr_to ON friend_requests(to_uid, status);
            CREATE INDEX IF NOT EXISTS idx_fr_from ON friend_requests(from_uid, status);

            CREATE TABLE IF NOT EXISTS friends (
                user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                friend_id   TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                remark      TEXT NOT NULL DEFAULT '',
                source      TEXT NOT NULL DEFAULT '',
                created_at  INTEGER NOT NULL,
                PRIMARY KEY (user_id, friend_id)
            );

            -- Append-only per-user contact log: the cursor source for
            -- /api/friends/changes (address-book sync).
            CREATE TABLE IF NOT EXISTS contact_log (
                seq         INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                subject_id  TEXT NOT NULL,
                kind        TEXT NOT NULL,
                changed_at  INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_cl_user ON contact_log(user_id, seq);

            CREATE TABLE IF NOT EXISTS groups (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL,
                owner_uid   TEXT NOT NULL REFERENCES users(id),
                created_at  INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS group_members (
                group_id    TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
                user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                role        TEXT NOT NULL DEFAULT 'member',
                my_remark   TEXT NOT NULL DEFAULT '',
                joined_at   INTEGER NOT NULL,
                PRIMARY KEY (group_id, user_id)
            );
            CREATE INDEX IF NOT EXISTS idx_gm_user ON group_members(user_id);

            CREATE TABLE IF NOT EXISTS media (
                id            TEXT PRIMARY KEY,
                size          INTEGER NOT NULL,
                content_type  TEXT NOT NULL DEFAULT 'application/octet-stream',
                uploader      TEXT NOT NULL DEFAULT '',
                has_thumb     INTEGER NOT NULL DEFAULT 0,
                created_at    INTEGER NOT NULL
            );

            -- WP-09: a virtual ledger only. Amounts are integer minor units
            -- (分), never floats, so conservation is exact.
            CREATE TABLE IF NOT EXISTS wallets (
                user_id   TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
                balance   INTEGER NOT NULL DEFAULT 0 CHECK (balance >= 0)
            );
            CREATE TABLE IF NOT EXISTS ledger (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                kind         TEXT NOT NULL,
                amount       INTEGER NOT NULL,
                counterparty TEXT NOT NULL DEFAULT '',
                ref_id       TEXT NOT NULL DEFAULT '',
                memo         TEXT NOT NULL DEFAULT '',
                created_at   INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_ledger_user ON ledger(user_id, id);
            CREATE TABLE IF NOT EXISTS redpackets (
                id            TEXT PRIMARY KEY,
                from_uid      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                conv_id       TEXT NOT NULL DEFAULT '',
                total         INTEGER NOT NULL,
                count         INTEGER NOT NULL,
                claimed       INTEGER NOT NULL DEFAULT 0,
                claimed_total INTEGER NOT NULL DEFAULT 0,
                claimers      TEXT NOT NULL DEFAULT '[]',
                memo          TEXT NOT NULL DEFAULT '',
                status        TEXT NOT NULL DEFAULT 'open',
                created_at    INTEGER NOT NULL,
                expire_at     INTEGER NOT NULL
            );
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
