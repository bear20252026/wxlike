# H-35 子项：跨 hub 投递回执

审计条目：`H-35 MQ 投递语义 / 背压`（默认生产）。

上一批次收口了离线队列预算、多设备部分失败和半帧连接；本批次收口
**跨 hub 投递只在 MQ 入队成功就退休 outbox** 的问题。

## 根因

`deliver_via_route` 对远端路由只做：

```text
routes.route_of(user) == Some(h) -> mq.publish(hub_queue(h), frame)
```

`publish` 成功只表示帧进了目标 hub 的队列，不表示目标 hub 写进了实时连接或
离线队列。源端 `fanout_content` 随后直接 `mark_content_delivered`，于是目标
hub 投递失败时源 outbox 已经显示“已送达”，patrol 不会再补投。

## 修复

`crates/net/src/wire.rs` 追加两个 **hub-to-hub only** 帧（放在 `Resume` 之后，
客户端可见的 variant index 不变）：

- `RoutedMsg { source_hub, envelope }`：源 hub 转发内容帧时包一层，带上自己的
  hub id；
- `DeliveryReceipt { msg_key, recipient }`：目标 hub 写成功/落 durable inbox 后
  发回源 hub。

`crates/net/src/mq_hub.rs`：

- `deliver_via_route` 返回 `RouteOutcome::{Delivered, RemoteQueued}`。
  本地写成功、本地离线落盘、无路由的瞬时帧都算 `Delivered`；远端只算
  `RemoteQueued`。
- `fanout_content` 只对 `Delivered` 调 `mark_content_delivered`；远端收件人保持
  pending，等回执。
- 目标 hub 的 `deliver_bytes` 先执行 `deliver_envelope`（写实时连接或落离线队列），
  成功后才 `mq.publish(hub_queue(source_hub), DeliveryReceipt)`。回执发布失败
  返回 `Err`，目标队列不 ack，可靠队列按 lease 重投；源端继续 pending。
- 源 hub 收到 `DeliveryReceipt` 后调 `mark_content_delivered(msg_key, recipient)`，
  只有这一步才退休 outbox。
- 客户端侧 `dispatch_control` 把这两个帧归入“server-only”，客户端发来一律忽略。

语义是 **at-least-once**：目标写成功但回执丢失时，源端会重投，客户端按
`msg_id` 去重。不会出现“源端以为送达、目标其实没收到”的静默丢失。

## 兼容性

- 两个新帧只在 MQ 队列里走，绝不发给 WebSocket/QUIC 客户端；`WireFrame` 的
  客户端 fixture 和 v2/v3 变体顺序不变。
- 滚动升级期间，旧 hub 收到 `RoutedMsg` 会按“坏帧”吃掉（`deliver_bytes` 的
  `Err` 分支），不会崩溃；源 outbox 因收不到回执保持 pending，新版本全部上线后
  重投补齐。代价是升级窗口内跨 hub 消息延迟，不是丢失。

## 验证

- `cargo test -p wxlike-net --test cross_hub`：4 passed，新增
  `a_remote_delivery_receipt_retires_the_source_outbox`：源 hub 配 `Store`，
  目标 hub 收到并写出后，轮询源 `pending_deliveries(msg_key)` 直到为空；没有
  回执时该断言不会通过。
- `cargo test -p wxlike-net`：全量 unit + 21 个集成测试文件全绿（含
  `cross_hub`、`delivery_outbox`、`offline_reconnect`、`group_routing`）。
- `cargo check -p wxlike-client-web --target wasm32-unknown-unknown --all-targets` 通过。
- `cargo fmt --all -- --check`、`cargo clippy -p wxlike-net --all-targets -D warnings`、
  `python3 scripts/count_unwraps.py --fail-on-increase --details` 通过。

## 仍未收口（H-35 保持 open）

- **离线行仍是用户级**：v3 `StoredAck` 已按 `conn/conv/device` 退休 inflight，
  但 `offline_inbox` 没有设备维度；“一台设备 ack 删除用户级离线行”的语义
  仍需产品/协议拍板。
- **广播不进离线队列**：广播只覆盖当前在线连接；离线成员依赖信箱/历史补拉。
- **Redis Streams 全局 backlog 预算**：H-29 已去掉会删 pending 的 `MAXLEN`，
  消费者长期不前进时 stream 仍会增长；全局预算/背压尚未实现。
