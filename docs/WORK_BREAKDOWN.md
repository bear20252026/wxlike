# wxlike 开发任务书（WBS + 验收标准）

> 版本 2026-10-16。面向外包/协作开发者。**每条验收标准都必须可用命令或测试验证**，
> 无法客观验证的表述一律不作为验收项。
> 配套阅读：[reference-projects.md](reference-projects.md)（架构参考）、[THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md)（许可边界）。

---

## 0. 总则（对所有工作包生效）

### 0.1 架构铁律

1. **一切业务逻辑必须在 Rust**（workspace 内 crate）。前端（Tauri/TS）只允许 UI 渲染与调用 Rust command；发现业务逻辑写进 TS 即验收不通过。
2. 单一 workspace（`wxlike/`），新模块一律新建 crate 并加入 members；禁止引入第二套构建体系（不得用 Go/Java/Python 写服务端组件）。
3. 错误处理：库 crate 用 `thiserror` 定义错误枚举；二进制与边界用 `anyhow`。禁止 `unwrap()/expect()` 出现在非测试代码路径（`expect` 仅允许在启动期配置加载）。
4. 依赖政策：新增依赖需在 PR 描述说明理由；优先纯 Rust 实现（RustCrypto 系）；**禁止引入任何闭源 IM SDK / 云厂商绑定 SDK**；禁止 GPL 依赖（含传递）。
5. 所有网络协议变更必须改 `wxlike-core::types` 并 bump `Envelope::version`，兼容旧版本字段（`#[serde(default)]`）。
6. 无正当理由禁止 `unsafe`；确需使用时必须在 PR 中附 safety 注释与替代方案对比。

### 0.2 全局完成定义（DoD，每个 PR 都要过）

| # | 检查 | 命令 | 标准 |
|---|------|------|------|
| D1 | 构建 | `cargo build --workspace` | 0 error |
| D2 | Lint | `cargo clippy --workspace --all-targets -- -D warnings` | 0 warning |
| D3 | 测试 | `cargo test --workspace` | 全绿，且本 WP 新增测试 ≥ 对应工作包表中的数量 |
| D4 | 格式 | `cargo fmt --check` | 无 diff |
| D5 | 演示 | README 可复制的命令序列 | 验收人粘贴执行，5 分钟内看到声明效果 |
| D6 | 文档 | crate 级 `//!` 模块文档 + 公开 API doc | `cargo doc --no-deps` 无 broken link |

### 0.3 验收流程（防偷懒机制）

1. **三件套交付**：每个 WP = 代码 + 测试 + 演示命令。缺一不验收。
2. **对抗测试**：验收人持有"对抗测试集"（黑盒，针对边界与失败路径：断线、重连、重复消息、超长字段、并发、错误密码、越权 token）。承包方测试全绿但对抗集不过 = 不通过。
3. **PR 拆分**：单个 PR 除测试与生成代码外 diff ≤ 800 行；巨型 PR 直接拒收。
4. **禁止以 mock 交差**：凡接口契约写明"连真实后端/真实存储"的，用内存 mock 替代实现即不通过（trait 的内存实现只在契约允许处有效）。
5. **度量留痕**：涉及性能的 WP 必须附基准脚本（`scripts/bench_*.rs`）与本机输出数据，数字达不到标准即不通过。
6. **进度以 git 为准**：口头汇报无效；每个 WP 对应分支 + PR，验收合并到 main。

### 0.4 现有基线（必须复用，禁止重做后"卖"一遍）

| crate | 已有能力（验收人已验证） |
|-------|--------------------------|
| `wxlike-core` | `Envelope`（含 `to` 收件人）、X25519+HKDF+ChaCha20Poly1305（`crypto::Identity/seal/open`）、SQLite `Store` |
| `wxlike-mq` | `MessageQueue`/`RouteTable` trait；`MemoryMq`/`MemoryRoutes`；`RedisMq`/`RedisRoutes`（feature `redis`，已编译验证） |
| `wxlike-net` | QUIC 传输（rustls，ALPN `wxlike/1`）、分帧、`WireFrame::{Hello,Msg,Ping}`、广播 hub、**MQ 路由 hub（跨节点定向/广播）**、`OfflineStore`+`MemoryOffline`（代码已入，验收时随 WP-10 一并验证） |
| `wxlike-dpage` | DPG1 打包/解包 CLI（桩+zstd） |
| `wxlike-container` | Boa JS 宿主、`wxhost` 桥、页面生命周期、研究装载器 |
| `server` | axum HTTP + MQ hub，env 选择后端 |

