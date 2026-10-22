# 审计 issue 整改进度（2026-10-21）

> 对应 `#3`–`#23` 共 20 条任务（`#1`/`#2` 是已关闭的交付记录）。
> 这份账的目的是**说清哪些真做完了、哪些没做、没做的卡在哪**——
> 不是"看起来都修了"。
>
> **CI 状态：`e316d2e` 十关全绿**（lint / quality / test×3 / doc / android / web / flutter / desktop）。
> 协议改动带出来的测试回归分两批修完：主 workspace 那批（`history_search_api`、
> `mailbox_mirror`、`policy_integration`、`ws_gateway`）在 #3 当轮；桌面端那批
> （`p2_messages` 三条）**一直红到本轮**——因为 Tauri 测试没进 CI，没人跑。
> 这正好是 #18 收尾的理由，见 §二点七。

## 一、已闭环（17 条）

| issue | 严重度 | 状态 | 提交 | 一句话 |
|---|---|---|---|---|
| #3 | P0 | ✅ 完整 | `099a3d5` | `Hello` 只带 token，身份由服务端解析；无认证器 = 拒绝一切 |
| #4 | P0 | ✅ 完整 | `099a3d5` + 本轮 | fail closed + **往返解密已绿**（自己发的密文不再走接收路径去解） |
| #6 | P1 | ✅ 完整 | `b65057d` | 全部因子通过才清空失败计数；TOTP 失败会计数并审计 |
| #12 | P1 | ✅ 完整 | `c806402` | QUIC 证书持久化到数据目录，重启后 pin 仍匹配 |
| #14 | P1 | ✅ 完整 | `12bd86c` + 本轮 | Range 只读那一段；**下载要授权**（签名 URL 或"登录 + 归属"） |
| #19 | P2 | ⚠️ 一半 | `b65057d` | 限流来源只认真实 peer；**算术 captcha 未换** |
| #21 | P1 | ✅ 完整 | `69c4d4d` | 桌面锁文件同步 TLS 修复；纠正"必须升 ureq 3"的错误判断 |
| #5 | P1 | ✅ 完整 | `cc0a516` | 已启用 2FA 后重新 enroll 必须先证明身份（当前码或密码） |
| #16 | P1 | ✅ 完整 | `47812b2` | compose 的 nats 缩进错 + 缺启动凭据；实测 `up` 后 health 通过 |
| #18 | P2 | ✅ 完整 | `8573b24` + 本轮 | Web / Flutter / **桌面（Tauri 命令层）** 三端全进 CI |
| #10 | P1 | ✅ 完整 | `1a1ed05` | 离线队列 claim/ack/release + 30s lease；发一半断连不再丢 |
| #11 | P1 | ✅ 完整 | `1a1ed05` + 本轮 | 落库成功后才标记 seen；**delivery outbox 按收件人记账，重试只补没收到的人** |
| #15 | P1 | ⚠️ 大部分 | 本轮 | 密钥加密落盘、开关进服务端、明文/转发拦住、TTL 过滤；**服务端兜住转发与搜索**（移动端仍无密聊） |
| #7 | P1 | ✅ 完整 | 本轮 | 视频控制面要登录 + 归属 + 配额；播放面与控制面拆开 |
| #8 | P1 | ✅ 完整 | 本轮 | 回调目标默认只认管理员白名单；私网/回环/重绑定/重定向全拒 |
| #9 | P1 | ✅ 完整 | 本轮 | 跨节点消费"投递成功才 ack"；目标 hub 上没人就落他的离线队列 |
| #13 | P1 | ✅ 完整 | 本轮 | 用户通道禁广播/禁伪造 Notice；直聊三元组必须自洽 |
| #14 | P1 | ✅ 完整 | 本轮 | 媒体下载要授权：会话成员 / 上传者 / 短期签名 URL；头像走 public |
| #17 | P1 | ✅ 完整 | 本轮 | FFI 搬进后台 isolate；CI 校验 release 合并清单带 INTERNET |
| #19 | P2 | ✅ 完整 | 本轮 | 伪造转发头换不了限流桶；恢复入口失败计数 + 可过人机校验 |

合计：**17 条完整**。

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

## 二、还没做（1 条部分 + 1 条运维债）

| issue | 严重度 | 为什么没做 | 需要什么 |
|---|---|---|---|
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

**#11**——分两步：先做到"落库**成功之后**才标记 `seen`"（`append` 失败时客户端
用同一个 msg_id 重试能真正进来）；后来用 delivery outbox 补齐了"重复 msg_id 时
只补没收到的收件人"，见 §二点九。

