//! SQLite access for the platform data plane.
//!
//! One `Connection` behind a `Mutex` (the task book's required shape): callers
//! lock, run fully-synchronous SQL, and drop the guard before any `await`, so
//! the guard is never held across a suspension point. Every statement is
//! parameterized — string-built SQL is banned by the architecture rules.
//!
//! ADR-28 (#326) adds the second half of that rule: an `async fn` must not run
//! the synchronous SQL at all — not even without holding the guard across an
//! `await`, because `self.db().lock()` inside an axum handler occupies a tokio
//! worker for the whole transaction. Two entry points exist for that:
//!
//! * [`Db::blocking`] — for code that already has a `Db`: the closure gets
//!   `&mut Connection` **on a blocking thread**, and the whole transaction has
//!   to fit inside it;
//! * `offload`（本模块内）— for handlers that call an existing synchronous `Platform`
//!   method: the *call* moves to the blocking pool, the transaction boundary
//!   does not move at all.
//!
//! Both deliberately hand the caller a *connection*, never a guard, so nothing
//! can escape the blocking task and no transaction can be split across two of
//! them.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::Connection;

use crate::error::{PlatformError, Result};

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
        //
        // rusqlite 0.41: `Connection::profile` was removed. The replacement is
        // `trace_v2` with an event-code mask — we register only
        // `SQLITE_TRACE_PROFILE` (statement-finished events), so the callback
        // fires once per completed statement with its wall time.
        //
        // The callback signature changed from `fn(&str, Duration)` to
        // `FnMut(TraceEvent<'_>)`; we extract the SQL text from
        // `StmtRef::sql()` and the duration from the `Profile` variant.
        conn.trace_v2(
            rusqlite::trace::TraceEventCodes::SQLITE_TRACE_PROFILE,
            Some(slow_sql_log_v2),
        )?;
        let db = Self {
            inner: Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        db.migrate_friends_meta()?;
        db.migrate_groups_admin()?;
        db.migrate_security_columns()?;
        db.migrate_p4()?;
        db.migrate_communities()?;
        db.migrate_prekey_signature()?;
        db.migrate_identity()?;
        db.migrate_relations()?;
        db.migrate_g05()?;
        db.migrate_round5()?;
        db.migrate_account()?;
        Ok(db)
    }

    /// G02-05（#114）注销：**排队标记**。
    ///
    /// `deleted_at` 只回答一件事——"这个账号已经不再属于活人"。所有发凭证的
    /// 入口（登录口令校验、会话签发）都按它 fail-closed，因此**不等 worker 跑完
    /// 清理，账号的数据就已经取不到了**；行本身保留到清理完成，`groups.owner_uid`
    /// 这类没有 CASCADE 的外键才不会因为一次 DELETE 把整条链顶回来。
    ///
    /// 原地补列（与 `migrate_security_columns` 同一手法）：老行 `NULL` = 未注销。
    fn migrate_account(&self) -> Result<()> {
        let c = self.lock();
        if !Self::has_column(&c, "users", "deleted_at")? {
            c.execute_batch("ALTER TABLE users ADD COLUMN deleted_at INTEGER;")?;
        }
        Ok(())
    }

    /// 审计第 5 轮：两个小表——
    /// * `qr_ticket_creator`：扫码登录 HTTP 适配层的归属表（哪张票是谁建的，
    ///   以及服务端持有的 PKCE verifier——见 `qr_login` 模块头）。规则本体在
    ///   `identity_qr`（features crate 建），这里只补 HTTP 面需要的映射；
    /// * `wallet_idem`：钱包写接口的幂等键（`Idempotency-Key` → 24h 内重放
    ///   同一结果），见 `wallet` 模块。
    fn migrate_round5(&self) -> Result<()> {
        let c = self.lock();
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS qr_ticket_creator (
                ticket_id   TEXT PRIMARY KEY,
                creator     TEXT NOT NULL,
                verifier    TEXT NOT NULL,
                created_at  INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS wallet_idem (
                user_id      TEXT NOT NULL,
                key          TEXT NOT NULL,
                request_hash TEXT NOT NULL,
                response     TEXT NOT NULL,
                created_at   INTEGER NOT NULL,
                PRIMARY KEY (user_id, key)
            );",
        )?;
        Ok(())
    }

    /// G02-02：关系域补充表（群可见性 / 解散、好友标签、关系审计）。
    /// 与身份域同理：DDL 在 features crate 里，这里只执行。
    fn migrate_relations(&self) -> Result<()> {
        let c = self.lock();
        wxlike_relations::migrate(&c)?;
        Ok(())
    }

    /// G02-01：身份域的刷新链 / step-up / 设备表。建表语句在 features crate 里
    /// （`crates/features/identity`），这里只负责在平台库上执行——规则只有一份。
    fn migrate_identity(&self) -> Result<()> {
        let c = self.lock();
        wxlike_identity::migrate(&c)?;
        Ok(())
    }

    /// AR-1（第四轮审计 C-1）：prekey 目录补签名列。老库的既有 bundle 无
    /// 签名，客户端验签 fail-closed 会拒绝它们——属主重新 publish 即恢复，
    /// 这是把"服务端可换身份密钥"的口子关上的代价，见 docs/threat-model.md。
    /// G05 内容/社交/收藏/检索：新增表 + 对既有 moments/favorites 的增量列。
    ///
    /// 回滚兼容：所有新表都是 `IF NOT EXISTS`，新列只增不删；旧二进制读带新列
    /// 的库时按显式列名查询，不受影响。删除本迁移的代码**不会**回收列，也不需要。
    fn migrate_g05(&self) -> Result<()> {
        let c = self.lock();
        c.execute_batch(SCHEMA_G05)?;
        for (table, col, ddl) in [
            ("moments", "audience_group", "TEXT NOT NULL DEFAULT ''"),
            (
                "favorites",
                "source_kind",
                "TEXT NOT NULL DEFAULT 'message'",
            ),
            ("favorites", "source_id", "TEXT NOT NULL DEFAULT ''"),
            ("favorites", "snapshot_label", "TEXT NOT NULL DEFAULT ''"),
            ("favorites", "sort_order", "INTEGER NOT NULL DEFAULT 0"),
            ("favorites", "revoked", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            if !Self::has_column(&c, table, col)? {
                c.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {col} {ddl};"))?;
            }
        }
        // 必须在增量列之后建：旧库的 favorites 先补 source_kind/source_id 才能
        // 建这个部分唯一索引。
        c.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_favorites_object
                 ON favorites(user_id, source_kind, source_id) WHERE source_id <> '';",
        )?;
        Ok(())
    }

    fn has_column(c: &rusqlite::Connection, table: &str, col: &str) -> Result<bool> {
        let mut stmt = c.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        for name in rows {
            if name? == col {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn migrate_prekey_signature(&self) -> Result<()> {
        let c = self.lock();
        let mut have = Vec::new();
        {
            let mut stmt = c.prepare("PRAGMA table_info(prekey_bundles)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            for name in rows {
                have.push(name?);
            }
        }
        if !have.iter().any(|name| name == "signature") {
            c.execute_batch(
                "ALTER TABLE prekey_bundles ADD COLUMN signature TEXT NOT NULL DEFAULT '';",
            )?;
        }
        Ok(())
    }

    /// P4 表：人机校验、恢复码、对账、举报工单、秘密聊天设置、审核挂起队列，
    /// 以及会话的设备列（设备管理页要按设备下线）。
    fn migrate_p4(&self) -> Result<()> {
        // 建表之后还有"老库补列 + 回填"的活，所以这里要留住连接。
        let c = self.lock();
        c.execute_batch(SCHEMA_P4)?;

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
        // media 补 conv_id（issue #14）：老库里的媒体没有归属会话，
        // 只能由上传者自己取——宁可严一点，也不要默认公开。
        {
            let mut have = Vec::new();
            {
                let mut stmt = c.prepare("PRAGMA table_info(media)")?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
                for name in rows {
                    have.push(name?);
                }
            }
            if !have.iter().any(|name| name == "conv_id") {
                c.execute_batch("ALTER TABLE media ADD COLUMN conv_id TEXT NOT NULL DEFAULT '';")?;
            }
            if !have.iter().any(|name| name == "public") {
                c.execute_batch("ALTER TABLE media ADD COLUMN public INTEGER NOT NULL DEFAULT 0;")?;
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
        self.lock().execute_batch(SCHEMA)?;
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

    /// ADR-28: run one synchronous SQLite critical section on the blocking pool.
    ///
    /// The `Mutex<Connection>` guard is acquired *inside* the closure and
    /// dropped there, so a caller cannot carry a guard — or an open transaction
    /// — back across an `await`. The whole transaction has to fit in `f`, which
    /// is exactly the point: moving the thread must not move the atomic boundary.
    pub async fn blocking<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        let ran = tokio::task::spawn_blocking(move || {
            let mut conn = inner.lock().unwrap_or_else(PoisonError::into_inner);
            f(&mut conn)
        })
        .await;
        join_blocking(ran)
    }
}

/// A blocking task that panicked or was cancelled is an internal failure, not a
/// client error: the SQL either completed or did not, and the caller cannot
/// retry it by fixing its input.
fn join_blocking<T: Send>(
    ran: std::result::Result<Result<T>, tokio::task::JoinError>,
) -> Result<T> {
    ran.map_err(|err| PlatformError::Internal(format!("blocking db task failed: {err}")))?
}

/// ADR-28 offload for handlers: move an existing synchronous `Platform` call
/// onto the blocking pool.
///
/// Deliberately coarse — the *call* crosses the thread boundary, the
/// transaction inside it does not. `f` may use every synchronous method of the
/// platform unchanged (`db().lock()`, `with_tx`, the same ordering), so
/// converting a handler cannot split a transaction in half or reorder its
/// statements; it only stops the await context from owning the wait.
///
/// The `platform` argument is the shared handle the handler already holds: a
/// `&Platform` cannot be moved into a `'static` task, an `Arc<Platform>` can.
pub(crate) async fn offload<T, F>(platform: &crate::PlatformState, f: F) -> Result<T>
where
    F: FnOnce(&crate::Platform) -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let state = Arc::clone(platform);
    let ran = tokio::task::spawn_blocking(move || f(&state)).await;
    join_blocking(ran)
}

/// `UPDATE` / `DELETE` 影响行数为 0 → `NotFound`：**唯一**的一处判定。
///
/// 写语句的 `WHERE` 里带着归属条件（`AND user_id = ?`、`AND author = ?`），
/// 所以"没这行"与"不是你的"落进同一个 0。这时候**必须**回同一个 404：
/// 分开回（403 vs 404）就等于告诉对方"这个 id 存在，只是不属于你"——
/// 一个可枚举的账号/内容id 列表就这么送出去了。
///
/// 这条规则原来是 `page::ensure_updated`，但它是**任何写操作**共用的，
/// 不是列表语义（`page` 里另外四件才是一回事）。搬到这里，
/// 让 `page` 只讲列表，写路径的判定只有一处。
///
/// 各 feature 里曾各自手抄一遍 `if changed == 0 { NotFound }`（17 处），
/// 抄错的形式不是编译失败而是**语义漂移**：把 0 行当成"冲突"或"禁止"，
/// 于是同一张表上出现两种 404/409 口径，客户端分流跟着分叉。
pub(crate) fn ensure_updated(changed: usize) -> Result<()> {
    if changed == 0 {
        return Err(PlatformError::NotFound);
    }
    Ok(())
}

/// Statements slower than this land in the log with their text (truncated).
const SLOW_SQL_MS: u64 = 100;

/// Trace callback for rusqlite 0.41+ (`trace_v2`). Replaces the old
/// `Connection::profile` hook. Only registered for `SQLITE_TRACE_PROFILE`
/// events, so this fires once per completed statement.
///
/// The callback receives a `TraceEvent` enum; we match on `Profile` to get
/// the statement reference and its elapsed `Duration`. The SQL text comes
/// from `StmtRef::sql()` (the prepared statement's original SQL).
fn slow_sql_log_v2(event: rusqlite::trace::TraceEvent<'_>) {
    if let rusqlite::trace::TraceEvent::Profile(stmt_ref, elapsed) = event {
        if elapsed.as_millis() >= SLOW_SQL_MS as u128 {
            let text: String = stmt_ref.sql().chars().take(160).collect();
            tracing::warn!(elapsed_ms = elapsed.as_millis() as u64, stmt = %text, "slow sql");
        }
    }
}

// G00-02 纯移动拆分：全部建表 SQL 移至子模块 db/schema.rs（内容与 main
// 提交态逐字节一致）。
mod schema;
use schema::{SCHEMA, SCHEMA_P4};

/// G05：内容、社交、收藏与检索。表名按域加前缀，避免与平台既有表混淆。
const SCHEMA_G05: &str = r#"
            -- G05-01 朋友圈草稿：发布前的文本/媒体/可见性可反复保存，
            -- 发布成功后客户端删除；表本身不含"已发布"状态。
            CREATE TABLE IF NOT EXISTS moment_drafts (
                id             TEXT PRIMARY KEY,
                author         TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                text           TEXT NOT NULL DEFAULT '',
                media          TEXT NOT NULL DEFAULT '[]',
                visibility     TEXT NOT NULL DEFAULT 'friends',
                audience_group TEXT NOT NULL DEFAULT '',
                updated_at     INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_moment_drafts_author
                ON moment_drafts(author, updated_at);

            -- 内容 -> 媒体引用。删除内容时同步删行；用于回答"附件还被谁引用"
            -- 以及未来按引用计数清理孤儿对象。唯一约束让重复发布幂等。
            CREATE TABLE IF NOT EXISTS content_media_refs (
                media_id   TEXT NOT NULL,
                ref_kind   TEXT NOT NULL,
                ref_id     TEXT NOT NULL,
                owner      TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (ref_kind, ref_id, media_id)
            );
            CREATE INDEX IF NOT EXISTS idx_cmr_media ON content_media_refs(media_id);

            -- G05-02 发布者（一个用户一个发布者；不承诺导入微信后台）
            CREATE TABLE IF NOT EXISTS publishers (
                id              TEXT PRIMARY KEY,
                owner           TEXT NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
                name            TEXT NOT NULL,
                bio             TEXT NOT NULL DEFAULT '',
                avatar_media_id TEXT NOT NULL DEFAULT '',
                created_at      INTEGER NOT NULL
            );

            -- 文章：doc 是受限文档结构的 JSON，body_html 是服务端渲染结果。
            -- seq 同时是订阅未读游标（只增不减）。
            CREATE TABLE IF NOT EXISTS articles (
                seq          INTEGER PRIMARY KEY AUTOINCREMENT,
                id           TEXT NOT NULL UNIQUE,
                publisher_id TEXT NOT NULL REFERENCES publishers(id) ON DELETE CASCADE,
                author       TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                title        TEXT NOT NULL DEFAULT '',
                doc          TEXT NOT NULL DEFAULT '{}',
                body_text    TEXT NOT NULL DEFAULT '',
                body_html    TEXT NOT NULL DEFAULT '',
                state        TEXT NOT NULL DEFAULT 'draft',
                version      INTEGER NOT NULL DEFAULT 1,
                created_at   INTEGER NOT NULL,
                updated_at   INTEGER NOT NULL,
                published_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_articles_pub
                ON articles(publisher_id, seq);
            CREATE INDEX IF NOT EXISTS idx_articles_state
                ON articles(state, seq);

            CREATE TABLE IF NOT EXISTS article_versions (
                article_id TEXT NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
                version    INTEGER NOT NULL,
                doc        TEXT NOT NULL,
                saved_at   INTEGER NOT NULL,
                PRIMARY KEY (article_id, version)
            );

            -- 订阅：read_cursor = 已读到的文章 seq。未读 = 比它大的已发布文章。
            CREATE TABLE IF NOT EXISTS publisher_follows (
                user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                publisher_id TEXT NOT NULL REFERENCES publishers(id) ON DELETE CASCADE,
                read_cursor  INTEGER NOT NULL DEFAULT 0,
                created_at   INTEGER NOT NULL,
                PRIMARY KEY (user_id, publisher_id)
            );
            CREATE TABLE IF NOT EXISTS article_reads (
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                article_id TEXT NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
                read_at    INTEGER NOT NULL,
                PRIMARY KEY (user_id, article_id)
            );
            -- 在看/分享/收藏是三个独立动作，各自一行。
            CREATE TABLE IF NOT EXISTS article_reactions (
                article_id TEXT NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                kind       TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (article_id, user_id, kind)
            );

            -- G05-03 笔记：version 是乐观锁；写入必须带 expected_version。
            CREATE TABLE IF NOT EXISTS notes (
                id         TEXT PRIMARY KEY,
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                title      TEXT NOT NULL DEFAULT '',
                body       TEXT NOT NULL DEFAULT '',
                version    INTEGER NOT NULL DEFAULT 1,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_notes_user ON notes(user_id, updated_at);
            CREATE TABLE IF NOT EXISTS note_versions (
                note_id  TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
                version  INTEGER NOT NULL,
                title    TEXT NOT NULL,
                body     TEXT NOT NULL,
                saved_at INTEGER NOT NULL,
                PRIMARY KEY (note_id, version)
            );

            -- 表情包：包级许可字段用于"版权"约束；安装是每用户独立行。
            CREATE TABLE IF NOT EXISTS sticker_packs (
                id         TEXT PRIMARY KEY,
                owner      TEXT NOT NULL DEFAULT '',
                name       TEXT NOT NULL,
                license    TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS stickers (
                id         TEXT PRIMARY KEY,
                pack_id    TEXT NOT NULL REFERENCES sticker_packs(id) ON DELETE CASCADE,
                media_id   TEXT NOT NULL,
                name       TEXT NOT NULL DEFAULT '',
                sort_order INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_stickers_pack ON stickers(pack_id, sort_order);
            CREATE TABLE IF NOT EXISTS user_stickers (
                user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                sticker_id   TEXT NOT NULL REFERENCES stickers(id) ON DELETE CASCADE,
                sort_order   INTEGER NOT NULL DEFAULT 0,
                installed_at INTEGER NOT NULL,
                PRIMARY KEY (user_id, sticker_id)
            );

            -- G05-04 视频号：发布记录只是引用；能否播以 video_refs 的
            -- state='succeeded' 为准（转码门禁）。
            CREATE TABLE IF NOT EXISTS video_posts (
                seq        INTEGER PRIMARY KEY AUTOINCREMENT,
                id         TEXT NOT NULL UNIQUE,
                author     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                media_id   TEXT NOT NULL,
                caption    TEXT NOT NULL DEFAULT '',
                state      TEXT NOT NULL DEFAULT 'published',
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_video_posts_author
                ON video_posts(author, seq);
            CREATE INDEX IF NOT EXISTS idx_video_posts_state
                ON video_posts(state, seq);
            CREATE TABLE IF NOT EXISTS video_likes (
                video_id   TEXT NOT NULL REFERENCES video_posts(id) ON DELETE CASCADE,
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (video_id, user_id)
            );
            CREATE TABLE IF NOT EXISTS video_comments (
                seq        INTEGER PRIMARY KEY AUTOINCREMENT,
                id         TEXT NOT NULL UNIQUE,
                video_id   TEXT NOT NULL REFERENCES video_posts(id) ON DELETE CASCADE,
                author     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                text       TEXT NOT NULL,
                deleted    INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_video_comments
                ON video_comments(video_id, seq);
            CREATE TABLE IF NOT EXISTS video_follows (
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                creator    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (user_id, creator)
            );

            -- G05-05 公开内容索引：只放可被"公开检索"投影的字段。
            -- 写入与内容发布同事务、删除同步，所以不存在异步落后窗口。
            CREATE TABLE IF NOT EXISTS content_index (
                doc_id     TEXT PRIMARY KEY,
                kind       TEXT NOT NULL,
                owner      TEXT NOT NULL,
                audience   TEXT NOT NULL DEFAULT 'public',
                group_name TEXT NOT NULL DEFAULT '',
                title      TEXT NOT NULL DEFAULT '',
                body       TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_content_index_recent
                ON content_index(created_at DESC, doc_id DESC);
            CREATE INDEX IF NOT EXISTS idx_content_index_kind
                ON content_index(kind, created_at DESC);

            -- 通知收件箱：read_at 在服务端，跨端共享。"事务事件生成"由调用方
            -- 与业务写同事务插入；dedupe_key 保证重试/重复点赞不刷屏。
            CREATE TABLE IF NOT EXISTS notifications (
                seq         INTEGER PRIMARY KEY AUTOINCREMENT,
                id          TEXT NOT NULL UNIQUE,
                user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                kind        TEXT NOT NULL,
                actor       TEXT NOT NULL DEFAULT '',
                target_kind TEXT NOT NULL DEFAULT '',
                target_id   TEXT NOT NULL DEFAULT '',
                text        TEXT NOT NULL DEFAULT '',
                dedupe_key  TEXT NOT NULL DEFAULT '',
                created_at  INTEGER NOT NULL,
                read_at     INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_notifications_user
                ON notifications(user_id, seq DESC);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_notifications_dedupe
                ON notifications(user_id, dedupe_key) WHERE dedupe_key <> '';

            -- G05-06 状态：到期由服务端 now 判定；cleared_at 保留审计/事件痕迹。
            CREATE TABLE IF NOT EXISTS statuses (
                id                  TEXT PRIMARY KEY,
                owner               TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                kind                TEXT NOT NULL,
                text                TEXT NOT NULL DEFAULT '',
                media_id            TEXT NOT NULL DEFAULT '',
                background_media_id TEXT NOT NULL DEFAULT '',
                audience            TEXT NOT NULL DEFAULT 'friends',
                version             INTEGER NOT NULL DEFAULT 1,
                created_at          INTEGER NOT NULL,
                updated_at          INTEGER NOT NULL,
                expires_at          INTEGER NOT NULL,
                cleared_at          INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_statuses_owner
                ON statuses(owner, updated_at DESC);
            CREATE TABLE IF NOT EXISTS status_background_drafts (
                owner      TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
                media_id   TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS status_reactions (
                status_id  TEXT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (status_id, user_id)
            );
            "#;

#[cfg(test)]
mod adr28_evidence {
    //! #326 验收第 3、4 条：并发证据与改前/改后数字。
    //!
    //! 两种写法跑在**同一个 runtime、同一份内存库**里，所以"改前/改后"的 tick 数
    //! 来自同一次运行，不是两次环境对比。runtime 只给 1 个 worker：一条 250 ms 的
    //! 同步事务占住 worker 时，旁边的计时任务**拿不到调度**——这正是 ADR-26 里
    //! 那句"换 PostgreSQL 不会自动治好它"要治的东西（连接池同样会串行）。

    use super::Db;
    use crate::error::PlatformError;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use wxlike_core::tx::ConnectionTx as _;

    const HOLD: Duration = Duration::from_millis(250);
    const TICK: Duration = Duration::from_millis(10);
    /// `armed` 还没被 handler 写入时的哨兵值。
    const NOT_ARMED: u64 = u64::MAX;

    fn one_worker_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("single-worker tokio runtime")
    }

    fn fresh_db() -> Db {
        Db::open(None).expect("in-memory platform db")
    }

    /// 交给 handler 的"报到"句柄：`Window` 借不到 `'static`，所以单独给一份可克隆的。
    #[derive(Clone)]
    struct Arm(Arc<AtomicU64>, Instant);

    impl Arm {
        /// 记下"我从这一刻起占住 worker"。调用点与占用之间**不许有 await**，
        /// 否则计时任务能插进一次调度，窗口就又开早了。
        fn now(&self) {
            self.0
                .store(self.1.elapsed().as_millis() as u64, Ordering::SeqCst);
        }
    }

    /// 一次测量的现场：只在 handler 占用 worker 的那段窗口里数计时步。
    struct Window {
        ticks: Arc<AtomicUsize>,
        armed_at: Arc<AtomicU64>,
        start: Instant,
    }

    impl Window {
        fn new() -> Self {
            Self {
                ticks: Arc::new(AtomicUsize::new(0)),
                armed_at: Arc::new(AtomicU64::new(NOT_ARMED)),
                start: Instant::now(),
            }
        }

        fn arm(&self) -> Arm {
            Arm(Arc::clone(&self.armed_at), self.start)
        }

        /// 每 `TICK` 一跳，只数 `armed_at .. armed_at + HOLD`。
        ///
        /// 关键改动：窗口**不再**从"计时任务开工"算起，而是从 handler 自己报到的
        /// 那一刻算起。旧写法里 worker 被占死，计时任务在窗口内一次都排不上；等它
        /// 醒来窗口已经关掉 ⇒ `ticks == 0` 是**构造出来的**，不是调出来的阈值。
        ///
        /// 原实现把 deadline 取在计时任务自己开工时，于是它与 handler 抢同一个
        /// worker，谁先被调度是竞态——慢一点的 runner 上它能先攒 4~7 跳，
        /// `old_ticks <= 3` 就无端红了（main 的 ci run 42409122864，windows job）。
        /// 那类"把等时间改成等事件"的修法只能用注入延迟验证，加压复跑验不出来。
        fn start_driver(&self) -> tokio::task::JoinHandle<()> {
            let ticks = Arc::clone(&self.ticks);
            let armed_at = Arc::clone(&self.armed_at);
            let start = self.start;
            let hold = HOLD.as_millis() as u64;
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(TICK).await;
                    let now = start.elapsed().as_millis() as u64;
                    let armed = armed_at.load(Ordering::SeqCst);
                    // 失控护栏：handler 一直没报到也别把用例挂死。不参与改前/改后比较。
                    let base = if armed == NOT_ARMED { 0 } else { armed };
                    if now - base >= 4 * hold {
                        return;
                    }
                    if armed != NOT_ARMED && now - armed < hold {
                        ticks.fetch_add(1, Ordering::SeqCst);
                    }
                }
            })
        }

        fn ticks(&self) -> usize {
            self.ticks.load(Ordering::SeqCst)
        }
    }

    /// 把 handler 作为**任务**放上唯一的 worker——这才是 axum handler 的形状。
    /// （早先我把它写在 `block_on` 的 future 里，结果测的是调用线程，数字全是假的。）
    async fn old_case(db: Db) -> (i64, usize) {
        let window = Window::new();
        let driver = window.start_driver();
        let arm = window.arm();
        let handler = tokio::spawn(async move {
            arm.now();
            let c = db.lock();
            std::thread::sleep(HOLD);
            c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0)).unwrap()
        });
        let value = handler.await.unwrap();
        driver.await.unwrap();
        (value, window.ticks())
    }

    async fn new_case(db: Db) -> (i64, usize) {
        let window = Window::new();
        let driver = window.start_driver();
        let arm = window.arm();
        let handler = tokio::spawn(async move {
            arm.now();
            db.blocking(move |c| -> std::result::Result<i64, PlatformError> {
                std::thread::sleep(HOLD);
                c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))
                    .map_err(PlatformError::from)
            })
            .await
            .unwrap()
        });
        let value = handler.await.unwrap();
        driver.await.unwrap();
        (value, window.ticks())
    }

    #[test]
    fn the_blocking_entrypoint_frees_the_worker_that_sync_sql_occupied() {
        // 同一个 runtime、同一次运行里量两种写法 ⇒ "改前/改后"不跨环境比较。
        let rt = one_worker_runtime();
        let (old_value, old_ticks, new_value, new_ticks) = rt.block_on(async {
            let (v_old, t_old) = old_case(fresh_db()).await;
            let (v_new, t_new) = new_case(fresh_db()).await;
            (v_old, t_old, v_new, t_new)
        });
        println!(
            "ADR-28 并发证据：持有 {}ms / 计时步长 {}ms ⇒ 占用窗口内 tick：改前={old_ticks}，改后={new_ticks}",
            HOLD.as_millis(),
            TICK.as_millis()
        );
        assert_eq!((old_value, new_value), (1, 1), "两条路径必须读出同一个结果");
        assert_eq!(
            old_ticks, 0,
            "旧写法把唯一的 worker 占死，计时任务在占用窗口内一次都排不上；实测 {old_ticks} 跳 \
             ⇒ 窗口又开早了（计时与 handler 在抢同一个 worker）。这是测量形状的问题，\
             放宽阈值只会让这个用例不再测量它声称的东西"
        );
        // 名义上 HOLD/TICK 跳；留 5 倍调度余量。旧写法再怎么抖都到不了这个量级，
        // 因为窗口关着；所以这条断言仍然能抓住"blocking 入口退回 worker 上跑"的回归。
        let floor = HOLD.as_millis() as usize / TICK.as_millis() as usize / 5;
        assert!(
            new_ticks >= floor,
            "改后 worker 应当空出来、计时任务能跑：期望至少 {floor} 跳，实测 {new_ticks}"
        );
    }

    #[test]
    fn the_transaction_boundary_stays_inside_one_closure() {
        let rt = one_worker_runtime();
        let rows = rt
            .block_on(async {
                let db = fresh_db();
                db.blocking(|c| {
                    c.with_tx(|tx| -> std::result::Result<i64, PlatformError> {
                        tx.execute_batch(
                            "CREATE TABLE IF NOT EXISTS adr28_probe(x INTEGER PRIMARY KEY);
                             INSERT INTO adr28_probe VALUES (1), (2);",
                        )?;
                        Ok(tx.query_row("SELECT COUNT(*) FROM adr28_probe", [], |r| r.get(0))?)
                    })
                })
                .await
            })
            .expect("transaction on the blocking pool");
        assert_eq!(rows, 2);
    }

    #[test]
    fn a_panicking_critical_section_is_internal_not_a_leaked_panic() {
        let rt = one_worker_runtime();
        let outcome = rt.block_on(async {
            let db = fresh_db();
            db.blocking::<_, i64>(|_| panic!("boom: sql 里不该发生的崩溃"))
                .await
        });
        assert!(
            matches!(outcome, Err(PlatformError::Internal(_))),
            "阻塞池里的崩溃必须归成 Internal（可重试与否由调用方判），实际 {outcome:?}"
        );
    }
}

#[cfg(test)]
mod zero_rows_rule {
    //! [`super::ensure_updated`] 的规则钉桩：**0 行只能有一种解释**。
    //!
    //! 这条规则的价值全在"统一"上——调用方的 `WHERE ... AND owner = ?` 让
    //! "没这行"和"不是你的"落进同一个 0，所以 0 必须**无条件**回 `NotFound`。
    //! 一旦某个 feature 改成 `Conflict` / `Forbidden`，同一张表上就出现了两种
    //! 口径，客户端分流跟着分叉，而 id 存在与否也随之可枚举。
    //!
    //! 这里只守映射本身；"两种情况确实都返回 0"由调用方的 SQL 与各 feature
    //! 的端到端用例覆盖。

    use super::ensure_updated;
    use crate::error::PlatformError;

    #[test]
    fn zero_rows_is_always_not_found() {
        assert!(
            matches!(ensure_updated(0), Err(PlatformError::NotFound)),
            "0 行必须回 NotFound；回 Conflict/Forbidden 等于承认这个 id 存在"
        );
    }

    #[test]
    fn non_zero_rows_passes() {
        assert!(ensure_updated(1).is_ok(), "影响 1 行是成功");
        assert!(ensure_updated(7).is_ok(), "影响多行同样是成功，不是错误");
    }
}