---

## 2. 工作包总览与依赖

```
WP-01 platform(账户) ──> WP-02 好友 ──> WP-04 群聊
        │                    │
        └──> WP-03 消息强化 ─┴──> WP-07 朋友圈
WP-05 媒体（依赖 WP-01）
WP-06 推送（依赖 WP-01）
WP-08 音视频（依赖 WP-01/WP-02，独立大件）
WP-09 钱包（依赖 WP-01/WP-03）
WP-10 MQ 可靠性升级（独立，可并行）
WP-11 UI 桌面端（依赖 WP-01..05 的 API 面）
WP-12 E2EE 强化（依赖 WP-03）
WP-13 运维基线（独立，尽早）
```

| WP | 名称 | 预估（人日） | 新增测试下限 |
|----|------|-------------|--------------|
| WP-01 | platform：账户/会话/资料 | 8 | 10 |
| WP-02 | 好友关系 | 6 | 10 |
| WP-03 | 消息强化（去重/回执/撤回/引用/多端） | 8 | 12 |
| WP-04 | 群聊 | 15 | 15 |
| WP-05 | 媒体消息与对象存储 | 8 | 8 |
| WP-06 | 系统推送 | 6 | 5 |
| WP-07 | 朋友圈 | 10 | 10 |
| WP-08a | 音视频：语音通话（WebRTC） | 25 | 8 |
| WP-08b | 音视频：视频通话 | 15 | 5 |
| WP-09 | 钱包/红包/转账（虚拟闭环） | 10 | 15 |
| WP-10 | 离线持久化 + Redis Streams 至少一次 | 5 | 8 |
| WP-11 | 桌面 UI（Tauri2） | 15 | 5 |
| WP-12 | E2EE：double ratchet + 证书校验 | 10 | 10 |
| WP-13 | 运维基线（compose/监控/压测） | 6 | 3 |

合计 ≈ 147 人日（约 7 人月，单人全职）。关键路径：WP-01 → 02 → 04 → 11。

---

## 3. 工作包详规

### WP-01 platform：账户 / 会话 / 资料（8 人日）

**目标**：把 `wxlike-platform` 建成唯一的业务数据面（HTTP API + SQLite），hub 只管推送。

**交付物**：新 crate `crates/platform`；`server` 集成（默认开启）。

**接口契约**（必须实现为以下形状，路径可微调但语义不变）：

```
POST /api/register   {name, password}              -> 200 {user_id, token} | 409 重名
POST /api/login      {name, password}              -> 200 {user_id, token} | 401
GET  /api/me         Authorization: Bearer <token> -> 200 {user_id, name, avatar, signature}
PUT  /api/profile    Bearer + {avatar?, signature?, name?} -> 200
GET  /api/profile/{user_id}                        -> 200 公开字段 | 404
POST /api/logout     Bearer                        -> 204
```

**技术要求**：
- SQLite 表：`users(id TEXT PK, name UNIQUE, pw_salt, pw_hash, avatar, signature, created_at)`；`sessions(token PK, user_id, created_at, expires_at)`。
- 密码：每用户随机盐 + PBKDF2-HMAC-SHA256 ≥ 100_000 轮（RustCrypto `pbkdf2`）；禁止明文/单轮哈希。
- token：`rand::thread_rng` 32 字节 hex；有效期 30 天；`/api/me` 之外的写接口必须校验。
- SQLite 连接用 `Mutex<Connection>` 封装，所有 SQL 参数化（拼接 SQL = 验收不通过）。

