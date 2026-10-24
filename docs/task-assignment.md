# wxlike 任务分工说明书

> 版本 1.0 · 2026-10-20 · 适用对象：接手本项目的工程师（可多人并行）
> 配套文档：`master-blueprint.md`（总蓝图）、`parallel-tracks.md`（并行契约）、
> `reference-mirror.md`（参考仓库清单）、`THIRD-PARTY-NOTICES.md`（借用登记）

---

## 0. 怎么用这份文档

1. 先读 §1（项目是什么）与 §2（为什么没有安卓/网页）——**这两节决定你该领哪张卡**。
2. 再读 §3（全员通用规范）——**这是硬性要求，不是建议**。
3. 到 §5 领一张任务卡。每张卡自带：目标、输入、交付、**可执行的验收命令**、依赖、文件所有权。
4. 一卡一分支一 worktree（§3.3）。做完提 PR 给集成方，**不要自己 push main**。

---

## 1. 项目是什么、现在到哪了

**wxlike**：微信级 IM 的 Rust 实现。服务端 + 桌面端 + 一套工具链（CLI / MCP / 部署脚本）。

### 1.1 规模（实测）

| 指标 | 值 |
|---|---|
| crate | 20 个（core / net / message / platform / rtc / sfu / video / risk / moderation / report / ledger / push / search / container / dpage / mq / cmd / client / cli / mcp） |
| Rust 代码 | 50,081 行 |
| 测试 | 602 个 |
| CI | 五关：`test(macos)` `test(linux)` `test(windows)` `lint` `doc` |
| 许可 | **AGPL-3.0-only**（对外提供网络服务须提供源码，AGPL §13） |

### 1.2 分层（谁依赖谁，单向）

```
ui/src（TypeScript，浏览器可运行）
   ├── ui/src-tauri（Rust 壳：Tauri 命令、本地 SQLite、消息泵）
   │        └── crates/client（协议层：HTTP 传输 + 命令执行器）
   │                 └── crates/cmd（命令目录 + 权限中心）
   └── （网页端将直接对接 HTTP + WS，见 Zone A）

crates/core        协议类型 / 信封 / E2EE / 本地存储 / 发送队列
crates/net         QUIC 传输、hub（扇出/镜像/投递策略）、WS 载体、诊断、心跳
crates/message     三态信箱（direct/group/channel）、每属主序列号、五层同步
crates/platform    控制面（用户/好友/群/社群/钱包/风控/审计…）+ HTTP 路由
server             装配：平台 + hub + HTTP + QUIC
crates/rtc · sfu   1:1 通话、房间管控、SFU
crates/video       点播（HLS 方向）
crates/container · dpage   小程序容器与包格式
crates/cli · mcp   命令面的两个入口（只读 + 执行）
```

**关键设计**：命令面（`crates/cmd`）是唯一的能力目录，HTTP / CLI / MCP / Tauri 四个入口
共用同一份 catalog 与同一个 `Policy`。**新增功能 = 往 catalog 加一条 + 在一个入口实现**。

### 1.3 进度（对照蓝图 P1–P6，加权约 43%）

| 阶段 | 权重 | 完成度 | 剩余主战场 |
|---|---|---|---|
| P1 体验补齐 | 1.5w | 90% | 回执 UI 细节 |
| P2 真机验收 | 1.5w | 75% | 双机音视频验收（需真机） |
| P3 读扩散/社群 | 5w | 50% | **社群层（community/thread/频道角色）**、多设备 |
| P4 运营/资金/通话 | 3.5w | 65% | 支付四层仍是演示级 |
| P5 移动端/直播/小程序 | 6w | **15%** | **Flutter 三端 = 0** |
| P6 发布工程 | 3w | 25% | adminx 后台、规模化 |

---

## 2. 为什么没有安卓端、没有网页端（先回答这个）

这两个不是"忘了"，是**排序的结果**。说清楚原因，你才知道该从哪接手。

