//! 存储端口（G00-04 S2 第一步）：把"用哪种存储、怎么连、怎么探活"从 `Db` 里
//! 抽出来，为 SQLite → PostgreSQL 割接留一个唯一入口。
//!
//! # 为什么这一步端口没有完全屏蔽 rusqlite
//!
//! `crates/platform/src/domain/repo/*.rs` 的 12 个 feature 文件、共 51 个函数
//! 直接吃 `&rusqlite::Connection`（本步刻意不动，见 `docs/g00-04-storage-port.md`）。
//! 如果端口现在就要求 repo 通过它访问连接，那 51 个签名要一起改，远超"引入端口"
//! 这一步的范围。所以本步的端口只做三件事：
//!
//! 1. [`StorageProfile`]：从 env 解析"用哪种存储"，非法值受控报错；
//! 2. [`StorageDialect`]：提供方言差异（占位符 `?1` vs `$1`），供后续 repo 改造；
//! 3. [`Storage`] / [`SqliteStorage`]：负责连接创建、pragma、慢查询日志、探活、
//!    schema 迁移（G00-04 S2 把 `Db::open` 的迁移链一并归入存储端口）。
//!
//! `Db` 继续通过 [`SqliteStorage::lock`] 拿到 `MutexGuard<Connection>`，所以既有
//! `db.lock()` / `db.blocking()` 与全部调用方零改动。PG 实现落地、repo 改造把
//! `&Connection` 换成本端口后，rusqlite 才会被挡在端口后面。
//!
//! # env 命名
//!
//! * `WXLIKE_STORAGE`：`sqlite`（缺省）/ `postgres`。非法值 → [`PlatformError::Storage`]。
//! * `WXLIKE_PG_URL`：`WXLIKE_STORAGE=postgres` 时的 DSN（sqlx 风格 URL）。
//!   没有复用 `WXLIKE_DB`：那是 SQLite **文件路径**，让它同时承载 URL 会使得
//!   "切了 profile 忘了改 WXLIKE_DB" 静默把 `/data/wxlike.db` 当 DSN 解析。
//!   专用变量让割接是显式的。
//! * `WXLIKE_DB`：仍只表示 SQLite 路径，镜像进 [`StorageProfile::Sqlite`]，
//!   供后续 `composition/storage-profile.rs` 使用；[`crate::Db::open`] 的
//!   `path` 参数仍是权威（`None` 永远是内存库，`Platform::in_memory()` 不能因为
//!   设了 `WXLIKE_DB` 就悄悄写成文件库）。

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::Connection;

use crate::error::{PlatformError, Result};

// G00-02 纯移动拆分：全部建表 SQL 移至子模块 db/schema.rs（内容与 main
// 提交态逐字节一致）。G00-04 S2：随迁移链一起归到存储端口。
#[path = "db/schema.rs"]
mod schema;
use schema::{SCHEMA, SCHEMA_P4};

/// 方言：SQL 占位符与名称的差异都在这里，后续 repo 改造只读这一个类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageDialect {
    Sqlite,
    Postgres,
}

impl StorageDialect {
    /// 日志/文档里用的名字（与 `WXLIKE_STORAGE` 的取值一致）。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            StorageDialect::Sqlite => "sqlite",
            StorageDialect::Postgres => "postgres",
        }
    }

    /// 第 `index` 个绑定占位符（1 起，SQL 惯例）。
    ///
    /// SQLite：`?1` / `?2`（既有 repo 的写法）；PostgreSQL：`$1` / `$2`。
    #[must_use]
    pub fn placeholder(self, index: usize) -> String {
        match self {
            StorageDialect::Sqlite => format!("?{index}"),
            StorageDialect::Postgres => format!("${index}"),
        }
    }
}

/// `WXLIKE_STORAGE=postgres` 但没给 DSN 时的受控错误文案。
const PG_URL_MISSING: &str = "WXLIKE_STORAGE=postgres 需要 WXLIKE_PG_URL（sqlx 风格 DSN）";

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

/// 存储配置：profile 决定方言，连接信息按方言分开。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StorageProfile {
    /// SQLite：`path = None` 是内存库；`Some` 是文件路径（镜像 `WXLIKE_DB`）。
    Sqlite { path: Option<PathBuf> },
    /// PostgreSQL：sqlx 风格 URL / DSN。
    Postgres { url: String },
}

