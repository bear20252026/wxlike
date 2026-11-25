# G00-04 S2 第一步：Storage 端口与 StorageProfile

> **2026-11-25 PG 批次 15 现状（优先于下方批次 14）**
>
> - `favorites`、`inbox`、`communities`、`recovery` 四个 repo 全部切到
>   `StorageConnection` 通用端口；`create_community` / `insert_thread` 的 PG 分支用
>   `sqlx::Transaction`（同一条池连接 `BEGIN/COMMIT`），SQLite 分支保留
>   `unchecked_transaction`。
> - 收藏拖拽排序不再依赖 `with_tx`：改成一条 `UPDATE ... CASE id`，`WHERE` 里的
>   `COUNT(*) = n` 保证缺一即 0 行受影响，单语句在两种后端都原子。
> - `users` / `sessions` domain 不再直接构造 `SqliteUsersRepo` / `SqliteSessionsRepo`：
>   新增 `BackendUsersRepo` / `BackendSessionsRepo`，按 `StorageConnection` 选择
>   SQLite repo 或 `PostgresUsersRepo` / `PostgresSessionsRepo`。
> - M014 给 `notifications` 补 `(user_id, dedupe_key) WHERE dedupe_key <> ''`
>   部分唯一索引，使 `inbox::notify` 的 `ON CONFLICT` 在 PG 上有约束可推断。
> - 仍 SQLite-only：`account`（`wxlike_identity::preferences` 的 `&Connection`）、
>   `qr_login`（`identity_qr` 无 PG 兼容 DDL）、`governance`；`sessions` 的
>   刷新链 / step-up / `ensure_user_active` 走 `c.sqlite()?`，PG 下受控报错。
> - 本批验证：`cargo check --workspace --all-targets`、`cargo clippy -p wxlike-platform
>   --all-targets -- -D warnings`、`cargo test -p wxlike-platform`（全 target ok）、
>   `cargo fmt --all -- --check`、`python3 scripts/quality-gate.py`（171 magic / 4 dup）
>   均通过。

> **2026-11-24 PG 批次 14 现状（覆盖下文早期描述）**
>
> - `Db` 已能按 `StorageProfile::Postgres` 打开：`DbStorage` 分 `Sqlite` / `Postgres(Arc<PgStorage>)`，
>   `Db::acquire()` 在 PG 下交 `StorageConnection::Postgres(&PostgresPool)`，`Db::dialect()` 返回
>   `Postgres`。`Db::lock()` 改成 `Result`：SQLite 正常返回守卫，PG 返回受控错误
>   （`postgres 后端没有 SQLite 互斥守卫；调用点须改用 Db::acquire`），不 panic。
> - 新增 `crates/platform/src/storage/port.rs`：`Param` / `Params` / `params!` / `StorageRow` /
>   `FromStorage` / `OptionalExt` / `StorageStatement`，让同一份 repo 代码在两种后端执行；
>   `?N` 由 `pg_sql()` 翻成 `$N`。
> - 26 个旧 repo 已从 `conn.sqlite()?` 切到通用端口：
>   `notes, status, friends, relations, stickers, audit, devices, notification, read, trust,
>   groups, moments, media, secret, wallet, articles, video_feed, video_refs, reconcile, live,
>   groups_admin, call, keys, perms, report, content_index`。
> - PG DDL：M014 一份文件里两批——三张内容/工单表（`reports`、`content_media_refs`、
>   `content_index`）+ 六十三张平台旧表（好友/群/社群/钱包/朋友圈/设备/收藏/内容/状态等，
>   `schema/proposals/G05/0003_content_pg_compat.sql` 后半），另有 M011 的
>   `users`/`sessions` 兼容视图与 M012 的 `media` 视图。26 个已端口化 repo 的表面已齐；
>   旧表省略外键，只保列/唯一约束，是割接期兼容层，不是最终模型。
> - 未切：`account`（依赖 `wxlike_identity::preferences` 的 `&Connection`）、`communities` /
>   `favorites` / `inbox`（依赖 `unchecked_transaction` / `with_tx`）、`users` / `sessions`
>   （domain 仍直接 `SqliteUsersRepo` / `SqliteSessionsRepo`）、`qr_login`、`governance`。
> - `with_tx` / `unchecked_transaction` 事务端口、`Db::blocking()` 的 PG 等价路径、
>   未切 repo 的 DDL/路由都还没落；`G00-04-PG` 仍 open。
> - 本批验证：`cargo check --workspace --all-targets`、`cargo clippy -p wxlike-platform
>   --all-targets -- -D warnings`、`cargo test -p wxlike-platform`（40 个 test target 全 ok）、
>   `cargo fmt --all -- --check`、`python3 scripts/quality-gate.py`（171 magic / 4 dup）均通过。

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
同步临界区搬上阻塞池）。批次 14 后 `Db` 持有 `DbStorage`（`Sqlite` 或
`Postgres(Arc<PgStorage>)`）：

