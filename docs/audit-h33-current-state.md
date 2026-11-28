# H-33：危险操作预览的服务端当前态与确认绑定

审计条目：`H-33 Destructive preview current state`（默认生产，High）。

## 根因

`crates/admin/src/g14.rs::danger_request` 把客户端 body 里的 `current` 直接塞进
`DangerRequest.current`；`crates/admin/src/identity.rs::danger_prepare` 用
`project(&req.current)` 生成预览，指纹只含 `op/target/reason/until_ms`。结果是：

- 预览里展示的"当前状态"完全由调用者提供，可以伪造；
- 确认令牌不绑定当前状态；预览后对象被别的人/路径改掉，旧令牌仍能执行；
- 执行审计的 `before` 用上游回执的 `effect.before`，而不是操作人确认时看到的状态。

## 修复

### 1. 服务端复读（平台只读视图）

`PlatformPort` 新增 `current_state(object_kind,target,actor)`；生产 `HttpPort`
通过 `Upstream` 发 `GET`：

- `GET /api/admin/identity/users/{id}`
- `GET /api/admin/identity/devices/{id}`
- `GET /api/admin/identity/groups/{id}`

平台侧新增设备/群视图，均先做 Admin audience 鉴权：

- `wxlike_identity::governance::AdminDeviceView`：只含设备元数据，**不含**
  `identity_key`；
- `wxlike_relations::governance::AdminGroupView`：群名、状态、群主、可见性、
  创建时间；冻结/解散后同一 GET 返回 `frozen` / `dissolved`。

用户视图继续复用已有的 `GET /api/admin/identity/users/{id}`。

### 2. 客户端 `current` 退场

`DangerRequest.current` 和 `DangerBody.current` 删除；旧客户端即使继续发
`current`，serde 也会忽略。预览里的 `current` 一律来自上面的服务端 GET。

### 3. 确认绑定当前状态

新增 `fingerprint_with_state`：在原有 `action/target/reason/until_ms` 之外加入
`current_digest`（服务端复读后经白名单投影的摘要）。

- 预览：`prepare_with_state(..., Some(&current_view))`；
- 确认：再次 `current_state` 复读，计算同一指纹后才 `consume`；
- 预览后状态变化 → 指纹不匹配 → `422`，域命令不会发出；
- 批量处置保持原有指纹（没有单目标当前态语义）。

单目标危险动作的审计 `before` 改为操作人确认过的服务端预览；上游回执的
`before` 放在 `detail.upstream_before` 对照，`detail.confirmed_current` 保留
完整预览。失败路径也记录 `confirmed_current`。

## 验证

```text
cargo test -p wxlike-admin --lib                                 129 passed
cargo test -p wxlike-admin --test g14_02_identity                 9 passed
cargo test -p wxlike-admin --test g02_09_live_evidence            2 passed
cargo test -p wxlike-identity --lib view::tests                   4 passed
cargo test -p wxlike-relations --lib governance::tests            8 passed
cargo test -p wxlike-platform --test governance_admin              7 passed
cargo check -p wxlike-admin -p wxlike-platform --all-targets --locked  ok
```

新增覆盖：

- `admin_never_sees_message_plaintext`：客户端伪造 `current` 被忽略，服务端状态
  进入预览，正文/密文/会话密钥仍被投影丢弃；
- `state_change_between_preview_and_confirm_aborts_before_domain`：
  预览后状态变化，确认返回 `422` 且 `FixturePort` 没有收到域命令；
- `confirmation_is_bound_to_the_server_state`：相同请求、不同当前态，令牌不匹配；
- `device_and_group_current_views_are_read_from_the_server`：设备/群 GET 可读，
  设备视图不含 `identity_key`，群冻结后 GET 返回 `frozen`；
- `device_view_never_carries_the_identity_key` 与
  `group_view_reflects_server_state_after_governance` 守住域侧投影。

## 结论

H-33 关闭：危险操作预览与确认不再信任客户端状态，确认令牌绑定服务端当前态，
审计保留确认时的预览与上游实际变更两份证据。