impl StorageProfile {
    /// 缺省 = sqlite。`storage` 取自 `WXLIKE_STORAGE`，`pg_url` 取自
    /// `WXLIKE_PG_URL`，`sqlite_path` 取自 `WXLIKE_DB`。
    ///
    /// 抽成纯函数是为了让"缺省/显式/非法"可测，不必动进程级 env。
    pub fn parse(
        storage: Option<&str>,
        pg_url: Option<&str>,
        sqlite_path: Option<&str>,
    ) -> Result<Self> {
        let storage = storage.map(str::trim).filter(|s| !s.is_empty());
        match storage.unwrap_or("sqlite") {
            "sqlite" => Ok(Self::Sqlite {
                path: sqlite_path
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && *s != ":memory:")
                    .map(PathBuf::from),
            }),
            "postgres" => {
                let url = pg_url
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| PlatformError::Storage(PG_URL_MISSING.to_string()))?;
                Ok(Self::Postgres {
                    url: url.to_string(),
                })
            }
            other => Err(PlatformError::Storage(format!(
                "WXLIKE_STORAGE={other:?} 无法解析：只接受 sqlite / postgres"
            ))),
        }
    }

    /// 从进程 env 解析。缺省 sqlite。
    ///
    /// 为什么读 env 的函数和纯解析分开：`Db::open` 走这里，测试走 [`Self::parse`]，
    /// 避免并行测试互相改 env。
    pub fn from_env() -> Result<Self> {
        Self::parse(
            std::env::var("WXLIKE_STORAGE").ok().as_deref(),
            std::env::var("WXLIKE_PG_URL").ok().as_deref(),
            std::env::var("WXLIKE_DB").ok().as_deref(),
        )
    }

    #[must_use]
    pub fn dialect(&self) -> StorageDialect {
        match self {
            StorageProfile::Sqlite { .. } => StorageDialect::Sqlite,
            StorageProfile::Postgres { .. } => StorageDialect::Postgres,
        }
    }
}

impl fmt::Display for StorageProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageProfile::Sqlite { path } => match path {
                Some(p) => write!(f, "sqlite({})", p.display()),
                None => write!(f, "sqlite(:memory:)"),
            },
            // URL 可能带密码：只留 scheme+user，凭据不进日志。
            StorageProfile::Postgres { url } => write!(f, "postgres({})", mask_pg_url(url)),
        }
    }
}

/// 抹掉 DSN 的密码（`postgres://user:pass@host/...` → `postgres://user:***@host/...`）。
fn mask_pg_url(url: &str) -> String {
    if let Some(scheme_end) = url.find("://") {
        let rest = &url[scheme_end + 3..];
        if let Some(at) = rest.find('@') {
            let userinfo = &rest[..at];
            if let Some(colon) = userinfo.find(':') {
                let user = &userinfo[..colon];
                return format!("{}://{}:***@{}", &url[..scheme_end], user, &rest[at + 1..]);
            }
        }
    }
    url.to_string()
}

/// 连接/事务入口。今天只有 SQLite 一种实现；PG 落地时加第二个变体。
///
/// 为什么现在就有枚举、却还不屏蔽 rusqlite：repo 层直接吃 `&Connection`，
/// 所以 SQLite 变体只能包 `MutexGuard<'_, Connection>`。等 repo 改造完，
/// `Postgres` 变体包 sqlx 事务，这个枚举才真正把方言差异挡在端口后面。
pub enum StorageConnection<'a> {
    Sqlite(MutexGuard<'a, Connection>),
}

/// 存储端口。`Db` 持有具体实现（`SqliteStorage`）；repo 改造后改为持句柄。
pub trait Storage: Send + Sync {
    /// 方言（占位符/名称）。
    fn dialect(&self) -> StorageDialect;

    /// 探活：`SELECT 1`。连接不可用时返回受控错误，不 panic。
    fn health(&self) -> Result<()>;

    /// 连接/事务入口。SQLite 交出互斥锁守卫；PG 后续交出事务句柄。
    fn acquire(&self) -> StorageConnection<'_>;
}

