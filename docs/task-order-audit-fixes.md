# 外包任务单：审计修复批次（第四轮审计 + Tier 0 基准之后的整改）

> 版本 1.0 · 2026-10-23 · 发包方：项目所有者 · 集成方：见 §6
> 配套：`task-assignment.md`（通用规范，**硬性要求**）、`parallel-tracks.md`（并行契约）、
> `reference-mirror.md`（Tier 0 镜像位置）、`audit-tier0-benchmark.md`（对拍基准）、
> `AUDIT_REPORT.md`（架构风险来源 AR-1…AR-7）
> **来源**：main 已合入第四轮审计修复（PR #62），本单分发的是其后剩余的 16 个 open issue。

---

## 0. 怎么用这份文档

1. 通读 §1 全局架构——**不知道自己的改动落在架构哪一层，就不要动手**。
2. 查 §3 文件所有权矩阵——**只许改自己名下的路径**；改到别人名下 = PR 直接拒收。
3. 查 §4 波次表——你的卡属于哪一波、前置接口是否已就位。
4. 领卡（§5），按 `task-assignment.md` §3 的通用规范干活：**一卡一分支一 worktree**，
   分支名 `fix/<任务号>`（如 `fix/T-A1`），PR 对 main，**禁止 push main**。
5. 每张卡自带验收命令，跑不过不许交；对抗测试是验收人另出的黑盒集，交付方测试全绿
   但对抗集不过 = 不通过。

---

## 1. 全局架构（改动定位图）

### 1.1 分层与依赖（单向，禁止逆向）

```
 Flutter(apps/mobile) ──FFI── crates/ffi ──┐
 Tauri2 桌面端 ui/ ────────────────────────┤
        │  ui/src-tauri（壳：命令/本地库/消息泵）      │
        └── crates/client（HTTP/WS 协议层）
                 └── crates/cmd（命令目录+权限）
─ 服务端 ─────────────────────────────────────────
 server（装配：axum HTTP + QUIC hub + 推送 + 检索）
   ├─ crates/platform   唯一业务数据面（SQLite；账户/好友/群/钱包/媒体/
   │      │              朋友圈/预密钥目录/审计/风控中间件 risk_mw）
   │      └─ crates/risk     风控引擎（进程内滑动窗口；多节点化 = T-B1）
   │      └─ crates/limits   常量库（T-B1 收编）
   ├─ crates/net         QUIC/WS 传输、mq_hub（扇出/补发/投递策略/指标）
   │      └─ crates/mq       MQ 抽象（Memory/Redis Streams/NATS）+ 路由表
   ├─ crates/push        离线推送（payload 不含正文）
   └─ crates/rtc · sfu   1:1 通话 / 房间管控 · SFU
─ 客户端内核 ──────────────────────────────────────
 crates/core    Envelope + E2EE(vodozemac) + 本地存储 + 发送队列 + at-rest 加密
 crates/container · dpage   小程序容器（Boa）+ DPG1 包格式
 crates/admin · cli · mcp   管理面/命令入口（**ureq3 迁移在途 = T-Z 独占**）
 crates/video   点播（ureq3 在途 = T-Z 独占）
```

### 1.2 关键数据流（改动前先想清楚落在哪段）

- **发消息**：client → platform HTTP（风控中间件 `risk_mw::gate` 统一计数）或 QUIC/WS →
  hub `route_envelope`（投递策略 `DeliveryPolicy`：blocked/muted/send_gate）→ 在线直投 /
  离线 `OfflineStore`（补发时执行秘密会话 TTL 判定 `expired_secret_frame`）。
- **E2EE 建会话**：`POST/GET /api/keys`（预密钥目录，双层 Ed25519 签名）→
  `Crypto::encrypt_dm`（fail-closed 验签）→ Olm 棘轮。
- **资金**：唯一入金口 `Platform::fund()`（无 HTTP 面的真实渠道缝）；`/api/dev/fund`
  是管理员鉴权的演示铸币口。
