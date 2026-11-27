# H-09 批量封禁 step-up 与两阶段确认

审计基线 `60895e18`：`/api/g14/identity/batch` 只经过 `write_caller` 的 Destructive 角色检查，随后直接 `JobService::run_batch`；单目标封禁的 `danger_prepare` 有 step-up + preview + confirmation token consume，批量路径两者都没有。

## 修复

1. `IdentityService::batch_prepare(actor, role, reason, targets, confirm)`：
   - 复用 `required_reason` 与 `upstream::check_target`；
   - 目标排序去重；
   - 角色矩阵 `Identity + Destructive`；
   - `StepUp::require(actor, Destructive)`；
   - 集合指纹 `batch_target_hash = SHA-256({"targets": sorted_targets})`；
   - 无 `confirm` 时返回 `BatchPreview`（目标列表、hash、total、一次性令牌、下一步提示）；
   - 有 `confirm` 时 `Confirmer::consume(token, fingerprint("batch_suspend", hash, reason))`，令牌一次性且绑定目标集合/理由。
2. `batch_suspend` 改成两阶段响应：预览返回 `{"trace":..., "result": <preview>}`，执行才调用 `run_batch`；step-up/令牌失败走 `record_denial` 落审计后返回。
3. `Receipt` 增加 `target_hash` 字段，写入 `g14_job.payload.receipt`；执行作业与预览共享同一个集合 hash，事后能核对“批的到底是不是这一批”。
4. 现有批量测试统一走 `Env::batch` 助手：先取预览令牌，再带 `confirm` 执行；新增 `batch_requires_step_up_and_a_single_use_confirmation` 覆盖：
   - 无 step-up 403，域侧零调用；
   - 预览 200 且域侧零调用；
   - 带 confirm 执行 2 个目标；
   - job payload 的 `target_hash` 与预览一致；
   - 同一 confirm 第二次 422，域侧仍只有 2 次调用。

## 验证

- `cargo test -p wxlike-admin --test g14_02_identity`：8 passed。
- `cargo test -p wxlike-admin --test g14_05_jobs`：5 passed。
- `cargo test -p wxlike-admin --test g14_01_authn_order`：8 passed。
- `cargo test -p wxlike-admin --lib`：122 passed。
- `cargo fmt --check` 通过。

## 边界

- `JobService::run_batch` 仍是内部 API，单元测试可直接调用；生产写入口只有 `/api/g14/identity/batch`，已强制两阶段。新增调用方必须复用 `batch_prepare`。
- 确认令牌内存态、TTL 5 分钟；进程重启后预览作废，这是原有 `Confirmer` 的语义。
- 目标列表在预览里可见（管理员身份面），审计只落集合 hash；逐目标审计仍由 `audit_batch_item` 写。