**验收标准**：
- A1：`cargo test -p wxlike-platform` ≥ 10 个测试：注册/重名 409/登录对错/错误 token 401/资料读写/过期 token 401/token 唯一性。
- A2：用 `tower::oneshot`（或起临时端口 + `curl --noproxy '*'`）完成"注册→登录→改资料→读资料"四步脚本，输出进 README。
- A3：并发 50 个注册请求（tokio join_all）无死锁、无重复 name。
- A4：对抗项：超长 name(10KB)、空密码、SQL 注入串 `'; DROP TABLE users;--` 全部安全拒绝，库完好。

---

### WP-02 好友关系（6 人日）

**目标**：申请-同意的双向好友模型 + 通讯录同步。

**接口契约**：
```
POST /api/friends/requests      Bearer + {to_user_id, message?}  -> 201（进对方收件箱）
POST /api/friends/requests/{req_id}/accept | decline              -> 200
GET  /api/friends/requests/incoming | /outgoing                   -> 列表
GET  /api/friends                                                 -> 好友列表（含备注/来源）
PUT  /api/friends/{user_id}/remark   {remark}                     -> 200
DELETE /api/friends/{user_id}                                    -> 204（双向解除）
GET  /api/friends/changes?since=<cursor>                          -> 增量（通讯录同步）
```

**技术要求**：
- 表 `friend_requests(id, from_uid, to_uid, message, status, created_at)`（from+to 唯一，防重复申请）；
  `friends(user_id, friend_id, remark, created_at)` 双向两行，事务写入。
- 好友申请/同意事件必须产生一条**系统 Envelope**（`MsgKind::System`）经 MQ hub 推给在线方（复用 `route_envelope`），验证端到端可见。

**验收标准**：
- A1：≥10 测试：申请/重复申请 409/同意后双向可见/拒绝/删除后列表消失/remark 更新/不能加自己/增量游标正确。
- A2：集成测试（`crates/net/tests/`）：alice 与 bob 互为好友后，一方加好友产生的 System 消息能通过 hub 送达在线的另一方（断言收到 `MsgKind::System`）。
- A3：对抗项：伪造 `from_uid`（用他人 token 替别人申请）、accept 不存在的 req、重复 accept——全部 4xx，不产生脏数据。

---

### WP-03 消息强化：去重 / 回执 / 撤回 / 引用 / 多端（8 人日）

**目标**：把 Envelope 升级到可靠消息语义，这决定后面所有功能的底座。

**接口契约**（改 `wxlike-core::types`，`version: 2`，旧字段全部 `#[serde(default)]`）：
```rust
pub struct Envelope {
    // 现有字段不变，新增：
    pub msg_id: [u8; 16],                 // 客户端生成，全局去重键
    pub quote_of: Option<[u8; 16]>,       // 引用哪条消息
    pub receipt: Option<Receipt>,         // Read { up_to_msg_id } | Delivered
}
pub enum MsgBody {
    // 现有…
    Recall { target: [u8; 16] },          // 撤回指令
}
```
hub/存储行为：
- `msg_id` 去重：hub 端 LRU（≥10 万条）+ `messages.msg_id UNIQUE`，重复直接丢弃但**不报错**。
- 回执：收到 `receipt` 的 hub 转投给原 sender（无论在线与否，离线走 OfflineStore）。
- 撤回：投递 `Recall` 给会话对端；存储层把目标消息标记 `withdrawn=1`，`list_messages` 返回占位（"消息已撤回"），原始 body 不再出库。
- 多端同步：`Store` 增加 `device_cursor(device_id, conv_id, last_seq)`；新 API `sync_since(device_id, conv_id)` 只返回增量；同一用户第二个设备登录后通过 sync 补齐。

**验收标准**：
- A1：≥12 测试：重复 msg_id 只入库一条/回执路由到发送者/离线回执走暂存/撤回后读取为占位且原文不可取/引用字段往返/游标增量不含已同步消息/版本 1 消息仍可解析。
- A2：集成测试：alice 撤回 → bob 端收到 Recall 且查询结果为占位。
- A3：对抗项：10 并发发同一 msg_id（恰好入库一次）；伪造他人 msg_id 撤回（403/忽略）。

---

### WP-04 群聊（15 人日）—— 工程量最大，允许拆两个 PR

**目标**：群组模型 + 群消息路由 + 群管理。

