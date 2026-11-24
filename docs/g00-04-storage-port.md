# G00-04 S2 第一步：Storage 端口与 StorageProfile

- 车道：`feat/g00-04-s2-storage-port` @ `wt/s2-storage-port`，基线 main `69d002cf`。
- 范围：引入 `Storage` 端口与 `StorageProfile`，让 `Db` 的打开路径经过 profile；
  SQLite 行为不变，既有测试与调用方零改动。
- 不在本步：`domain/repo/*.rs` 改造、PG 真实连接、数据导入、restore drill。
- 代码：`crates/platform/src/storage.rs`（新增）、`crates/platform/src/db.rs`
  （`Db` 改为持有 `SqliteStorage`，迁移链纯移动进存储端口）。

## 1. 端口契约

### 1.1 profile 与方言

`StorageProfile`（`crates/platform/src/storage.rs`）：

| 变体 | 字段 | env |
|---|---|---|
| `Sqlite { path: Option<PathBuf> }` | `None` = 内存库；`Some` = 文件路径 | `WXLIKE_STORAGE=sqlite`（缺省），路径镜像 `WXLIKE_DB` |
| `Postgres { url: String }` | sqlx 风格 DSN | `WXLIKE_STORAGE=postgres` + `WXLIKE_PG_URL` |

- `WXLIKE_STORAGE` 缺省 `sqlite`；非法值 → `PlatformError::Storage`（受控，不 panic、不 `unwrap`）。
- **没有复用 `WXLIKE_DB` 承载 DSN**：那是 SQLite **文件路径**。复用会让"切了 profile
  忘了改路径"静默把 `/data/wxlike.db` 当 DSN 解析，割接事故在启动时才炸。专用
  `WXLIKE_PG_URL` 让割接是显式的。
- `Db::open(path)` 的 `path` 参数仍是权威：`None` 永远是内存库。`Platform::in_memory()`
  不能因为进程里设了 `WXLIKE_DB` 就悄悄写成文件库。profile 的 `Sqlite::path` 是
  同一配置的镜像，供后续 `composition/storage-profile.rs` 使用。

`StorageDialect`：

| 方言 | `placeholder(index)` | `name()` |
|---|---|---|
| `Sqlite` | `?1` / `?2` | `sqlite` |
| `Postgres` | `$1` / `$2` | `postgres` |

占位符从 1 起（SQL 惯例）。后续 repo 改造按 `dialect().placeholder(i)` 生成绑定，
不再手抄 `?N` / `$N`。

### 1.2 连接 / 事务入口

`Storage` trait：

- `dialect()`：方言。
- `health()`：`SELECT 1`，连接不可用时返回受控错误。
- `acquire()`：连接/事务入口。`StorageConnection::Sqlite(MutexGuard<Connection>)`
  保持旧形状；`StorageConnection::Postgres(&PostgresPool)` 已落，事务句柄待 repo
  改造后再收窄。

`SqliteStorage` 另有 `lock()`（`MutexGuard<Connection>`）与 `blocking()`（ADR-28：
同步临界区搬上阻塞池）。`Db` 持有 `SqliteStorage`：

- `Db::lock()` → `Storage::acquire()`，再解出 SQLite 守卫；
- `Db::acquire()` → 原样交出 `StorageConnection`，供 repo 迁移期使用；
- `Db::blocking()` → `SqliteStorage::blocking()`；
- `Db::open()` → `StorageProfile::from_env()` + `SqliteStorage::open(profile, path)`。

所以既有 `db.lock()` / `db.blocking()` 的签名与全部调用方不变。

### 1.3 事务边界

本步**不移动**事务边界：

- `Db::blocking` 的闭包在阻塞池里取锁、跑完整事务、释放锁；`offload` 把整个
  `Platform` 调用搬上阻塞池。两者语义逐字不变。