- **口令**：注册即 argon2id；遗留 pbkdf2 行登录时透明升级；会话 token 只以 sha256 摘要落库。

---

## 2. 任务 ↔ issue 对照总表

| 任务号 | 线 | open issue | 一句话 |
|---|---|---|---|
| T-A1 | A 密钥材料 | #47 | 弃用/移除 core::crypto 弱加密脚手架 |
| T-A2 | A 密钥材料 | #48 + #15(密钥持久化半边) | 设备密钥静态加密升级：口令派生 + 快照格式 v2 |
| T-A3 | A 密钥材料 | #53(core 部分) | core 请求路径 unwrap 清理 |
| T-B1 | B 风控线 | #61(L-2) + #32(多节点) | risk 多节点化（GCRA/Redis）+ 收编 limits |
| T-B2 | B 风控线 | #32(AR-5 余量) | 风控映射扩展到 groups/communities/live/call |
| T-C1 | C 传输线 | #32(Synapse 升级项) | SendGate 增加 Sleep 降级 + 限流指标标签 |
| T-C2 | C 传输线 | #56 | dpage next_id 冲突检测 |
| T-C3 | C 传输线 | #53(net 部分) | net/offline、mq_hub unwrap 清理 |
| T-D1 | D 桌面端 | #31(客户端半边) | 桌面端 TURN 短期凭据对接 |
| T-D2 | D 桌面端 | #25(遗留) | 安全码/设备验证 UI |
| T-D3 | D 桌面端 | #15(客户端半边) + #48(UI 半边) | 秘密聊天设置 UI、TTL 本地清除、口令解锁 |
| T-E1 | E 通话线 | #58 | SFU join 鉴权 + 成员校验 + 人数上限 |
| T-E2 | E 通话线 | #59 | STUN 属性 pad4 修正 |
| T-F1 | F 治理线 | #61(L-3) | CI 增加 cargo audit + 覆盖率闸门 |
| T-F2 | F 治理线 | #55 | 许可元数据统一（MIT→AGPL 描述） |
| T-Z1 | Z 迁移线 | #60 + #22(收尾) | admin upstream 3xx 语义修正；ureq3 全量收尾 |
| T-Z2 | Z 迁移线 | #53(admin/client/ffi/video 部分) | 四个在途 crate 的 unwrap 清理 |

已关闭不要重复领：#57（hub 停机注销，`87dd471`+`40ace05` 已修）、#29/#30（已修）。
领卡前先 `gh issue view <n>` 确认仍 OPEN。

---

## 3. 文件所有权矩阵（唯一防冲突机制，违者拒收）

| 线 | 独占路径 | 禁触路径 |
|---|---|---|
| **A** | `crates/core/**`（`crypto.rs`、`e2ee.rs`、`outbox.rs`） | `ui/src-tauri/**`（D 的接口消费者）、`crates/ffi/**`（Z） |
| **B** | `crates/risk/**`、`crates/limits/**`、`crates/platform/src/risk_mw.rs`、`crates/platform/src/recovery.rs`（risk_gate 定义处） | `crates/net/**`（C）、其余 platform 业务模块 |
| **C** | `crates/net/**`、`crates/dpage/**`、`crates/mq/**` | `crates/risk/**`（B）、`server/src/lib.rs` |
| **D** | `ui/src-tauri/**`、`ui/src/**`、`apps/mobile/lib/**` | `crates/core/**`（A 的接口，只调用不修改） |
| **E** | `crates/sfu/**`、`crates/rtc/**` | `crates/platform/src/groups*`（成员校验走既有 pub API） |
| **F** | `.github/**`、`scripts/**`、仓库元数据（GitHub 描述/README 徽章）、`deny.toml` | 任何 `crates/**` |
| **Z** | `crates/admin/**`、`crates/client/**`、`crates/ffi/**`、`crates/video/**`、`server/tests/**`、`server/src/web_static.rs` | 其余全部 |
| **集成方** | 根 `Cargo.toml`、`Cargo.lock`、`server/src/lib.rs`、`crates/platform/src/lib.rs` | — |