**接口契约**：
```
POST /api/groups                  {name, member_ids[]}   -> 200 {group_id}
POST /api/groups/{id}/members     {user_ids[]}（仅群主/管理员）-> 200
DELETE /api/groups/{id}/members/{uid}（仅群主）            -> 204
POST /api/groups/{id}/leave                              -> 204
PUT  /api/groups/{id}/remark-mine {remark}               -> 200
GET  /api/groups/{id}                                    -> 群信息+成员列表
GET  /api/groups/mine                                    -> 我的群列表
```
群消息（复用 QUIC 通道）：`Envelope.conv_id = group_id`，hub 必须按**群成员表**投递（替代"to 空即广播"的语义），@提醒用 `MsgBody::Custom{kind:"mention", payload}`。

**技术要求**：
- 表：`groups(id, name, owner_uid, created_at)`、`group_members(group_id, user_id, role, my_remark, joined_at)`。
- 投递策略（写扩散， scaffold 明确选型）：hub 收到群消息 → 查成员表 → 对每个**在线**成员直投/跨 hub 转发，**离线成员写 OfflineStore**；群成员缓存 hub 内存 + 变更时失效。
- 群操作全部产生 System Envelope 进群（"xxx 加入群聊"），多端与历史可见。
- 权限：踢人/拉人仅 owner/admin；普通成员发消息权限默认开。

**验收标准**：
- A1：≥15 测试：建群默认含创建者/非群主踢人 403/退群后不再收群消息/离线成员上线补收（依赖 WP-10 持久化前用 MemoryOffline）/@提醒只有被@者带标记/群成员缓存失效正确。
- A2：**三人跨 hub 测试**（核心）：alice@hub-a、bob@hub-b、carol@hub-b 同群，alice 发群消息，bob/carol 都收到且 alice 不收到。
- A3：性能基准 `scripts/bench_group.rs`：200 人群、50 消息/s 注入，本地直投 p99 < 100ms（debug 构建放宽至 < 300ms），输出留档。
- A4：对抗项：往未加入的群发消息（403 且不入库）、拉 1001 人进群（按上限 1000 拒绝）、重复拉同一人（幂等）。

---

### WP-05 媒体消息与对象存储（8 人日）

**目标**：图片/文件/语音条的上传下载与消息引用。

**接口契约**：
```
POST /api/media         Bearer + multipart(bytes, sha256) -> 200 {media_id, size}
GET  /api/media/{id}    -> 200 bytes（带 Content-Type/Length；支持 Range: bytes=）
```
- 存储：本地目录（`WXLIKE_MEDIA_DIR`，默认 `data/media`），按 sha256 内容寻址（去重）；**接口抽象成 `trait MediaStore`**（`LocalDirStore` 现在，S3/MinIO 实现留接口）。
- 消息侧：`MsgBody::Bytes` 携带 `media_id` 引用而非裸数据；客户端 GET 拉取。
- 图片自动生成缩略图（`image` crate，最长边 256px），GET 带 `?thumb=1`。

**验收标准**：
- A1：≥8 测试：上传下载往返/同内容去重（相同 sha 只存一份）/Range 断点（0-99 字节）/未知 media_id 404/超限（>100MB）413/缩略图尺寸正确/未登录上传 401。
- A2：集成测试：alice 发 `MsgBody::Bytes(media_id)` → bob 收到 → GET 得到与上传一致的字节（sha256 相等）。
- A3：对抗项：上传时声明 sha256 与实际不符 → 422；遍历路径 `../../etc/passwd` 作为 media_id → 404 且无文件系统泄漏。

---

### WP-06 系统推送（6 人日）

**目标**：App 不在前台时也能收到"有新消息"的通知。

**交付物**：`crates/push`：
- `trait PushProvider { async fn send(&self, device_token, payload) -> Result; }` + `NoopProvider`（默认）+ `FcmProvider`（HTTP v1 API，凭据从 env 注入，无凭据时构造失败并回退 Noop）。APNs 留接口与 TODO 文档（需要苹果账号，无法离线验收）。
- platform 表 `devices(user_id, device_token, platform, last_seen)`；登录/连接时注册。
- hub 判定收件人**无任何在线连接**时调用 PushProvider（payload 只含"谁发了消息"，不含正文——正文走 E2EE）。