- `Db::lock()` → `Result<MutexGuard<Connection>>`：SQLite 正常返回；PG 返回受控错误，
  不 panic、不静默降级；
- `Db::acquire()` → `StorageConnection`：SQLite 交 `Sqlite(guard)`，PG 交
  `Postgres(&PostgresPool)`；
- `Db::blocking()` → SQLite 原样转发；PG 返回受控错误（16 处 blocking 调用点待改）；
- `Db::open()` → `StorageProfile::from_env()`，再按 profile 分派到 SQLite 或 PG。

`lock()` 由 `MutexGuard` 改 `Result` 是公开签名变化，全仓 97 处调用点已同步改造。

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
- `Db::open` 的 postgres profile 已可用：走 `Db::open_profile(Postgres)` → `PgStorage::open`
  → `PostgresPool::open` + `health()`；不再返回「尚未实现」。

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
- 多数 feature 文件仍直接吃 `&rusqlite::Connection`。已迁移 32 个仓库、共 353 个函数：
  `notification`（3）、`audit`（2）、`devices`（3）、`read`（2）、
  `video_refs`（2）、`qr_login`（8）、`notes`（8）、`favorites`（15）、
  `stickers`（13）、`friends`（17）、`status`（17）、`media`（3）、`keys`（5）、
  `report`（5）、`inbox`（5）、`perms`（5）、`trust`（4）、`secret`（5）、
  `recovery`（9）、`reconcile`（8）、`content_index`（8）、`groups_admin`（17）、`relations`（14）、`groups`（21）、`communities`（28）、`moments`（24）、`video_feed`（24）、`account`（12）、`wallet`（19）、`live`（11）、`articles`（33）、`governance`（3）。这些 repo 参数改为
  `&StorageConnection<'_>`，通过 `StorageConnection::sqlite()` 拿旧守卫；
  对应 domain 模块的调用点改走 `Db::acquire()`。PG 变体当前返回受控
  `PlatformError::Storage`，不 panic。
- `notes` 是首个事务内迁移的 repo：`StorageConnection::SqliteTx(&Connection)`
  承接 `with_tx` 闭包里的 `&Transaction`，repo 不再要求 `&mut`；`favorites`
  的跨域撤权走 `revoke_source_tx(tx, ...)` 同一桥接。
- `governance` 是最后一个旧签名 repo：三个函数虽已吃 `&StorageConnection`，但
  保留 `rusqlite::Result`（identity 端口要按 `IdError::Db` 原样上报），因此用
  `StorageConnection::sqlite_ref()` 取旧句柄；PG 变体返回 `InvalidQuery`。至此
  `domain/repo/*.rs` 的 SQL 函数全部不再以 `&Connection` 为参数。

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
  `notification` 三个函数扩到 32 个仓库、353 个函数。
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

## 7. 批次 16：PG 事务端口 + repo 层全覆盖

批次 15 之后还有三处 repo 没端口化：`account`、`qr_login`、`governance`。它们不是
SQL 方言问题，而是缺两样基础设施：PG 事务和 identity 兼容表。

**PG 事务端口**。`PostgresPool::with_tx` 从池里借一条连接，显式 `BEGIN`，闭包同步
执行，结束按结果 `COMMIT` / `ROLLBACK`。关键点是**不要用一个外层 `block_on` 包住
闭包**：闭包里的端口语句仍逐条 `block_on`，外层再包会嵌套 runtime 死锁。PG 的事务
状态属于那条连接本身，逐条 `block_on` 之间事务保持。`StorageConnection` 新增
`PostgresTx { conn, pool }`，`PortConn::PgTx` 在同一连接上执行
execute/query_row/query_map。`Db::with_tx` 对 SQLite 走 `ConnectionTx`，对 PG 走
`PgStorage::with_tx`，调用方拿到的都是 `StorageConnection`。