### 2.1 网页端：其实已经有 90%，只差一层适配

`ui/src` 是**纯 TypeScript + DOM**（jotai 管状态，esbuild 打包），产物 `ui/dist/`
就是 `index.html + app.js + styles.css`——它本来就是个网页，Tauri 只是给它套了个壳。

真正缺的只有两件：

1. **`invoke` 的浏览器实现**。`ui/src/tauri.ts` 里所有调用都是
   `window.__TAURI__.invoke("send_text", {...})`。浏览器里没有 Tauri IPC，
   需要一个同名函数，把命令名翻译成 HTTP 请求（`send_text` → `POST /api/conv/{c}/send`）。
   这是**一张卡的工作量**（A1），不是一个月。
2. **WebSocket 客户端**。服务端已经有 `/ws` 网关（`server/src/lib.rs::ws_routes`），
   说的是**同一套 bincode `WireFrame`**。浏览器要说得先写一个 JS 侧的 bincode 编解码
   （参考 `refs/tweb` 的二进制协议层）。这是第二张卡（A3）。

之所以一直没做：我优先做了服务端深度（信箱一致性、审计、通话），
而网页端不影响"引擎是否正确"。它影响的是"能不能给别人用"——现在该补了。

### 2.2 安卓端：架构决定了，但从没开工

ADR-11 定的是 **Flutter + UniFFI**：`crates/*` 用 UniFFI 暴露给 Dart，Flutter 只写 UI。
这个决定是对的（一套 Rust 逻辑，三端复用），但有三个现实障碍：

| 障碍 | 说明 | 现在的状态 |
|---|---|---|
| 客户端逻辑原本长在 Tauri 壳里 | `ui/src-tauri/src/lib.rs` 曾 2080 行，装着 AppState / 本地库 / E2EE / 消息泵，UniFFI 包不了 | **已部分解决**：协议层抽成了 `crates/client`（UniFFI 要绑的就是它 + `crates/core`）。壳现在 2249 行，剩下的是**桌面端专属**编排（本地库、泵、窗口命令）——移动端不需要它，用 Dart + 绑定重写 |
| 需要真机才能验收 | 后台保活、推送、切网重连，模拟器测不出来 | **仍是障碍**：需要一台安卓机 |
| 没有 Flutter 工程骨架 | 从零 | **未开始**，Zone B 就是它 |

一句话：**安卓不是"技术上做不到"，是"需要真机 + 一整条 Flutter 工程，而我这边无法验证"**。
所以它最适合交出去——任务卡按"能独立验收"切开（B1 绑定层可以纯 Rust 侧验收）。

---

## 3. 全员通用规范（硬性）

### 3.1 技术栈与语言

| 区域 | 语言 | 说明 |
|---|---|---|
| 服务端 / 核心 / 工具链 | **Rust**（edition 2021，workspace 统一版本） | 所有业务逻辑必须在 Rust 侧，**不要**在客户端语言里重写一遍 |
| 桌面端 UI | TypeScript（无框架，jotai + DOM） | 已有 3,679 行 TS，改动请沿用现有风格 |
| 网页端 | TypeScript | 复用 `ui/src`，只换传输层（Zone A） |
| 安卓端 | Dart（Flutter）+ **Rust via UniFFI** | Dart 只写 UI；协议/存储/E2EE 全部走 Rust 绑定 |
| 脚本 / 部署 | Bash（`deploy/`） | 编辑 `deploy/**`、`*.sh` 必须用 LF 换行 |

**铁律**：任何"业务规则"（谁能看这条消息、未读怎么算、消息怎么排序）只能写在 Rust 里。
客户端语言里出现业务规则 = 三端必然不一致。

### 3.2 DoD（完成的定义）：六关全绿才算做完——**由云端 CI 判定，不在本地跑**

```bash
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo fmt --all -- --check
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
cd ui && npx tsc --noEmit
```

