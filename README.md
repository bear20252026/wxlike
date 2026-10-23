# wxlike

[![License: AGPL-3.0-only](https://img.shields.io/badge/license-AGPL--3.0--only-blue.svg)](LICENSE)

Rust 跨平台 IM（微信形态）骨架 + 本地复用研究容器。自研协议与实现，**不含任何腾讯代码进入分发物**。

> **许可：AGPL-3.0-only**（2026-10-18 起，原 GPL-3.0）。项目定位为研究实验，非商用——借鉴引用遵循
> [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。AGPL 与 GPL-3.0 同族且**允许并入**：
> GPL-3.0 代码（tweb 等）可直接用于本仓库；AGPL-3.0 代码（Element/Synapse/spreed/Signal）
> 也可直接复制，合并后的作品整体为 AGPL-3.0。代价是本平台**对外提供网络服务时须向用户
> 提供源码**（AGPL §13），这正是本项目"研究/内部测试"定位下不产生实际义务的原因
> （Telegram 客户端交互范式、MTProto 分层设计等）。

## 架构

```
wxlike/
├─ crates/
│  ├─ core/        # Envelope 消息类型（含 to 收件人）· X25519+HKDF+ChaCha20Poly1305 E2EE · SQLite 存储
│  ├─ mq/          # ★ MQ 抽象（MessageQueue + RouteTable）· Memory 后端 · Redis 后端(feature)
│  ├─ net/         # QUIC 长连接 · 分帧 · 广播 hub（单节点）/ MQ 路由 hub（多节点）
│  ├─ dpage/       # 动态页包格式 DPG1：桩头(magic+id+len) + zstd 载荷（借鉴 CSO 桩+载荷设计）
│  ├─ container/   # JS 引擎宿主（Boa）· wxhost 桥 · 页面生命周期 · 研究装载器
│  ├─ platform/    # ★ 业务数据面（WP-01）：axum HTTP + SQLite 账户/好友/群/钱包/媒体/朋友圈/预密钥
│  ├─ push/        # ★ 系统推送（WP-06）：离线设备通知，payload 结构上不含消息正文
│  └─ rtc/         # ★ 音视频通话（WP-08）：str0m 纯 Rust WebRTC + 通话状态机 + 信令
├─ server/         # 开发服务器：platform API + MQ 路由 hub + /metrics + presence + 推送分发
└─ ui/             # Tauri2 桌面端（独立构建，见下）
```

## 平台 API（WP-01：账户 / 会话 / 资料）

唯一业务数据面，hub 只负责推送。契约见 [docs/WORK_BREAKDOWN.md](docs/WORK_BREAKDOWN.md) WP-01。

```bash
# 5 分钟内可复现的四步演示（进程内跑真实 axum router，无需起服务）
cargo run -p wxlike-platform --example wp01_demo
```

实际输出（token/user_id 为随机值）：

```
1) POST /api/register   -> 200 OK
   {"token":"92c26f1e...cf9b4a","user_id":"07a4254e8f5301e5"}
2) POST /api/login      -> 200 OK
   {"token":"e8777f14...3843985","user_id":"07a4254e8f5301e5"}
3) PUT  /api/profile    -> 200 OK
   {"avatar":"alice.png","name":"alice","signature":"rust && quic","user_id":"07a4254e8f5301e5"}
4) GET  /api/me         -> 200 OK
   {"avatar":"alice.png","name":"alice","signature":"rust && quic","user_id":"07a4254e8f5301e5"}
5) GET  /api/me (no Authorization)   -> 401 Unauthorized (expected 401)
5a) GET /api/me (forged token)       -> 401 Unauthorized (expected 401)
5b) GET /api/me (register's token)   -> 200 OK (expected 200: separate live session)
6) POST /api/logout                  -> 204 No Content (expected 204)
7) GET  /api/me (after logout)       -> 401 Unauthorized (expected 401)
```

起服务器后同样的接口可以 curl（默认 SQLite 落 `data/wxlike.db`）：

```bash
cargo run -p wxlike-server
curl --noproxy '*' -s -X POST http://127.0.0.1:8099/api/register \
  -H 'content-type: application/json' -d '{"name":"alice","password":"hunter2"}'
```

安全要点：口令 PBKDF2-HMAC-SHA256 100_000 轮 + 每用户随机盐（轮数入库，可后续上调）；
token 为 32 字节随机 hex、有效期 30 天、过期即惰性清理；`/api/me` 与所有写接口强制 Bearer 校验；
SQL 全部参数化，且输入层再挡一层超长/控制字符/引号分号（WP-01 A4）。
`GET /api/profile/{id}` 只回 `user_id/name/avatar`——`signature` 仅本人可见。

## 好友关系（WP-02）

申请—同意双向模型：`friends` 存**两行**（a→b、b→a，同一事务），所以备注是各自的；
`contact_log` 是 append-only 的通讯录变更流，`GET /api/friends/changes?since=<seq>` 靠它做增量同步。

```bash
# 端到端验证：平台 HTTP 产生的好友事件经 hub 送达在线客户端
cargo test -p wxlike-net --test friends_system_event
```

覆盖两件事：在线方立刻收到 `MsgKind::System`（申请、同意各一条，`to` 精确指向收件人）；
离线方则被 `MemoryOffline` 暂存，下次 `Hello` 时补发。身份只从 token 取，
请求体里的 `from_uid` 一律忽略（伪造他人申请是 401/403，不产生脏数据）。

`platform` 不依赖 `net`：事件通过 `trait EventSink` 反向注入，server 挂 `HubHandle`，
测试挂 recorder——所以业务层可以单测，不必起 QUIC。

## 朋友圈（WP-07）

**可见性按"读时"判定，不是发快照**。`friends` 可见的 moment 谁能看，取决于*此刻*谁是
互为好友——所以解除好友后，对方那些仅好友可见的历史立刻从你的 feed 和详情里消失
（A1 要的就是这条）。另一种设计是发布时把受众 ACL 物化下来，那样解除好友后历史仍会
继续可见，我们没选它，这段就是任务书要求的"需文档说明选择"。

feed 本身按契约只含**自己 + 好友**的动态，所以陌生人的 `public` 帖不会进你的时间线；
`public` 只放宽"按 id 打开某一条"的权限。

游标方向：**feed 是时间倒序**（契约要求），所以 `since` 是水位线——传上一页返回的
`next_cursor` 拿"严格更早"的一页。这与钱包流水、通讯录增量的升序游标方向相反——
倒序分页必然如此，不是两套设计打架。页大小上限 50。

- 发布：文本上限 5000（超限拒绝）、图片最多 9 张（九宫格）、`media_ids` 必须已存在
  （引用不存在的对象是 422，不留悬空引用）；纯文本空白且无图 → 422。
- 点赞幂等：重复点赞仍 200 且 `added:false`，只有一行；取消点赞不存在的行是 no-op。
- 评论树用邻接表表达：`reply_to` 指父评论 id，附带 `reply_to_name` 供展示，
  客户端据此还原树形；**回复必须指向同一 moment 下的评论**，跨帖回复 422。
- 删除是软删（`deleted=1`），详情/feed 都立刻消失，二次删除是 404 而不是怪异错误；
  只有作者能删（他人 403）。
- 陌生人访问 `friends` 可见的 moment 一律 404（不是 403）——不给"存在但看不见"这种
  可被探测的信息。

```bash
cargo test -p wxlike-platform --test moments    # 14 项
```

## 钱包 / 红包 / 转账（WP-09，虚拟记账）

**边界**：这是虚拟记账闭环，不接任何真实支付渠道——真实货币收单属于持牌合规问题，
不在本仓库范围。资金进入系统的唯一入口是 domain 层的 `Platform::fund()`，
**没有对应的 HTTP 接口**，那条缝正是将来接持牌渠道结算回调的位置。

金额一律用整数最小单位（分），不用浮点，守恒检查才能精确成立。
`wallets.balance` 带 `CHECK (balance >= 0)`，即使应用层逻辑写错，SQLite 也会挡住负余额。

- **转账 = 一个事务**：双方余额变动 + 两条流水（`transfer_out` / `transfer_in`，共享同一
  `ref_id`）一起提交或一起不提交。余额不足时整个事务回滚，**不产生任何流水**。
- **守恒不变量是 `money_supply()`**：所有钱包余额 + 未领取红包的剩余托管额。
  只看钱包会"凭空少钱"——红包创建时总额已从发送方扣走，但还没人领，这笔钱在 escrow 里。
- **红包创建即冻结**：总额从发送方余额扣走并入账 `redpacket_out`；
  每份额随机但**保证剩余名额都还能拿到 >=1**，最后一份吃掉余数，所以份额之和恰等于总额。
- 发红包的人**不能领自己的包**（403）；同一人重复领（409）；领完即 `finished`，再领 409。
- **24 小时未领完自动退回**：`spawn_redpacket_sweeper`（server 每 60s 跑一次），
  退款本身也是一条流水 `redpacket_refund`；扫描幂等，跑第二次返回 0。
  测试直接改 `expire_at` 再调 `sweep_expired_redpackets()`，不靠 sleep 一天。
- **流水游标是升序**（`id > since ORDER BY id`），和通讯录增量同一套语义。
  这里原本写成了 `id > since` 配 `ORDER BY id DESC`——翻页会把最新那几行重复读回来，
  是分页最经典的一种错，测试里用"走完全部、每行恰好见一次"钉住。
- 转账/红包/领取各产生 `MsgBody::Custom{kind:"wallet.*"}` 进会话；
  红包可带 `conv_id` 指定发到哪个会话，不带则落在以该红包 id 命名的通道里（文档化的回退）。

```bash
cargo test -p wxlike-platform --test wallet    # 16 项
```

灵魂测试：同一账户 **100 并发各转 1 分** → 余额精确 -100、流水恰好 100 条、
`money_supply` 分毫不动；红包 3 份被 5 人并发抢 → 恰好 3 人成功、2 人 409、份额之和 == 冻结总额。

## 群聊（WP-04）

群 id 直接当 `Envelope.conv_id` 用。hub 收到群消息时按**成员表**投递（不再是
"`to` 为空就广播给所有在线连接"）：先查成员 → 非成员直接丢帧且**不入库** →
其余成员逐个定向投递，在线直投、跨 hub 转发、离线进 `OfflineStore`。

成员表由 `platform` 持有，hub 通过 `trait Membership` 反查（实现在 server 里做适配，
`platform` 不依赖 `net`）；`platform` 侧带进程内缓存，任何成员变更即时失效。

**扇出是并发的**：每条流各自有写锁，串行 `await` 会让单条消息的延迟随群规模线性增长
（200 人群实测把 p50 顶到 236ms）；改成并发投递后 release p99 = 11ms。

`@提醒`：客户端发 `MsgBody::Custom{kind:"mention", payload:{text, user_ids}}`，
hub 按收件人逐个改写——被 @ 到的人收到 Custom 标记，其余群成员只收到纯文本。

权限：拉人 = owner/admin，踢人 = 仅 owner，退群 = 普通成员（owner 必须先转移群主）；
群成员上限 1000（批量拉人整批拒绝，不会半提交）；建群/拉人/踢人/退群都产生
`MsgKind::System` 进群。`my_remark` 属于成员自己的那一行（本人对群的备注），
不是观察者给别人的别名。

```bash
cargo test -p wxlike-platform --test groups     # 20 项（权限/上限/幂等/缓存失效）
cargo test -p wxlike-net --test group_routing   # 5 项（含三人跨 hub、离线补收、@ 改写）
cargo run -p wxlike-net --release --example bench_group   # 200 人群 50msg/s 延迟基准
```

基准数据与两个"假数字"的排查过程见 [docs/bench/wp04_group.md](docs/bench/wp04_group.md)。

## 媒体消息（WP-05）

`media_id` **就是** 内容的 SHA-256（hex），所以"同一文件传两次"天然只有一份——
去重来自寻址方式，不是额外记账。落盘路径 `WXLIKE_MEDIA_DIR`（默认 `data/media`），
按 `{前2位}/{sha}` 分片，缩略图 `{sha}.thumb.png` 同目录。

```bash
cargo test -p wxlike-platform --test media      # 14 项
cargo test -p wxlike-net --test media_delivery  # 2 项端到端
```

要点：

- **抽象**：`trait MediaStore`（put/get/size_of/put_thumb/get_thumb），`LocalDirStore` 是今天，
  MinIO/S3 换个实现即可，handler 不动。测试里注入过内存实现验证这条缝真的独立。
- **路径穿越**：id 在拼路径**之前**就校验为"恰好 64 位十六进制"，不是事后过滤——
  `../../etc/passwd`、`..%2f` 变体、长度差一位、合法长度但非 hex 全部 404，且没有任何文件读操作发生。
- **摘要校验**：客户端可带 `sha256` 字段，与实际内容不符直接 422（大写也认，比较前统一小写）。
- **Content-Type 不认客户端声明**：按 magic bytes 嗅探（PNG/JPEG/GIF/WEBP），
  声明 `text/plain` 的 PNG 仍以 `image/png` 回。
- **Range**：`bytes=a-b` / `a-` / `-N`（后缀式）都支持，返回 206 + `Content-Range`，
  越界回 416 + `bytes */total`；`Accept-Ranges: bytes` 恒在。
- **大小上限**：路由层 `DefaultBodyLimit` 卡 100MB（契约值），handler 内还有
  `options.max_media_bytes` 二次检查并回 413——后者可注入小值，好让 413 路径
  在测试里真的跑得到，而不是分配 100MB 或干脆没覆盖。
- **缩略图**：最长边 256，保持宽高比（1024x768 → 256x192）；已小于 256 的不放大；
  解不了的"图片"不报错，只是没有 thumb（`has_thumb:false`，`?thumb=1` 回 404）。
- **消息侧只传引用**：`MsgBody::Bytes(media_id.as_bytes())`，帧里没有图像字节。

## MQ 与多节点路由（Rust 移植自 box-im 的 MIT 设计）

每个 hub 消费自己的队列 `wxlike:msg:hub:{hub_id}`；路由表记录 `用户 → hub`。
**定向消息**（`Envelope.to` 非空）按收件人路由：在本 hub 直投，在别的 hub 经 MQ 队列转发；
**广播**（`to` 为空）扇出到所有 hub。发送者不回显；离线收件人进 `OfflineStore` 暂存，
下次 `Hello` 时补发。

**投递语义（WP-10）**：Redis 走 **Streams 消费者组**（`XADD`/`XREADGROUP`/`XACK`），
本地投递成功后才 ack；进程中途挂掉时条目留在 pending，巡检任务用 `XAUTOCLAIM`
在 ≥60s 后认领重投——即 at-least-once。旧的 `RedisMq`（LPUSH/BRPOP）已标注
deprecated：BRPOP 读出即从队列消失，投递中途失败就真丢了。
离线暂存默认落 SQLite（`SqliteOffline`，与 `WXLIKE_DB` 同文件），hub 重启不丢暂存帧。
内存后端 `MemoryReliableMq` 提供同样的 ack/reclaim 语义，无 Redis 也能验。

```bash
# 单节点（零依赖，内存 MQ）
./target/debug/wxlike-server.exe

# 双节点跨机路由：先起 Redis，编译带 redis feature，两实例不同端口
cargo build -p wxlike-server --features redis
WXLIKE_HUB_ID=hub-1 WXLIKE_MQ=redis WXLIKE_QUIC=0.0.0.0:9443 WXLIKE_HTTP=0.0.0.0:8099 ./target/debug/wxlike-server.exe &
WXLIKE_HUB_ID=hub-2 WXLIKE_MQ=redis WXLIKE_QUIC=0.0.0.0:9444 WXLIKE_HTTP=0.0.0.0:8098 ./target/debug/wxlike-server.exe &

# 真机验证 Streams（不设 REDIS_URL 则自动跳过，不是硬编 skip）
REDIS_URL=redis://127.0.0.1:6379 cargo test -p wxlike-mq --features redis -- --ignored
```

聊天示例演示（bob 的输出）：

```
[bob] connected to 127.0.0.1:9443
[conv1] alice: hello everyone (broadcast)      ← 广播扇出
[conv1] alice: directed just for bob           ← to=["bob"] 定向
```

许可与移植声明见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)（wxlike 本体 **AGPL-3.0-only**；MQ 设计移植自 box-im MIT 设计，未含其 Java 代码）。

## 快速开始

```bash
cargo build
cargo test                 # workspace 247 tests + ui 7（计数随代码走，以 CI 为准）
                           # 覆盖：账户/好友/群聊/媒体/钱包/朋友圈/消息语义/可靠队列/
                           #       E2EE(棘轮+预密钥+TOFU)/推送分发/音视频状态机与媒体回环/运维恢复

# 起服务器
./target/debug/wxlike-server.exe            # HTTP :8099, QUIC :9443
curl --noproxy '*' http://127.0.0.1:8099/health

# 两个终端各跑一个客户端互发（每行一条，:quit 退出）
cargo run -p wxlike-net --example chat -- 127.0.0.1:9443 alice
cargo run -p wxlike-net --example chat -- 127.0.0.1:9443 bob

# 动态页全链路演示：目录 -> .dpage -> 容器执行生命周期
cargo run -p wxlike-container --example run_demo_page

# 动态页 CLI
cargo run -p wxlike-dpage --bin dpage -- pack <dir> <out.dpage>
cargo run -p wxlike-dpage --bin dpage -- info <in.dpage>
```

## 消息语义（WP-03：Envelope v2）

协议 `version: 2`，新增 `msg_id`（客户端生成的 16 字节去重键）、`quote_of`（引用）、
`receipt`（已投/已读回执），以及 `MsgBody::Recall` / `MsgBody::Custom{kind,payload}`。
v1 的字节仍可解码（bincode 是位置式编码，`from_bytes` 显式回退到 v1 布局并升级，
`msg_id` 保持全零表示"无去重键"）。`Custom` 的 payload 以 JSON 文本承载——
bincode 不支持 `deserialize_any`，直接放 `serde_json::Value` 会在编组时炸掉。

hub 的入站顺序（`crates/net/src/mq_hub.rs`）：

1. **发送者校验**：帧里的 `sender` 必须等于该连接 `Hello` 的身份，否则整帧丢弃——
   这挡住了伪造他人的撤回；
2. **控制帧**（回执 / 撤回）：只路由、不进消息表。回执按 `msg_id` 反查原作者后定向回投
   （收件人列表去重，避免作者被通知两次）；撤回只有在存储层认可时才转发，
   因此被拒绝的撤回连痕迹都不留；
3. **内容帧去重**：hub 内 10 万条 LRU 快路径 + `messages.msg_id` 部分 UNIQUE 索引兜底，
   重复投递静默丢弃、不报错。

撤回是**破坏性**的：目标行的 body 被占位文本覆盖并置 `withdrawn=1`，原文再也读不出来
（`list_messages` / `sync_since` 同样只回占位）。多端同步靠 `device_cursor(device, conv, last_seq)`，
`sync_since` 只回增量，第二台设备登录时全量补齐。

```bash
# 端到端：去重 / 撤回 / 回执路由 / 伪造拦截
cargo test -p wxlike-net --test reliable_msg     # 8 项
cargo test -p wxlike-core --test envelope_v2     # 19 项
```

手动演示（两个终端）：`cargo run -p wxlike-net --example chat -- 127.0.0.1:9443 alice`，
普通行广播、`bob> xxx` 定向、`:recall` 撤回上一条、`:read` 给最后收到的消息发已读回执。

## E2EE 强化（WP-12）

`core::e2ee` 基于 **vodozemac**（Matrix 同款 Olm/Megolm 实现，Apache-2.0）——
手写 double ratchet 最容易"happy path 全绿、上线即坏"，所以棘轮用审计过的库，
我们只写库明确留给嵌入方的三件事：**128 条严格有序重排窗口**（vodozemac 原生 40，
任务书要求 ≥64）、**重放拒绝**（Olm 密文可再生、Megolm 索引水位按 (群,发送者,会话) 记账）、
**重复 pre-key 拆包**（会话未建立前 vodozemac 每条都包 pre-key，接收端第二次消费同一把
一次性密钥必然失败——收发两侧都做了兼容）。

预密钥目录 `POST/GET /api/keys`：**GET 是认领语义**，每次弹出一把一次性密钥，
两个发起者永远拿不到同一把；库存见底返回空列表，属主 `bundle_refilled` 补货。
QUIC 传输默认 **TOFU 钉扎**（首连记指纹进本地文件，之后证书变了直接拒绝），
skip-verify 保留给 `--insecure-lab` 显式开关。群聊 Megolm sender-key，N 人群不做 O(N²)；
成员变更轮换密钥，新成员读不到轮换前历史。威胁模型（保护什么/不保护什么——
**元数据不隐藏**）见 [docs/threat-model.md](docs/threat-model.md)。

```bash
cargo test -p wxlike-core --test e2ee          # 17 项（1000 条交替/64 窗口乱序/篡改/重放/预密钥耗尽）
cargo test -p wxlike-platform --test keys      # 8 项（认领不重复/目录建会话端到端）
cargo test -p wxlike-net --test tofu           # 5 项（首连钉扎/伪造证书拒绝/跨名隔离）
```

## P1 体验补齐（合同 §第五条 P1：五件套 + 历史 + 回执 + 秘密聊天）

P1 四项交付全部落地，新增 24 项测试（下限 10）：

**① E2EE 接入客户端消息路径**（秘密聊天雏形 + 回退日志）：UI Rust 层持有本会话
Olm 账户（`AppState.secret`），`secret_enable` 发布预密钥目录；锁形开关开启后
`send_secret_text` 以 `Custom{kind:"e2ee.dm"}` 密文进消息路径，接收端经
`render_event` 解密后渲染 🔐 气泡。**回退是文档化行为**：对端无预密钥/预密钥耗尽/
加密失败时 warn 日志 + 明文发送（消息不丢），P4 完整秘密聊天时改为拒绝。

**② 消息操作五件套**：
- **正在输入**：`Custom{kind:"typing"}` 瞬态帧——hub 在 `handle_inbound` 早期识别，
  只投在线（`route_or_fanout(..., park_offline=false)`），**不落库、不暂存、不推送**；
  这是 P3 `high_priority` 通道的过渡形态。前端 2.5s 节流发送、5s 自动消失。
- **转发/合并转发**：`msg.forward`（单条快照）/`msg.merge`（多条 bundle），快照式——
  原文撤回不影响已转发副本；右键菜单 + 多选栏（checkbox）驱动。
- **收藏**：`crates/platform/src/favorites.rs`，**快照存储**（原文撤回/删除后自己的
  收藏还在），结构化列 + `created_at` 游标分页；全部按 owner 隔离，他人收藏一律 404
  （反探测）。
- **搜索协议**：`Store::search_messages` 在 Rust 侧对解码后的 preview 文本做
  大小写折叠子串匹配（SQLite `CAST(blob) LIKE` 对带长度前缀的 bincode 不可靠且只折
  ASCII），服务端 `GET /api/search/messages` 只在**授权会话集**内搜
  （组=成员表，直聊=conv 两半），跨用户零泄漏。

**③ UI 历史消息加载**（云同步闭环）：`Store::page_messages`（newest-first 翻页，
`next_before_seq` 指向本页最旧一行）→ server `GET /api/history/{conv_id}`（同一条
鉴权规则）→ UI「加载更早的消息」按钮，按 `msg_id` 去重合并。撤回行回占位文本，
原文不可读回。

**④ 已读回执 UI**：打开会话/聊天窗内收到消息即回发 `Receipt::Read`（hub 按
`msg_id` 反查原作者定向回投），自己气泡 ✓ → ✓✓。

测试：`core/store_page_search`(6) · `platform/favorites`(5) · `net/typing_channel`(3)
· `server/history_search_api`(5) · `ui/ui_commands` p1_*(5)。演示命令序列见
`server/tests/history_search_api.rs`（真实栈：起 stack→注册→QUIC 收发→翻页→鉴权拒绝）。

```bash
cargo test -p wxlike-server --test history_search_api   # 5 项（翻页/鉴权/搜索范围/通配符字面量/瞬态帧）
cargo test -p wxlike-net --test typing_channel           # 3 项（在线送达/不落库/不暂存）
cargo test -p wxlike-platform --test favorites           # 5 项（快照/所有权/分页/校验/HTTP 面）
```

服务结构说明：`server/src/lib.rs` 现承载全部装配（`start_stack`：platform + hub +
HTTP 路由），`main.rs` 只读环境变量——集成测试直接 `wxlike_server::start_stack`
起真实栈，不写 mock。

## P2 真机验收 + 体验（合同 §第五条 P2）

**① 消息类型扩展（新增测试 17 项，下限 8）**：语音条 / 文件 / 贴纸 / 视频四类，
全部走 `MsgBody::Custom`（协议零新增变体），载荷 schema 在
`core::msgmeta`（类型化 serde + 校验 + 波形算法）：
- **语音条**：`msg.voice`，40 桶峰值保持波形（`waveform_from_samples`：桶内取峰、
  按最响桶归一化 0–100、全静音不加 NaN）+ 时长 + `asr` 转文字占位（P4 接入）；
  webview 端 MediaRecorder 录制、AudioContext 解码 PCM 算波形，播放时按进度点亮。
- **文件**：`msg.file`，名字拒绝路径分隔符与 `..`（渲染安全），size/sha256 引用校验。
- **贴纸**：`msg.sticker`，media 内容寻址 + pack/id 标签；图片消息右键"存为贴纸"。
- **视频**：`msg.video`，引用 video-service（video_id/playlist_url/play_url/时长/
  倍速档位）；气泡内缩略占位 + 原生 `<video>` 播放 + 0.5–2.0x 变速。

**② video-service 最小版**（新 crate `wxlike-video`，ADR-21，合同"上传→播放闭环"）：
上传 → FFprobe 元数据 → **一次编码两产物**（720p 封顶 MP4 faststart + `-c copy`
切 4s HLS 分片，避免二次转码）→ 1s 缩略图 PNG → 原子落盘
（`{root}/{video_id}/…`，video_id=源字节 SHA-256，重传幂等）。
路由：`POST /api/video`（multipart）、`GET …/playlist.m3u8 | seg/{name} | thumb.png
| play.mp4 | meta`；id 与分片名先校验后拼路径（`../` 变体全部 404，无文件读）。
坏源上传失败即清理临时目录，不留半成品。演示：

```bash
ffmpeg -y -f lavfi -i testsrc=duration=3:size=640x360:rate=24 -shortest /tmp/demo.mp4
curl --noproxy '*' -F bytes=@/tmp/demo.mp4 http://127.0.0.1:8099/api/video
curl --noproxy '*' http://127.0.0.1:8099/api/video/{id}/playlist.m3u8   # #EXTM3U + seg_*.ts

# 测试（ffmpeg 在 PATH 或 WXLIKE_FFMPEG 指定时；同 Redis 真机先例用 --ignored）
cargo test -p wxlike-video -- --ignored        # 真实转码 3 项
cargo test -p wxlike-video                     # 纯逻辑 4 项（无需 ffmpeg）
```

**③ 本地消息库**（客户端 SQLite，"打开即渲染"）：`ui/src-tauri` 的 `LocalStore`，
app 启动即开（`local_autopen`），收发两侧落库（发送侧存解密后形态）；typing/回执
**设计上不入库**。重启后 `local_history` 无网即读，服务端历史只补更早的行。

**④ 头像接媒体**：`upload_avatar` = 媒体上传 + profile.avatar=media_id；公开资料带
id，对端经 `/api/media/{id}` 渲染。"我"页可设置头像并即时回显。

**⑤ 消息列表虚拟化**：聊天窗 DOM 只保留最近 300 个气泡，顶部"显示更早的 N 条"
按需扩窗（与服务端分页是两层独立的"更早"）。

**环境受限项（合同验收需甲方环境/真机，命令已备好）**：
- WP-08 双机音视频（≥60s 不断流）：`cargo test -p wxlike-rtc` 34 项全绿 +
  `--example` 双端脚本就绪，需两台真机实录。
- WP-10 Redis 真机：本机无 Redis；`REDIS_URL=redis://… cargo test -p wxlike-mq
  --features redis -- --ignored` 在有 Redis 的环境一键跑。
- WP-11 tauri dev 真机演示：`cd ui && npm i && npx tauri dev`（构建链已验证）。
- mac·Linux 构建验证：Tauri 2 前端与 Rust workspace 均为跨平台构建，需对应主机。

## P4 运营合规 + 账本 + 通话房间（合同 §第五条 P4）

三项独立能力 + 一条发送闸门，**新增测试 58 项**（合同下限 18）。完整三件套索引见
[docs/p4-report.md](docs/p4-report.md)，这里只列关键结论：

- **三层治理不互相调用**（ADR-20）：`wxlike-risk`（进程内滑动窗口，零 I/O）/
  `wxlike-moderation`（归一化 + trie，变形绕过 `p0rn`、`ＰＯＲＮ`、`傻 逼`、零宽字符全部命中）/
  `wxlike-report`（状态机，非法流转即 409）。发送侧经 `DeliveryPolicy::send_gate`
  在 hub 帧级拦截——被拒/被挂起的帧不落库、不扇出。
- **对账三条线**：逐用户 `balance == SUM(带符号流水)`、`供给 == 钱包 + escrow == 发行量`、
  红包算术。5 分钟一轮，异常写 `reconcile_runs` + 审计 `wallet.reconcile`。
- **通话房间**（`rtc::room`）：名册/静音/踢人（禁入）/锁门/录制/主持人移交，纯内存
  状态机，不占群信箱序列号。
- **账号面补齐**：恢复码（一次性、argon2id 摘要、重置即吊销全部会话）、算术 captcha
  （一次性 + 5 分钟）、按来源限流（428 触发取题）、设备管理（`/api/sessions` 列表 +
  按 sid 远程下线 + 一键退出其他设备，UI 在"我"页）。
- **秘密聊天产品面**：会话开关、消失消息 TTL（30s/5m/1h/1d/7d）、60 位安全码
  （两端一致，对方换设备即失效）。

```bash
cargo test -p wxlike-risk -p wxlike-moderation -p wxlike-report   # 26
cargo test -p wxlike-platform --test p4_ops                        # 16
cargo test -p wxlike-rtc --test room                               # 13
cargo test -p wxlike-net --test send_gate                          # 2
cargo test -p wxlike-ledger --test conservation                    # 账本守恒
```

## P5 第一增量：直播控制面 + 小程序包签名（合同 §第五条 P5）

P5 是 8 周量级，本轮先交付**不依赖移动端工具链**的两块，其余项在
[docs/p5-report.md](docs/p5-report.md) 逐条列明状态与阻塞原因（不含"已完成"的虚报）。

- **直播控制面**（`platform::live`，9 项测试）：开播限 `live` 频道 + 社群管理员；
  推流凭据是自包含的 `<exp>.<hmac>`（ingest 节点只需共享 secret），**停播后未过期
  的凭据也立即失效**；观众计数/峰值落库；弹幕走 `high_priority`，只发给在房观众，
  测试直接断言频道时间线为空（不变式 3），且弹幕同样过内容审核。
- **video 多分辨率**（`video`，真实 ffmpeg 用例）：360/720/1080 阶梯（只降不升）、
  每档独立编码后 `-c copy` 切 HLS、master playlist 带实测 BANDWIDTH/RESOLUTION，
  档位分片走 `/api/video/{id}/{h}/seg/{name}`；`play.mp4` 取最高档。
- **小程序包签名**（`container::package`，7 项测试）：ed25519 验签 + 防降级 +
  同版本拒装；`im_bridge` 授予面 = 清单声明 ∩ 宿主白名单，白名单外权限只记录不授予
  （不变式 6）；入口路径 `../` 与非法版本号在清单校验阶段拒绝。

- **消息索引（首次实际借用 Telegram 代码）**：`ui/src/vendor/tweb/slicedArray.ts`
  复制自 tweb（GPL-3.0，可并入 AGPL-3.0 作品）的 `SlicedArray`，替换手写的 `Map + sort` 分页合并；
  同源的 `ListenerSetter` 用于菜单外点关闭的监听器清理。适配与来源见文件头与
  `THIRD-PARTY-NOTICES.md`。

```bash
cargo test -p wxlike-platform --test p5_live     # 9
cargo test -p wxlike-container --test package    # 7
bash ui/scripts/check-msgindex.sh                # 7（tweb 索引合并语义）
```

## 系统推送（WP-06）

`crates/push`：`PushProvider` trait + `NoopProvider`（默认，带计数）+
`FcmProvider`（HTTP v1，凭据从 `WXLIKE_FCM_*` 环境变量注入，无凭据构造即失败回退 Noop；
APNs 留 trait 槽位——需要苹果开发者账号，无法离线验收）。
**payload 结构上没有正文字段**：只有 from_user + conv_id，聊天正文走 E2EE 不进推送。
hub 在路由表查无任何在线连接时（且仅此时）触发 `OfflinePush` 钩子，
分发到该用户全部注册设备；设备表 `POST/GET /api/devices` 按平台 upsert（最新 token 胜出）。

```bash
cargo test -p wxlike-platform --test devices    # 6 项（注册/替换/多端共存/鉴权）
cargo test -p wxlike-push                       # 6 项（payload 永不含正文 + hub 离线触发端到端）
```

## 音视频通话（WP-08）

`crates/rtc`，媒体走 **str0m**（纯 Rust WebRTC；任务书禁 libwebrtc C++ 绑定）：
- **状态机**（`state`）：主叫/被叫各自角色化，非法迁移一律拒绝不留活尸；
  离线呼叫**立即** Failed（30s 响铃超时、10s 对端静默看门狗由 `CallManager::check_deadlines` 驱动）；
- **信令**（`signal`）：复用 QUIC 通道的 `MsgBody::Custom{kind:"rtc-signal"}`，
  hub 的防伪造同样管着信令；双方同抢（glare）用 `(from, call_id)` 字典序仲裁，两侧必收敛同一结果；
- **媒体**（`media`）：真实 ICE/DTLS/SRTP——虚拟线（进程内）与**真 UDP 回环**两条验证路径；
  WP-08b 视频 m-line + `BitrateController` 码率阶梯（骤降快砍、余量缓爬、封顶）；
- **ICE 配置**（`ice`）：内置最小 STUN binding 客户端（`WXLIKE_STUN_URL`），
  TURN 是配置接口（`WXLIKE_TURN_URL`，部署侧起 coturn，无 TURN 时同网段可通即验收环境）；
- **采集与编码**：`audio` feature（cpal + Opus，已验证 960 采样往返），
  `camera` feature（nokhwa）；无头 CI 默认关闭。
- server 暴露 `GET /api/presence/{user_id}`（路由表即真相）供呼叫前探测。

```bash
cargo test -p wxlike-rtc                        # 34 项（状态机/信令防伪造/媒体回环/码率阶梯/真实 hub 全流程）
```

## 运维基线（WP-13）

- **可观测**：hub 连接数/吞吐/离线积压/投递延迟直方图（Prometheus 文本格式，
  p99 用 `histogram_quantile` 原生查询）；`/metrics` + `WXLIKE_LOG` tracing 全链路 +
  SQLite 慢查日志（>100ms 语句带 SQL 文本）。
- **压测**（全量目标，release，报告 [docs/bench/wp13_hub.md](docs/bench/wp13_hub.md)）：
  **10 000 并发连接、1 000 msg/s、60s：60 000 条全部送达、断连 0、p99 = 5ms**（预算 50ms）。

```bash
cargo run --release -p wxlike-net --example bench_hub    # 全量目标
BENCH_CONNS=300 BENCH_RATE=200 BENCH_SECS=8 cargo run --release -p wxlike-net --example bench_hub  # 冒烟
```

- **崩溃恢复**：hub 被杀 → 同址重启 → 客户端 `connect_retry` 退避重连（30s 内），
  暂存与已持久化消息零丢失（`cargo test -p wxlike-net --test ops_recovery`）。
- **一键起栈**：`docker compose up -d --build`（redis + server + SeaweedFS S3；
  MinIO 公共镜像已从 Docker Hub/quay 下架，任何 S3 端点都兼容 `MediaStore` trait）；
  健康检查齐全，`curl http://127.0.0.1:8099/health` 与 `/metrics` 即验收。
  `WXLIKE_DEV_FAUCET=1` 时额外挂载 `/api/dev/fund`（虚拟资金演示用，生产勿开）。

## 动态页包（DPG1）

借鉴逆向研究所得的"桩 + 压缩载荷"思路（自研格式，非 CSO）：

```
+--------+---------+--------+-------------+------------------+
| DPG1   | version | id(32) | payload_len | zstd(bincode files) |
| 4B     | 4B      | 4B     | 8B          | ...               |
+--------+---------+--------+-------------+------------------+
```

页面 = `manifest.json`（name/main_js/title）+ `main.js` + 任意资产。宿主经 `wxhost` 桥提供能力：`log / setTitle / storeSet / storeGet / nowMs`；生命周期钩子 `onLoad(payload)` / `onShow()`。

## 本地复用研究容器（严格限本地实验）

```bash
cargo run -p wxlike-container --example research_wmpf -- <path-to-JS>
# 默认加载 ../../source_recovered/wmpf_js/WAWebview.js
```

现状（2026-10-16）：Boa 0.20 已能**完整解析** 4.6MB 的 WAWebview.js 并执行至首个缺失的浏览器全局（`TypeError: not a callable function`）——迭代方式：按报错逐个补 shim（`examples/research_wmpf.rs` 内）。

**边界**：`source_recovered/wmpf_js/*` 是腾讯版权代码，仅限本地研究，禁止随任何构建产物分发。

## 桌面 UI（WP-11，Tauri2 + TypeScript + jotai）

`ui/` 独立于 workspace 构建。**所有服务器交互都是 Rust command**——前端只渲染，
从不自己开 HTTP/QUIC 连接（架构铁律 §0.1.1）；状态管理 jotai（vanilla API）；
esbuild 打包进 `ui/dist`（frontendDist）。QUIC 连接走 WP-12 的 TOFU 钉扎，
指纹落在应用数据目录。已覆盖：注册登录、会话列表（滑动操作/搜索展开/未读角标）、
聊天气泡（收发分侧）、图片消息（九宫格 + 缩略图点击看原图）、好友（申请/同意/备注）、
建群/群聊、红包（发送/拆/余额）、全量深色模式、聊天页转场。

```bash
# 前置：Rust(MSVC) + Node ≥18 + WebView2（Win10/11 自带）+ VS Build Tools
cd ui
npm i
npm run build            # tsc --noEmit + esbuild 打包到 dist/
npx tauri dev            # 首次编译 src-tauri 后弹出窗口（干凈环境 ≤15 分钟）

# 命令层的 Rust 侧测试（打真实 in-process 服务器，禁 mock）
cd ui/src-tauri && cargo test --test ui_commands   # 16 项
cargo clippy --all-targets                          # 0 warning（任务书 A4）
```

## Rust 版本注意（已踩坑记录）

- rustls 0.23：方法名是 `with_custom_certificate_verifier`；`dangerous()` 在 builder 与 ClientConfig 上都有
- quinn 0.11：rustls 配置需经 `quinn::crypto::rustls::QuicClientConfig::try_from(..)` 包装
- QUIC 流是懒打开：客户端 `open_bi()` 后必须先写一帧（空帧即可），否则服务端 `accept_bi()` 永远挂起
- Boa 0.20：`NativeFunction` 仅接受 `Copy` 闭包/函数指针 → 桥状态放 thread_local，`BoaEngine::new()` 重置
- zstd 0.13 `encode_all` 收 `R: Read`，传 `slice` 而非 `&Vec`

## 下一步

1. ~~消息模型对齐~~（WP-03 已完成）~~多端同步~~（device_cursor + sync_since 已完成）
2. ~~UI~~（WP-11 已完成）、~~E2EE double ratchet~~（WP-12 vodozemac 已完成）
3. Android：`cargo-ndk` 出 .so + JNI 桥
4. container：换 quickjs-ng 引擎，继续 WA* shim 迭代
5. 端到端最终验收剧本（任务书 §4：双机 + 双 hub + UI 全流程录屏）
6. P5：Flutter 三端 / 直播频道 / 小程序 M1–M5 / video 完整版 / Web 工程