当时试过直接重跑 fanout，`ten_replays_of_one_msg_id_are_delivered_once` 立刻红——
hub 没有 per-recipient 投递状态，重跑会把整批人再收一遍。所以那次没硬来，
留到 outbox 落地。

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

## 二点八、视频面：准入与 SSRF（#7 / #8）

这两条是审计里**唯一能被外部直接命中**的入口，所以先做。

### 切面

| 面 | 路由 | 认证 |
|---|---|---|
| 播放 | `GET /api/video/{id}` 及其 `playlist/seg/play.mp4/thumb/select` | 匿名 |
| 控制 | `POST /api/video`、`POST/GET /api/video/jobs*` | `Bearer` 令牌 |

播放面保持匿名是**故意的**：`<video src>` 与 HLS 播放器不会给分片带
`Authorization`（与 #14 的媒体面同一约束）。要给播放加鉴权，正确解是短期签名
URL，那是另一条任务。

控制面的闸门是 **axum middleware**（`policy::guard`），在**解析请求体之前**
挡掉匿名请求。只在 handler 里查是不够的：`Multipart` extractor 先跑，匿名的
`POST /api/video/jobs` 会先回 400，而且合法的 500 MiB multipart 会被完整读进
内存——这正是要防的那件事。`auth = None` 的语义是**拒绝一切**，与 hub 同一约定。

### 归属与配额

- `video_jobs.owner`（老库用 `pragma_table_info` + `ALTER TABLE` 补列）与
  `video_owners(video_id, owner, size)`；
- 别人的作业一律 **404**（403 等于确认它存在）；重试只认提交者；
- 配额：单用户 4 个排队作业 / 2 GiB、全站 32 GiB、单用户累计上传 2 GiB，
  超了**提交时**就 429，不等 worker 转完；
- 拒绝形状 `Rejected { kind }`：`Quota` → 429、`BadCallback` → 400。

### 回调（SSRF）

默认**不接受客户端给的 `callback_url`**；管理员用
`WXLIKE_VIDEO_CALLBACK_ALLOW=host1,host2` 开白名单。校验两处：提交时查协议/
主机/IP 字面量，发送前**重新解析 DNS**（只查一次会被重绑定绕过），任何一个
解析结果落在私网就整体拒绝。拒绝的地址族：回环、RFC1918、链路本地、CGNAT、
组播、保留段、`0.0.0.0`、以及 v4-mapped IPv6。回调还带共享令牌
`x-wxlike-video-token`（与平台侧同一个 `WXLIKE_VIDEO_CALLBACK_TOKEN`）——
以前视频侧根本不发这个头，配了令牌的部署里回调会一直 401。

详见 `docs/video-control-plane.md`，验收用例在
`crates/video/tests/video_access.rs`（8 条，含令牌与白名单的边界）。

## 二点九、投递可靠性：确认、补投与 durable inbox（#9 / #11）

这两条是同一个问题的两面，改的是同一段代码：

- **#9**：跨节点消费 `consume_entry` 之后无条件 ack——目标 hub 上没人连着、
  写连接失败，都算"投递完成"，消息只剩源端归档；
- **#11**：`seen` 命中就 `return`，所以"客户端用正确的幂等 id 重试"反而补不上
  上次失败的收件人。

### 现在的语义分三层

| 层 | 在哪 | 含义 |
|---|---|---|
| 已持久化 | `messages` | 归档是权威（拉历史能找回） |
| 已投递 | `delivery_outbox(msg_id, recipient, state)` | 交给了实时连接**或**收件人的离线队列 |
| 已读 | 信箱游标 | 与投递无关 |

### 扇出

`fanout_content` 只投 `state = 'pending'` 的收件人：

1. 落库成功后 `ensure_deliveries(msg_id, recipients)`（幂等，`ON CONFLICT DO NOTHING`）；
2. 逐个投递，成功当场 `mark_delivered`；
3. 同 msg_id 再来一次（客户端重试）→ 只补还 pending 的人。已投递的人
   一条都不会再收（`ten_replays_of_one_msg_id_are_delivered_once` 仍然绿）。

### 投递的去路

`deliver_via_route` 三条分支，返回 `Err` 表示**没交出去**：

- 在本 hub → `deliver_to` **await** 每个连接的写（以前是 `spawn` 后立刻返回，
  调用方永远不知道有没有人收到）；写成功数 > 0 算完成，否则落离线队列；
- 在别的 hub → 投给那个 hub 的队列；publish 失败 → 任务留 pending，下次重试；
- 谁都不知道他在哪 → 落离线队列。