少一关 = 没做完。以上六关**全部由 GitHub Actions 判定**（`ci.yml` + `audit.yml`，
PR 上约 5–15 分钟出结果），交付方**不要在本地跑**：多 worktree 并行时本地 target/
缓存曾膨胀到 224G（2026-10-23 已全部清理）。本地只允许秒级轻检查：
`scripts/quality-gate.py`、`scripts/count_unwraps.py`、`cargo fmt --check`。
确需本地调试时设共享缓存 `CARGO_TARGET_DIR`（所有 worktree 一份），用完即删。
PR 全绿 = DoD 满足，合并权在集成方。

### 3.3 分支与工作树（多人并行时最重要的一条）

```bash
# 一条任务一张卡，一张卡一个 worktree
git worktree add ../worktrees/<card-id> -b feat/<card-id>
cd ../worktrees/<card-id>
# ... 干活、提交 ...
```

- **一条卡一个 worktree**。共用目录会抢文件与 git index，冲突不可恢复。
- **只提交，不 push**。推送与 CI 由集成方统一做（每条分支各推一次 = 一次 7 分钟 CI）。
- 分支命名：`feat/<card-id>`、`fix/<card-id>`。卡 ID 见 §5（如 `A1`、`B2`）。
- 合并：集成方 `git merge --no-ff`，跑全量测试，绿了才 push。

### 3.4 提交信息

```
<type>(<scope>): <一句话>

<为什么这么改，不是改了什么——diff 已经说明了改了什么>

验证：<跑了什么命令，结果如何>
```

`type` ∈ `feat|fix|refactor|test|docs|chore|perf`。
**提交信息里有反引号时，写成文件用 `git commit -F`**——内联 `-m` 会被 shell 当命令替换。

### 3.5 借用参考代码的三步动作（用户明确要求，违反视为违规）

0. **先查自己的仓库**：`grep` 一下有没有同名能力。曾经在 `platform` 层重写了一遍
   `crates/message` 已有的未读/游标，只能回滚。
1. **再查参考库**：Tier 0 八个 → `refs/` 43 个仓库（清单见 §4）。
2. **先给结论再动手**：三行内写清「照搬 / 参照重写 / 自研」+ 上游路径 + 许可。
   没写结论就开始写代码 = 违规。
3. **能搬就搬**：同语言（Rust↔Rust、TS↔TS）优先 vendoring 或加依赖；
   跨语言（C++/Go/Java/TS）按其**分层与状态机**重写，不逐行翻译。

### 3.6 借用必须登记

任何复制进仓库的代码，在 `THIRD-PARTY-NOTICES.md` 的 "Vendored code" 表**按文件级**登记：
上游 URL + 路径 + 许可 + 借了什么 + 适配了什么；代码文件头写来源块。
`refs/` 在仓库外，CI 只克隆本仓库——**借用必须 vendoring 进仓库或改成 crates.io 依赖**。

### 3.7 禁止事项

| 禁止 | 原因 |
|---|---|
| push `main` / force push | main 是保护分支，且集成方要统一跑 CI |
| 改 §6.1 的热点文件（不在你所有权内） | 多分支同时改 = 语义级冲突 |
| 加"顺手"依赖 | 每个依赖都要过审计；先问 |
| `--no-verify` 跳过 hook | 会绕过 fmt/clippy，CI 必红 |
| 在客户端语言里写业务规则 | 三端不一致的根源 |
| 静默降级（出错就返回空/默认值） | 本项目明确禁止"看着能用、结果全空"，见搜索索引的教训 |

---

## 4. 参考代码库（本地已克隆，直接 grep）

> 全部在 `D:/mimo/test/refs/`（43 个仓库，仓库外，不参与 CI）。
> 刷新：`cd /d/mimo/test/refs/<repo> && git pull --ff-only`。
> 详细"按缺口查表"见 `docs/reference-mirror.md`。

### 4.1 Tier 0（最高优先级，用户指定）