**identity 兼容表**。M014 追加 11 张 identity 表（`identity_device`、
`identity_throttle`、`identity_step_up`、`identity_reset_token`、
`identity_challenge`、`identity_consent`、`identity_qr`、`identity_pref`、
`identity_job`、`identity_export_link`）和 `qr_ticket_creator`。账号注销/导出、
扫码登录、step-up、限速桶的旧 SQL 在 PG 上因此有表可落。

**三个 repo 端口化**：

- `account`：`deleted_at`、`open_delete_job`（直接查 `identity_job`，不再借
  `preferences::list`）、`wallet_balance`、六个导出取数、`purge_rows`、
  `mask_rows`、`anonymize_user` 全部走端口。导出行的 JSON 由
  `StorageRow::json_value` 按 SQLite/PG 实际列类型生成；PG 的 `BYTEA` 仍只报长度。
- `qr_login`：定位、creator/verifier、登记、scan hint、状态闸门、ticket row、
  抢占消费、显示名全部走端口。
- `governance`：镜像、群数、举报数走端口。PG profile 下 `GovernanceLinks` 直接复用
  `Db` 的 `Arc<PostgresPool>`（`Db::postgres_pool`），不再要求第二条 SQLite sidecar；
  SQLite 文件库仍用 sidecar，内存库仍跳过镜像。

**覆盖结果**：`domain/repo` 下所有对外 SQL 函数都已走 `StorageConnection` 端口；
`communities` / `sessions` 里残留的 `.sqlite()` 只在明确的 SQLite 事务/治理分支。
`Db::blocking` 仍是 SQLite-only——它签名的 `&mut Connection` 无法由 PG 连接满足。

**还没切的域入口**（下一步，不是本批）：

- `account` 的 `schedule_deletion` / 作业 claim-finish / 偏好读写仍用 `lock()` +
  `preferences::*`；完整 PG 注销/导出需要把作业生命周期和偏好端口一起搬。
- `qr_login` 的 `create/bind/poll/scan/approve/reject` 仍调
  `wxlike_identity::qr_login` 的 `&Connection` 状态机；repo 层已 PG-ready，域层仍
  SQLite-only。
- `sessions` 的 `issue_refresh` / `rotate_refresh` / step-up / `revoke_refresh_of_sid`
  仍是 SQLite 入口。
- identity 治理命令（suspend/disable）仍接受 `&Connection`；平台侧只完成了镜像与
  计数。

**验证**：`cargo check --workspace --all-targets`、`cargo clippy -p wxlike-platform
--all-targets -- -D warnings`、`cargo fmt --check` 全绿；`cargo test -p
wxlike-platform --lib` 69 passed / 13 ignored（新增 3 条 PG ignored 用例）；
`quality-gate` 嵌套 0、重复窗口 2、魔法数字 171；`check_domain_sql` 0；
`check_gaps` 0；contract 0 error。

## 8. 批次 18：扫码登录状态机端口化

批次 16 只把 QR 的**读写 SQL** 搬进 repo；域层仍 `sqlite(&conn)?` 后调
`wxlike_identity::qr_login::{create,bind,scan,approve,reject,poll}`。这些函数吃
`&Connection`，PG profile 下整条扫码登录不可用。

直接把状态机 SQL 复制进平台 repo 会留下第二份二维码实现，也会让质量门禁的跨文件
重复窗口从 4 涨到 41。最终选的是**把状态机本身做成后端无关**：

- identity 新增 `QrExec`（`exec` / `query` 两条原语）、`QrParam`、`QrKind`、
  `QrValue`；`SqliteExec` 用 `rusqlite` 实现它，原有 `&Connection` 函数继续可用。
- `create_with` / `bind_with` / `scan_with` / `approve_with` / `reject_with` /
  `poll_with` 是端口版状态机；SQL、状态迁移、条件 UPDATE 抢占仍只有一份。`exchange`
  与 `refresh` 需要设备/会话写入和 SQLite 事务，仍留在 identity 的 `&Connection`
  入口，不在本批范围。
