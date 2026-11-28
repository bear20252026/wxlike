# H-35 子项：离线队列预算与发送中止

审计条目：`H-35 MQ 投递语义 / 背压`（默认生产）。

本批次只收口 H-35 的三个可独立验证的风险点，**不把 H-35 标成全部关闭**：

1. 离线队列只有每用户上限（生产 `5_000` 行），没有全库行数/字节预算；
2. 多设备投递只要有一台设备写成功，就把整个用户当成已投递；
3. 写超时/写失败后没有废弃连接，QUIC 半帧之后可能继续接帧。

## 1. 全局离线预算

`crates/net/src/offline.rs` 新增 `OfflineLimits`：

| 限制 | 默认值 | 口径 |
|---|---:|---|
| `max_total_rows` | 100,000 行 | 所有用户合计 |
| `max_total_bytes` | 256 MiB | 所有用户帧字节合计（不含 SQLite 页/索引） |

- `MemoryOffline::new` / `SqliteOffline::open` 使用默认预算；测试可走
  `with_limits` / `open_with_limits` 收紧。
- `enqueue` 在**插入前**同时检查全局行数与字节数；超预算返回
  `offline queue budget exceeded`，**不驱逐任何已有帧**。调用方
  （outbox / reliable queue）保持 pending，等有容量再重试。
- 每用户 `cap_per_user` 仍然保留：达到单用户上限时只淘汰该用户最旧的帧，
  全局计数同步扣减。
- SQLite 用 `DELETE ... RETURNING LENGTH(frame)` 在同一个写锁内完成淘汰/ack
  与计数扣减；打开已有库时用 `COUNT(*) + SUM(LENGTH(frame))` 初始化计数，
  进程重启不会把预算清零。
- 行数/字节计数是内存原子量，所有增减都在 `conn`/`inner` 锁内完成，避免
  “检查通过后两个并发 enqueue 同时越界”。

预算耗尽时不丢帧、不返回成功：这让背压落在发送方的重试循环，而不是
静默把旧消息删掉。

## 2. 多设备投递不再“一台成功 = 全用户成功”

`crates/net/src/mq_hub.rs`：

- `deliver_to` 返回 `DeliveryWrite { written, failed }`，逐连接记录写成功/
  写失败或超时。
- `deliver_here` 只有 `written > 0 && failed == 0` 才直接返回成功；否则
  （含“一台成功、一台超时”）在 `park_offline` 时落一份 durable copy。
  客户端按 `msg_id` 去重，已收到的设备不会重复展示。
- 广播路径 `deliver_local` 也改成逐连接 `write_frame_to`：同样是
  2 秒写超时 + 失败中止，一台慢设备不再拖死整轮广播。

## 3. 超时/写失败后废弃整条连接

`crates/net/src/conn.rs`：

- `FrameTx` 新增 `abort()`；默认退化为 `close()`。
- `PeerConn` 增加 `poisoned: AtomicBool`：`send_frame` 持锁后先检查，已废弃
  则直接报错；`close()` / `abort()` 都置位。
- QUIC 的 `abort()` 调用 `SendStream::reset(0)`：对端丢弃未读完的字节，
  不会把半帧和后续帧拼在一起；WS 没有流级 reset，关闭发送侧。
- `mq_hub` 的每条连接写都有 2 秒上界；失败/超时后调用 `abort()`，abort 自身
  再套 500 ms 上界，载体坏了也不能卡死投递循环。

## 验证

- `cargo test -p wxlike-net --lib`：58 passed，新增
  - `offline::tests::memory_global_row_budget_rejects_and_recovers`
  - `offline::tests::memory_global_byte_budget_counts_every_user`
  - `offline::tests::sqlite_global_budget_is_enforced_across_users`
  - `conn::tests::abort_poisons_the_connection_and_calls_the_carrier`
  - `conn::tests::close_also_stops_further_writes`
  - `mq_hub::delivery_tests::a_partial_multi_device_write_parks_a_durable_copy`
- `cargo test -p wxlike-net`：全量 unit + 21 个集成测试文件全绿，含
  `proto_v3`、`multi_device`、`delivery_outbox`、`offline_reconnect`、
  `cross_hub`、`group_routing`。
- `cargo fmt --all -- --check`、`cargo clippy -p wxlike-net --all-targets -D warnings`、
  `python3 scripts/count_unwraps.py --fail-on-increase --details`、
  `python3 scripts/platform_surface_audit.py --max-pub 0 --check-facades` 通过。

## 仍未收口（H-35 保持 open）

- **跨 hub 回执**已在后续批次收口，见 `docs/audit-h35-remote-receipts.md`。
- **离线行仍是用户级**：v3 `StoredAck` 已按 `conn/conv/device` 退休 inflight，
  但 `offline_inbox` 没有设备维度；“一台设备 ack 删除用户级离线行”的语义
  仍需产品/协议拍板。
- **广播不进离线队列**：广播只覆盖当前在线连接；离线成员依赖信箱/历史补拉，
  不在本批次改变。

这三项需要协议或产品决策，不能靠本轮局部加固冒充关闭。