**共享文件仲裁规则**：
1. `Cargo.lock`：各线**只在自己 crate 的 `Cargo.toml` 加依赖**，提交前 `cargo check`
   本地再生成但**允许 PR 里带锁文件冲突**——集成方统一 `cargo update -p` 处理，不要求
   各线自行解冲突；
2. 根 `Cargo.toml`（workspace.dependencies）只有 F（T-F1 引入 audit/coverage 工具链时）
   可动，且必须先在 PR 描述声明；
3. `server/src/lib.rs` 与 `crates/platform/src/lib.rs` 本波**冻结**：任何人需要这两个
   文件出现"接线"级改动（如新路由、新中间件挂载），改为在 PR 里附 patch 片段，由
   集成方在 Wave 3 统一应用；
4. 平台 HTTP 契约变更（新端点/改语义）必须先改 `docs/api.md` 并在 PR 描述置顶声明——
   D 线对着 `docs/api.md` 开发，不对着别人的实现分支开发。

---

## 4. 波次（合并调度，防合并地狱的时序闸）

```
Wave 1（立即并行，零文件交集）
  T-A1  T-A3  ─ A 线（core 内部自洽）
  T-B1  T-B2  ─ B 线（risk/limits/risk_mw）
  T-C1  T-C2  T-C3 ─ C 线（net/dpage/mq）
  T-D1        ─ D 线第一卡（TURN 客户端，只依赖已存在的 /api/turn/credentials）
  T-E1  T-E2  ─ E 线（sfu/rtc）
  T-F1  T-F2  ─ F 线（CI/元数据）
  T-Z1  T-Z2  ─ Z 线（在途迁移收尾）
        │
Wave 2（依赖 Wave 1 的接口定稿）
  T-A2  ─ A 线：先发"接口 PR"（只有签名+stub+测试，≤200 行），接口冻结后
  T-D2  T-D3 ── D 线对着冻结接口开发 UI（§5 的契约 AC-1/AC-2）
        │
Wave 3（集成与完美平衡，仅集成方）
  ① 统一再生成 Cargo.lock；根 Cargo.toml 汇总；应用冻结文件补丁
  ② 全仓 `cargo clippy --workspace --all-targets -- -D warnings` + 三平台 test + fmt
  ③ #53 全仓 unwrap 计数复核（CI 加静态计数闸门，防回潮——见 T-F1 交付物）
  ④ #23/#32/#34 路线图状态刷新；对抗测试黑盒集统一回归
```

---

## 5. 任务卡（七条线）

### 线 A：密钥材料与密码学卫生

**共同上下文**：core 的 at-rest 现状是 `WXSC1` 魔数 + 设备密钥 XChaCha20（`e2ee.rs`
`seal_at_rest/open_at_rest`），设备密钥与密文同目录裸存（#48 的根因）。

#### T-A1 弃用弱加密脚手架（#47）
- **目标**：`crates/core/src/crypto.rs`（static-static X25519+HKDF+ChaCha20，无前向保密，
  全仓零调用者）退出公共 API。选项按优先级：①直接删除（若有文档承诺 1.0 移除）；②
  `#[deprecated]` + 模块级 `//!` 警示 + 编译期拒绝（`#[doc(hidden)]`）。威胁模型已宣布
  1.0 移除，**推荐①**。
- **验收**：`grep -rn "core::crypto" crates server ui/src-tauri apps/mobile/lib` 零命中；
  `cargo test -p wxlike-core` 全绿；`cargo doc --no-deps` 无 broken link。
- **参考**：`docs/threat-model.md`"兼容与迁移"节（本项目自己的退役计划）。

