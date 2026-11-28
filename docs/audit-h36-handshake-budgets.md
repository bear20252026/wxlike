# H-36：握手、预认证连接与注册的资源边界

审计条目：`H-36 握手/预认证连接无资源边界`（默认生产）。

## 修复

1. `crates/net/src/policy.rs::Limits`
   - 新增 `handshake_timeout_ms`（默认 5s）：从连接进入 hub 到完成 Hello/Auth
     的总 deadline，慢握手不能再无限占用任务与缓冲区。
   - 新增 `max_preauth_conns`（默认 1024）：单 hub 未认证连接总数上限。
   - 新增 `max_preauth_conns_per_ip`（默认 32）：同一对端 IP 的未认证连接上限。
   - 新增 `max_credential_bytes`（默认 8 KiB）：v3 `ClientHello` 之后的
     credential 帧独立上限，不再继承 `max_frame_bytes` 的整帧预算。
2. `crates/net/src/mq_hub.rs`
   - `PreauthBudget` 用 RAII permit 同时占用全局与每 IP 槽位；握手成功、失败、
     超时或任务取消都会释放，不会泄漏计数。
   - `serve_transport` 先取 permit，再用 `tokio::time::timeout` 包住
     `accept_hello`；超时先发 `HelloReject("handshake timed out")` 再断开。
   - `read_credential` 单独校验 `max_credential_bytes`，超限先拒绝再断开。
   - `register_peer` 把 per-user 上限检查与 peers 插入放在同一个写锁内；
     并发握手不能再同时看到 `max - 1` 而一起放行。
   - `HelloAck`/`Auth` 移到注册成功之后发送；被上限拒绝的连接不会先收到
     `HelloAck`。注册后回执发送失败会走 `release_conn` 清理。
3. `crates/net/src/conn.rs`
   - `PeerConn` 携带可选的 `peer_ip`；QUIC 从 `remote_address()` 取，WS 从
     axum `ConnectInfo<SocketAddr>` 取。地址只用于预认证预算，不参与身份或授权。
4. `server/src/lib.rs`
   - `/ws` 处理器读取真实对端 `SocketAddr` 并传入 `peer_conn`。仍使用
     `into_make_service_with_connect_info`，不信任可伪造的 `X-Forwarded-For`。

## 测试与证据

- `cargo test -p wxlike-net --test proto_v3`：16 passed，新增 5 个用例：
  - 8 个并发握手抢同一账号的 1 个名额，只有 1 个收到 `HelloAck`；
  - 50ms 未发 Hello 的连接收到 `handshake timed out`；
  - v3 credential 超过 16 字节被拒；
  - 同一 IP 的第 2 条预认证连接被拒，首条认证后槽位释放、第 3 条可进；
  - 全局预算为 1 时，不同 IP 的第 2 条也被拒。
- `cargo test -p wxlike-net`：全量通过（含 QUIC/WS/route/reliable_msg 等套件）。
- `cargo check -p wxlike-server --all-targets` 通过。

## 边界

- 预认证预算按 hub 实例计数，不是跨节点全局；多 hub 部署需要每节点分别配置。
  审计要求的是单 hub 的资源边界，跨节点总量应由入口层/负载均衡另设上限。
- WS 的 per-IP 预算使用 TCP 对端地址；若部署在反向代理后，所有客户端会共享
  代理地址，需要入口层按真实客户端限流，或把代理地址列为可信并显式传递。
- QUIC 的传输层握手发生在 `serve_transport` 之前，本修复约束的是应用层
  Hello/Auth 与未认证连接预算，不改变 quinn 自身的 accept 队列。
- 没有做真实公网慢握手/海量连接压测；行为由上述进程内传输用例覆盖。
