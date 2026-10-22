# 审计 issue 整改进度（2026-10-21）

> 对应 `#3`–`#23` 共 20 条任务（`#1`/`#2` 是已关闭的交付记录）。
> 这份账的目的是**说清哪些真做完了、哪些没做、没做的卡在哪**——
> 不是"看起来都修了"。
>
> **CI 状态：`e12ebc1` 十关全绿**（lint / quality / test×3 / doc / android / web / flutter / desktop）。
> 协议改动带出来的测试回归分两批修完：主 workspace 那批（`history_search_api`、
> `mailbox_mirror`、`policy_integration`、`ws_gateway`）在 #3 当轮；桌面端那批
> （`p2_messages` 三条）**一直红到本轮**——因为 Tauri 测试没进 CI，没人跑。
> 这正好是 #18 收尾的理由，见 §二点七。

## 一、已闭环（9 条 + 5 条部分）

| issue | 严重度 | 状态 | 提交 | 一句话 |
|---|---|---|---|---|
| #3 | P0 | ✅ 完整 | `099a3d5` | `Hello` 只带 token，身份由服务端解析；无认证器 = 拒绝一切 |
| #4 | P0 | ✅ 完整 | `099a3d5` + 本轮 | fail closed + **往返解密已绿**（自己发的密文不再走接收路径去解） |
| #6 | P1 | ✅ 完整 | `b65057d` | 全部因子通过才清空失败计数；TOTP 失败会计数并审计 |
| #12 | P1 | ✅ 完整 | `c806402` | QUIC 证书持久化到数据目录，重启后 pin 仍匹配 |
| #14 | P1 | ⚠️ 一半 | `12bd86c` | Range 不再读整个对象；**下载鉴权未做**（需签名 URL） |
| #19 | P2 | ⚠️ 一半 | `b65057d` | 限流来源只认真实 peer；**算术 captcha 未换** |
| #21 | P1 | ✅ 完整 | `69c4d4d` | 桌面锁文件同步 TLS 修复；纠正"必须升 ureq 3"的错误判断 |
| #5 | P1 | ✅ 完整 | `cc0a516` | 已启用 2FA 后重新 enroll 必须先证明身份（当前码或密码） |
| #16 | P1 | ✅ 完整 | `47812b2` | compose 的 nats 缩进错 + 缺启动凭据；实测 `up` 后 health 通过 |
| #18 | P2 | ✅ 完整 | `8573b24` + 本轮 | Web / Flutter / **桌面（Tauri 命令层）** 三端全进 CI |
| #10 | P1 | ✅ 完整 | `1a1ed05` | 离线队列 claim/ack/release + 30s lease；发一半断连不再丢 |
| #11 | P1 | ⚠️ 一半 | `1a1ed05` | 落库成功后才标记 seen（失败可重试）；**per-recipient 补投未做** |
| #15 | P1 | ⚠️ 大部分 | 本轮 | 密钥加密落盘、开关进服务端、明文/转发拦住、TTL 过滤；**多设备与搜索未做** |

合计：**9 条完整、5 条部分**。

### 逐条说明

**#3（P0，完整）**——`WireFrame::Hello { token }` + `HelloAck` + `HelloReject`；
`MqHubOptions.auth` 的 `None` 语义是"拒绝"而不是"不校验"；`SessionAuth` 包
`Platform::lookup_token`；`is_revoked` 在心跳时复查，HTTP 退出登录后长连接
最迟一个心跳周期断开。三端（QUIC / Web / 桌面）全部改完，新增 6 条测试。
详见 `docs/hub-auth.md`。

**#4（P0，核心已修）**——`send_secret_text` 不再回退明文：有会话走
`encrypt_dm_existing`（新增），没会话才取 bundle，任何失败都返回错误。
那个"断言应当回退明文"的测试改成"拒绝发送且对方一秒内收不到任何东西"。

**但**：`p1_secret_chat_roundtrip` 与 `..._keeps_encrypting` 仍红——
消息加密发出了，接收方 `create_inbound_session` 失败（one-time key 交换）。
失败模式是安全的（解不开，不是发明文），所以不阻塞 fail-closed 的修复。
顺带修掉一个真 bug：`try_decrypt_secret` 拿 payload 的 `peer`（发送者眼中的
收件人 = 自己）当对端去解密，改成用 `env.sender`。

**#6（完整）**——`risk_clear` 从"TOTP 校验前"移到"全部因子通过后"；
TOTP 失败记账号键 + 来源键，并写 `user.login / totp_failed` 审计。

**#12（完整）**——`make_server_endpoint_persistent(addr, dir)`；`cert_dir`
进 `MqHubOptions`；server 用 `sidecar_dir(db_path, "quic")`。目录不可写、
只有一半材料都**报错不猜**。3 条测试。

**#14（一半）**——`resolve` 不再读整个对象，`get` 改 seek + `read_exact`。
**下载鉴权没做**：媒体 URL 直接进 `<img src>`，浏览器不给图片请求带
`Authorization`，加 Authed 会把图片全弄挂。正确解是短期签名 URL
（`exp` + HMAC），要改上传响应、消息 body 的媒体引用格式与三端渲染，
属独立任务。