**验收标准**：
- A1：≥5 测试：设备注册/多设备保留最新/Noop 调用计数（用带计数器的 test provider 断言"离线时被调用、在线时不被调用"）/无效 token 不崩。
- A2：集成测试：bob 完全离线 → alice 发消息 → test provider 计数 +1 且 payload 不含消息正文。

---

### WP-07 朋友圈（10 人日）

**目标**：发布/时间线/点赞评论/可见性（好友圈语义）。

**接口契约**：
```
POST /api/moments                     {text, media_ids[], visibility} -> 201 {moment_id}
GET  /api/moments/feed?since=<cursor> -> 自己+好友的动态（时间倒序，分页 ≤50）
DELETE /api/moments/{id}              （仅作者）
POST /api/moments/{id}/likes          -> 200（幂等：重复点赞仍 200）
DELETE /api/moments/{id}/likes
POST /api/moments/{id}/comments       {text, reply_to_comment_id?}
GET  /api/moments/{id}                -> 详情（含点赞列表、评论树）
```
- 可见性 `visibility: Public | Friends`（scaffold 仅这两种）；`Friends` = 仅互为好友可见（查 WP-02 好友表）。
- media_ids 引用 WP-05 的媒体；时间线 feed 是**读扩散**（查好友表聚合，scaffold 规模允许）。

**验收标准**：
- A1：≥10 测试：发布含图/好友可见（非好友 404）/点赞幂等/取消点赞/评论树 reply 关联/删除仅作者/feed 分页游标不重不漏/feed 不含已删好友的历史（保持可见性快照语义需文档说明选择）。
- A2：对抗项：非好友读 Friends 动态、伪造他人 moment_id 删除、1000 字符以上文本（按上限 5000 拒绝或接受但需文档）。

---

### WP-08 音视频通话（25 + 15 人日，独立大件，允许分包）

**WP-08a 语音（25 人日）**
- 新 crate `crates/rtc`：信令走现有 QUIC 通道（`MsgBody::Custom{kind:"rtc-signal"}`），媒体走 **`str0m`**（纯 Rust WebRTC，禁用 libwebrtc C++ 绑定）。
- 通话状态机：Invite → Ringing → Accepted/Rejected/Cancelled/Hungup/Failed（超时 30s 未接听自动 Cancel），全程消息可追溯。
- NAT 穿透：内置 STUN（公共 Google STUN 可配置）+ 落地 TURN 配置接口（`WXLIKE_TURN_URL`，无 TURN 时同网段可通即为验收环境）。
- 音频：Opus 编解码（`opus` crate 或 audiopus）、采集回放跨平台抽象（桌面 cpal）。
- A1：≥8 测试：状态机全迁移路径（含非法迁移拒绝）/信令超时/双方同抢（glare）仲裁/无 TURN 时本地回环媒体连通。
- A2：**可听验收**：两台机器（或两个进程+虚拟声卡）互通语音 ≥ 60 秒不断流，MOS 主观记录；同机回环比特率/丢包指标由脚本输出。
- A3：对抗项：对离线用户发起呼叫 → 立即 Failed 而非挂起；通话中对方进程被杀 → 本端 10s 内进入 Failed。

**WP-08b 视频（15 人日）**：在 08a 上加视频轨道（VP8/VP9 via str0m），采集用桌面摄像头抽象（`nokhwa` 或同等），分辨率/码率自适应（REMB/TWCC 基础档）。A1：5 测试（媒体协商含视频 m-line、带宽降级触发）；A2：可看验收：双机视频 ≥ 5 分钟，分辨率不低于 640x480@15fps（本机回环）。

---

### WP-09 钱包 / 红包 / 转账（虚拟闭环，10 人日）

**边界声明（写进交付文档首页）**：本 WP 是**虚拟记账系统**，不接任何真实支付渠道；真实货币收单属于持牌合规问题，不在本仓库范围。

