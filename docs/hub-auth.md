# 长连接身份：凭据，不是 `user_id`

> 2026-10-21 · 对应 issue #3（P0 / 审计编号 A01）· 影响 `crates/net`、`server`、
> `ui`（桌面 + 网页）、以及所有连 hub 的客户端。

## 一、修的是什么

改动前的握手帧是：

```rust
WireFrame::Hello { user_id: String }
```

hub 收到就 `peers.entry(user_id)` → `bind_user` → `offline.drain(user_id)`。
**没有任何校验**。而 `user_id` 是 16 位十六进制，出现在每条消息的 `sender`
字段里，日志、转发、截图都能泄露。

知道它的人可以：

| 攻击 | 后果 |
|---|---|
| 用别人的 `user_id` 连上来 | 收到那个人的全部实时消息 |
| 同上，且那个人有离线消息 | `drain` 是**删除式**的：消息被取走，真正的收件人再连就空了 |
| 用别人的 `user_id` 发消息 | 以那个人的身份说话 |

HTTP 的 Bearer、密码、TOTP 都保护不了这条路径——它根本不经过 axum。

## 二、新协议

```rust
pub enum WireFrame {
    Hello { token: String },                                   // 0
    HelloAck { user_id: String, session_id: String },           // 1
    HelloReject { reason: String },                            // 2
    Msg(Envelope),                                             // 3
    Ping,                                                      // 4
}
```

握手顺序：

```
client                              hub
  |-- Hello { token } ------------->|
  |                                 |  auth.authenticate(token)
  |                                 |     None -> HelloReject + close
  |<-- HelloAck { user_id, sid } ---|
  |                                 |  注册 peers / 绑路由 / drain 离线队列
```

三条硬规则：

1. **客户端不知道自己的 `user_id`**，它是 `HelloAck` 告诉它的。发消息时用
   `HelloAck` 里的值填 `Envelope::sender`，不是自己拼。
2. **认证失败时什么都不做**：不注册、不绑路由、不 `drain`。离线队列只在
   认证通过后才被取走。
3. **拒绝要先给原因**：`HelloReject { reason }` 先发，再关连接。没有这一帧，
   客户端只能看到 `connection lost: closed by peer: 0`，分不清"凭据过期"和
   "网络断了"。

> QUIC 的实现细节：`close()` 会丢掉未发出的流数据，所以 `reject_hello` 在
> 发完拒绝帧后等了 100ms 再关。WS 载体是立即发出的，这段等待对它无影响。

## 三、`auth` 字段的语义：`None` = 拒绝

`MqHubOptions::auth: Option<Arc<dyn HubAuthenticator>>`。

**`None` 表示拒绝所有连接，不是"不校验"**。一个没有认证器的 hub 就是
"报个名字就能冒充"的服务，默默放行比拒绝更危险——拒绝会让配置错误立刻
暴露，放行会让它一直安静地错下去。

装配层（`server/src/lib.rs`）的实现：

```rust
struct SessionAuth { platform: PlatformState }

impl HubAuthenticator for SessionAuth {
    fn authenticate(&self, token: &str) -> Option<HubIdentity> {
        self.platform.lookup_token(token).ok().flatten().map(|user_id| HubIdentity {
            user_id,
            session_id: session_fingerprint(token),  // 指纹，不是 token 本身
        })
    }
    fn is_revoked(&self, token: &str) -> bool {
        !matches!(self.platform.lookup_token(token), Ok(Some(_)))
    }
}
```

- `session_id` 发给客户端的是 token 的 **sha256 前 16 hex**，不是 token：
  客户端拿它做"这是我的哪个设备"的对照，泄露了也换不来凭据。
- `is_revoked` 在**收到 `Ping` 时**复查。HTTP 退出登录 / 远程下线之后，
  长连接最迟在一个心跳周期内断开。查库失败按"未吊销"处理——抖动不该把
  所有人踢下线。

## 四、三个载体都要改

| 载体 | 改了什么 |
|---|---|
| QUIC（桌面/移动） | `Client::connect_verified(addr, name, **token**, pins)`；内部先收 `HelloAck` 再返回，`Client::user_id` 来自服务端 |
| WebSocket（浏览器） | `ui/src/web/wire.ts` 的 bincode 变体序号 **0/1/2/3/4**；`hub.ts` 的 `connect(url, token)`，`HelloAck` 记身份、`HelloReject` 停止重连 |
| FFI（Flutter） | 不受影响：移动端走 HTTP，不连 QUIC hub |

`ui/src-tauri` 的 `connect_hub` 与 `net_diag` 都改成从 `AppState::auth_token()`
取凭据（新增的访问器）。

> **变体序号是隐式契约**：bincode 只写 index，不写字段名。Rust 那边调整
> `WireFrame` 的声明顺序，`wire.ts` 与 `wire.selftest.mjs` 的 fixtures 必须
> 同步改，否则线上表现为"帧解析错位"而不是编译错误。

## 五、测试

`crates/net/tests/hub_auth.rs`（6 条）：

- 没有认证器 → 拒绝，错误里说清是"服务端没配认证器"
- 空凭据 / 未知凭据 → 拒绝
- `HelloAck` 的 `user_id` 来自认证器（不是客户端传的 token 本身）
- 凭据被吊销 → 下一次心跳断开
- `DenyAll` 与"没配"区分开（都是拒绝，但原因不同）
- `StaticAuth` 的语义（测试/本地开发用）

`ui/src-tauri/tests/ui_commands.rs`：

- `p1_secret_chat_refuses_to_send_without_peer_bundle`（原
  `..._falls_back_to_cleartext...`）：断言拒绝发送，且对方一秒内收不到任何东西
- 全部 12 处 `connect_hub` 改用凭据

`ui/src/web/wire.selftest.mjs`：变体序号、`HelloAck` / `HelloReject` 往返、
6 个 fixture 的 index 从 01 改 03。

## 六、还没做的（明确列出，不是忘了）

1. **短期 hub ticket**：现在直接把长期 Bearer token 交给长连接。浏览器场景
   更安全的做法是先用已认证的 HTTP 换一个**短期、单次**的 ticket，再拿它
   `Hello`。这样 token 不进 URL（WS 的 query string 会进日志）、也不长期驻留
   在 hub 内存里。
2. **设备绑定与吊销联动**：`HubIdentity` 目前只有 `user_id` + 会话指纹。
   要做到"设备列表里点下线 → 那条连接立刻断"，需要把 `device_id` 也带进
   identity，并在 hub 侧维护"用户 → 连接集合"的反向索引。
3. **`Hello` 的限速**：现在任何人都可以无限次尝试 `Hello`（每次都要查一次
   session 表）。应当在连接级做失败计数，超过阈值直接拒绝并断开。

这三条都记在 issue #3 的验收项里，属于后续增量，不阻塞本次修复。