#### T-A2 设备密钥静态加密 v2（#48 + #15 密钥持久化半边）⚠️ 接口卡
- **目标**：快照加密从"裸设备密钥"升级为"口令派生"：`argon2id(passphrase, salt)` →
  32B 密钥 → XChaCha20（沿用现有 AEAD）。产物三件：
  1. **接口 PR（先发，Wave 2 的闸门）**：`Device::seal_with_passphrase /
     open_with_passphrase` + `Snapshot` 格式版本 `WXSC1 → WXSC2`（旧文件明确报错拒绝，
     不做静默迁移），≤200 行，先合先冻结；
  2. 盐与 KDF 参数随密文头部存储（PHC 字符串自带，照搬 `platform/src/security.rs`
     的 argon2 用法）；
  3. 错误类型：口令错 ≠ 文件损坏，两个可区分变体（D 线 UI 要分别提示）。
- **文件所有权**：`crates/core/src/e2ee.rs`、`Cargo.toml`（argon2 已在依赖树，加声明即可）。
- **接口契约 AC-1**（D 线依赖，冻结后不得改名）：
  ```rust
  pub fn seal_with_passphrase(&self, passphrase: &str) -> Result<Vec<u8>>;
  pub fn open_with_passphrase(passphrase: &str, blob: &[u8]) -> Result<Self>;
  pub const SNAPSHOT_MAGIC_V2: &[u8; 5] = b"WXSC2";
  ```
- **对抗测试**：错口令/篡改/旧格式/WXSC1 文件四类拒绝；正确口令跨进程恢复。
- **参考（可搬）**：`refs/Signal-Desktop/app/main.main.ts:1731` 起的 sqlcipher key 生成与
  持久化决策树（"没找到密钥→生成新钥"的分支顺序照搬）；`crates/platform/src/security.rs`
  的 `hash_argon2id`（PHC 格式现成实现）。

#### T-A3 core unwrap 清理（#53 子集）
- **范围**：`crates/core/src/outbox.rs` 及 core 其余非测试 `unwrap/expect`。规则：请求/
  投递路径换 `?` + 语义化错误；**启动期配置加载允许保留 expect**（合同 0.1.3 原文）。
- **验收**：`grep -rn "unwrap()\|expect(" crates/core/src --include="*.rs" | grep -v test`
  计数较基线下降且逐处有说明（PR 表格：位置→理由）。

---

### 线 B：风控线（risk + limits + risk_mw）

#### T-B1 风控多节点化 + 收编 limits（#61 L-2）
- **目标**：`RiskEngine` 抽出 `RiskStore` trait（文档里早已预留："接口就是将来
  `RiskStore` trait 的形状"——`crates/risk/src/lib.rs` 模块头）；进程内实现保持默认；
  新增 `redis` feature 的 GCRA 实现（多节点共享 Redis 状态）。
- **GCRA 可搬实现**：`refs/zulip/zerver/lib/rate_limiter.py` + 同目录 `.lua` 脚本
  （TAT 单键、原子判定+推进一体）——按原语境重写为 Rust+redis-rs（script 调用），**注意
  保留引擎现有"时间由调用方传入"的可测性设计**（Zulip 的 `_testing_clock` 与之同构，
  Redis 路径用 `TIME` 命令时须留注入点）。
- **limits 收编**：`crates/limits` 保留 crate 名（core/client 依赖它，改名会污染所有人
  的 Cargo.toml——见 §3 仲裁规则），在其 `lib.rs` 文档头声明"常量库 + 阈值配置"定位，
  把 `RiskConfig` 默认值迁到此处统一管理（B 线内部移动，无跨线冲突）。
- **验收**：Redis 路径的并发双引擎一致测试（两个引擎实例共享一个 redis：同键计数不超
  阈值）；`cargo test -p wxlike-risk -p wxlike-platform` 全绿。
- **对抗测试**：双进程同时打满同一 (caller,target) 键 → 恰好在全局阈值处 429，两进程
  各自窗口不重复计数。

#### T-B2 风控映射扩展（#32 AR-5 余量）
- **目标**：`crates/platform/src/risk_mw.rs` 的 `action_for` 映射表扩展到
  groups（建群/拉人/踢人 POST）、communities（发帖）、live（开播）、call（信令 POST）
  四个面；risk 引擎按需加 Action（B 线内部自洽，无跨线冲突）。