跨节点消费（`deliver_bytes`）现在返回 `Result`：**`Ok` 才 ack**。目标 hub 上
没人连着就 `park` 进它自己的离线队列（claim/ack + 30s lease，就是 #10 那张表）；
`park` 失败则不 ack，等 lease 到期由 patrol 重投。

### 还没做到"端到端已读确认"

离线队列的 ack 仍然是"**写进 socket**"就算，不是"客户端处理完"。所以
"socket 写成功但客户端随即崩溃"这个窗口里，消息只在归档里，靠客户端重连后
拉历史/同步补偿。真正的端到端确认要加一个 `Ack{msg_id}` 帧并让 claim/ack
等它——那是协议改动，单独排期。文档里不把它说成"已解决"。

### 验收

`crates/net/tests/delivery_outbox.rs`（3 条，都带故障注入）：

- `offline_recipient_gets_a_durable_copy_instead_of_a_lost_ack` —— 人不在线 →
  离线队列里有 1 条，投递任务记为 delivered，重连能取到；
- `replay_fills_the_missing_recipient_without_duplicating` —— 落队列注入失败 →
  任务留 pending；同 ID 重试 3 次后 carol 补齐（且只有 1 条），bob 一条没多收，
  归档仍然只有 1 行；
- `cross_hub_frame_for_an_absent_user_is_parked_not_dropped` —— 路由指向 hub-b
  但人不在 → hub-b 落**自己**的离线队列，源 hub 不重复存。

## 二点十、用户通道的边界（#13）

三条以前"合法普通账号"就能做的事，现在全部关掉：

| 行为 | 以前 | 现在 |
|---|---|---|
| 空 `to` 的内容帧 | 发给本 hub 所有在线用户 + 其他 hub 各一份 | 非群会话直接丢弃（群靠成员表扇出，不看 `to`） |
| `Notice`（系统文案） | 跳过 `send_gate` 与关系过滤，谁都能发 | 只有 `HubHandle::publish`（服务端内部入口）能发 |
| `d-<a>~<b>` 里的 `to` | 想填谁填谁 | sender 必须是 a、b 之一，`to` 必须是另一个人 |

三个细节值得记下来：

1. **不能按 `kind` 判断系统帧**。`kind_of()` 把 `Custom`（typing、@、红包、
   通话信令）也归成 `MsgKind::System`，按 kind 拦会把用户自己的功能帧一起打死。
   形状才是契约：系统文案只有 `MsgBody::Notice` 这一种。
2. **回执必须放行**。读回执的 body 就是空 `Notice` + `receipt`，所以"禁掉
   Notice"要排除控制帧（receipt / recall / typing），否则读回执和撤回一起失灵。
3. **服务端内部入口不经过这条路**。平台的好友/群生命周期通知走
   `HubHandle::publish` → `route_envelope`，不受用户侧规则约束——
   `friends_system_event` 与 `only_the_server_can_send_system_notices` 两条
   测试钉住了"真的系统通知照样送达"。

顺带修掉两个连带问题：

- **写连接必须超时**。`deliver_to` 改成 await 之后，对端不读流会把发送者的
  入站循环一起拖死（本地跑测试直接挂住 20 分钟）。加 2s 超时，超时按"没写成功"
  处理 → 落离线队列。
- **没有 msg_id 的老帧（v1）不能被当成"全都投递过"**。`hex::encode(NO_MSG_ID)`
  是一串 0，查投递任务得到空列表，于是被误判成"已投递完"而一条都不发。

验收：`crates/net/tests/broadcast_guards.rs` 4 条 + `delivery_policy.rs` 的
`only_the_server_can_send_system_notices` + `cross_hub.rs` 里"用户广播被拒、
服务端广播照旧"。

## 二点十一、媒体下载授权（#14）

`media_id` 是 SHA-256：知道文件就能算出它，日志和转发也会泄露它。
**内容地址不是权限**——所以下载必须另有凭据。

### 两条凭据，二选一

| 凭据 | 谁用 | 怎么判 |
|---|---|---|
| `Authorization: Bearer` | 桌面 / 移动端（走 Rust 命令层） | 上传者本人，或媒体所属会话的成员 |
| `?exp=&sig=` 短期签名 | 浏览器 `<img>`、分享页（带不了头） | `hex(HMAC-SHA256(key, "<id>:<exp>"))`，过期即废 |

签名由 `GET /api/media/{id}/url` 签发（默认 5 分钟，上限 1 小时），
签发前**先按同一套规则判权限**——没权限的人连签都签不出来。