- `with_tx`（`wxlike_core::tx`）仍由调用方在 `&mut Connection` 上使用。
- PG 实现的事务边界在 S2 后续落地：sqlx 事务句柄通过 `StorageConnection::Postgres`
  交给 repo，`with_tx` 的等价包装再对齐。**不允许**把一次事务拆到多个 `await` 上。
  当前已落 `PostgresPool`（sqlx 池 + 专属 runtime），但只服务新端口
  `PostgresUsersRepo` / `PostgresSessionsRepo`；`Db::lock()` 仍只返回 SQLite 连接。

### 1.4 错误口径

- 新增 `PlatformError::Storage(String)`：profile 非法、缺 `WXLIKE_PG_URL`、
  PG 连接/池初始化失败。
- `status()` → 500；`From<PlatformError> for DomainError` → `internal("storage: {msg}")`。
- 与 `PlatformError::Sqlite` 的分工：`Sqlite` 是"一条 SQL 跑失败了"，`Storage` 是
  "存储配置 / 连接生命周期本身有问题"。两者都不把底层细节给客户端。
- `Db::open` 的 postgres profile 仍返回 `PlatformError::Storage("postgres profile
  尚未实现（G00-04 S2）：…")`，**不 `todo!()`、不 panic**；新端口走
  `PostgresPool::open`，不经过 `Db::open`。

### 1.5 健康检查

`SqliteStorage::open` 打开后立即 `health()`（`SELECT 1`），连不上直接失败，不把
问题拖到第一次业务查询。`Storage::health()` 也是后续 readiness 端点的端口方法。

## 2. 为什么先让 SQLite 跑通

- repo 层直接吃 `&rusqlite::Connection`。端口若现在就强制 repo 走它，51 个签名一起改，
  风险与 diff 都远超"引入端口"这一步。
- 先落 `SqliteStorage` + profile，让 `Db::open` 经过端口，端口契约（方言 / 错误 / 探活）
  先被真实调用验证；PG 实现再对着同一契约写。
- 本步端口**刻意不屏蔽 rusqlite**：`StorageConnection::Sqlite` 直接包
  `MutexGuard<Connection>`，`Db::lock()` 也仍返回 `MutexGuard<Connection>`。这是为
  零回归付出的代价，repo 改造后收回。

## 3. 后续 repo 改造的范围与分批

现状（`crates/platform/src/domain/repo/`）：

- 35 个 `.rs`（34 个 feature 文件 + `mod.rs`）。
- 多数 feature 文件仍直接吃 `&rusqlite::Connection`。已迁移 27 个仓库、共 275 个函数：
  `notification`（3）、`audit`（2）、`devices`（3）、`read`（2）、
  `video_refs`（2）、`qr_login`（8）、`notes`（8）、`favorites`（15）、
  `stickers`（13）、`friends`（17）、`status`（17）、`media`（3）、`keys`（5）、
  `report`（5）、`inbox`（5）、`perms`（5）、`trust`（4）、`secret`（5）、
  `recovery`（9）、`reconcile`（8）、`content_index`（8）、`groups_admin`（17）、`relations`（14）、`groups`（21）、`communities`（28）、`moments`（24）、`video_feed`（24）。这些 repo 参数改为
  `&StorageConnection<'_>`，通过 `StorageConnection::sqlite()` 拿旧守卫；
  对应 domain 模块的调用点改走 `Db::acquire()`。PG 变体当前返回受控
  `PlatformError::Storage`，不 panic。
- `notes` 是首个事务内迁移的 repo：`StorageConnection::SqliteTx(&Connection)`
  承接 `with_tx` 闭包里的 `&Transaction`，repo 不再要求 `&mut`；`favorites`
  的跨域撤权走 `revoke_source_tx(tx, ...)` 同一桥接。

改造目标：`&Connection` 参数换成端口句柄（`&StorageConnection` 或 `&dyn Storage`），
SQL 里的 `?N` 换成 `dialect().placeholder(i)`。建议分批：

