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
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::Connection;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use crate::error::{PlatformError, Result};

// G00-02 纯移动拆分：全部建表 SQL 移至子模块 db/schema.rs（内容与 main
// 提交态逐字节一致）。G00-04 S2：随迁移链一起归到存储端口。
mod migrate;
#[path = "db/schema.rs"]
mod schema;

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
        // 大小写不敏感：env 由人写，`SQLITE` 与 `SQLite` 都当作 sqlite；
        // 但拼写错误（`sqlte`）仍然报错，不做模糊匹配。
        let kind = storage.unwrap_or("sqlite");
        if kind.eq_ignore_ascii_case("sqlite") {
            Ok(Self::Sqlite {
                path: sqlite_path
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && *s != ":memory:")
                    .map(PathBuf::from),
            })
        } else if kind.eq_ignore_ascii_case("postgres") {
            let url = pg_url
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| PlatformError::Storage(PG_URL_MISSING.to_string()))?;
            Ok(Self::Postgres {
                url: url.to_string(),
            })
        } else {
            Err(PlatformError::Storage(format!(
                "WXLIKE_STORAGE={kind:?} 无法解析：只接受 sqlite / postgres"
            )))
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

/// PostgreSQL 连接句柄（G00-04 S2 的第二段）。
///
/// 现有 `Db` / 51 个 repo 函数仍只认 `&rusqlite::Connection`，所以这里**不是**
/// `Db` 的替代，而是给新端口（`PostgresUsersRepo` / `PostgresSessionsRepo`）
/// 用的连接池。等 repo 全量改造时，再让 `Db` 持有它并统一 `lock()` 的返回类型。
///
/// 端口方法目前是同步 trait；sqlx 是异步 API，所以这里在专属 runtime 上
/// `block_on`。调用点必须把它放在阻塞上下文（`Db::blocking` / 普通线程）里；
/// 不要在 async 任务里直接调 `PostgresUsersRepo` 的方法。
pub struct PostgresPool {
    pool: PgPool,
    runtime: Arc<tokio::runtime::Runtime>,
}

impl PostgresPool {
    /// 打开连接池并跑一次 `SELECT 1`。`url` 是 sqlx 风格 DSN。
    pub fn open(url: &str) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| PlatformError::Storage(format!("postgres runtime: {e}")))?;
        let pool = runtime
            .block_on(async { PgPoolOptions::new().max_connections(8).connect(url).await })
            .map_err(|e| PlatformError::Storage(format!("postgres connect: {e}")))?;
        let this = Self {
            pool,
            runtime: Arc::new(runtime),
        };
        this.health()?;
        Ok(this)
    }

    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// 在专属 runtime 上执行一个 sqlx future。若已经在 tokio 上下文里，
    /// 用 `block_in_place` 把当前线程让给 runtime；current-thread runtime
    /// 下会 panic，调用方必须保证是 multi-thread/阻塞上下文。
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => tokio::task::block_in_place(|| handle.block_on(future)),
            Err(_) => self.runtime.block_on(future),
        }
    }

    pub fn health(&self) -> Result<()> {
        self.block_on(async { sqlx::query("SELECT 1").execute(&self.pool).await })
            .map(|_| ())
            .map_err(|e| PlatformError::Storage(format!("postgres health: {e}")))
    }
}

