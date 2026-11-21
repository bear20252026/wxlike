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
- `acquire()`：连接/事务入口。今天只有 `StorageConnection::Sqlite(MutexGuard<Connection>)`；
  PG 落地后加第二个变体（sqlx 事务/连接）。

`SqliteStorage` 另有 `lock()`（`MutexGuard<Connection>`）与 `blocking()`（ADR-28：
同步临界区搬上阻塞池）。`Db` 持有 `SqliteStorage`：

- `Db::lock()` → `Storage::acquire()`；
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

### 1.4 错误口径

- 新增 `PlatformError::Storage(String)`：profile 非法、缺 `WXLIKE_PG_URL`、
  PG profile 未实现。
- `status()` → 500；`From<PlatformError> for DomainError` → `internal("storage: {msg}")`。
- 与 `PlatformError::Sqlite` 的分工：`Sqlite` 是"一条 SQL 跑失败了"，`Storage` 是
  "存储配置 / 连接生命周期本身有问题"。两者都不把底层细节给客户端。
- PG 未实现返回 `PlatformError::Storage("postgres profile 尚未实现（G00-04 S2）：…")`，
  **不 `todo!()`、不 panic**。

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
- **12 个 feature 文件、共 51 个函数**直接吃 `&rusqlite::Connection`
  （`&Connection` / `&rusqlite::Connection`）：
  `account`(7) / `articles`(9) / `content_index`(4) / `favorites`(4) / `friends`(1) /
  `inbox`(1) / `moments`(5) / `notes`(1) / `qr_login`(5) / `status`(4) / `stickers`(1) /
  `video_feed`(9)。

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
- 接入点：`impl Storage for PgStorage`，`StorageConnection::Postgres` 包 sqlx
  事务/连接；`StorageProfile::Postgres { url }` 直接喂给 `sqlx::PgPool` / `PgConnection`。
- ADR-28 的等价物：sqlx 是 async，PG 路径的 repo 改造天然在 async 上下文里跑，
  不需要 `spawn_blocking`；但事务边界仍要在一个 `&mut PgConnection`（或一次
  `&mut Transaction`）里，不能跨 `await` 拆分。
- 本步**不写 sqlx，也不加依赖**：未使用的依赖会进 `Cargo.toml` 与 dependency-tree
  护栏，等 PG 实现真正落地再加。

## 5. 本步刻意没做的事

- 不改 `domain/repo/**`：51 个 `&Connection` 签名原样（见 §3 分批）。
- 不实现 PG 真实连接：`SqliteStorage::open` 对 postgres profile 返回受控错误。
- 不动 `server/**`、`ci/**`、`schema/**`、`integration-requests/**`、其它 worktree。
- 未更新 `ci/jobs/policy/known-gaps.json` 的 `G00-04-PG`：本步只落端口，
  `close_when`（PG profile 落地 + restore drill 跑 PG）未满足，gap 保持 open。
- `db/schema.rs` 未改一字节，仅由 `storage.rs` 用 `#[path = "db/schema.rs"]` 引用
  （G00-02 的纯移动产物随迁移链一起归位）。

## 6. 自检（本机，未跑任何 cargo / npm / docker / psql）

| 检查 | 退出码 |
|---|---|
| `python3 ci/jobs/tests/test_ci.py` | 0 |
| `python3 ci/jobs/build/check_gaps.py` | 0 |
| `python3 ci/jobs/contract/check_contracts.py` | 0 |
| `python3 ci/jobs/compose.py --check` | 0 |

Rust 测试（`storage` 模块内的 profile 解析 / 方言 / 探活 / PG 受控错误用例）随
CI 的 `cargo test --workspace` 跑；本机按硬纪律不编译。
