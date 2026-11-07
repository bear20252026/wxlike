# UP-TINODE · `server/store/types/types.go` → `crates/features/identity/src/lib.rs`

@@HEAD@@
- 次要来源：`server/auth/auth.go:207-229`（`Rec`）、`server/auth/token/auth_token.go:26-39`（`tokenLayout`）

## 抄了哪个 upstream_path 的哪些行为

只借**字段选择与列语义**，不借 DDL 方言：

1. `types.Credential{User, Method, Value, Resp, Done, Retries}`（`types.go:867`）
   → 确认 `identity_challenge` 需要"期望响应 + 是否已消费 + 重试次数"三态，
   并新增 `identity_retry(bucket, attempts, created_at, updated_at, expires_at)`
   来承载"重试次数属于凭据而不是属于单次挑战"这一条（`bucket` = `method:value`，
   即上游 PCache 的 `code_<method>:<value>` 键）；
2. `auth.Rec{Uid, AuthLevel, Lifetime, Features, State, Credential}`（`auth.go:207`）
   → `identity_session_token(sid, user_id, auth_level, features, serial, issued_at, expires_at)`；
   `Lifetime` 不存列（由 `expires_at - now` 现算，与上游 `time.Until(expires)` 一致）；
3. `tokenLayout{Uid, Expires, AuthLevel, SerialNumber, Features}`（`auth_token.go:26`）
   → 上面那张表里的 `auth_level / features / serial` 三列，含义与单位对齐
   （`serial` 全局单行 `identity_token_serial(id CHECK (id = 1), serial, rotated_at)`）。

## Rust 化改了什么

- 上游的列类型由各 adapter 决定（Postgres 里 `AccessMode` 存 `varchar`、`Credential` 存表）；
  这里只有 SQLite，全部用本平台既有的 `TEXT/INTEGER` 口径；
- `identity_token_serial` 用 `CHECK (id = 1)` 钉成单行表，
  避免"取最新一条"要写 `ORDER BY rotated_at DESC LIMIT 1` 这种依赖排序的读法；
- 建表沿用本文件既有的 `SCHEMA` 常量 + `migrate()` 幂等风格，
  三张新表都是**本平台自有表**（不 ALTER 平台的 `sessions`），
  所以没有 `ensure_*` 补列分支。

## 没抄什么

- `ObjHeader`（`Id/CreatedAt/UpdatedAt/DeletedAt/TouchedAt/Tombstone/State`）这套通用列：
  本平台已有自己的 `created_at/updated_at` 口径，不引入 `DeletedAt *time.Time` 软删语义；
- `User`/`Topic`/`Subscription`/`File`/`DeviceDef` 各表的其余列；
- `Credential.Retries` 在 SQL adapter 里的 `UpdatedAt` 排序取"活动凭据"那套查询
  （`GetActiveCred` 用 `ValidFor` 时间窗）：本平台的到期直接落在 `expires_at` 列上比较；
- MongoDB/RethinkDB 的映射 tag（`bson:"..."`）。