1. **只读、无金额**：`favorites` / `status` / `content_index` / `video_feed` 的读路径。
   先验证方言占位符与端口入口，风险最低。
2. **普通写路径**：`account` / `articles` / `moments` / `qr_login` / `notes` /
  `stickers` / `inbox` / `friends`。带事务与幂等，逐文件改、逐文件跑既有用例。
3. **金额 / 幂等最重**：ledger / commerce 这类域，以及后续补建的 `wallet` repo
   （目前是 2 行占位、无 SQL 函数）放最后，与数据导入、影子读对账一起做。

每批都要过：既有 SQLite 用例不变绿 + PG 用例（sqlx）在 CI 的 postgres service 上绿。

## 4. sqlx 的接入位置

- PG 驱动用 **sqlx**（编译期校验 + 自带池）。
- 已经落了最小接入：`crates/platform/src/storage.rs` 的 `PostgresPool`，持有
  `sqlx::PgPool` + 专属 multi-thread runtime；`PostgresUsersRepo` /
  `PostgresSessionsRepo` 通过它执行 `$1` 查询。
- 已落 `impl Storage for PgStorage` 与 `StorageConnection::Postgres`；
  `Db` 仍只认 `MutexGuard<Connection>`，旧 repo 的端口迁移仍在分批推进。
- ADR-28 的等价物：sqlx 是 async，端口 trait 目前是同步的，`PostgresPool::block_on`
  负责桥接；调用点必须在阻塞上下文（`Db::blocking` / 普通线程），不要在 async
  任务里直接调。
- 未使用依赖护栏已满足：sqlx 已被 `PostgresPool` 与 PG repo 真实使用，不再是死依赖。

## 5. 本步刻意没做的事

- 不改 `domain/repo/**` 其余 `&Connection` 签名（见 §3 分批）；端口 pilot 已从
  `notification` 三个函数扩到 27 个仓库、275 个函数。
- `Db::open` 仍不打开 postgres profile；PG 连接只在新端口 `PostgresPool` 中存在。
- 不动 `server/**`、`ci/**`、`schema/**`、`integration-requests/**` 之外的东西。
- `G00-04-PG` gap 仍 open：restore drill 已在 GitHub Actions `44989751565` 的 `pg-migration` 中实跑通过，但 `close_when` 的另一半（旧 repo 切 PG / PG profile）未满足；本轮只完成了首批端口与 DDL/导入/恢复闭环。
- `db/schema.rs` 未改一字节，仅由 `storage.rs` 用 `#[path = "db/schema.rs"]` 引用
  （G00-02 的纯移动产物随迁移链一起归位）。

## 6. 自检与 CI

本轮起本地不再编译 Rust；构建、clippy、workspace check 与 PostgreSQL 作业全部交给 GitHub Actions。下表保留上一轮已完成的本地结果作为历史证据，后续以 `pg-migration` / `test` / `clippy` 的 CI 结果为准。

| 检查 | 结果 |
|---|---|
| `python ci/jobs/tests/test_ci.py`（或 `unittest discover`） | 106 tests，绿 |
| `python ci/jobs/build/check_gaps.py` | 0 |
| `python ci/jobs/contract/check_contracts.py` | 0 |
| `python ci/jobs/compose.py --check` | 0；`generated/ci.yml` 已重新 compose |
| `cargo fmt --all -- --check` | 绿 |
| `cargo clippy -p wxlike-platform --all-targets -- -D warnings` | 绿 |
| `cargo check -p wxlike-platform --all-targets` | 绿 |
| `cargo test -p wxlike-platform --test media` | 14 passed |
| `cargo test -p wxlike-platform --lib domain::repo::` | SQLite 2 passed |
| `WXLIKE_TEST_PG_URL=… cargo test -p wxlike-platform --lib domain::repo:: -- --ignored` | PG16 2 passed |

| `cargo check --workspace --all-targets` | 绿（清理一次旧的 `serde` 元数据后复跑，12m） |