- **注意**：**先读这四个模块的 routes()**，映射只声明"写入口"（POST/PUT/DELETE 中有
  副作用的）；像 live 的推流回调（video 回调带 ingest token）不走用户风控，**不要**
  把回调面也闸进去（会打断直播）。
- **验收**：沿用 `write_gates.rs` 既有模式每面 2 条（轰炸 429 + 正常用量不受影响）；
  `mapped_actions()` 表与 `action_for` 双向单测保持一致。
- **参考**：本轮已落的 `risk_mw.rs` 自身 + `#29` 关闭说明（设计先例）。

---

### 线 C：传输线（net / dpage / mq）

#### T-C1 SendGate 增加 Sleep 降级 + 限流指标标签
- **目标**：`crates/net/src/policy.rs` 的 `SendGate` 增加 `Sleep { delay_ms }` 变体；
  mq_hub 在投递前按 delay 退避（tokio sleep，**有上限**，超过上限转 Reject——照搬
  Synapse 的 sleep/reject 双闸思想）。`crates/net/src/metrics.rs` 的限流指标加
  `rate_limiter_name` 标签（照搬 `synapse_rate_limit_reject` 的标签设计）。
- **接口契约 AC-2**：`SendGate` 是 pub enum，加变体 = 全部实现者要补 match——PR 里
  必须列出仓库内全部 `impl DeliveryPolicy` 的位置并逐一适配（grep `impl DeliveryPolicy`）。
- **参考（可搬）**：`refs/synapse/synapse/util/ratelimitutils.py`（sliding window +
  `should_sleep`/`should_reject` 双闸 + `retry_after_ms` 计算）、`refs/synapse/synapse/
  config/ratelimiting.py:89-96`（rc_message 参数形状）、`refs/synapse/synapse/api/
  errors.py:602`（LimitExceededError 默认 429）。

#### T-C2 dpage 包 id 冲突检测（#56）
- **目标**：`crates/dpage/src/lib.rs` `next_id` 的 FNV-1a 目录名派生 → 冲突检测：
  装载时 id 已存在则报错并要求显式 id（或改用内容摘要 id，二选一，PR 说明取舍）。
- **验收**：同名目录哈希碰撞的回归测试。

#### T-C3 net unwrap 清理（#53 子集）
- **范围**：`crates/net/src/offline.rs`、`mq_hub.rs` 非 panic 路径的 unwrap/expect。
  mq_hub 的锁中毒 `unwrap_or_else(PoisonError::into_inner)` 属既有惯例**不算**违规。
- **验收**：同 T-A3 格式。

---

### 线 D：桌面端（ui/src-tauri + ui/src）

**共同上下文**：所有卡只消费 Rust 命令与 HTTP 契约，**禁止**把业务逻辑写进 TS
（合同 0.1.1 铁律）。对接 A 线的 T-A2 必须等接口 PR 合并（契约 AC-1）。

#### T-D1 TURN 短期凭据对接（#31 客户端半边）
- **目标**：`ui/src-tauri/src/callrtc.rs` 删除 `TurnConfig::from_env()` 的静态凭据读取，
  改为启动/到期前向 `GET /api/turn/credentials` 取（Bearer 登录态）；到期前 5 分钟
  刷新；凭据只在内存，**不落盘不进日志**。
- **契约**（服务端已上线，`server/src/lib.rs` `turn_routes`）：响应
  `{username, password, ttl_seconds, uris}`；密码 = `base64(HMAC-SHA1(secret, username))`，
  客户端**原样透传**给 TURN 服务器，不做任何本地验证。
- **验收**：集成测试（起本地 server + 配 `WXLIKE_TURN_SECRET`）取凭据成功、过期刷新、
  未配 secret 时得到 404 并走无 TURN 降级。
- **参考**：`spreed/docs/TURN.md`（客户端使用 static-auth-secret 凭据的语义）、
  服务端纯函数 `turn_rest_credentials` 的测试用例（响应形状的唯一权威）。

