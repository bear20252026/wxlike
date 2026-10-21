# 审计 issue 整改进度（2026-10-21）

> 对应 `#3`–`#23` 共 20 条任务（`#1`/`#2` 是已关闭的交付记录）。
> 这份账的目的是**说清哪些真做完了、哪些没做、没做的卡在哪**——
> 不是"看起来都修了"。
>
> **CI 状态：`ff2859c` 七关全绿**（lint / quality / test×3 / doc / android）。
> 改协议带出来的三处测试回归（`history_search_api`、`mailbox_mirror`、
> `policy_integration`、`ws_gateway`）已全部修完——它们都是"还在用
> `user_id` 当凭据连接"，现在统一用注册返回的 token。

## 一、本轮已闭环（6 条 + 1 条部分）

| issue | 严重度 | 状态 | 提交 | 一句话 |
|---|---|---|---|---|
| #3 | P0 | ✅ 完整 | `099a3d5` | `Hello` 只带 token，身份由服务端解析；无认证器 = 拒绝一切 |
| #4 | P0 | ⚠️ 核心已修 | `099a3d5` | 秘密聊天 fail closed（不再回退明文）；**往返解密仍红**，归 #15 |
| #6 | P1 | ✅ 完整 | `b65057d` | 全部因子通过才清空失败计数；TOTP 失败会计数并审计 |
| #12 | P1 | ✅ 完整 | `c806402` | QUIC 证书持久化到数据目录，重启后 pin 仍匹配 |
| #14 | P1 | ⚠️ 一半 | `12bd86c` | Range 不再读整个对象；**下载鉴权未做**（需签名 URL） |
| #19 | P2 | ⚠️ 一半 | `b65057d` | 限流来源只认真实 peer；**算术 captcha 未换** |
| #21 | P1 | ✅ 完整 | `69c4d4d` | 桌面锁文件同步 TLS 修复；纠正"必须升 ureq 3"的错误判断 |
| #5 | P1 | ✅ 完整 | `cc0a516` | 已启用 2FA 后重新 enroll 必须先证明身份（当前码或密码） |
| #16 | P1 | ✅ 完整 | `47812b2` | compose 的 nats 缩进错 + 缺启动凭据；实测 `up` 后 health 通过 |
| #18 | P2 | ⚠️ 大部分 | `8573b24` | Web（tsc + bincode 自测）与 Flutter（analyze + test）进 CI；**Tauri 未接** |

合计：**6 条完整、4 条部分**。

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

## 二、本轮没做（13 条）

| issue | 严重度 | 为什么没做 | 需要什么 |
|---|---|---|---|
| #7 | P1 | 视频入口匿名：要认证 + 所有权 + 配额 + 有界队列，涉及 `video` crate 与三端调用 | 给 `VideoStore` 加 owner，`/api/video*` 挂 Authed，加并发/磁盘预算 |
| #8 | P1 | `callback_url` SSRF：要做目标 allowlist + DNS/IP 校验 + 禁重定向 | 新增 URL 校验模块 + 出口隔离，需网络层配合 |
| #9 | P1 | 跨节点确认不以投递成功为条件：要 durable inbox + 两阶段确认 | 改 `mq_hub` 消费路径，影响面大 |
| #10 | P1 | 离线队列先删后发：要 lease/claim 状态机 | 改 `SqliteOffline` schema + 消费循环 |
| #11 | P1 | 去重先于持久化：要"消息 + 投递任务"同一事务 | 改 `handle_inbound` 的顺序与失败恢复 |
| #13 | P1 | 空 `to` 广播 / 伪造 Notice：要收紧 `policy` 并给服务端内部事件开独立入口 | 改 `DeliveryPolicy` + 新增内部 publish 通道 |
| #15 | P1 | 秘密聊天闭环：设置没接入发送/转发、TTL 不落实、密钥只在进程内 | 大工程（持久化 + 多端 + 搜索一致性），也是 #4 往返失败的上游 |
| #17 | P1 | 后半（同步 FFI 阻塞主 isolate）：要把 ureq 调用搬进后台 isolate | 改 `service.dart` + 生成的绑定调用方式 |
| #18 剩余 | P2 | 只剩 Tauri：它在独立 workspace，Linux 上要 `libwebkit2gtk-4.1-dev` 等系统依赖 | 一个 job + apt 依赖，失败模式是"apt 版本不对"而非代码 |
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

## 三、为什么没有"全部完成"

单条 #3 就改了 8 个 crate（`net`/`server`/`platform`/`core`/`ui`/`ui/src-tauri`
/`ui/src/web`/`tests`）和**三端协议**：帧格式变了，Web 的 bincode 变体序号、
桌面的连接参数、诊断工具、18 处测试构造点全部要同步。这类改动不能切碎，
切碎了就是"改一半的协议"，比不改更危险。

剩下 13 条里有 6 条（#7 #8 #9 #10 #11 #15）是**改数据模型或消息主链路**，
每条都要动 schema 与失败恢复路径；#16 需要 docker 才能验证；#22 有现成任务单
但工作量是 2–3 人日。

按"能不能验证"排序，我优先做了**改完就能用测试钉住**的那些（#3 #6 #12 #19），
以及**只改一行就能止住泄漏**的那些（#4 的 fail-closed、#14 的读放大）。

## 四、下一步建议顺序

1. **#16**（半小时，需要一台有 docker 的机器）——它是"一条命令启动"的承诺，
   现在 `compose config` 都过不了。
2. **#5**（半天）——TOTP 重置越权是账号接管链的一环，改动可控。
3. **#15 + #4 的往返**（2–3 天）——秘密聊天要么闭环，要么明确标成"实验特性"。
   现在这个状态最糟：UI 上开着开关，实际发不出去也解不开。
4. **#10 + #11**（2–3 天）——消息可靠性，两者改的是同一段代码，一起做。
5. **#7 + #8**（2 天）——视频面的准入与 SSRF。
6. **#18**（1 天）——把 Tauri / Web / Flutter 接进 CI，否则上面这些修完
   还是可能被下一次改动悄悄破坏。