/// SQLite 存储：连接创建、pragma、慢查询日志、探活、schema 迁移。
///
/// 连接仍包在 `Arc<Mutex<Connection>>` 里——这是 `Db::lock()` 的既有形状，
/// 本步不改调用方。`Db::open` 的迁移链也搬了进来（`db/schema.rs` 用
/// `#[path = "db/schema.rs"]` 引用，文件本身未改一字节）。
#[derive(Clone)]
pub struct SqliteStorage {
    profile: StorageProfile,
    inner: Arc<Mutex<Connection>>,
}

impl SqliteStorage {
    /// 按 profile 打开连接。`path` 参数是权威（`None` = 内存库）。
    ///
    /// PG profile 本步不实现：返回受控错误，不 panic、不 `todo!()`。
    pub fn open(profile: StorageProfile, path: Option<&str>) -> Result<Self> {
        if profile.dialect() != StorageDialect::Sqlite {
            return Err(PlatformError::Storage(format!(
                "postgres profile 尚未实现（G00-04 S2）：{profile}"
            )));
        }
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
        let storage = Self {
            profile,
            inner: Arc::new(Mutex::new(conn)),
        };
        // 打开即探活：连接真的能跑 `SELECT 1` 才交给调用方，避免把"连不上"
        // 拖到第一次业务查询才暴露。
        storage.health()?;
        // schema 演进：迁移链随打开路径一起归到存储端口（G00-04 S2）。
        storage.migrate()?;
        storage.migrate_friends_meta()?;
        storage.migrate_groups_admin()?;
        storage.migrate_security_columns()?;
        storage.migrate_p4()?;
        storage.migrate_communities()?;
        storage.migrate_prekey_signature()?;
        storage.migrate_identity()?;
        storage.migrate_relations()?;
        storage.migrate_g05()?;
        storage.migrate_round5()?;
        storage.migrate_account()?;
        storage.migrate_governance_columns()?;
        tracing::debug!(
            dialect = storage.dialect().name(),
            profile = %storage.profile,
            "storage ready"
        );
        Ok(storage)
    }

    /// 互斥锁守卫。`Db::lock()` 直接转发，既有调用方零改动。
    pub fn lock(&self) -> MutexGuard<'_, Connection> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[must_use]
    pub fn profile(&self) -> &StorageProfile {
        &self.profile
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

    /// G02-09 治理镜像列（schema/proposals/G02/governance.sql，集成请求
    /// REQ-G00-governance-users-migration）：users 表归平台，治理状态要在
    /// 平台登录/会话路径可读。域自有表（identity_user_governance 等）已随
    /// `wxlike_identity::migrate` 幂等落地；这三个补列是**镜像**，权威在域表。
    fn migrate_governance_columns(&self) -> Result<()> {
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
            ("suspended", "INTEGER NOT NULL DEFAULT 0"),
            ("disabled", "INTEGER NOT NULL DEFAULT 0"),
            ("suspended_until_ms", "INTEGER"),
        ] {
            if !have.iter().any(|name| name == col) {
                c.execute_batch(&format!("ALTER TABLE users ADD COLUMN {col} {ddl};"))?;
            }
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
}

impl Storage for SqliteStorage {
    fn dialect(&self) -> StorageDialect {
        StorageDialect::Sqlite
    }

    fn health(&self) -> Result<()> {
        let c = self.lock();
        c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))
            .map(|_| ())
            .map_err(PlatformError::from)
    }

    fn acquire(&self) -> StorageConnection<'_> {
        StorageConnection::Sqlite(self.lock())
    }
}

/// A blocking task that panicked or was cancelled is an internal failure, not a
/// client error: the SQL either completed or did not, and the caller cannot
/// retry it by fixing its input.
pub(crate) fn join_blocking<T: Send>(
    ran: std::result::Result<Result<T>, tokio::task::JoinError>,
) -> Result<T> {
    ran.map_err(|err| PlatformError::Internal(format!("blocking db task failed: {err}")))?
}

/// Statements slower than this land in the log with their text (truncated).
const SLOW_SQL_MS: u64 = 100;