#### T-D2 安全码/设备验证 UI（#25 遗留）
- **目标**：会话详情页展示对方 `identity_signing` 指纹（`Device::safety_number()` 的
  core API 已存在，需经由 client/ffi 暴露——**注意**：`crates/client|ffi` 是 Z 的地盘，
  所以 D2 的 Rust 半边（把 safety_number 加进 client 命令面）**划入 T-Z2 的清单**，
  D2 只做 TS/UI 渲染与比对交互；两边以 `docs/api.md` 新条目为契约。
- **交互照搬**：`refs/element-web` 的设备验证流程（Emoji 比对/"标记为已验证"状态机）——
  本地镜像可直接对照 `src/verification` 相关组件。
- **验收**：两台实例互验一致/不一致两条路径的 UI 测试。

#### T-D3 秘密聊天客户端闭环（#15 客户端半边 + #48 UI 半边）
- **目标**：①设置页（开关+TTL 档位选择，对 `PUT /api/secret/{conv}`）；②TTL 本地清除
  （落库消息按 `sent_at + ttl` 过期清理的定时器）；③口令解锁 UI（对接 T-A2 契约 AC-1：
  首次设口令 / 解锁 / 错口令与损坏文件分别提示）。
- **前置**：T-A2 接口 PR 已合并；服务端 TTL 语义见 `secret_enforcement` 测试与
  `#30` 关闭说明（服务端只兜底补发与检索，**本地清除是客户端义务**——这正是 #15 的
  剩余缺口）。
- **参考（可搬交互）**：`refs/Signal-Android` disappearing messages 的设置页信息架构
  （会话级覆盖全局级、倒计时档位）；错口令提示文案参照 T-A2 的双错误变体。

---

### 线 E：通话线（sfu / rtc）

#### T-E1 SFU join 鉴权 + 房间管控（#58，接线后即 P0）
- **目标**：`crates/sfu/src/lib.rs` `join(peer, offer, video)` 补三道闸：①调用者身份
  （Bearer → platform 会话）；②房间 membership（走 `crates/platform` 既有 pub API，
  **只调用不修改**）；③房间人数上限 + "用他人 id 踢人"的防御（join 与已有 peer 冲突时
  拒绝而非顶替）。
- **参考（可搬）**：`refs/spreed/lib/` 房间参与者管理（join/leave/force-mute 的状态机
  与权限判断）；`refs/WuKongIM/pkg/cluster` 的节点注册鉴权形制。
- **验收**：非成员 404（无存在性探测）、超员拒绝、重名 peer 冲突拒绝——三条对抗测试；
  `cargo test -p wxlike-sfu` 全绿。

#### T-E2 STUN 属性 pad4 修正（#59）
- **目标**：`crates/rtc` STUN 属性编码 `alen % 4` → 正确的 4 字节对齐 padding
  （RFC 5389 §15：属性 32bit 对齐，padding 不计入 length）。
- **参考**：RFC 5389 原文 + 仓库内 str0m 依赖的 STUN 实现（`cargo tree` 找到后对照其
  编码函数——str0m 是 audited 实现，**照它的语义重写，不要 vendor**）。
- **验收**：奇数长度属性编码回环测试（encode→decode 恢复原值 + wire 长度 = 4 对齐）。

---

### 线 F：治理线（CI / 元数据 / 门禁）

#### T-F1 CI 增加 cargo audit + 覆盖率闸门（#61 L-3）
- **目标**：`.github/workflows/ci.yml`：①新增 `cargo audit` job（周跑 + PR 跑，失败
  阻断，`deny.toml` 已有许可闸与之互补）；②`cargo llvm-cov` 覆盖率 job——**先非阻断**
  （报告上传 artifact + PR comment），运行两周后再设阈值（本单写明：阈值由集成方
  Wave 3 依据实测基线定，不许拍脑袋）。
- **交付物（本单特别要求）**：#53 防回潮脚本 `scripts/count_unwraps.py`：统计非测试
  unwrap/expect 计数并输出 diff——CI 挂非阻断徽章，计数回潮即在 PR 评论可见。
