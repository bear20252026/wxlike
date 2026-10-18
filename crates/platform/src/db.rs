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
        let mut conn = match path {
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
        // WP-13 slow-query log: SQLite itself times every statement, so the
        // threshold applies to what actually ran, not to lock acquisition.
        conn.profile(Some(slow_sql_log));
        let db = Self {
            inner: Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        db.migrate_friends_meta()?;
        db.migrate_groups_admin()?;
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

            -- P3 ADR-24 拉黑：前置闸门（申请/陌生人请求/hub 收件过滤共用）
            CREATE TABLE IF NOT EXISTS blocks (
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                blocked_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (user_id, blocked_id)
            );

            -- P3 ADR-24 陌生人消息请求箱（不变式 9）：非好友首条消息不产生会话
            CREATE TABLE IF NOT EXISTS message_requests (
                id         TEXT PRIMARY KEY,
                from_uid   TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                to_uid     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                text       TEXT NOT NULL DEFAULT '',
                status     TEXT NOT NULL DEFAULT 'pending',
                created_at INTEGER NOT NULL,
                UNIQUE (from_uid, to_uid, status)
            );
            CREATE INDEX IF NOT EXISTS idx_mr_to ON message_requests(to_uid, status);

            -- P3 用户设置：是否允许陌生人发起（默认允许；流程必须存在）
            CREATE TABLE IF NOT EXISTS user_settings (
                user_id         TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
                allow_strangers INTEGER NOT NULL DEFAULT 1
            );

            CREATE TABLE IF NOT EXISTS groups (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL,
                owner_uid   TEXT NOT NULL REFERENCES users(id),
                created_at  INTEGER NOT NULL
            );
            -- P3 群管理补齐列（老库原地补列见 migrate_groups_admin）
            -- announcement/approval/all_muted

            -- P3 群公告（单条置顶）
            CREATE TABLE IF NOT EXISTS group_announcement (
                group_id  TEXT PRIMARY KEY REFERENCES groups(id) ON DELETE CASCADE,
                text      TEXT NOT NULL DEFAULT '',
                media_id  TEXT NOT NULL DEFAULT '',
                pinned_by TEXT NOT NULL DEFAULT '',
                updated_at INTEGER NOT NULL
            );
            -- P3 单人禁言（until_ms 定时自然解除）
            CREATE TABLE IF NOT EXISTS group_mutes (
                group_id TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
                user_id  TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                until_ms INTEGER NOT NULL,
                PRIMARY KEY (group_id, user_id)
            );
            -- P3 邀请 token（带过期；审批开的群入群转请求）
            CREATE TABLE IF NOT EXISTS group_invites (
                token      TEXT PRIMARY KEY,
                group_id   TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
                created_by TEXT NOT NULL REFERENCES users(id),
                expires_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_gi_group ON group_invites(group_id);
            -- P3 入群审批请求
            CREATE TABLE IF NOT EXISTS group_join_requests (
                group_id     TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
                user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                status       TEXT NOT NULL DEFAULT 'pending',
                requested_at INTEGER NOT NULL,
                PRIMARY KEY (group_id, user_id)
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

            -- WP-07 moments. `seq` is the paging cursor (monotonic, so equal
            -- timestamps cannot create duplicates or gaps); `id` is the public id.
            CREATE TABLE IF NOT EXISTS moments (
                seq         INTEGER PRIMARY KEY AUTOINCREMENT,
                id          TEXT NOT NULL UNIQUE,
                author      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                text        TEXT NOT NULL DEFAULT '',
                media       TEXT NOT NULL DEFAULT '[]',
                visibility  TEXT NOT NULL DEFAULT 'friends',
                deleted     INTEGER NOT NULL DEFAULT 0,
                created_at  INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_moments_author ON moments(author, seq);
            CREATE TABLE IF NOT EXISTS moment_likes (
                moment_id   TEXT NOT NULL REFERENCES moments(id) ON DELETE CASCADE,
                user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                created_at  INTEGER NOT NULL,
                PRIMARY KEY (moment_id, user_id)
            );
            CREATE TABLE IF NOT EXISTS moment_comments (
                seq         INTEGER PRIMARY KEY AUTOINCREMENT,
                id          TEXT NOT NULL UNIQUE,
                moment_id   TEXT NOT NULL REFERENCES moments(id) ON DELETE CASCADE,
                author      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                text        TEXT NOT NULL,
                reply_to    TEXT NOT NULL DEFAULT '',
                deleted     INTEGER NOT NULL DEFAULT 0,
                created_at  INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_mc_moment ON moment_comments(moment_id, seq);

            -- WP-12: fetchable E2EE pre-key bundles. `otks` is a JSON array of
            -- unclaimed one-time keys; claiming pops from it (see keys.rs).
            CREATE TABLE IF NOT EXISTS prekey_bundles (
                user_id          TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
                identity         TEXT NOT NULL,
                identity_signing TEXT NOT NULL DEFAULT '',
                otks             TEXT NOT NULL DEFAULT '[]',
                updated_at       INTEGER NOT NULL
            );

            -- WP-06: push targets. One row per (user, platform): a re-register
            -- on the same platform replaces the token ("keep the latest"),
            -- different platforms coexist.
            CREATE TABLE IF NOT EXISTS devices (
                user_id       TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                platform      TEXT NOT NULL,
                device_token  TEXT NOT NULL,
                last_seen     INTEGER NOT NULL,
                PRIMARY KEY (user_id, platform)
            );

            -- P1 favorites: a snapshot per favorited message (survives the
            -- original being recalled or deleted). Structured columns, not a
            -- JSON blob, so paging stays an index walk.
            CREATE TABLE IF NOT EXISTS favorites (
                id            TEXT PRIMARY KEY,
                user_id       TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                source_conv   TEXT NOT NULL DEFAULT '',
                source_msg_id TEXT NOT NULL DEFAULT '',
                kind          TEXT NOT NULL DEFAULT 'text',
                text          TEXT NOT NULL DEFAULT '',
                media_id      TEXT NOT NULL DEFAULT '',
                sender        TEXT NOT NULL DEFAULT '',
                sent_at_ms    INTEGER NOT NULL DEFAULT 0,
                created_at    INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_favorites_user ON favorites(user_id, created_at);
            "#,
        )?;
        Ok(())
    }

    /// P3 群管理列（公告/审批开关/全员禁言）：老库原地补列。
    fn migrate_groups_admin(&self) -> Result<()> {
        let c = self.lock();
        let mut have = Vec::new();
        {
            let mut stmt = c.prepare("PRAGMA table_info(groups)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            for name in rows {
                have.push(name?);
            }
        }
        for (col, ddl) in [
            ("announcement", "TEXT NOT NULL DEFAULT ''"),
            ("approval", "INTEGER NOT NULL DEFAULT 0"),
            ("all_muted", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            if !have.iter().any(|name| name == col) {
                c.execute_batch(&format!("ALTER TABLE groups ADD COLUMN {col} {ddl};"))?;
            }
        }
        Ok(())
    }

    /// P3 关系元数据列（星标/标签/分组）：老库原地补列，无重写。
    fn migrate_friends_meta(&self) -> Result<()> {
        let c = self.lock();
        let mut have = Vec::new();
        {
            let mut stmt = c.prepare("PRAGMA table_info(friends)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            for name in rows {
                have.push(name?);
            }
        }
        for (col, ddl) in [
            ("starred", "INTEGER NOT NULL DEFAULT 0"),
            ("tags", "TEXT NOT NULL DEFAULT '[]'"),
            ("friend_group", "TEXT NOT NULL DEFAULT ''"),
        ] {
            if !have.iter().any(|name| name == col) {
                c.execute_batch(&format!("ALTER TABLE friends ADD COLUMN {col} {ddl};"))?;
            }
        }
        Ok(())
    }

    /// A poisoned lock only happens if SQL panicked mid-statement; SQLite
    /// connections survive that, so recovering the guard beats crashing.
    pub fn lock(&self) -> MutexGuard<'_, Connection> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Statements slower than this land in the log with their text (truncated).
const SLOW_SQL_MS: u64 = 100;

/// Profile callback (rusqlite `trace` feature). A plain fn, as required.
fn slow_sql_log(sql: &str, elapsed: std::time::Duration) {
    if elapsed.as_millis() >= SLOW_SQL_MS as u128 {
        let text: String = sql.chars().take(160).collect();
        tracing::warn!(elapsed_ms = elapsed.as_millis() as u64, stmt = %text, "slow sql");
    }
}
