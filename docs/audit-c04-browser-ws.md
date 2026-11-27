# C-04 关闭记录：默认浏览器实时通道走 v3 二进制协议

- 审计基线：`60895e18`
- 问题：`apps/desktop-web` 默认选 WASM core；`crates/client-web/src/ws.rs` 只解析 text JSON，不发 token/ClientHello/Hello/Subscribe，也没有游标恢复；服务端 `/ws` 只收 bincode binary。
- 结论：**C-04 关闭**（浏览器默认路径已能完成认证、订阅、收发、重连、按会话 seq 续传）。

## 实现

### 1. 浏览器 bincode 镜像

`crates/client-web/src/wire.rs` 按字段位置镜像 `crates/net/src/wire.rs` 与 `crates/core/src/types.rs`：

- `WireFrame` 11 个变体顺序一致；
- `Envelope` 11 个字段顺序一致；
- `MsgBody` 5 个、`Receipt` 2 个变体顺序一致；
- `Custom` payload 与 `json_bytes` 一样，在 bincode 里是 JSON 文本。
- 帧上限 16 MiB，超限/截断/尾部多余都拒绝。

`Envelope` 用本地镜像而不是依赖 `wxlike-core`，因为 core 带 rusqlite/vodozemac，不能编进 wasm；漂移由下面的字节测试和 contract 脚本钉住。

### 2. `/ws` 状态机

`crates/client-web/src/ws.rs`：

1. `onopen` 发 `ClientHello{device_id, protocol:3, caps:Resume}` + `Hello{token}`；
2. 收 `Auth` 后发 `Subscribe{scopes}`，再按本地 `last_seq` 逐会话发 `Resume{conv_id, cursor}`；
3. `Msg` → `EventEnvelope`（`event_id`/`conv`/`seq`/`kind`），`typing` 丢弃，`Recall`/`ReadReceipt` 走对应事件类型；
4. `HelloReject` 进入 fatal，不再重试；`Ping` 每 25s 发一次；`Event` 范围提示只作为缺口信号，不伪造消息；
5. `onclose`/`onerror` 后 `next()` 指数退避重连（1s→15s，最多 6 次），保留每个会话 `seq`，重连即 `Resume`；
6. 连接代次 `generation` 防止旧 socket 的 close 回调影响新连接；`Drop`/`close` 摘回调并关 socket。

### 3. UI 形状

`wire::ui_event_json` 在 WASM 出口把 Rust 的 `kind/conv/emitted_at_ms` 转成桌面 `core-contract.ts` 的 `event_type/aggregate_id/server_time_ms`，`payload` 直接是 `ChatMessage` 形状。这样 `wasm-core` 适配器不再收到 `kind` 字段而静默跳过。

## 验证

- `cargo test -p wxlike-client-web --lib`：11 passed（含 4 条 wire/事件映射）。
- `cargo clippy -p wxlike-client-web --all-targets -- -D warnings`。
- `cargo check -p wxlike-client-web --target wasm32-unknown-unknown`。
- `cargo test -p wxlike-net --test browser_wire`：4 passed，客户端编码 ↔ 真实 `wxlike_net::wire::WireFrame` 双向解码。
- `wasm-pack test --headless --chrome crates/client-web`：8 passed（原有 IndexedDB/WebCrypto/fetch/Web Locks）。
- `bash scripts/test-ws-browser.sh`：9 passed，真实 Chrome 连本地 fixture，走 `ClientHello/Hello/Auth/Subscribe`，收 `Msg(42)`，断线后重连并 `Resume(42)`，收 `Msg(43)`。
- `wasm-pack build` 产出 `packages/wasm-client`；`apps/desktop-web npm run typecheck` 通过。

## 边界

- 未声明 `StoredAck` capability：浏览器目前不向服务端发 `Ack`，断线窗口靠 `Resume` + 服务端 parked mailbox / HTTP 历史补齐；要支持“服务端确认已落库”再补 `Ack` 与 `EventSource` 的回执接口。
- `Event` 范围提示未触发自动 HTTP 补齐；缺口由上层历史分页负责。
- 真实双端 E2E（两台浏览器互发）仍需在联调环境跑；本地 fixture 只钉协议与重连，不替代业务 E2E。