拒绝的形状：匿名 **401**（连"这个 id 存不存在"都不说）、登录但无权 **404**
（403 等于确认它存在）。

### 归属从哪来

上传时用 multipart 的 `conv` 字段声明（`media.conv_id`）。没有声明 = **只有上传者**
能取，不是"默认公开"。`conv` 必须是自己所在的会话，否则 403——不能把附件塞进
别人的会话。

头像这类"所有登录用户都要看"的资源走 `visibility=public`（`media.public`），
**仍然要求登录**：匿名枚举不做。一旦公开就不回收（别的用户引用着它）。

### 顺带

- `Platform::may_read_conv` 成为唯一一份"谁能读这个会话"的规则，
  `server::authorize_conv` 改为调它（两份实现迟早漂移）；
- 签名密钥空的时候自动生成（空 HMAC key 的签名谁都能算）；多实例部署要用
  `WXLIKE_MEDIA_SECRET` 配成同一个值；
- 老库迁移：`media.conv_id` / `media.public` 用 `pragma_table_info` 补列，
  老媒体的归属是空 → 只有上传者能取（宁可严一点）。

### 验收

`crates/platform/tests/media_access.rs`（5 条）：

- 非成员知道 id 也拿不到（匿名 401 / 外人 404 / 会话两端 200）；
- 没声明会话 = 只有上传者能取；
- 签名 URL 不带令牌可下、篡改签名 401、过期 401、无权限签不出来；
- 公开资源登录用户都能看、匿名仍 401；
- 不能把附件挂到别人的会话上（403）。

`media.rs` 的既有用例改成带凭据/带会话（14 条），`net/tests/media_delivery.rs`
的两条端到端用例也改成"上传声明会话 + 对方带令牌取"。

## 二点十二、移动端 FFI 与 Android 权限（#17）

### 权限：源码对不算，合并结果对才算

`INTERNET` 现在写在 `apps/mobile/android/app/src/main/AndroidManifest.xml`（上一轮加的）。
但"源码里写了"不等于"包里有了"：Flutter 模板只把它放在 `src/debug/`（给 hot reload），
第三方依赖也可能贡献或意外移除权限。所以 CI 的 android job 在 `flutter build apk
--release` **之后**去读合并出来的 `build/**/release/AndroidManifest.xml`，没有
`android.permission.INTERNET` 就让整关红。

症状描述也写进注释了：`Permission denied (os error 13)`，看起来像服务端没起，
其实根本没联网权限。

### 同步 FFI：搬进后台 isolate，不是"每次起一个"

绑定层是同步的（`crates/client` 用 ureq，单次超时 10 秒）。以前这些调用直接跑在
UI isolate 上，服务端慢几秒整页就卡住——滚动、取消、进度全停。

现在 `apps/mobile/lib/src/service.dart` 里有一个**长期存活的 worker isolate**：

- 界面侧 `SendPort` 发 `[id, op, args]`，等自己的 `Completer`；
- worker 持有唯一的 `WxlikeClient`（uniffi object，背后是 Rust 的 Arc + 一条
  SQLite 连接），命令表 `_dispatch` 就是原来的方法转发；
- 返回值是 uniffi 的 record / enum / exception，**都是纯 Dart 类**，能跨界；
- 初始化失败 / worker 崩溃都会让 `start()` 抛错，不会静默卡住。

**为什么不是"每次调用起一个 isolate"**：`WxlikeClient` 跨 isolate 传不了，
每次新建都要重开库、重起补发线程。所以 worker 必须长期活着，`close()` 用
`Isolate.kill`——卡在 ureq 里的调用不会理"请退出"这种礼貌请求。

`session()` 保持**同步**：改成本地缓存，登录/登出/注册时刷新。它只读本地状态，
没有阻塞风险；界面代码（`main.dart`）因此一行没改。

### 验收

- `flutter test` 里新增 `慢 FFI 调用不阻塞 UI isolate`：对一个连不上的地址发
  `login()`（最长 10 秒），同时 `Future.delayed(200ms)` 必须在 2 秒内兑现。
  如果 FFI 又把 isolate 堵住，这个定时器只能等调用跑完才醒 → 失败。
- CI：release 合并清单必须带 INTERNET。
- 真机验收（`docs/mobile-acceptance.md`）：注册、登录、发消息；服务端故意延迟
  数秒时页面仍能滚动、取消、显示进度。

## 二点十三、秘密聊天的服务端兜底（#15 的服务端半边）

桌面端的拦截一直在（发送链、转发、明文、TTL 过滤），但**客户端自觉不能当安全
边界**——HTTP 控制面是另一条入口。这轮把服务端自己守的那半边补上：