| 项目 | 路径 | 许可 | 借什么 |
|---|---|---|---|
| **Zulip** | 未镜像 | Apache-2.0 | 主题化线程（topic/recipient 双层）、消息编辑历史 |
| **Rocket.Chat** | 未镜像 | MIT（`ee/` 除外） | 权限矩阵（**已借** → `platform/src/perms.rs`）、REST/Realtime API 形状 |
| **Mattermost** | 未镜像 | AGPL-3.0 | adminx 后台、插件面 |
| **Signal-Desktop** | 未镜像 | AGPL-3.0 | 发送队列/重试、附件分片 |
| **Signal-Android** | 未镜像 | AGPL-3.0 | 会话状态机、媒体压缩、通知聚合 |
| **Element Web** | `refs/element-web` | AGPL-3.0 | 房间列表虚拟化、E2EE 设备验证 UI |
| **Synapse** | 未镜像 | AGPL-3.0 | 事件 DAG、同步模型 |
| **Nextcloud Talk** | `refs/spreed` | AGPL-3.0 | 群通话房间 UI、mesh 上限（**已借** → `rtc::sfu`） |

> 未镜像的四个是体积原因（600 MB–2.5 GB）。需要时自己浅克隆：
> `git clone --depth 1 <url>` 到 `refs/`。

### 4.2 Telegram 系（协议/同步/媒体的第一参考）

| 路径 | 上游 | 借什么 |
|---|---|---|
| `refs/tweb` | morethanwords/tweb | **Web 客户端**：二进制协议编解码（Zone A3 必读）、虚拟列表、媒体缓存 |
| `refs/grammers` | Lonami/grammers | Rust MTProto 客户端：分层、session、上传下载 |
| `refs/teamgram-server` | teamgram | Go 服务端：seq/pts、多端同步 |
| `refs/telegram_rust` `refs/tuigram` `refs/ferogram` `refs/hungry-rs` `refs/only-talk-rs` | 各自 | Rust 客户端骨架、TUI、媒体处理 |
| `refs/gramsrv` `refs/mytelegram` | 各自 | Go 服务端变体 |

### 4.3 按主题（挑最相关的）

| 主题 | 路径 | 借什么 |
|---|---|---|
| 读扩散 / 信箱 / 序列号 | `refs/WuKongIM` `refs/open-im-server` `refs/goim` | 频道订阅、五层差分同步（**已借** → `crates/message`） |
| 移动端 IM 网络调度 | `refs/mars`（MIT） | 智能心跳 3.5–10 分钟、长短连降级、防雪崩、动态超时、僵尸任务（**已借** → `crates/net/heartbeat.rs`、`crates/core/outbox.rs`、`crates/net/diag.rs`） |
| 小程序容器 | `refs/Rong` `refs/dimina` | 双线程模型、Bridge 协议 |
| 直播 | `refs/xiu` `refs/blivedm_rs` | RTMP ingest、弹幕协议 |
| 推送 | `refs/gorush` `refs/push_server` | APNs/FCM 扇出 |
| 通话 / SFU | `refs/atm0s-media-server` | Rust + str0m 同栈，分层参照重写 |
| 搜索 | `refs/tantivy` | **已是真依赖** |
| 支付 | `refs/cala` `refs/obix` `refs/dtmrs` `refs/hyperswitch` | 账本、支付编排 |
| 前端工程 | `refs/HuLa` `refs/HuLa-IM-Tauri` `refs/TangSengDaoDaoWeb` | 会话列表/消息操作的交互细节 |
| 一致性/复制 | `refs/iroh` `refs/ousia` `refs/ISM` | QUIC 打洞、状态同步 |
| 限流/排序 | `refs/sorted_set_nif` | 排行榜/未读计数的数据结构 |

### 4.4 按缺口查表（先查这里，再自己设计）