- **参考**：现有 `deny.toml`（许可/advisory 闸已在此）、`ci.yml` 既有五关的 job 骨架。

#### T-F2 许可元数据统一（#55）
- **目标**：GitHub 仓库描述、README 徽章、`Cargo.toml workspace.package` 三处对齐为
  AGPL-3.0-only（`LICENSE` 已正确）。这是纯元数据 PR，**5 分钟内可完成**，Wave 1 优先
  交掉。

---

### 线 Z：迁移收尾线（admin / client / ffi / video —— 既有在途工作的正式化）

**共同上下文**：ureq 2→3 迁移已并入 main（#33），这四个 crate 是其属地。**其他线
禁止进入这四个目录**，防止踩在途工作。

#### T-Z1 admin upstream 3xx 语义修正（#60）
- **目标**：`crates/admin/src/upstream.rs` 迁移后手工判 `(400..600)`，3xx（重定向）
  会落入"成功"分支。决策：admin 面不应跟随重定向（凭证可能被转发到 Location 指向的
  第三方——这是 SSRF/凭据外泄面）——配置 ureq 3 **不跟随重定向**，4xx/5xx 按错误分类，
  3xx 显式报错。
- **验收**：3xx/4xx/5xx/2xx 四类状态的单测表。

#### T-Z2 迁移四 crate 的 unwrap 清理 + safety_number 命令面（#53 子集 + D2 的 Rust 半边）
- **范围**：`crates/admin/src/read.rs`、`crates/client/src`、`crates/ffi/src`、
  `crates/video/src` 请求路径 unwrap 清理（规则同 T-A3）。
- **附加**：把 `Device::safety_number()` 经 client 命令面/ffi 暴露给 UI（T-D2 的
  Rust 半边，契约以 `docs/api.md` 新条目为准，与 D 线对齐后实现）。

---

## 6. 集成与完美平衡（Wave 3，仅集成方执行）

1. **合并顺序**：按 Wave 1 → 2 → 3；同线内 PR 串行（A2 在 A3 之后），跨线任意顺序。
2. **冲突仲裁**：两线都声称需要冻结文件（`server/src/lib.rs` 等）时，集成方在 Wave 3
   统一应用 patch 片段；期间任何人改了冻结文件，rebase 责任自负。
3. **统一平衡**：Cargo.lock 再生成 → workspace clippy `-D warnings` → 三平台测试 →
   fmt → `cargo doc --no-deps` → `scripts/count_unwraps.py` 基线报告 → 对抗黑盒集回归。
4. **完成定义**：本单全部任务合入后，open issue 应只剩：#15（客户端半边随 T-D3 关闭）、
   #23（总跟踪）、#31（随 T-D1 关闭）、#34（审计跟踪归档）。#32/#47/#48/#53/#55/#56/
   #57/#58/#59/#60/#61 应全部可关。
5. **进度跟踪**：每线在对应 issue 下报告"领卡→接口 PR→实现 PR→对抗测试结果"四步；
   #23 挂本单链接作为索引。

## 7. 规模与人力建议

| 线 | 卡数 | 预估人日 | 适合的人 |
|---|---|---|---|
| A | 3 | 4–5 | 有密码学工程经验（接口卡 A2 是全单质量枢纽） |
| B | 2 | 4–5 | 熟悉 Redis/Lua 与限流语义 |
| C | 3 | 3–4 | 熟悉 tokio/异步投递路径 |
| D | 3 | 5–6 | Tauri/TS UI + 一点 Rust |
| E | 2 | 3–4 | WebRTC/STUN 经验 |
| F | 2 | 1–2 | DevOps（可兼职） |
| Z | 2 | 2–3 | 原迁移作者（上下文在手上） |
| 集成 | — | 2 | 项目所有者或指定架构师 |
| **合计** | **17 卡** | **~24–29 人日** | 6–7 人并行，2–3 周内收口 |