**接口契约**：
```
POST /api/wallet/transfer  Bearer + {to_uid, amount, memo?}   -> 200 {transfer_id}
POST /api/wallet/redpacket {amount, count, memo?}             -> 200 {redpacket_id}（冻结总额）
POST /api/wallet/redpacket/{id}/claim                          -> {amount}（抢到的份额；抢完 409）
GET  /api/wallet/balance                                       -> {balance}
GET  /api/wallet/ledger?since=                                 -> 流水（分页）
```
- 表：`wallets(user_id PK, balance)`、`ledger(id, user_id, kind[transfer_in/out, redpacket_in/out...], amount, counterparty, ref_id, created_at)`、`redpackets(id, from_uid, total, count, claimed_json, status, expire_at)`。
- **一致性铁律**：转账 = 单事务内双方余额变动 + 双流水；余额不可为负；红包总额创建时冻结（从余额扣），24h 未领完自动退回（后台任务）。
- 消息侧：转账/红包/领取各产生 `MsgBody::Custom{kind:"wallet.transfer|wallet.redpacket|wallet.redpacket.claim"}` 进会话。

**验收标准**：
- A1：≥15 测试，其中并发测试必含：**同一账户 100 并发各转 1 元，余额精确减少 100、流水恰好 100 条**（无丢失无重复——这是本 WP 的灵魂测试）；红包 count=3 被 5 人抢，恰好 3 人成功、总额守恒；过期退回后 from 余额恢复、ledger 有退回记录。
- A2：对抗项：转给不存在的用户/转账金额 ≤ 0 或 > 余额（事务回滚且无流水）/claim 已抢完的红包（409 且不产生流水）/伪造他人红包 claim（token 校验）。

---

### WP-10 MQ 可靠性升级 + 离线持久化（5 人日）

**目标**：把现有 at-most-once 升到 at-least-once，离线暂存落盘。

- `RedisStreamsMq`（feature `redis`）：XADD/XREADGROUP（group=`wxlike`，consumer=进程标识）/XACK；新增 trait `ReliableQueue: MessageQueue { async fn ack(&self, queue, id); async fn consume_entry(...) -> (id, payload); }`；XPENDING 巡检 + XAUTOCLAIM 认领超时（≥60s）未 ack 的消息。
- `SqliteOfflineStore`：实现现有 `OfflineStore` trait（表 `offline_inbox(user, frame BLOB, enqueued_at)`，drain 即删），hub 默认改用它（替代 MemoryOffline），文件路径 `WXLIKE_DB`。
- 旧 BRPOP 的 `RedisMq` 保留并标注 deprecated。

**验收标准**：
- A1：≥8 测试：Streams 发布消费往返/consumer group 双消费者瓜分/未 ack 的消息在 XAUTOCLAIM 后可被重新消费/SqliteOffline 跨进程持久（写→重开连接→drain 仍在）/cap 生效。
- A2：**env 门控真机测试**：`REDIS_URL` 存在时 `cargo test -p wxlike-mq --features redis -- --ignored` 全绿；不存在时自动 skip（测试内检查 env，不许硬编 skip）。
- A3：随附验证 WP 前文遗留：`MemoryOffline` 的 store-and-forward 集成测试（alice 发给未连接的 bob → bob 连接 → Hello 后收到暂存消息）。

---

### WP-11 桌面 UI（Tauri2，15 人日）

**目标**：`ui/` 跑起来，覆盖注册登录/会话列表/聊天（文字+图片）/好友/群聊/钱包入口。

**技术要求**：
- 前端框架任选（Vue3/React），**必须 TypeScript**；状态管理用原子化（jotai/zustand/Pinia 任一）。
- 所有后端交互走 Tauri command（Rust），禁止前端直连 QUIC/HTTP（`@tauri-apps/api` 之外的传输即违规）。
- 复刻清单（验收逐条对）：会话列表滑动操作、搜索展开动画、消息气泡（区分收发）、图片九宫格、深色模式全量适配、聊天页转场。
- 媒体：图片上传走 `invoke("upload_media")`。