| 你要做的事 | 先看 |
|---|---|
| 网页端 `invoke` 适配 | `ui/src/tauri.ts`（现有实现）+ `ui/src/api.ts`（命令清单） |
| 浏览器二进制协议 | `refs/tweb`（`src/lib/mtproto/` 的 serializer/deserializer） |
| Flutter 绑定面 | `crates/client/src/lib.rs` + `crates/client/src/execute.rs` |
| 安卓后台保活 | `refs/mars`（`stn/src/smart_heartbeat.cc`）+ 本仓库 `crates/net/src/heartbeat.rs` |
| 社群/话题/频道角色 | Zulip（topic 模型）、Rocket.Chat（权限矩阵，已有 `perms.rs` 可扩展） |
| 运营后台 | Mattermost（admin 面板结构） |
| HLS 点播 | `crates/video` 现有骨架 + RFC 8216（`#EXTM3U`） |

---

## 5. 分区与任务卡

> 六个区，25 张卡。**区与区之间零文件重叠**，区内卡片按依赖排序。
> 每张卡：目标 / 输入 / 交付 / 验收 / 所有权 / 参考 / 预估。

### Zone A：网页端（TypeScript，复用 `ui/src`）

> 负责人：1 人。前置：无（服务端 `/ws` 已可用）。

**A1 · `invoke` 浏览器适配层** ⭐ 起手卡
- 目标：让 `ui/src` 在浏览器里跑起来，不依赖 Tauri。
- 输入：`ui/src/tauri.ts`（现在的 invoke/listen）、`ui/src/api.ts`（30 处 `invoke`，就是命令清单）。
- 交付：`ui/src/web/transport.ts` 实现同签名的 `invoke`/`listen`；命令名 → HTTP 映射表
  （`send_text` → `POST /api/conv/{c}/send`，`local_*` 类命令用 IndexedDB 或降级为空实现）。
- 验收：`npm run build:web` 产出 `ui/dist-web/`；起 `wxlike-server` 后浏览器能登录、看会话列表、发一条消息。
- 所有权：`ui/src/web/**`、`ui/package.json`（加一个 script）。
- 参考：`ui/src/api.ts` 的命令清单就是契约；`crates/platform` 的路由是服务端真相。

**A2 · 静态托管**
- 目标：服务端能直接发网页，不需要额外 nginx。
- 交付：`server/src/lib.rs` 加 `/app/*` 静态路由（`tower-http::services::ServeDir`），
  `WXLIKE_WEB_DIR` 指定目录；未配置时返回 404 而不是 panic。
- 验收：`curl -I http://127.0.0.1:8099/app/` 返回 200 + `text/html`。
- 所有权：`server/src/web_static.rs`（新建）+ `server/src/lib.rs` 两行（集成方代合）。

**A3 · 浏览器 WS 客户端** ⭐ 难点卡
- 目标：浏览器能收实时消息（QUIC 浏览器不支持，必须走 `/ws`）。
- 输入：`server/src/lib.rs::ws_routes`（**同一套 bincode `WireFrame`，Hello 先发**）。
- 交付：`ui/src/web/wire.ts`：`WireFrame` 的 JS 编解码（`Hello` / `Msg` / `Ping` / `Pong` / 回执）。
- 验收：浏览器登录后，另一客户端发消息，页面 200ms 内出现。
- 所有权：`ui/src/web/wire.ts`。
- 参考：`refs/tweb` 的 `serializer`/`deserializer`（同思路：先写 schema 表再写读写函数）。
- **注意**：不要自己发明 JSON 协议，服务端只有 bincode 一种。

**A4 · 浏览器能力补齐**
- 目标：媒体/通知/存储在浏览器里可用。
- 交付：图片消息用 `fetch` + Blob（替代 Tauri 的 base64 命令）、`Notification` API、
  IndexedDB 存最近 N 条（替代 SQLite）。
- 验收：刷新页面后最近 50 条仍在；收到消息时有系统通知（用户授权后）。