- 平台 `domain/repo/qr_state.rs` 只做 `StorageConnection` → `QrExec` 适配：
  参数转 `Param`，按 `QrKind` 取行，`NotFound` 折成 `None`，其余映射
  `IdError::Storage`。`Ticket` 建票 + 绑定 + `qr_ticket_creator` 登记在
  `Db::with_tx` 里一次完成，SQLite/PG 同一段代码。

**结果**：`create_qr_ticket`、`qr_ticket_state`、`scan_qr_ticket`、
`decide_qr_ticket` 不再有 `sqlite()` 降级；`domain/qr_login.rs` 的 `sqlite` 辅助
函数已删除。`repo/qr_login.rs` 的旧 `register_creator` 删除，创建者登记归
`qr_state`。

**验证**：`cargo test -p wxlike-identity` 全绿（QR 状态机 10 条集成测试仍走
`&Connection` 包装）；`cargo test -p wxlike-platform --test qr_login_http` 5 passed；
`cargo test -p wxlike-platform --lib` 70 passed / 14 ignored；新增
`qr_state::pg_tests::postgres_qr_state_moves_from_pending_to_approved`（ignored）。
`quality-gate` 重复窗口 4 → 2、嵌套 0、魔法数字 171；`provenance` pass=84。

## 9. 批次 19：账号导出取数端口化

`export_document_for` 的六个域早就走 repo 端口，但函数签名仍是 `&Connection`，
偏好又直接调 `preferences::get(c)`，因此 `Platform::export_document` 只能用
`lock()`，PG profile 下整份导出不可用。

- `export_document_for` 改为接收 `StorageConnection`；`preferences::get` 换成
  `repo::preferences::get(port, user)`，与批次 17 的偏好读同一实现。
- `Platform::export_document` 用 `db.acquire()`，不再取 SQLite 守卫。
- `run_export_job` 先 `db.with_tx(|c| export_document_for(c, &user_id))` 同步算文档，
  再 `await` 写媒体仓；锁/事务不跨 `.await`。

**边界**：作业 `finish` / `claim` 仍走 `preferences` 的 `&Connection`
（`Db::blocking`），所以完整 PG 导出 worker 还差一步；本批先把“取数”这一大块
从 SQLite-only 摘出来。`cargo test -p wxlike-platform --test account_surfaces`
16 passed；quality gate 重复窗口 2、嵌套 0、魔法数字 171。

## 10. 批次 20：社区/话题事务统一到 `Db::with_tx`

`create_community` 与 `insert_thread` 原来各自分叉：PG 里手写
`pg.block_on(begin…commit)` + `$N` SQL，SQLite 里 `unchecked_transaction` + `?N`
SQL。同一件事两份 SQL、两条控制流，而且只有 repo 函数知道“这两行必须一起成功”。

- 两个 repo 函数降为**单条语句**：`create_community` 插社区行 + 成员行，
  `insert_thread` 插帖子行 + 订阅行；不再自带事务，也不再匹配
  `StorageConnection::Postgres`。
- 调用方 `Platform::create_community` / `Platform::create_thread` 改用
  `Db::with_tx`，SQLite 与 PG 走同一段闭包；任何一行失败整体回滚。
- PG 测试改为在 `db.with_tx` 里建社区/帖子，清理也走同一个端口；`pg_err` 与
  `sqlx` 直连分支删除。

**验证**：`cargo test -p wxlike-platform --lib domain::communities` 4 passed；
`quality-gate` 重复窗口 2、嵌套 0、魔法数字 171；`cargo check -p wxlike-platform`
绿。

## 11. 批次 21：抽出共享 `sqlport` 与平台适配器

批次 18 的 `QrExec` 证明“状态机 SQL 只留一份、后端用 trait 适配”可行，但它当时
还住在 `qr_login` 里，平台适配器 `StorageExec` 也住在 `repo/qr_state.rs`。下一个
域（账号作业、刷新令牌）要复用同一套原语，先把它抽正：

- identity 新增 `sqlport` 模块：`SqlParam`（Text/Int）、`ColKind`、`SqlValue`、
  `SqlExec`、`SqliteExec`。`qr_login` 只做 `pub use` 别名（`QrParam` 等），
  对平台侧的公开路径不变。
