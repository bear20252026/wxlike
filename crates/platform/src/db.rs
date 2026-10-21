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
        db.migrate_security_columns()?;
        db.migrate_p4()?;
        db.migrate_communities()?;
        Ok(db)
    }

    /// P4 表：人机校验、恢复码、对账、举报工单、秘密聊天设置、审核挂起队列，
    /// 以及会话的设备列（设备管理页要按设备下线）。
    fn migrate_p4(&self) -> Result<()> {
        let c = self.lock();
        c.execute_batch(
            r#"
            -- 人机校验（captcha）：只存答案摘要，一次性使用
            CREATE TABLE IF NOT EXISTS captcha (
                id          TEXT PRIMARY KEY,
                answer_hash TEXT NOT NULL,
                created_at  INTEGER NOT NULL,
                expires_at  INTEGER NOT NULL,
                used        INTEGER NOT NULL DEFAULT 0
            );

            -- 恢复码：一次性，摘要存储
            CREATE TABLE IF NOT EXISTS recovery_codes (
                user_id    TEXT PRIMARY KEY,
                code_hash  TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                used_at    INTEGER
            );

            -- 对账运行记录（每次 reconcile 一行，便于看趋势与追责）
            CREATE TABLE IF NOT EXISTS reconcile_runs (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                at_ms         INTEGER NOT NULL,
                users_checked INTEGER NOT NULL,
                wallet_sum    INTEGER NOT NULL,
                escrow        INTEGER NOT NULL,
                supply        INTEGER NOT NULL,
                funded_total  INTEGER NOT NULL,
                mismatches    INTEGER NOT NULL,
                detail        TEXT NOT NULL,
                ok            INTEGER NOT NULL
            );

            -- 举报工单（状态机在 wxlike-report，这里只持久化）
            CREATE TABLE IF NOT EXISTS reports (
                id           TEXT PRIMARY KEY,
                reporter     TEXT NOT NULL,
                target_kind  TEXT NOT NULL,
                target_id    TEXT NOT NULL,
                reason       TEXT NOT NULL,
                detail       TEXT NOT NULL,
                state        TEXT NOT NULL,
                action_json  TEXT,
                handler      TEXT,
                created_at   INTEGER NOT NULL,
                updated_at   INTEGER NOT NULL,
                history_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_reports_reporter ON reports(reporter, created_at);
            CREATE INDEX IF NOT EXISTS idx_reports_state ON reports(state, created_at);

            -- 秘密聊天：会话级消失消息 TTL（0/缺行 = 关闭）
            CREATE TABLE IF NOT EXISTS secret_settings (
                conv_id    TEXT PRIMARY KEY,
                enabled    INTEGER NOT NULL DEFAULT 1,
                ttl_ms     INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL
            );

            -- 秘密聊天：安全码核对状态（每会话每用户一行）
            CREATE TABLE IF NOT EXISTS secret_verifications (
                conv_id       TEXT NOT NULL,
                user_id       TEXT NOT NULL,
                peer_identity TEXT NOT NULL,
                verified_at   INTEGER NOT NULL,
                PRIMARY KEY (conv_id, user_id)
            );

            -- 先审后发：挂起待审的消息（审核通过后才允许投递）
            CREATE TABLE IF NOT EXISTS moderation_queue (
                id         TEXT PRIMARY KEY,
                conv_id    TEXT NOT NULL DEFAULT '',
                sender     TEXT NOT NULL,
                text       TEXT NOT NULL,
                labels     TEXT NOT NULL,
                decision   TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_modqueue_created ON moderation_queue(created_at);

            -- P5 直播会话（信令/弹幕独立于频道时间线，ADR-13）
            CREATE TABLE IF NOT EXISTS live_sessions (
                id           TEXT PRIMARY KEY,
                channel_id   TEXT NOT NULL,
                host         TEXT NOT NULL,
                stream_key   TEXT NOT NULL UNIQUE,
                state        TEXT NOT NULL,
                recording    INTEGER NOT NULL DEFAULT 0,
                started_at   INTEGER NOT NULL,
                ended_at     INTEGER,
                viewers      INTEGER NOT NULL DEFAULT 0,
                peak_viewers INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_live_channel ON live_sessions(channel_id, state);

            -- 在房观众：弹幕收件集来源
            CREATE TABLE IF NOT EXISTS live_viewers (
                session_id TEXT NOT NULL,
                user_id    TEXT NOT NULL,
                joined_at  INTEGER NOT NULL,
                PRIMARY KEY (session_id, user_id)
            );
            "#,
        )?;

        // 会话表补设备列（设备管理页：按设备下线、最近活跃）
        let mut have = Vec::new();
        {
            let mut stmt = c.prepare("PRAGMA table_info(sessions)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            for name in rows {
                have.push(name?);
            }
        }
        for (col, ddl) in [
            ("sid", "TEXT"),
            ("device_id", "TEXT NOT NULL DEFAULT ''"),
            ("device_name", "TEXT NOT NULL DEFAULT ''"),
            ("ip", "TEXT NOT NULL DEFAULT ''"),
            ("last_seen", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            if !have.iter().any(|name| name == col) {
                c.execute_batch(&format!("ALTER TABLE sessions ADD COLUMN {col} {ddl};"))?;
            }
        }
        // 老会话没有 sid：补一个（12 hex），保证设备页能寻址
        let missing: Vec<String> = {
            let mut stmt = c.prepare("SELECT token FROM sessions WHERE sid IS NULL OR sid = ''")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        for token in missing {
            let sid = crate::auth::random_hex(6);
            c.execute(
                "UPDATE sessions SET sid = ?1 WHERE token = ?2",
                rusqlite::params![sid, token],
            )?;
        }
        Ok(())
    }

    /// Zone C · C1/C3：频道级角色表 + 话题订阅的静音列。
    ///
    /// `channel_role` 是**新表**，直接建；`thread_subs.muted` 走"老库原地补列"
    /// （与 `migrate_groups_admin` 同一手法）：没有旧行要搬迁，缺列就是缺功能。
    ///
    /// 角色名写字符串而不是复用 `community_members` 的整数编号：社区级三档
    /// （0 成员 / 1 群主 / 2 管理员）里**没有 guest**，而频道级必须能表达
    /// "只看不能传附件"。整数字典两套并存迟早会有人拿错，字符串自带可读性。
    fn migrate_communities(&self) -> Result<()> {
        let c = self.lock();
        c.execute_batch(
            r#"
            -- 频道级角色：有行才覆盖社区角色，无行则继承（见 communities.rs）
            CREATE TABLE IF NOT EXISTS channel_role (
                channel_id TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                role       TEXT NOT NULL CHECK (role IN ('owner','moderator','member','guest')),
                granted_by TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                PRIMARY KEY (channel_id, user_id)
            );
            CREATE INDEX IF NOT EXISTS idx_channel_role_user
                ON channel_role(user_id, channel_id);
            "#,
        )?;

        let mut have = Vec::new();
        {
            let mut stmt = c.prepare("PRAGMA table_info(thread_subs)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            for name in rows {
                have.push(name?);
            }
        }
        // 目前只有一个待补列。单元素 `for` 会被 clippy 拦（single_element_loop），
        // 直接展开；将来要补第二列时再改成 `vec![...]` 遍历。
        let (col, ddl) = ("muted", "INTEGER NOT NULL DEFAULT 0");
        if !have.iter().any(|name| name == col) {
            c.execute_batch(&format!("ALTER TABLE thread_subs ADD COLUMN {col} {ddl};"))?;
        }
        Ok(())
    }

    fn migrate(&self) -> Result<()> {
        let c = self.lock();
        c.execute_batch(
            r#"
            -- 幂等重放：agent 会重试，同一个 key 必须返回第一次的结果。
            -- 只存"写操作"的结果，读操作不需要。
            CREATE TABLE IF NOT EXISTS idempotency (
                key        TEXT PRIMARY KEY,
                response   TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS conv_owner (
                conv_id    TEXT PRIMARY KEY,
                owner_type TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS users (
                id           TEXT PRIMARY KEY,
                name         TEXT NOT NULL UNIQUE,
                pw_salt      TEXT NOT NULL,
                pw_hash      TEXT NOT NULL,
                pw_rounds    INTEGER NOT NULL,
                pw_algo      TEXT NOT NULL DEFAULT 'pbkdf2',
                totp_secret  TEXT,
                totp_enabled INTEGER NOT NULL DEFAULT 0,
                avatar       TEXT NOT NULL DEFAULT '',
                signature    TEXT NOT NULL DEFAULT '',
                created_at   INTEGER NOT NULL
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

            -- P3 支柱 C：社区/频道/话题（schema 形状借用 TangSengDaoDaoServer
            -- 的群/成员表：角色 0 成员 1 群主 2 管理员；tenant_id 预留列）
            CREATE TABLE IF NOT EXISTS communities (
                id           TEXT PRIMARY KEY,
                name         TEXT NOT NULL,
                owner_uid    TEXT NOT NULL REFERENCES users(id),
                member_limit INTEGER NOT NULL DEFAULT 10000,
                announcement TEXT NOT NULL DEFAULT '',
                tenant_id    TEXT NOT NULL DEFAULT 'default',
                created_at   INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS community_members (
                community_id TEXT NOT NULL REFERENCES communities(id) ON DELETE CASCADE,
                user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                role         INTEGER NOT NULL DEFAULT 0,
                joined_at    INTEGER NOT NULL,
                PRIMARY KEY (community_id, user_id)
            );
            CREATE TABLE IF NOT EXISTS channels (
                id           TEXT PRIMARY KEY,
                community_id TEXT NOT NULL REFERENCES communities(id) ON DELETE CASCADE,
                name         TEXT NOT NULL,
                kind         TEXT NOT NULL DEFAULT 'text',
                tenant_id    TEXT NOT NULL DEFAULT 'default',
                created_at   INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_channels_community ON channels(community_id);
            CREATE TABLE IF NOT EXISTS threads (
                id         TEXT PRIMARY KEY,
                channel_id TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
                root_seq   INTEGER NOT NULL,
                title      TEXT NOT NULL DEFAULT '',
                created_by TEXT NOT NULL REFERENCES users(id),
                tenant_id  TEXT NOT NULL DEFAULT 'default',
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_threads_channel ON threads(channel_id);
            CREATE TABLE IF NOT EXISTS thread_messages (
                thread_id  TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                seq        INTEGER PRIMARY KEY AUTOINCREMENT,
                sender     TEXT NOT NULL,
                text       TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS thread_subs (
                thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                user_id   TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                PRIMARY KEY (thread_id, user_id)
            );

            -- P3 多级免打扰（ADR-19 决策层）：每用户 × 每会话
            CREATE TABLE IF NOT EXISTS notification_settings (
                user_id  TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                conv_id  TEXT NOT NULL,
                level    TEXT NOT NULL DEFAULT 'all',
                keywords TEXT NOT NULL DEFAULT '[]',
                PRIMARY KEY (user_id, conv_id)
            );

            -- P3 审计日志（不变式 7：与运行日志——tracing/Loki——物理分离，
            -- 这里是独立审计表；检索面随 P6 adminx 落地）
            CREATE TABLE IF NOT EXISTS audit_log (
                id     INTEGER PRIMARY KEY AUTOINCREMENT,
                at_ms  INTEGER NOT NULL,
                actor  TEXT NOT NULL DEFAULT '',
                action TEXT NOT NULL,
                target TEXT NOT NULL DEFAULT '',
                detail TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS idx_audit_actor ON audit_log(actor, at_ms);
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

    /// P4 安全列（pw_algo/totp_*）：老库原地补列。
    fn migrate_security_columns(&self) -> Result<()> {
        let c = self.lock();
        let mut have = Vec::new();
        {
            let mut stmt = c.prepare("PRAGMA table_info(users)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            for name in rows {
                have.push(name?);
            }
        }
        for (col, ddl) in [
            ("pw_algo", "TEXT NOT NULL DEFAULT 'pbkdf2'"),
            ("totp_secret", "TEXT"),
            ("totp_enabled", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            if !have.iter().any(|name| name == col) {
                c.execute_batch(&format!("ALTER TABLE users ADD COLUMN {col} {ddl};"))?;
            }
        }
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