**验收标准**：
- A1：`cd ui && npm i && npx tauri dev` 在 Win10/11 干净环境 15 分钟内可运行（README 含前置条件）。
- A2：≥5 个 Rust command 的 Rust 侧测试（mock 前端不验，command 层必须验）。
- A3：演示视频或现场操作：注册→登录→加好友→单聊（含图片）→建群→群聊→红包，全程 UI 操作无控制台报错。
- A4：`cargo build -p wxlike-ui` 0 error 0 warning（clippy 同）。

---

### WP-12 E2EE 强化（10 人日）

**目标**：把 static-static 升级到 **Double Ratchet**，QUIC 证书从 skip-verify 改为真实校验。

- 新模块 `wxlike-core::ratchet`：X3DH 风格预共享密钥协商（预密钥表经 platform 上传/拉取：`POST/GET /api/keys`）+ 对称 ratchet + DH ratchet（RustCrypto）。
- 每消息头含 ratchet 公钥+计数；乱序到达可解（out-of-order 支持 ≥ 64 条窗口）。
- QUIC：客户端预置 CA 或 TOFU（首连钉扎指纹，存本地），替换 `connect_insecure`（保留为 `--insecure-lab` 显式开关）。
- 兼容：ratchet 失败回退旧 static-static 需打日志（过渡期），文档写明迁移计划。

**验收标准**：
- A1：≥10 测试：双向 1000 条交替消息全解/乱序 64 窗口内全解/中间人篡改任一密文必失败/重放旧密文被计数器拒绝/预密钥耗尽自动补充。
- A2：TOFU 测试：首连记录指纹 → 二连伪造证书 → 拒绝。
- A3：文档：威胁模型一页（保护什么、不保护什么——元数据不隐藏要写明）。

---

### WP-13 运维基线（6 人日）

- `docker-compose.yml`：redis + server（含 platform）+ MinIO（media S3 后端）一键起；健康检查齐全。
- 可观测：`tracing` 全链路（hub 转发、platform SQL 慢查），`/metrics`（`metrics` crate + Prometheus 文本格式）暴露：在线连接数、消息吞吐、队列积压、p99 延迟。
- 压测脚本 `scripts/bench_hub.rs`：单 hub **10_000 并发连接、每秒 1_000 条 Envelope 注入，持续 60s**：断连率 0、p99 端到端（hub 入→出）< 50ms（release 构建），报告落 `docs/bench/`。
- 崩溃恢复：kill -9 server 后重启，已连接客户端 30s 内自动重连成功且不丢已 ack 消息（测试覆盖）。

**验收标准**：A1：compose up 后 `curl /health` 与 `/metrics` 正常；A2：压测数字达标准并附机器配置说明；A3：≥3 测试（重连/恢复/metrics 字段存在）。

---

## 4. 端到端最终验收剧本（全部 WP 合并后）

在两台干净机器上，用 UI（WP-11）+ 两个 hub（WP-13 compose）+ Redis：

1. 机器 A 注册 alice，机器 B 注册 bob（WP-01）
2. alice 通过二维码/ID 加 bob 好友，bob 同意（WP-02）
3. 双方互发文字/图片/引用/撤回（WP-03/05），飞行模式一方后消息暂存、恢复在线补收（WP-10）
4. 建三人群，@提醒，踢人验证（WP-04）
5. 发红包，另一方拆（WP-09），账本守恒
6. 发朋友圈，非好友不可见（WP-07）
7. 语音通话 60 秒（WP-08a）
8. 全程杀掉 server 重启，客户端自动恢复（WP-13）

每一步录屏存档。任何一步需要"重启进程/改代码/跑额外脚本救场"即视为未通过。

## 5. 阶段与支付节点建议

| 阶段 | 工作包 | 建议结算 |
|------|--------|----------|
| M1 可用通信 | WP-01+02+03+10 | 30% |
| M2 群与媒体 | WP-04+05 | 25% |
| M3 可看可用 | WP-11+07 | 25% |
| M4 通话与强化 | WP-08+09+12+06+13 | 20%（音视频单独议价） |

> 每个里程碑按 DoD（0.2）+ 对应 WP 验收条 + 对抗测试集执行；对抗测试集由验收人维护，不提前公开，随里程碑更新。