**A5 · 打包与部署**
- 目标：一个可发布的静态包。
- 交付：`scripts/build-web.sh`（tsc + esbuild + 复制 index.html/styles）+ 缓存头说明 +
  版本号注入（`__VERSION__`）。
- 验收：产物 < 400 KB（gzip 后），首屏 < 1s（本地）。

### Zone B：安卓端（Flutter + Rust UniFFI）

> 负责人：1–2 人。前置：真机一台。**B1 可以完全离线做**（纯 Rust 侧验收）。

**B1 · UniFFI 绑定层** ⭐ 起手卡（无需真机）
- 目标：把 `crates/client` 暴露成 Kotlin/Swift 可调用的 API。
- 输入：`crates/client/src/lib.rs`（`Http` + 领域方法）、`crates/client/src/execute.rs`（命令执行器）。
- 交付：新 crate `crates/ffi`，用 `uniffi` 0.28+ 定义 `WxlikeClient`：
  `register/login/connect/send_text/history/unread/mark_read/outbox_status`，
  错误类型 `WxlikeError`（网络/鉴权/权限三类）。
- 验收：`cargo test -p wxlike-ffi`（Rust 侧）；`cargo run -p wxlike-ffi --bin uniffi-bindgen generate --language kotlin`
  产出 Kotlin 文件且能编译。
- 所有权：`crates/ffi/**`、根 `Cargo.toml` 一行。
- 参考：UniFFI 官方 `examples/`；`crates/client` 就是唯一需要暴露的面。

**B2 · Flutter 工程骨架**
- 目标：三屏能跑（登录 / 会话列表 / 聊天）。
- 交付：`apps/mobile/`（Flutter 工程，不放进 Cargo workspace）；接 B1 的绑定。
- 验收：`flutter build apk --debug` 成功；真机装上能登录并看到会话列表。
- 所有权：`apps/mobile/**`。

**B3 · 本地存储与离线**
- 目标：断网能看历史，发不出去的消息进队列。
- 交付：Dart 侧用 B1 暴露的 `outbox_*` + 本地 SQLite（`sqflite`）存渲染用数据；
  **不要**在 Dart 里重算未读（那是 `crates/message` 的事）。
- 验收：飞行模式下打开应用能看到历史；发一条消息显示"待发送"；恢复网络后自动发出。

**B4 · 推送通道**
- 目标：后台能收到消息。
- 输入：`crates/push`（**FCM HTTP v1 已实现**，APNs 未做；另有 `aggregator.rs` 做通知聚合）、
  `platform` 的 `push_devices` 表。
- 交付：Android 侧接 `firebase_messaging` + 把设备 token 注册到 `/api/push/devices`；
  服务端只需补 APNs（iOS 用）与 token 失效清理。
- 验收：应用杀进程后，另一台设备发消息，10 秒内收到系统通知。

**B5 · 真机验收清单**
- 目标：把"能跑"变成"敢用"。
- 交付：`docs/mobile-acceptance.md`，逐条打勾：双机收发、切 WiFi↔4G、后台 30 分钟保活、
  弱网（限速 200 kbps）下发送、进程被杀后 outbox 补发、通知点开进对应会话。
- 验收：清单全绿 + 录屏。

### Zone C：社群层（Rust，**串行，一个人做到底**）

> 负责人：1 人。**不要拆给多人**——四张卡都改 `platform` 的同一批文件。

**C1 · 数据模型与迁移**
- 交付：`community` / `thread` / `channel_role` 三张表 + 迁移；`platform/src/communities.rs` 扩展。
- 验收：`cargo test -p wxlike-platform --test communities` 新增用例全绿。
- 参考：Zulip 的 topic 模型（`zerver/models/recipients.py` 思路：topic 是消息的一等属性）。

**C2 · 频道权限矩阵**
- 交付：`channel_role`（owner/moderator/member/guest）+ 每条消息类型的发送权限；
  复用 `platform/src/perms.rs` 的判定函数形状。