/// Trace callback for rusqlite 0.41+ (`trace_v2`). Replaces the old
/// `Connection::profile` hook. Only registered for `SQLITE_TRACE_PROFILE`
/// events, so this fires once per completed statement.
///
/// The callback receives a `TraceEvent` enum; we match on `Profile` to get the
/// statement reference and its elapsed `Duration`. The SQL text comes from
/// `StmtRef::sql()` (the prepared statement's original SQL).
fn slow_sql_log_v2(event: rusqlite::trace::TraceEvent<'_>) {
    if let rusqlite::trace::TraceEvent::Profile(stmt_ref, elapsed) = event {
        if elapsed.as_millis() >= SLOW_SQL_MS as u128 {
            let text: String = stmt_ref.sql().chars().take(160).collect();
            tracing::warn!(elapsed_ms = elapsed.as_millis() as u64, stmt = %text, "slow sql");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_defaults_to_sqlite() {
        let p = StorageProfile::parse(None, None, None).unwrap();
        assert_eq!(p, StorageProfile::Sqlite { path: None });
        assert_eq!(p.dialect(), StorageDialect::Sqlite);
    }

    #[test]
    fn profile_explicit_sqlite_keeps_path() {
        let p = StorageProfile::parse(Some("sqlite"), None, Some("/tmp/wxlike.db")).unwrap();
        let StorageProfile::Sqlite { path } = p else {
            panic!("expected sqlite profile");
        };
        assert_eq!(path, Some(PathBuf::from("/tmp/wxlike.db")));
        // ":memory:" 归一成 None（内存库没有路径）
        let mem = StorageProfile::parse(Some("sqlite"), None, Some(":memory:")).unwrap();
        assert_eq!(mem, StorageProfile::Sqlite { path: None });
    }

    #[test]
    fn profile_postgres_needs_url() {
        let url = "postgresql://u:p@localhost:5432/wxlike";
        let p = StorageProfile::parse(Some("postgres"), Some(url), None).unwrap();
        assert_eq!(p.dialect(), StorageDialect::Postgres);
        let StorageProfile::Postgres { url } = p else {
            panic!("expected postgres profile");
        };
        assert_eq!(url, "postgresql://u:p@localhost:5432/wxlike");

        // 缺 DSN / 空白 DSN 必须受控报错
        for bad in [None, Some(""), Some("   ")] {
            assert!(
                matches!(
                    StorageProfile::parse(Some("postgres"), bad, None),
                    Err(PlatformError::Storage(_))
                ),
                "postgres 缺 DSN 必须报错，实际 {bad:?}"
            );
        }
    }

    #[test]
    fn profile_rejects_unknown_storage() {
        assert!(matches!(
            StorageProfile::parse(Some("mysql"), None, None),
            Err(PlatformError::Storage(_))
        ));
        assert!(matches!(
            StorageProfile::parse(Some(" SQLite "), None, None),
            Ok(StorageProfile::Sqlite { path: None })
        ));
    }

    #[test]
    fn dialect_placeholders_and_names() {
        assert_eq!(StorageDialect::Sqlite.placeholder(1), "?1");
        assert_eq!(StorageDialect::Sqlite.placeholder(3), "?3");
        assert_eq!(StorageDialect::Postgres.placeholder(1), "$1");
        assert_eq!(StorageDialect::Postgres.placeholder(3), "$3");
        assert_eq!(StorageDialect::Sqlite.name(), "sqlite");
        assert_eq!(StorageDialect::Postgres.name(), "postgres");
    }

    #[test]
    fn sqlite_storage_health_and_port_entry() {
        let storage = SqliteStorage::open(StorageProfile::Sqlite { path: None }, None).unwrap();
        assert_eq!(storage.dialect(), StorageDialect::Sqlite);
        storage.health().unwrap();

        // 端口入口交出 SQLite 守卫，且守卫能跑 SQL
        match storage.acquire() {
            StorageConnection::Sqlite(c) => {
                assert_eq!(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
            }
        }
    }

    #[test]
    fn postgres_profile_is_a_controlled_error_not_a_panic() {
        let profile = StorageProfile::Postgres { url: "postgresql://localhost/wxlike".into() };
        let err = SqliteStorage::open(profile, None).unwrap_err();
        assert!(matches!(err, PlatformError::Storage(_)));
        assert!(
            err.to_string().contains("postgres profile 尚未实现"),
            "错误文案要说明是未实现，实际：{err}"
        );
    }

    #[test]
    fn pg_url_is_masked_in_logs() {
        let p = StorageProfile::Postgres {
            url: "postgresql://alice:s3cret@localhost:5432/wxlike".into(),
        };
        let s = p.to_string();
        assert!(!s.contains("s3cret"), "DSN 密码不能进日志：{s}");
        assert!(s.contains("alice"), "{s}");
    }
}
