# 旧仓储规则迁移：domain 只留规则，repo 只留 SQL

## 结论

`crates/platform/src/domain/*.rs` 过去同时承担业务规则和 SQL，导致两个问题：

1. 想换 PostgreSQL 时，规则和 SQLite 语句绑死；
2. handler 层能看到 `&Connection`，新代码很容易再写一条绕过仓储层的 SQL。

本批先把身份域的 **users / sessions / security** 三个模块收口：它们不再直接写
SQL，全部走 `domain/repo` 的端口；`Platform` 的公开方法签名保持不变，旧调用点
不需要改。

## 现在的边界

- `domain/*.rs`：DTO、校验、状态机、错误映射、调用仓储端口。
- `domain/repo/*.rs`：SQLite/PostgreSQL SQL 与行映射；可见性
  `pub(in crate::domain)`，handler 层编译期看不到。
- `wxlike-persistence`：跨 crate 的 `UsersRepo` / `SessionsRepo` trait 与 DTO，
  不依赖 rusqlite/sqlx 驱动。
- `Platform`：旧方法的适配层，保持既有调用面；后续可以按同一模式逐个模块迁移。

本批新增的端口能力：

- `UsersRepo::active_exists`：未注销账号存在性，和资料更新使用同一口径；
- `UsersRepo::update_profile`：三条资料 UPDATE 在一个事务里完成；
- `UsersRepo::totp_secret` / `totp_enabled`：TOTP 读取不再由 domain 拼 SQL；
- `SessionsRepo::device_name_by_sid`：刷新轮换沿用旧设备名。

## 防回潮

新增 `ci/jobs/build/check_domain_sql.py` 和逐文件基线
`ci/jobs/policy/domain-sql-baseline.json`：

- 已列入基线的文件，SQL 数只能下降，不能上升；
- 新文件出现任何 SQL 直接失败；
- `--update` 默认拒绝把基线调高，必须显式 `--force`；
- 统计只看生产代码，遇到 root 级 `#[cfg(test)]` 就停止，测试夹具不算。

当前基线是 **30 个文件 / 400 条 SQL**；本批从 **33 个文件 / 425 条**降下来，
users、sessions、security 三个文件已经是 **0**。

## 下一批顺序

按剩余 SQL 数量排：

1. `account.rs`（67）：注册后资料/注销/恢复，规则密集，先拆读取与状态机；
2. `groups.rs` / `communities.rs`（各 29）：成员、权限、群状态；
3. `wallet.rs`（28）：余额、红包、幂等与账本边界；
4. `articles.rs` / `moments.rs` / `friends.rs`：内容与关系读模型；
5. 其余文件按 feature 逐个清到 0。

每批的验收标准一致：

- 该模块生产代码 SQL 计数为 0；
- `Platform` 公开签名不变；
- SQLite 与 PostgreSQL 实现都走同一个 trait；
- `check_domain_sql.py` 基线只降不升；
- CI 的 `pg-migration` / `test` / `audit` 全绿。

## 不要做的事

- 不要把新 SQL 写回 `domain/*.rs`，即使只读一条也不行；
- 不要在 handler 层直接 `db().lock()` 拼 SQL；
- 不要为了让旧测试过而复制一份 SQLite 语句；
- 不要先改 51 个旧签名；它们是对外适配面，迁移顺序应从实现内部开始。