| 行为 | 服务端现在 |
|---|---|
| 秘密会话的消息转发出去 | `POST /api/conv/{c}/forward` 403 |
| 普通消息转进秘密会话 | 同上 403（接收方会以为它受保护） |
| 消失消息到期 | 历史**和搜索**都不再返回（原来只过滤了历史） |

搜索那条是关键漏洞：历史里已经过滤，搜索结果却还能翻出到期消息——"消失"只
完成了一半。

### 仍然没做的（不吹）

- **移动端没有密聊**：`crates/ffi` 没暴露 e2ee 接口，`docs/mobile-acceptance.md`
  里记着这是已知偏差。验收清单里的"重装同步、多设备密钥变更"因此仍然为空。
- **多设备会话同步**：换设备 = 新身份 = 安全码变化（这是 E2EE 的固有语义，
  不是 bug），但"第二台设备加入后自动建会话"没有实现。
- **重新安装**：本地密钥没了，老消息永久解不开——E2EE 的固有代价，服务端
  只保证"不把过期消息重新发下去"。

### 验收

`server/tests/secret_enforcement.rs`（3 条，真栈）：

- `a_secret_message_cannot_be_forwarded_out` —— 403，且错误里说清原因；
- `a_normal_message_cannot_be_forwarded_into_a_secret_conv` —— 403；
- `expired_secret_messages_leave_both_history_and_search` —— 30 秒 TTL 到期后
  搜索与历史都取不到（唯一慢的一条，31 秒是等消失，不是卡住）。

## 二点十四、限流来源与恢复入口记账（#19）

`client_key` 上一轮已经改成"默认只认真实 peer"，但**没有测试证明它**，而且
恢复入口的记账是反的。这轮把两边都钉住：

| 场景 | 现在 |
|---|---|
| 直连部署（`trust_proxy=false`） | 转发头完全不读；同一个对端攒失败，换头换不掉 |
| 代理部署（`trust_proxy=true`） | 采信 `x-forwarded-for` 首段，不同头是不同桶 |
| 恢复入口 | 失败也 `risk_record`，成功才 `risk_clear` |

顺带修了一个真 bug：`ResetBody` 原来**没有人机校验字段**，于是风控一旦触发，
用户拿着正确的恢复码也永远 428——只能等窗口过期。失败计数应该是摩擦，
不是锁死，所以把 `captcha_id` / `captcha_answer` 补上了。

### 验收

`crates/platform/tests/rate_limit_source.rs`（3 条）：

- `a_spoofed_forwarded_header_cannot_reset_the_counter` —— 直连部署下每次换
  伪造 `x-forwarded-for`，第 5 次仍然要过人机校验；
- `behind_a_trusted_proxy_the_forwarded_header_is_the_bucket` —— 代理部署下
  同一个转发头攒到阈值触发挑战，换一个头立刻恢复（用注册来分桶最干净：
  注册只按来源计数，不掺账号键）；
- `wrong_recovery_codes_accumulate_failures` —— 前 4 次错码是"码不对"，
  第 5 次开始要人机校验，过了校验仍然因为码错被拒。

## 三、为什么没有"全部完成"

单条 #3 就改了 8 个 crate（`net`/`server`/`platform`/`core`/`ui`/`ui/src-tauri`
/`ui/src/web`/`tests`）和**三端协议**：帧格式变了，Web 的 bincode 变体序号、
桌面的连接参数、诊断工具、18 处测试构造点全部要同步。这类改动不能切碎，
切碎了就是"改一半的协议"，比不改更危险。

剩下的都是"改一半也算数"的增量：多设备密钥分发、人机校验、Redis 保留策略、
ureq 3 迁移。没有一条需要再动协议核心。

按"能不能验证"排序，我优先做了**改完就能用测试钉住**的那些（#3 #6 #12 #19），
以及**只改一行就能止住泄漏**的那些（#4 的 fail-closed、#14 的读放大）。

## 四、下一步建议顺序

1. **端到端投递确认**（1–2 天）——加 `Ack{msg_id}` 帧，让离线队列的 ack 等客户端
   处理完（现在只等"写进 socket"，见 §二点九）。
2. **#15 剩余**（3–5 天）——多设备 bundle、搜索过滤、本地真删除。
3. **#20**（1 天）——Redis Stream 保留策略与路由租约。
4. **#22**（2–3 人日）——ureq 3 迁移，有现成任务单
   `docs/task-ureq3-migration.md`。

> 排序依据：**能不能被外部直接利用** > **数据会不会丢** > **体验/债务**。
