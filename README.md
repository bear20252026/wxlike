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
cargo test                 # 107 tests: E2EE / 存储 / 打包 / 分帧 / 中继 / 桥 / 平台 API / 好友 / 消息语义 / 可靠队列

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
