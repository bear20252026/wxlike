# H-12：发布 decide/sign/publish/revoke 状态竞态

关闭日期：2026-11-27。审计基线 `60895e18`，代码基线 `e4ae60c`。

审计原文：`AUDIT-60895e18.md:210-215`。`publishing.rs` 的四个动作都先读状态、释放
SQLite 锁，再 `await` 外部端口，最后无条件 `UPDATE ... WHERE id = ?`。并发审批或并发
签名/发布/撤回可以同时通过前置检查，产生矛盾状态或重复外部副作用。

## 状态机 CAS

`crates/admin/src/publishing.rs`：

- `ItemState` 的 `decide_finish` 改成条件更新：

  ```sql
  UPDATE g14_review_item SET state = ?2, decided_by = ?3, reason = ?4, decided_at = ?5
   WHERE id = ?1 AND state = ?6
  ```

  `?6` 是 prepare 读到的旧状态，affected rows 必须为 1。并发 approve/reject、
  take_down/restore 只有第一个决定能落库；其余返回 422。
- `ReleaseState` 增加三个瞬时状态：`signing`、`publishing`、`revoking`。
- `sign` 在调用签名服务前 `draft -> signing` CAS 预占；只有预占成功的请求能进端口。
  成功 `signing -> signed`，失败退回 `draft`。
- `publish` 在调用发布服务前 `signed -> publishing` 预占；成功 `publishing ->
  published`，失败退回 `signed`（保留原“失败可重试、不产生新版本”的行为）。
- `revoke` 在调用撤回服务前 `published -> revoking` 预占；成功 `revoking -> revoked`，
  失败退回 `published`。
- 所有 finish 都是 `WHERE id = ? AND state = <瞬时状态>`，affected rows 必须为 1。

`value` 不变：`create_release` 的 `UNIQUE(app_id, version)` 继续保证不会因重试多版本。

## 外部操作幂等键

`ContentPort` 四个方法增加 `idempotency_key` 参数，`HttpContentPort` 通过
`Upstream::call_with_idempotency` 透传到 HTTP 头。键按资源与动作稳定生成：

- `review:<item_id>:<decision>`：`set_visibility`（下架/恢复本身是幂等 set）。
- `mini:<app_id>:<version>:sign`
- `mini:<app_id>:<version>:publish`
- `mini:<app_id>:<version>:revoke`

同一动作重试使用同一个键；上游若已执行，可按键返回原结果而不是再执行一次。
`FixtureContentPort` 记录键，单测可直接断言。

## reconcile

新增 `POST /api/g14/publishing/mini/releases/{id}/reconcile`，角色要求
`MiniApp + Write`。只处理瞬时状态：

- `signing -> draft`
- `publishing -> signed`
- `revoking -> published`

其它状态原样返回。reconcile 写审计 `mini.reconcile`，带 before/after 状态。它只回收
“进程在上游调用后、落库前中断”留下的卡死状态；真正的外部副作用重试仍复用原幂等键。

## 证据

- `cargo test -p wxlike-admin --lib`：126 passed。
- `cargo test -p wxlike-admin --lib publishing::tests`：10 passed。
  - `release_transitions_are_cas_guarded_and_reconcile_returns_to_retryable`：
    首次 `draft -> signing` 成功，第二次同状态预占失败；`signing` 经 reconcile 回
    `draft`；签名端口收到 `mini:app_1:1.0.0:sign`；`publishing` 经 reconcile 回
    `signed`；发布端口收到 `mini:app_1:1.0.0:publish`。
  - `publish_failure_can_be_retried_without_creating_a_new_version`：外部 503 后状态
    回到 `signed`，重试成功且不新增版本。
- `cargo test -p wxlike-admin --test g14_03_publishing`：8 passed。
  - `reconcile_resets_an_interrupted_release_transition`：直接把库置为 `signing`，
    HTTP reconcile 返回 `draft`。
  - 原有下架/恢复、签名/发布权限分离、发布失败重试均通过。

## 边界

默认 server 仍未实现 G07 的签名/发布/撤回端点，本机没有真实上游可验证“同一幂等键
只执行一次”。当前关闭的是 service 层状态竞态和键传递；接真实 provider 时须确认其
遵守 `Idempotency-Key` 语义，并补真实并发签名/发布/撤回记录。`reconcile` 是人工/运维
入口，不会自动扫描所有瞬时状态；批量对账可在后续 cron 中调用同一方法。