/// 连接/事务入口：SQLite 交出旧守卫，PG 交出连接池句柄。
///
/// 为什么还不完全屏蔽 rusqlite：repo 层仍直接吃 `&Connection`，所以 SQLite
/// 变体只能包 `MutexGuard<'_, Connection>`。等 51 个 repo 改造完，
/// `Postgres` 变体再收窄成 sqlx 事务，这个枚举才真正把方言差异挡在端口后面。
pub enum StorageConnection<'a> {
    Sqlite(MutexGuard<'a, Connection>),
    /// 事务内的借用：`with_tx` 闭包只给 `&Transaction`，repo 要能用同一组
    /// 端口签名，就必须有一个不拥有守卫、也不要求 `&mut` 的 SQLite 变体。
    SqliteTx(&'a Connection),
    /// PG 连接池句柄。事务边界后续由 sqlx 事务补齐；当前 PG repo 通过
    /// `PostgresPool::block_on` 在自己的 runtime 上执行。
    Postgres(&'a PostgresPool),
}

impl<'a> StorageConnection<'a> {
    /// 从事务闭包的 `&Transaction` 构造端口值。`Transaction` deref 到
    /// `Connection`，这里只保留连接借用，端口方法不需要事务对象本身。
    pub(crate) fn sqlite_tx(tx: &'a rusqlite::Transaction<'_>) -> Self {
        Self::SqliteTx(&**tx)
    }

    /// 取出 SQLite 连接。PG 变体明确返回受控错误，而不是 panic 或静默降级。
    ///
    /// 这是 repo 迁移期的过渡方法：只接受 `&Connection` 的 repo 先通过它拿
    /// 旧句柄；等该 repo 的 SQL 全部方言化后，调用点再改成直接匹配端口。
    pub(crate) fn sqlite(&self) -> Result<&Connection> {
        match self {
            StorageConnection::Sqlite(c) => Ok(&**c),
            StorageConnection::SqliteTx(c) => Ok(c),
            StorageConnection::Postgres(_) => Err(PlatformError::Storage(
                "postgres 连接尚未暴露 SQLite Connection；该 repo 仍在 SQLite 入口".into(),
            )),
        }
    }
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
#[derive(Clone, Debug)]
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
        let mut conn = open_sqlite(path)?;
        configure_sqlite(&mut conn)?;
        let storage = Self {
            profile,
            inner: Arc::new(Mutex::new(conn)),
        };
        // 打开即探活：连接真的能跑 `SELECT 1` 才交给调用方，避免把"连不上"
        // 拖到第一次业务查询才暴露。
        storage.health()?;
        // schema 演进：迁移链随打开路径一起归到存储端口（G00-04 S2）。
        storage.run_all()?;
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
}

/// 建连接：`None` 是内存库；文件路径先建父目录，再交给 SQLite。
fn open_sqlite(path: Option<&str>) -> Result<Connection> {
    match path {
        Some(p) => {
            if let Some(dir) = std::path::Path::new(p).parent() {
                if !dir.as_os_str().is_empty() {
                    std::fs::create_dir_all(dir)?;
                }
            }
            let conn = Connection::open(p)?;
            Ok(conn)
        }
        None => {
            let conn = Connection::open_in_memory()?;
            Ok(conn)
        }
    }
}

/// 连接级设置：WAL、外键、忙等，以及慢查询 trace；必须在迁移链之前完成。
fn configure_sqlite(conn: &mut Connection) -> Result<()> {
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
    Ok(())
}

/// PostgreSQL 存储实现：池 + 专属 runtime，打开时探活。
///
/// 还没接进 `Db`（`Db` 仍只构造 `SqliteStorage`）：51 个旧 repo 仍以
/// `&Connection` 为参数，先把 `StorageConnection::Postgres` 的端口形状立住。
pub struct PgStorage {
    profile: StorageProfile,
    pool: PostgresPool,
}

impl PgStorage {
    pub fn open(profile: StorageProfile) -> Result<Self> {
        let StorageProfile::Postgres { url } = &profile else {
            return Err(PlatformError::Storage(format!(
                "PgStorage 只接受 postgres profile：{profile}"
            )));
        };
        Ok(Self {
            pool: PostgresPool::open(url)?,
            profile,
        })
    }

    #[must_use]
    pub fn pool(&self) -> &PostgresPool {
        &self.pool
    }

    #[must_use]
    pub fn profile(&self) -> &StorageProfile {
        &self.profile
    }
}

impl Storage for PgStorage {
    fn dialect(&self) -> StorageDialect {
        StorageDialect::Postgres
    }

    fn health(&self) -> Result<()> {
        self.pool.health()
    }

    fn acquire(&self) -> StorageConnection<'_> {
        StorageConnection::Postgres(&self.pool)
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

        // 端口入口交出 SQLite 守卫，且守卫能跑 SQL。
        // 末尾分号不能省：`StorageConnection` 含 MutexGuard，match 作为表达式
        // 会让临时值活到块尾，`storage` 先被 drop 就报 E0597。
        match storage.acquire() {
            StorageConnection::Sqlite(c) => {
                assert_eq!(
                    c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0)).unwrap(),
                    1
                );
            }
            StorageConnection::SqliteTx(_) => panic!("acquire must not hand out a transaction"),
            StorageConnection::Postgres(_) => panic!("SqliteStorage must hand out sqlite"),
        };
    }

    #[test]
    fn postgres_profile_is_a_controlled_error_not_a_panic() {
        let profile = StorageProfile::Postgres {
            url: "postgresql://localhost/wxlike".into(),
        };
        let err = SqliteStorage::open(profile, None).unwrap_err();
        assert!(matches!(err, PlatformError::Storage(_)));
        assert!(
            err.to_string().contains("postgres profile 尚未实现"),
            "错误文案要说明是未实现，实际：{err}"
        );
    }

    #[test]
    fn pg_storage_exposes_postgres_connection() {
        let Ok(url) = std::env::var("WXLIKE_TEST_PG_URL") else {
            return;
        };
        let storage = PgStorage::open(StorageProfile::Postgres { url }).expect("postgres storage");
        assert_eq!(storage.dialect(), StorageDialect::Postgres);
        match storage.acquire() {
            StorageConnection::Postgres(pool) => pool.health().expect("pg health"),
            StorageConnection::Sqlite(_) => panic!("PgStorage must not hand out sqlite"),
            StorageConnection::SqliteTx(_) => panic!("PgStorage must not hand out sqlite tx"),
        };
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