- 验收：非成员发消息 403；guest 不能发媒体；moderator 能撤回他人消息。
- 参考：`refs/Rocket.Chat`（`app/authorization`）+ 本仓库 `perms.rs`（已借过一次，扩展它）。

**C3 · 话题订阅**
- 交付：`thread_subscription(user, thread, muted)`；未订阅的人不收该话题的推送，
  但能在频道时间线看到（与微信"话题"的差异点要写进文档）。
- 验收：订阅/取消订阅影响扇出名单（`MessageStore::members` 之外再加一层）。

**C4 · HTTP 契约**
- 交付：`GET/POST /api/community/{id}/threads`、`/api/channel/{id}/roles`；OpenAPI 片段进 `docs/api.md`。
- 验收：CLI 能用 `wxlike-cmd` 调到（命令面加 2 条）。

### Zone D：运营后台 adminx（Rust，独立进程）

> 负责人：1 人。**D1 已在进行中**（`worktrees/admin`，分支 `feat/adminx`）。

**D1 · 只读后台**（进行中）
- 交付：`crates/admin`：只读 SQLite、token 必填、`/health` `/api/stats` `/api/users`
  `/api/groups` `/api/audit` + 单页 HTML。
- 验收：6 项测试 + `WXLIKE_ADMIN_TOKEN` 缺失拒绝启动。

**D2 · 审计检索与工单**
- 交付：`/api/audit?actor=&command=&from=&to=`、`/api/reports`（`crates/report` 已有工单状态机）。
- 验收：按命令 id 过滤能查到"agent 执行过哪些写操作"。

**D3 · RBAC + MFA**
- 交付：管理员账号表 + TOTP（`platform/src/security.rs` 已有 `totp_verify` 可复用）+ 操作审计。
- 验收：没有 MFA 的管理员不能执行任何写操作；每次写都留痕。

### Zone E：媒体（Rust）

> 负责人：1 人。与 A–D 零冲突。

**E1 · video 的三块欠账**（`crates/video` 已实现转码 + HLS + 8 条路由）
- 现状：ffprobe 元数据、360/720/1080 阶梯转码、master/variant 清单、分片、缩略图、
  内容寻址（`video_id = sha256`）**都已实现并接进 server**（`/api/video/*`）。
- 缺的三样：
  1. **`Range` 请求支持**——`play.mp4` 现在整文件返回，`<video>` 拖进度条会失败；
  2. **测试为 0**——820 行代码没有任何测试，`rendition_heights` 的"只降不升"、
     清单格式、分片命名都该有断言；
  3. **播放统计**——谁看了多久（`crates/video` 里没有）。
- 验收：`curl -H 'Range: bytes=0-1023' .../play.mp4` 返回 206 + `Content-Range`；
  `cargo test -p wxlike-video` ≥ 6 项。

**E2 · 直播 ingest 对接**
- 输入：`refs/xiu`（Rust RTMP 服务端）、现有 `platform/src/live.rs`（推流凭据已实现）。
- 交付：ingest 服务把 RTMP 流转 HLS，落盘目录与 `live_ingest_secret` 校验打通。
- 验收：OBS 推流 → 浏览器 HLS 播放 30 秒不卡。

**E3 · 转码流水线**
- 交付：上传视频 → 三档转码 → 清单生成 → 回调平台；失败可重试（复用 outbox 思路）。
- 验收：上传 50 MB 视频，3 分钟内可播。

### Zone F：质量与发布（任何语言）

> 负责人：1 人。可与任意区并行。

**F1 · 万人群扇出基准**
- 交付：`benches/`（criterion）+ 报告：1 万人群一条消息的扇出耗时、信箱写入耗时。
- 验收：给出 p50/p99，并指出瓶颈在哪个函数。

**F2 · 安装包**
- 交付：`scripts/package.sh` 产出 `.dmg` / `.deb` / `.msi`（Tauri bundler 或 `cargo-bundle`）。
- 验收：三个平台上装完能启动并连上本机服务。

