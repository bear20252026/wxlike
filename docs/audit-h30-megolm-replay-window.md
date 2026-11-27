# H-30：Megolm replay 水位持久化与 cap 记账修正

- 审计基线：`60895e18`
- 修复提交：待填
- 审计条目：H-30「Megolm replay watermark 不持久化且 cap 记账错误」

## 根因

`crates/core/src/e2ee.rs` 的群消息解密用 `(group, sender, session)` 维护
`group_seen` 最高 index，但：

- `Snapshot` 不包含 `group_seen` / `group_seen_order`；`snapshot` / `restore`
  只保存群会话，重启后水位全丢，旧的 index 可以再次解密；
- `mark_group_seen` 在表满时无条件弹出最老 order 项，即使这次只是同一个
  `(group,sender,session)` 的新 index：会误删另一个仍活跃会话的水位，并把同一个
  key 再塞进 `group_seen_order`，导致 map 变小、order 无界增长；
- 现有测试只插入 unique key，没有覆盖重复 key 的 cap 路径。

## 修复

- `Snapshot` 增加 `group_seen: Vec<GroupSeenSnapshot>`，按 FIFO 顺序保存
  `(group, sender, session, index)`。
- `SNAPSHOT_VERSION` 升到 2；`restore` 接受 v1..v2。v1 snapshot 没有该字段，
  反序列化默认为空窗口，旧设备仍可恢复。
- `mark_group_seen` 先判断 key 是否已存在：存在只推进水位并返回，不驱逐、
  不重复写 order；不存在才走 cap 淘汰。
- `restore` 重建 map 和 order 时按快照顺序插入，重复 key 只更新水位不重复入队；
  超过 `GROUP_SEEN_CAP` 的旧前缀直接丢弃，保证内存上限。

## 测试

`cargo test -p wxlike-core --lib`：35 passed。
`cargo clippy -p wxlike-core --lib -- -D warnings` 通过。

新增：

- `group_seen_watermarks_survive_snapshot_restore`：两个会话的水位 snapshot/restore 后仍存在；
- `advancing_an_existing_watermark_does_not_evict_or_duplicate`：表满时推进已有 key，
  `group_seen` 与 `group_seen_order` 长度都不变，其他会话水位不被误删；
- `v1_snapshot_without_replay_window_still_loads`：删除 `group_seen` 字段、改回 v1 后仍能恢复。

## 边界

- 水位现在随每次 `snapshot()` 一起写入。`GROUP_SEEN_CAP` 上限是 4096 条，最坏情况下
  每次持久化会多写约几千条短字符串；这是“重启不重放”换来的体积成本，尚未做压缩或
  独立增量持久化。
- 仍只做客户端 replay 保护。底层 Megolm 库对旧 index 的独立拒绝、以及群 E2EE 的真实
  生产调用链，仍未接入/验证；审计原文也将其标为【潜在/API】。