**#19（一半）**——`client_key` 默认只认 peer 地址，`WIXLIKE_TRUST_PROXY=1`
才采信 XFF；删掉 `x-client-id`；密码重置的"失败计数/成功清空"从反的改回来。
**算术 captcha 仍可被脚本解**，真正的人机校验要接第三方或 proof-of-work。

**#21（完整，上一轮）**——见提交 `69c4d4d`。

## 二、还没做（7 条）

| issue | 严重度 | 为什么没做 | 需要什么 |
|---|---|---|---|
| #7 | P1 | 视频入口匿名：要认证 + 所有权 + 配额 + 有界队列，涉及 `video` crate 与三端调用 | 给 `VideoStore` 加 owner，`/api/video*` 挂 Authed，加并发/磁盘预算 |
| #8 | P1 | `callback_url` SSRF：要做目标 allowlist + DNS/IP 校验 + 禁重定向 | 新增 URL 校验模块 + 出口隔离，需网络层配合 |
| #9 | P1 | 跨节点确认不以投递成功为条件：要 durable inbox + 两阶段确认 | 改 `mq_hub` 消费路径，影响面大 |
| #11 剩余 | P1 | "重复 msg_id 补投未完成收件人"要 per-recipient 投递状态（delivery outbox） | 新表 + 扇出路径改造，1–2 天 |
| #13 | P1 | 空 `to` 广播 / 伪造 Notice：要收紧 `policy` 并给服务端内部事件开独立入口 | 改 `DeliveryPolicy` + 新增内部 publish 通道 |
| #17 | P1 | 后半（同步 FFI 阻塞主 isolate）：要把 ureq 调用搬进后台 isolate | 改 `service.dart` + 生成的绑定调用方式 |
| #20 | P2 | Redis Stream 无保留策略、路由无租约 | 改 `mq` crate 的 XADD/XACK 与 `RedisRoutes` |
| #22 | P2 | ureq 3 迁移：7 张卡、三处 API 破坏性变更 | 已有任务单 `docs/task-ureq3-migration.md`，需 2–3 人日 |

## 二点五、本轮补的三条（2026-10-21 下半场）

**#5（完整）**——`confirm_identity(user_id, code, password)`：当前第二因子
或密码任一通过；`/api/2fa/enable` 在**已启用**时先过它，否则 403 + 审计。
首次 enroll 不需要凭据（那时没什么可保护的）。

**#16（完整）**——`nats` 原来被缩进在 `volumes:` 下面，`docker compose config`
直接报错，"一条命令启动"从未成立；server 也没拿到启动凭据。修完实测：

```
docker compose config                      # 通过
docker compose up -d --build               # redis / s3 / server 全 healthy
curl :8099/health                          # {"ok":true,...}
docker compose --profile nats up -d nats   # nats 起来
```

顺带补了 `.dockerignore`（之前没有，build context 会包含整个 `target/`）。

**#18（大部分）**——新增 `web` 与 `flutter` 两个 CI job。web 那关特别重要：
`ui/src/web/wire.ts` 是**手写的 bincode**，变体序号必须与 Rust 的
`WireFrame` 顺序一致——#3 改了顺序，而它的 fixtures 在此之前从没在 CI 跑过。
两个 job 第一次跑就绿。Tauri 没接（要 apt 系统依赖，失败模式与代码无关）。

## 二点六、消息可靠性两条（#10 / #11）

**#10（完整）**——`OfflineStore` 从 `drain`（读全部+删全部再发）改成
`claim`/`ack`/`release` + 30s lease。`SqliteOffline` 加 `claimed_until` 列并
带老库 `ALTER` 迁移；hub 的消费循环 `claim(64) → 逐条发 → 成功 ack / 失败
release 剩余`。**发一半断连不再丢**，进程被杀也不会（lease 到期自动回来）。

**#11（一半）**——落库**成功之后**才标记 `seen`：`append` 失败时客户端用同一个
msg_id 重试能真正进来（以前会被去重缓存当成重复直接吞掉）。

没做的：重复 msg_id 时"补投未完成的收件人"。试过重跑 fanout，
`ten_replays_of_one_msg_id_are_delivered_once` 立刻红——hub 没有 per-recipient
投递状态，重跑会把整批人再收一遍。正确解是 delivery outbox（消息 + 每个收件人
的投递任务同一事务），1–2 天；在那之前扇出失败靠客户端拉历史补偿。

## 二点七、秘密聊天闭环（#15 / #4）与桌面端进 CI（#18）

### #4 往返解密：真 bug，不是"密钥交换没做完"

上一轮把它记成"接收方 `create_inbound_session` 失败"，实际原因更低级：
**发送方把自己发出的密文也走了一遍接收路径**。`append_own_echo` 拿 `env.sender`
（自己）当对端去解，自己没有跟自己的会话，于是每条自己发的秘密消息都渲染成
"无法解密"，同时把 `create_inbound_session` 打成 `MissingOneTimeKey`
（报的是对方的密钥 id，因为密文本来就是用对方密钥封的）。