**F3 · E2E 冒烟**
- 交付：`scripts/smoke.sh`：起服务 → 注册两个用户 → 加好友 → 发消息 → 查未读 → 撤回，
  全流程 curl/CLI，退出码即结论。
- 验收：`./scripts/smoke.sh` 在干净环境一次通过。

---

## 6. 协作规则

### 6.1 文件所有权（越界即冲突）

| 区域 | 允许改 | 不许碰 |
|---|---|---|
| Zone A | `ui/src/web/**`、`ui/package.json` | `ui/src-tauri/**`、`crates/**` |
| Zone B | `crates/ffi/**`、`apps/mobile/**` | 其余全部 |
| Zone C | `crates/platform/src/communities.rs`、`crates/platform/src/db.rs`（只加表） | `net/**`、`server/**`、`ui/**` |
| Zone D | `crates/admin/**` | 其余全部 |
| Zone E | `crates/video/**` | 其余全部 |
| Zone F | `scripts/**`、`benches/**`、`docs/**` | `crates/**` 的业务代码 |

**热点文件（任何 zone 都不许直接改，需要改动时提给集成方）**：

```
crates/platform/src/lib.rs      # 路由注册、PlatformOptions
server/src/lib.rs               # 装配、所有 HTTP 端点
ui/src-tauri/src/lib.rs         # 2100 行，命令与本地库
crates/net/src/mq_hub.rs        # 扇出、镜像、投递策略
Cargo.lock                      # 合并时以 main 为准重新生成
```

### 6.2 合并顺序（集成方执行）

1. 卡片自测六关（至少跑自己 crate 的那几关）；
2. 提交 PR，描述里写"改了什么 / 测了什么 / 留了什么没做 / 验收命令输出"；
3. 集成方 `git merge --no-ff` → 全量 `cargo test --workspace` → 绿了 push（一次 CI）；
4. 冲突以 main 语义为准，**重做**而不是硬合。

### 6.3 每日同步（三行，不要长）

```
完成：<卡号> <一句话>，验收命令：<命令> → <结果>
阻塞：<卡在哪，需要谁>
下一步：<明天做什么>
```

---

## 7. 验收节奏

| 里程碑 | 判定 | 谁验 |
|---|---|---|
| 单卡 | 卡上"验收"一栏的命令输出贴进 PR | 集成方 |
| 区 | 该区所有卡合并后，`cargo test --workspace` + 该区的手工验收清单 | 集成方 + 用户 |
| 版本 | P1–P6 某一阶段的所有区 | 用户 |

---

## 附录 A：常用命令

```bash
# 起服务（本机）
WXLIKE_DEV_INSECURE=1 WXLIKE_DB=/tmp/wx/wxlike.db WXLIKE_HTTP=127.0.0.1:8099 \
  WXLIKE_QUIC=127.0.0.1:9443 WXLIKE_MEDIA_DIR=/tmp/wx/media cargo run -p wxlike-server

# 命令行（命令面 18 条）
WXLIKE_AGENT_ALLOW_CONV='*' WXLIKE_TOKEN=<token> \
  cargo run -p wxlike-cli -- --server http://127.0.0.1:8099 inspect

# 前端
cd ui && npm run typecheck && npm run build

# 全量
cargo test --workspace
```

## 附录 B：术语

| 术语 | 含义 |
|---|---|
| 信箱（mailbox） | `crates/message` 的读扩散结构：每属主一条序列号，未读 = head − cursor |
| 属主（owner） | `Direct` / `Group` / `Channel` 三类会话 |
| 命令面 | `crates/cmd` 的 18 条命令，HTTP/CLI/MCP/Tauri 共用 |
| 五层差分 | 同步指令 `UpToDate / Delta / BatchCollapse / FullResync` 的判定 |
| outbox | 客户端发送队列，进程被杀也不丢 |
