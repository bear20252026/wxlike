# wxlike

Rust 跨平台 IM（微信形态）骨架 + 本地复用研究容器。自研协议与实现，**不含任何腾讯代码进入分发物**。

> **许可：GPL-3.0**（2026-10-16 起）。项目定位为研究实验，非商用——借鉴引用遵循
> [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。研究定位下已解锁 GPL 生态借鉴
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
│  └─ platform/    # ★ 业务数据面（WP-01）：axum HTTP + SQLite 账户/会话/资料，PBKDF2 口令哈希
├─ server/         # 开发服务器：platform API + MQ 路由 hub（WXLIKE_MQ=memory|redis）
└─ ui/             # Tauri2 脚手架（独立构建，见下）
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

许可与移植声明见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)（wxlike 本体 MIT；MQ 设计移植自 box-im MIT 设计，未含其 Java 代码）。

## 快速开始

```bash
cargo build
cargo test                 # 164 tests: E2EE / 存储 / 打包 / 分帧 / 中继 / 桥 / 平台 API / 好友 / 消息语义 / 群聊 / 媒体 / 钱包 / 可靠队列

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

## UI（Tauri2，独立构建）

`ui/` 为 Tauri2 脚手架（已从 workspace exclude）。构建需 Node + tauri CLI：

```bash
cd ui
npm i -D @tauri-apps/cli && npx tauri dev     # 首次会编译 src-tauri
```

前端 `ui/dist/index.html` 是零依赖静态页，通过 `invoke` 调用 Rust 命令；`src-tauri` 已接 `wxlike-core/net` 作为命令后端。

## Rust 版本注意（已踩坑记录）

- rustls 0.23：方法名是 `with_custom_certificate_verifier`；`dangerous()` 在 builder 与 ClientConfig 上都有
- quinn 0.11：rustls 配置需经 `quinn::crypto::rustls::QuicClientConfig::try_from(..)` 包装
- QUIC 流是懒打开：客户端 `open_bi()` 后必须先写一帧（空帧即可），否则服务端 `accept_bi()` 永远挂起
- Boa 0.20：`NativeFunction` 仅接受 `Copy` 闭包/函数指针 → 桥状态放 thread_local，`BoaEngine::new()` 重置
- zstd 0.13 `encode_all` 收 `R: Read`，传 `slice` 而非 `&Vec`

## 下一步

1. **server 拆分业务/推送**（学 box-im）：`hub`（纯推送）+ `platform`（业务 API）+ 多节点路由表接口——详见 [docs/reference-projects.md](docs/reference-projects.md)
2. **消息模型对齐**：`MsgBody::Custom{kind,payload}`（红包/位置/引用/撤回）、`msg_id` 去重、已读回执、withdrawn
3. **多端同步**：设备级会话游标增量补拉
4. UI：Tauri2 构建跑通，朋友圈九宫格/深色模式/滑动操作清单见参考文档
5. E2EE：static-static 升级 double ratchet
6. Android：`cargo-ndk` 出 .so + JNI 桥
7. container：换 quickjs-ng 引擎，继续 WA* shim 迭代