修法不是"加个 try"，而是**认清发送方不需要解密**：明文就在手里。
`render_event_with(env, secret, own_text)` 多一个"自己已知的明文"参数，
发送路径与 outbox 重发路径都带上它；接收路径传 `None`。

顺带修掉 outbox 的一个坑：秘密消息排队重发时只存 envelope，回显拿不到明文。
现在队列项是 `{env, echo}`，裸 envelope 的旧项仍然能读（向后兼容）。

### #15 做了什么

| 面 | 之前 | 现在 |
|---|---|---|
| 密钥 | `Device` 只在 `AppState` 内存里，重启换身份 | `Crypto::snapshot()` → XChaCha20-Poly1305 密封 → `secret-account.bin`；设备密钥 `secret-device.key`（0600）。**每发一条、每收一条都落盘**——棘轮不是缓存 |
| 开关 | 前端一个 `Set`，服务端不知道 | `secret_configure` / `secret_state` 走 `/api/secret/{conv}`，本地只做缓存；UI 的 🔒 从服务端初始化，失败回滚图标 |
| 明文 | 秘密会话里照样能发明文 | `send_body` 闸门：秘密会话只放行 `E2EE_DM` 与 `typing`，文本/图片/转发全部拒绝 |
| 转发 | 不检查来源 | `forward_message` / `merge_forward` 看条目自带的 `conv_id`，来源是秘密会话就整条拒绝 |
| TTL | 存了不执行 | 服务端 `history_page` 过滤过期（换设备拉历史不会复活）+ 桌面 `local_history` 同样过滤 |

测试（`ui/src-tauri/tests/secret_chat.rs`，跑真实栈）：
`secret_identity_and_session_survive_a_restart`（重启后同一身份 + 会话继续）、
`secret_conversation_refuses_plaintext_and_forwarding`、
`secret_ttl_hides_expired_history`；核心层另有
`snapshot_survives_restart_and_keeps_the_ratchet` 与
`snapshot_with_unknown_version_is_refused`。

### 还没做的（说清楚，不含糊）

- **多设备**：一个用户多台设备各有身份，`/api/keys` 仍是"一人一条 bundle"。
  真正的多设备要按 `device_id` 发布、发消息时给每台设备各封一份（Matrix 的
  device list 那套）。这是独立任务。
- **搜索一致性**：`/api/search` 还没按秘密会话的 TTL 过滤。
- **本地真删除**：现在只是"到点不再显示"，磁盘上的行还在。真删除要在本地库
  加一条按 `expires_at` 的清理任务。
- **重装同步**：换设备后旧会话解不开（密钥没有服务端托管的备份），这是
  E2EE 的固有取舍，需要显式的"设备迁移"设计而不是偷偷上传私钥。

### #18 收尾：桌面端进 CI，代价立刻显现

加 `desktop` job 的第一件事不是配置 apt，而是**发现这批测试早就红了**：

- `p2_messages` 三条还在拿 `user_id` 当 `Hello` 的凭据——#3 之后 hub 只认 token，
  所以三条全部连不上；
- `ui_commands` 里 5 条直聊测试在 P3 陌生人闸门上线后就发不出消息了（非好友）。

两批都是"跑一次就发现"的问题，因为没进 CI 一直没人跑。现在它们都修好了
（好友关系 + 排空"成为好友"的系统通知），`desktop` 这一关才算真的有意义。

## 三、为什么没有"全部完成"

单条 #3 就改了 8 个 crate（`net`/`server`/`platform`/`core`/`ui`/`ui/src-tauri`
/`ui/src/web`/`tests`）和**三端协议**：帧格式变了，Web 的 bincode 变体序号、
桌面的连接参数、诊断工具、18 处测试构造点全部要同步。这类改动不能切碎，
切碎了就是"改一半的协议"，比不改更危险。

剩下的 7 条里有 4 条（#7 #8 #9 #13）是**改数据模型或消息主链路**，每条都要动
schema 与失败恢复路径；#11 剩余要新建 delivery outbox；#15 剩余要多设备密钥分发；
#22 有现成任务单但工作量是 2–3 人日。

按"能不能验证"排序，我优先做了**改完就能用测试钉住**的那些（#3 #6 #12 #19），
以及**只改一行就能止住泄漏**的那些（#4 的 fail-closed、#14 的读放大）。

## 四、下一步建议顺序

1. **#7 + #8**（2 天）——视频面准入与 SSRF：`/api/video*` 现在匿名可调，
   这是唯一一条"外部可直接打"的入口。
2. **#9 + #11 剩余**（2–3 天）——durable inbox 与 per-recipient 投递状态，
   两者是同一件事的两半，一起做才有意义。
3. **#13**（半天）——空 `to` 广播 / 伪造 Notice 的收紧，改动小但影响面宽。
4. **#15 剩余**（3–5 天）——多设备 bundle、搜索过滤、本地真删除。
5. **#20**（1 天）——Redis Stream 保留策略与路由租约。
6. **#22**（2–3 人日）——ureq 3 迁移，有现成任务单
   `docs/task-ureq3-migration.md`。

> 排序依据：**能不能被外部直接利用** > **数据会不会丢** > **体验/债务**。