- 平台新增 `storage/sql_port.rs`：`StorageSqlExec` 把 `StorageConnection`
  实现成 `SqlExec`。参数经 `Params::new` 进入端口，行按 `QrKind` 取，
  `NotFound` 折成 `None`，其余映射 `IdError::Storage`。
- `repo/qr_state.rs` 删掉自己的适配器，只保留领域包装与创建者登记。

**效果**：SQL 仍在域内一份；适配逻辑平台一份；之后 `account_jobs` /
`sessions` 端口化可直接复用 `StorageSqlExec`，不用再抄一遍参数转换。
`cargo test -p wxlike-platform --test qr_login_http` 5 passed；quality gate
重复窗口 2、嵌套 0、魔法数字 171。

## 12. 批次 22：账号导出作业生命周期端口化

批次 19 只把“取数”切了端口；作业的 `request → claim → finish/fail → cancel`
仍在 `preferences` 的 `&Connection` 上。批次 21 的 `SqlExec` 正好是这批的底座：

- identity `preferences` 增加 `request_export_with` / `job_with` / `list_with` /
  `cancel_with` / `claim_with` / `finish_with` / `fail_with` / `export_link_with`；
  SQL 与状态规则仍只有一份，`&Connection` 版本只是 `SqliteExec` 包装。
- `sqlport` 增加 `query_many`（作业列表需要多行），SQLite 与平台适配器同步实现。
- 平台新增 `repo/account_jobs.rs`：一个薄包装层，把 `StorageConnection` 适配成
  `StorageSqlExec`。
- `Platform` 的 `request_export_job`、`account_job(s)`、`cancel_account_job`、
  `create_export_link`、`run_export_job` 的 finish、`run_account_jobs` 的 claim、
  `mark_job_failed` 全部改走 `acquire` / `with_tx`；`request_export + deleted_at`
  闸门在同一事务里。

**还剩**：`request_delete` 仍要 `session::logout_all` / `step_up::revoke_all`
（SQLite 入口），所以 `schedule_deletion` 与完整 PG 注销 worker 仍是下一批；
`take_export_download` 的 `redeem_link` 仍有 SQLite 单次事务。

**验证**：`cargo test -p wxlike-identity --lib preferences` 9 passed；
`cargo test -p wxlike-platform --test account_surfaces` 16 passed；新增
`account_jobs::pg_tests::postgres_account_export_job_is_created_listed_and_cancelled`
（ignored）。quality gate 重复窗口 2、嵌套 0、魔法数字 171；clippy 全绿。

## 13. 批次 23：刷新链签发与撤销端口化

`sessions` 的访问会话早已走 `BackendSessionsRepo`，但刷新链仍是 identity
`refresh::{issue_default,revoke_sid,revoke_device}` 的 `&Connection` 入口。本批只切
**签发 + 撤销**，轮换 `rotate` 仍留在 SQLite（它要事务内查 `sessions`、判
`FeatureNoLogin` 并写后继令牌，属于单独一批）：

- identity `refresh` 增加 `issue_with` / `issue_default_with` / `revoke_sid_with` /
  `revoke_device_with` / `revoke_user_with`；SQL 只有一份，`&Connection` 版本是
  `SqliteExec` 包装。
- 平台 `repo/sessions.rs` 增加 `issue_refresh`、`revoke_device_refresh`、
  `revoke_sid_refresh`，全部走 `StorageSqlExec`。
- `Platform::issue_refresh` 用 `db.with_tx`；`revoke_device_sessions` 删完访问会话
  后在同一端口作废设备链；`revoke_refresh_of_sid` 用 `db.with_tx`。`sqlite()` 不再
  出现在这两个撤销入口。

**验证**：`cargo test -p wxlike-platform --test sessions_http` 5 passed；
`cargo test -p wxlike-identity --lib refresh` 12 passed；新增
`repo::sessions::refresh_pg_tests::postgres_refresh_issue_and_revoke_share_the_port`
（ignored）。quality gate 重复窗口 2、嵌套 0、魔法数字 171；clippy 全绿。

**还剩**：`rotate_refresh` 与 step-up 仍 `lock()`；`rotate` 的 PG 事务化是下一批。

## 14. 批次 24：step-up 授予/消费端口化

`step_up` 的三条写路径（grant / consume / revoke_all）与刷新链同一形状，直接复用
`SqlExec`：

