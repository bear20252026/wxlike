# H-29：Redis Streams 裁剪不得删除未确认消息

- 审计基线：`60895e18`
- 修复提交：待填
- 审计条目：H-29「Redis Streams trim 可删除未 ack pending」

## 根因

`crates/mq/src/streams.rs` 的 `publish` 直接执行：

```
XADD <stream> MAXLEN ~ <max_len> * p <payload>
```

`MAXLEN` 按**条目年龄**裁剪，不认识 consumer group 的 pending 状态。消费者崩溃后留下的
pending 通常是最老的条目，下一批 `XADD MAXLEN` 会先把它删掉；等 patrol 执行
`XAUTOCLAIM` 时，条目已经不存在，消息静默丢失。

已有的 `sweep()` 能按 `XPENDING` 的最老 pending 计算安全边界，但它只在累计 ack 后触发，
无法撤销 `XADD` 已经执行的裁剪。

## 修复

- `publish` 改为普通 `XADD`，**不再**带 `MAXLEN`。
- 保留 `sweep()` 作为唯一裁剪路径：先读最老 pending；有 pending 就以它为界，只裁更老的
  已确认前缀；无 pending 时才用 `last-delivered-id` 作边界。
- `max_len` 改为 sweep 阈值：`XLEN <= max_len` 时不做额外裁剪；超过时只裁安全前缀。
  它不再是硬上限——硬上限会删除未投递/未确认消息，和 at-least-once 语义冲突。
- `sweep` 除了每 `sweep_every` 次 ack 触发外，也在每 `sweep_every` 次 publish 后触发，
  避免有消费者但 ack 稀疏时完全不裁剪。
- sweep 失败只记录日志，不影响已经成功的 publish；下一次 sweep 继续。

## 测试

- `cargo test -p wxlike-mq --features redis --lib`：10 passed。
- `cargo test -p wxlike-mq --features redis --test streams_redis --no-run`：集成测试编译通过。
- `cargo clippy -p wxlike-mq --features redis --all-targets -- -D warnings`：通过（仅剩
  workspace/manifest 的 unused dependency 警告）。
- 新增 `streams_retention_never_trims_pending`：`max_len=2`，先制造 backlog，读走最老一条
  但不 ack，再发布 20 条；断言 pending 仍为 1，且 `XAUTOCLAIM` 仍能领回原 id。
  旧实现会在这里删掉 pending。

## 边界

- 本机没有 Redis，新增的 `#[ignore = "requires REDIS_URL"]` 集成用例未实际执行；结论只到
  “代码路径与编译级验证”，不能当作真实 Redis 已经跑过。
- 不再有 `XADD MAXLEN` 硬上限：消费者长期不前进时 stream 会增长。这是有意的取舍——
  丢未确认消息比增长更糟；全局 backlog 预算/背压属于 H-35，未在本项内实现。
- `sweep()` 先 `XPENDING`、再 `XINFO`、再 `XLEN`、再 `XTRIM`，不是原子脚本；但这些读只
  用来计算**保守边界**：边界基于已观察到的 pending / last-delivered，读取之间新增的条目
  不会被裁到。若后续要进一步收紧，可改成单条 Lua 脚本。