- identity `step_up` 增加 `grant_with` / `grant_default_with` / `consume_with` /
  `revoke_all_with`；`PURPOSES` 白名单与单次消费的条件 UPDATE 逻辑不变。
- 平台 `repo/sessions.rs` 增加 `grant_step_up` / `consume_step_up` /
  `revoke_all_step_up`，走 `StorageSqlExec`。
- `Platform::grant_step_up` / `consume_step_up` 改 `with_tx`；`take_step_up` 的
  用途校验仍先做，消费仍是一次条件 UPDATE。

**仍未切**：`step_up_gate` / `step_up_denied` / `step_up_passed` 走
`wxlike_identity::throttle` 的 `&Connection`（限速桶需要单独一批）；`request_delete`
里的 `step_up::revoke_all` 也同样等 throttle/session 收口。

**验证**：`account_surfaces` 16 passed、`sessions_http` 5 passed；PG ignored 用例
追加 step-up 授予/消费；quality gate 重复窗口 2、嵌套 0、魔法数字 171；clippy 全绿。

## 15. 批次 25：step-up 限速桶端口化

`step_up_gate/denied/passed` 是账号面最频繁的写路径之一：每次敏感操作先查桶，
失败记一笔，成功清零。它原来全部走 `identity::throttle` 的 `&Connection`。

- identity `throttle` 增加 `check_with` / `record_failure_with` / `reset_with`；
  `record_failure` 保留 SQLite 事务包装（`SqliteExec(&tx)` 在同一事务里执行），
  端口版明确要求调用方 `with_tx`。
- `check_budget` / `spend_failure`（短信验证码预算）暂不动，它们属于登录/验证码
  流，下一批一起看。
- 平台 `repo/account.rs` 增加 `throttle_check` / `throttle_record` /
  `throttle_reset`；`Platform::step_up_gate` 用 `acquire`，`denied/passed` 用
  `with_tx`，失败只记日志、不把请求打成 500（与原来语义一致）。

**验证**：`account_surfaces` 16 passed；新增
`repo::account::pg_tests::postgres_step_up_throttle_blocks_after_one_failure`
（ignored）；quality gate 重复窗口 2、嵌套 0、魔法数字 171；clippy 全绿。

## 16. 批次 26：账号注销/导出下载全链路端口化

上一批留下两处 SQLite-only：`schedule_deletion` 仍 `lock()` + `preferences::
request_delete`，`run_delete_job` 仍 `Db::blocking` 里开 `unchecked_transaction`，
`take_export_download` 仍 `blocking(redeem_link)`。本批一次收掉：

- identity `session::logout_all` 增加 `logout_all_with`：四张表（sessions、
  identity_session_token、identity_refresh、identity_step_up）走同一 `SqlExec`，
  `&Connection` 版本用 `SqliteExec(&tx)` 保留事务。
- identity `preferences::request_delete_with`：查重、撤销全部凭证、排队全在同一
  `SqlExec` 上；`request_delete` 只负责 SQLite 事务包装。
- identity `preferences::redeem_link_with`：条件 UPDATE 标记已用 + 读产物，
  调用方 `with_tx`；`redeem_link` 保留 SQLite 事务包装。
- 平台 `Platform::schedule_deletion` 用 `Db::with_tx`：`deleted_at`、
  `open_delete_job`、`BackendSessionsRepo::count_for_user`、
  `AccountJobs::request_delete`、`BackendUsersRepo::mark_deleted` 全在同一事务；
  PG 下不再有 `sqlite()`。
- `run_delete_job` 改成 `Db::with_tx` 内同步完成：标记校验、钱包余额校验、
  `purge_rows` / `mask_rows` / `anonymize_user`、作业 finish；不再 `blocking`。
- `take_export_download` 用 `with_tx(account_jobs::redeem_link)`，再 await 读媒体仓。

**结果**：账号导出的 request → claim → export → finish → link → redeem，以及注销的
schedule → claim → purge → finish，在 PG profile 下都走同一端口。`run_account_jobs`
不再依赖 `Db::blocking`。

**验证**：`account_surfaces` 16 passed；`cargo test -p wxlike-platform --lib` 全绿；
PG ignored 用例补了 `request_delete`（注销作业 kind 断言）；quality gate 重复窗口 2、
嵌套 0、魔法数字 171；clippy 全绿。
