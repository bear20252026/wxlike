# wxlike 架构审计报告（第四轮）

> 审计人：首席软件架构审计 + 资深代码审查（自动化代理）
> 基线：main `62124c5`（2026-10-22）｜分支：`fix/audit-round4`（8 个 commit，17 文件，+994/−98）
> 交付：本报告 + [AUDIT_CHANGES.md](AUDIT_CHANGES.md) + GitHub issues [#25–#32](https://github.com/bear20252026/wxlike/issues)

---

## 1. 执行摘要

wxlike 是一个**完成度显著高于典型同阶段项目**的 Rust 跨平台 IM 骨架：25 个 crate 分层清晰
（core 加密 / mq 抽象 / net 传输 / platform 业务数据面 / server 组装），钱包用单事务原子写 +
CHECK 兜底，E2EE 建在 vodozemac（Matrix 同款）而非自研棘轮，测试带并发灵魂用例。这些是前三轮
审计（`docs/audit-report.md`、issue #3–#22）与本轮都复核确认的优点，不是客套。

但本轮从整体架构出发，确认了一个贯穿性的结论：

**系统的信任模型声明与实现之间存在系统性落差。`docs/threat-model.md` 明写 hub/platform
"零信任（能看、能改、能重放一切字节）"，而这个声明在四个架构边界上没有对应的机制支撑：**

1. **密钥分发边界（Critical）**——预密钥包无签名无验签，"零信任服务端"可以直接换掉受害者的
   身份密钥实施 MITM，用户侧无感知。E2EE 的全部正确性（vodozemac、重放拒绝、乱序窗口）
   建立在一个可以被服务端单方面攻破的密钥目录上。
2. **入口资源边界（High）**——铸币口靠一个环境变量裸奔、WS 在鉴权前不限帧、OTK 库存可被
   任意登录用户循环领空、Redis 口令写进对外可见的消费者名。共同根因：把"有损耗资源/凭据"
   当成了"普通配置或公开只读"。
3. **凭证静态存储边界（High）**——会话令牌明文入库（与口令的盐+慢哈希原则自相矛盾）；
   注册路径与文档声明的 argon2id 策略漂移（密码学策略没有单一事实来源）。
4. **不可信代码执行边界（High）**——dpage 解压无上限、JS 宿主无执行限制、`call_global` 用
   字符串拼源码再 eval。共同根因：宿主对"页包是不可信输入"这一立场没有贯彻到资源与执行面。

**本轮全部修复（8 commit）都归因到上述四条架构风险（AR-1…AR-4），零新增依赖**（签名复用
vodozemac 自带 Ed25519）。另有三条风险（AR-5 风控中间件化、AR-6 秘密聊天实时链路兜底、
AR-7 TURN 短期凭据）因涉及协议面/大重构，按"最小充分"原则**未在本轮实施**，已开 issue
附设计方案（#29、#30、#31），技术债打包在 #32。

**对架构决策的直接建议**：在 AR-1 的签名机制之上，下一步优先做 #29（风控中间件化）——
它是"漏覆盖是结构性"这一根因的根治；AR-6 决定秘密聊天能否宣称"服务端兜底"，应在 #15
验收前完成。

---

## 2. 审计范围、方法与证据来源

| 项 | 值 |
|---|---|
| 范围 | 全仓库：25 crate + `server` + `ui`（Tauri2，只读）+ `apps/mobile`（Flutter，只读） |
| 规模 | 229 个 `.rs` / 约 76.5k 行 Rust（`find … | wc -l`） |
| 基线 | main `62124c5`；工作区含他人在途的 ureq3 迁移（18 个未提交文件，属 issue #22，**本轮未触碰**） |
| 方法 | ①通读架构文档（WORK_BREAKDOWN / threat-model / master-blueprint / 前三轮审计与 issue #3–#23）建立系统模型 → ②按信任边界逐层读代码取证（file:line） → ③复核第三轮审计全部 Critical/High 发现（逐条在代码中回读，全部属实） → ④实施最小充分修复并逐 commit 验证 → ⑤组合态终验 |
| 工具链 | cargo 1.97.1（本机可用；第三轮审计者无工具链，本轮补齐了构建/测试证据） |
| 未覆盖 | 动态渗透/fuzz；Flutter 与 Tauri UI 的深度审查（仅抽查跨边界调用）；ureq3 迁移在途文件 |

**并行协作声明**：本轮审计期间有另一进程在同一工作树进行 ureq3 迁移（issue #22）并发生分支
切换，两次吞掉本轮未提交的改动（已在 commit 5826038 之前的重放中恢复并全部落盘）；本轮一次
H-3 提交曾误落到其新建分支 `feat/ureq3-migration`，已 cherry-pick 回 `fix/audit-round4` 并把
该分支指针还原到 main（`d4806d7` 成为悬空对象，可追回）。**建议：并行工作者约定独立 worktree。**

---

## 3. 系统架构现状（系统模型）

```
 Flutter(apps/mobile) ──FFI── wxlike-ffi ─┐
 Tauri2(ui) ──────────────────────────────┤
                                          ▼
                                     wxlike-client ──HTTP──► server(axum)
                                                                ├─ platform(API 面: 账户/好友/群/钱包/媒体/朋友圈/预密钥目录/风控/审计)
                                                                │    └─ SQLite（唯一业务数据面；EventSink 反向注入事件）
                                                                ├─ net hub（QUIC + WS 同一 wire 协议；多设备扇出/停车/去重/回执）
                                                                │    └─ mq 抽象（Memory / Redis Streams / NATS）+ 路由表
                                                                ├─ push（离线推送，payload 不含正文）
                                                                ├─ rtc/sfu（str0m WebRTC；TURN 静态凭据 ← AR-7）
                                                                └─ /metrics、/app 静态托管
 wxlike-core: Envelope + E2EE(vodozemac Olm/Megolm + 宿主三补丁) + SQLite Store
 wxlike-dpage+container: 动态页包(DPG1) + Boa JS 宿主（页包=不可信输入 ← AR-4）
 wxlike-risk: 进程内滑动窗口四态裁决（Register/Login/Send/Report + 本轮新增 ClaimKeys）
```

**关键数据流与事务边界**（复核确认，做得对的）：
- 钱包：转账/红包/领取各为**单事务**，原子 `UPDATE … WHERE balance >= ?` + CHECK 兜底，
  `money_supply()` 守恒有并发灵魂测试（100 并发转账分毫不差）。
- 预密钥目录：claim 弹出在单连接守卫下，两并发认领不重样（本轮改为 `[key, sig]` 对弹出）。
- 通讯录/流水/朋友圈三个分页游标方向各自的语义有测试钉住。
- platform 不依赖 net：`EventSink` 反转依赖，业务层可不起 QUIC 单测——分层质量高。

**模块边界评估**：crates 间无循环依赖（platform←server 单向；core 无业务依赖）；一处已知
双实现已收敛（`may_read_conv` 注释明说 server 改调 platform 的）。风险点集中在**横切关注点
的放置**（风控靠手工调用，AR-5）与**信任边界机制缺失**（AR-1…AR-4），不在分层结构本身。

---

## 4. 质量属性评估

| 属性 | 评级 | 依据（证据抽样） |
|---|---|---|
| 安全 | **C→B+**（本轮修复后） | 修复前：AR-1 Critical + 11 High 中 8 项本轮已闭合。遗留：AR-5 风控覆盖、AR-7 TURN、M-2 设备密钥同盘、M-1 弱 API 导出（#32） |
| 性能 | B | 钱包热路径单事务原子写；QUIC 帧有 8MiB 上限且先验长再分配；存在 `docs/bench` 与 `scripts/bench_*`。未做容量压测（研究阶段可接受）；argon2id 注册使平台测试套件从 ~3.5min → ~6min（真实代价，已如实测量） |
| 可靠性 | B− | MQ 有至少一次+回收巡检；红包 24h 扫描幂等。缺口：AR-6 实时链路 TTL 兜底缺失；M-7 请求路径 517 处 unwrap；M-11 路由表未接线影响多节点 |
| 可维护性 | B+ | crate 文档密度高、注释解释"为什么"；错误用 thiserror 分层；分页/游标语义有测试钉住。扣分：M-1 死代码导出、`limits` 名不副实、风控手工调用模式 |
| 可测试性 | A− | 平台层可纯进程内测（EventSink 反转）；并发/乱序/重放对抗用例真实；CI 三平台 + Android/Flutter/Tauri。缺口：无覆盖率门禁、无 fuzz（#32 L-3） |
| 可观测性 | B | /metrics（含 p99 直方图）、慢 SQL profile、审计表、风控计数。缺口：分布式追踪（单机研究阶段合理缺省） |
| 部署运维 | B− | compose/Dockerfile/部署文档齐；Redis 模式凭据泄漏已修（H-10）。缺口：无迁移回滚脚本演练；两条数据迁移（prekey 签名列、令牌哈希）均"旧行失效重发"口径，见 §7 |

---

## 5. 架构风险矩阵

| 风险ID | 层级 | 证据（file:line 为基线 62124c5 处） | 影响 | 概率 | 优先级 | 状态 |
|---|---|---|---|---|---|---|
| AR-1 | E2EE 信任边界 | core/e2ee.rs:137-145（无签名字段）、352-365（encrypt_dm 不验签）；platform/keys.rs:39-73（只查长度）；`grep identity_signing` 全仓库零验签 | 服务端可换身份密钥 MITM，静默解密一切 DM；威胁模型自述落空 | 高（任何能写 DB 的一方） | **Critical** | ✅ e40a9ec / #25 |
| AR-2 | 入口资源/凭据边界 | server/lib.rs:1139-1163（faucet 无 Authed）、492-505（WS 无 max_message_size）、579-581（URL 进消费者名）；platform/keys.rs:170-179（claim 零关系校验零限流） | 无限铸币；未认证 64MiB 帧；OTK 耗尽 DoS；Redis 口令泄漏运维面 | 高 | **High** | ✅ f53891d+7d21c72 / #26 |
| AR-3 | 凭证静态存储 | platform/db.rs:281-286（token 明文列）；users.rs:164（register 写 PBKDF2@100k，与 security.rs 声明的 argon2id 漂移）；auth.rs:20 | 库文件泄漏=全部登录态可用；新账户首登前处于弱哈希 | 中 | **High** | ✅ a14bdb6+bd2fcc1 / #27 |
| AR-4 | 不可信输入执行边界 | dpage/lib.rs:125-153（decode_all 无上限）；container/engine.rs:38-62（无 runtime limits；call_global 拼 JS 源码） | 解压炸弹 OOM；`while(true)` 冻结宿主；JS 注入 | 高 | **High** | ✅ cf8bd82 / #28 |
| AR-5 | 横切关注点放置 | recovery.rs:71-135 定义 risk_gate，全仓库仅 4 处调用；friends/media/groups/moments/wallet/communities 零风控 | 好友/媒体/朋友圈轰炸无约束；漏覆盖是结构性的 | 高（必然） | High | 📋 #29（附设计） |
| AR-6 | 策略一致性 | `grep -rn ttl net/message` 仅命中路由租约；secret_enforcement 只测 HTTP 面 | 秘密聊天"服务端兜底"在 QUIC/WS/MQ 链路为空 | 中 | Medium | 📋 #30（关联 #15） |
| AR-7 | 外部凭据管理 | rtc/ice.rs:45-70 静态凭据；ui/src-tauri/callrtc.rs:266-280 客户端读同一组 | TURN 被盗用为中继/反射源 | 中 | Medium | 📋 #31（附方案） |
| 技术债组 | 可维护性/可靠性 | M-1/M-2/M-5/M-6/M-7/M-11/M-12/L-2/L-3（明细见 #32 表格，均 file:line 取证） | 详见 #32 | 中低 | Medium/Low | 📋 #32 |

---

## 6. 详细发现（按风险ID归组，每条含修复方式）

### AR-1 / C-1 预密钥包无签名（→ e40a9ec）
证据见矩阵。修复采用 **Matrix 同款双层签名**（零新依赖，`vodozemac::Account::sign` 即
Matrix 的设备密钥签名操作）：
- `PreKeyBundle.signature`：Ed25519 对 `(user_id, identity)` 长度前缀规范化字节串的绑定签名
  ——正是反 MITM 的那一道；
- `one_time_key_signatures`：每把 OTK 单独签名。**为什么逐把签**：claim 弹出的是单键子集，
  批量签名会让认领结果不可验（这是本轮实现中发现并修正的设计点，子集验签有专项测试）；
- `encrypt_dm` 建会话前 fail-closed 验签（无签名/篡改/长度不匹配 ⇒ `UnverifiedBundle`，
  不存在"回退信任服务端"的分支，与 `encrypt_dm_existing` 的 fail-closed 原则同构）；
- `publish_keys` 拒绝非曲线点与无签名 bundle（目录卫生——防"发布垃圾键让用户不可达"投毒）；
- `Device::safety_number()`：SHA-256 分组 hex 安全码，供带外核对（TOFU 首连窗口的缓解，
  威胁模型 §3 已声明该需求但仓库无实现）。
- 迁移：`prekey_bundles` 补 `signature` 列；旧无签名 bundle 被客户端拒绝，属主重新 publish
  即恢复（研究/内部测试阶段的既定口径）。

### AR-2 入口边界四面（→ f53891d、7d21c72）
- **H-1 faucet**：必须 `x-admin-token`（复用 `admin_ok`）；未配 `WXLIKE_ADMIN_TOKEN` 时 faucet
  **不挂载**（沿用仓库"没配就是关"铁律，见 platform/lib.rs:129 注释）；单笔 ≤100 万分；fund
  流水本身即账本审计 + tracing 实时视图。
- **H-4 OTK 耗尽**：`may_claim_keys_of`（本人/好友/**同群**——同群非好友也要放行，因为群聊
  sender key 走 1:1 Olm 分发；陌生人一律 404，不做存在性探测，与 moments 陌生人策略同款）；
  risk 引擎新增 `Action::ClaimKeys`（键 `(caller,target)`，1h 窗口 Throttle=30/Deny=120；
  认领无 captcha 兑现面故只 Throttle/Deny；**成功才计数**，失败重试不吃配额）。
- **H-9 WS**：`max_message_size/max_frame_size = MAX_FRAME`（8MiB，QUIC 同款），未认证连接
  不再能推 64MiB 帧。
- **H-10 Redis**：消费者名 = URL 的 SHA-8 + pid，凭据不再出现在 XINFO/XPENDING/慢日志。

### AR-3 凭证存储（→ a14bdb6、bd2fcc1）
- **H-3 的架构归因**：不是"轮数不够大"这一单点，而是**密码学策略没有单一事实来源**——
  security.rs 文档写"新注册一律 argon2id"、登录有透明升级、恢复重置写 argon2id，唯独
  register 写 PBKDF2。修复让 register 直接 argon2id（PHC 串自带盐与参数），`DEFAULT_ROUNDS`
  100k→600k 仅作遗留行校验兜底（此类行首登仍会被 argon2id 重写）。
- **H-2**：token 列改存 `sha256(token)`（复用 core::msgmeta::sha256_hex，遵守仓库"不写第二份
  hash"的既有规则——server/lib.rs:408 注释）；六个读写点全部摘要比对，其中
  `revoke_other_sessions` 的保留判断若漏改会**误删当前会话**（本轮实现中真实发生并被
  `session_tokens_are_stored_as_digests` 测试拦下的缺陷，见 AUDIT_CHANGES CH-06 备注）。

### AR-4 执行边界（→ cf8bd82）
- **H-5**：`decode_capped` 64MiB 封顶流式解压，`take(MAX+1)` 探测溢出——溢出是**错误**而非
  静默截断（截断会让 bincode 报出误导性的反序列化错误）。200MiB 零填充炸弹回归测试。
- **H-7**：Boa `set_loop_iteration_limit(1_000_000)`（Boa 0.20 默认无上限）；50 万次正常循环
  通过的边界测试钉住额度不误伤。内存配额 Boa 0.20 未暴露——记入 #28 遗留。
- **M-3**：`call_global` 改为属性查找（`JsString` 键）+ `JsValue::from_json` 数据化传参，
  名字与参数都不再是代码。两条注入路径（恶意函数名、恶意字符串参数）各有回归测试。

---

## 7. 变更策略与实施记录

**策略**：只做归因到 AR-1…AR-4 的变更；每批 = 修复 + 对抗测试 + 分 crate 验证 + 立即 commit
（小步可回滚）；零新增依赖（Cargo.lock 未动，也避免与他人在途迁移冲突）；不触碰 ureq3 在途
文件与任何无关文件。

**实施顺序与验证记录**：

| 批次 | commit | 验证（当次运行） |
|---|---|---|
| 1 AR-1 | e40a9ec | core e2ee 25/25；platform keys 11/11 |
| 2 AR-2 | f53891d | server `cargo check` 0 错 0 警 |
| 3 AR-4 | cf8bd82 | dpage 5/5；container 5/5+4/4 |
| 4 AR-3/H-3 | a14bdb6 | platform 全套件绿（注意：该次运行因并行分支切换落在缺 AR-1 的树上，见 §2 声明；组合态在终验重跑） |
| 5 AR-2/H-4 | 7d21c72 | risk 7/7；platform keys 13/13 |
| 6 AR-3/H-2 | bd2fcc1 | platform api 17/17（含新摘要断言） |
| 7-8 测试补充 | ff4bef4、5826038 | risk 8/8（ff4bef4 曾提交失败测试，5826038 修正 check/record 语义——如实记录，未隐藏） |

**终验（组合态）**：`cargo test -p wxlike-core -p wxlike-platform -p wxlike-server -p wxlike-risk
-p wxlike-dpage -p wxlike-container -p wxlike-net -p wxlike-message` 与 workspace clippy 结果
见附录 A（以实际运行输出为准，本报告不预先填写结论）。

**明确不做的**（与理由）：AR-5 中间件化（涉及全部写接口的统一改造，值得独立 PR + 对抗集，
本轮先以设计草案 + 单点最尖锐项落地）；AR-6（协议面下沉 net 投递判定，影响两条链路语义，
需与 #15 验收清单对齐）；AR-7（要动 server API + 客户端取凭据协议 + coturn 配置三方）；
M-2 设备密钥（桌面端密钥派生方案需产品决策：口令派生 vs OS 钥匙串）；ureq3 在途文件（他人工作）。

---

## 8. 未解决风险与后续路线图

> **续批更新（同分支）**：路线图 1/2/3 已在续批落地——AR-5 中间件化（`c532810`，#29 已关）、
> AR-6 TTL 补发兜底（`2fe4a41`，#30 已关）、AR-7 服务端半边（`c541b82`，#31 保持开放等
> 客户端半边）；技术债组完成 M-5/M-6（`7f70382`）与 M-11（`87dd471`+`40ace05`，其中
> register_hub"零调用"的原表述已更正：启动接线在 net，缺的是停机注销）。明细见
> [AUDIT_CHANGES.md](AUDIT_CHANGES.md) CH-10…CH-14。#15 的"服务端兜底"实时链路缺口
> 自此闭合，#15 余下部分为客户端侧。

| 优先级 | 事项 | issue | 状态 |
|---|---|---|---|
| 1 | 风控中间件化（AR-5 根治） | #29 | ✅ 已修复（c532810） |
| 2 | 秘密聊天实时链路兜底 | #30 / #15 | ✅ 服务端兜底已闭环（2fe4a41）；#15 余下为客户端侧 |
| 3 | TURN 短期凭据 | #31 | ◐ 服务端半边已落（c541b82）；客户端半边开放 |
| 4 | 桌面端 safety_number UI + 身份变化告警 | #25 遗留 | 开放 |
| 5 | 技术债组 | #32 | ◐ M-5/M-6/M-11 已落；M-2/M-1/M-7/M-12/L-2/L-3 开放 |
| — | ureq3 迁移收尾 | #22 | 由在途作者完成（其 secret_enforcement 缺口已由其修复，server 套件复验全绿） |

---

## 附录 A：验证命令与输出摘要（实际运行，2026-10-22）

```text
分批验证（每批当次运行）
  cargo test -p wxlike-core --test e2ee          → 25 passed（含 6 条新对抗用例）
  cargo test -p wxlike-platform --test keys      → 13 passed（陌生人404 / 35轮耗尽撞429）
  cargo test -p wxlike-platform --test api       → 17 passed（含令牌摘要断言）
  cargo test -p wxlike-dpage                     → 5 passed（200MiB 炸弹被拒）
  cargo test -p wxlike-container                 → 5 + 4 passed（死循环抛错/注入不执行/边界不误伤）
  cargo test -p wxlike-risk                      → 8 passed（ClaimKeys 契约：只 Throttle/Deny、键隔离）

组合态终验（fix/audit-round4 全部 commit 之后）
  core+risk+dpage+container+message              → 123 passed / 0 failed
  net                                            → 99 passed / 0 failed
  platform 全套件                                → 全绿（schema_snapshot 初跑拦下漏同步的加列，
                                                   2b56d1f 补快照后通过——机制按设计生效）
  server                                         → 全绿，除 secret_enforcement 2/3：归因为他人在途
                                                   ureq3 迁移的 post() 助手缺口（证据与修法已评论
                                                   issue #22），非本轮变更所致
  cargo clippy --workspace --all-targets         → 0 error / 0 warning
  cargo fmt --check                              → 通过
```

未背书范围：`wxlike-admin / client / ffi / video` 的测试文件属他人在途迁移（issue #22 的
18 个脏文件），其测试结果不纳入本轮验证结论。

## 附录 B：与前几轮审计的关系

- 第三轮（docs/security-audit-2026-10-22.md，基线同为 62124c5）的 C-1、H-1…H-10、M-3、M-7
  等发现**逐条在代码中复核属实**后由本轮修复或转入 #29–#32；其"做得好"清单（vodozemac 三补丁、
  钱包事务、常量时间比较等）本轮再次抽样复核仍然成立。
- 本轮增量：①把单点发现**归因到四条架构风险**并按风险组织实施（含第三轮未展开的
  "批量 vs 逐把签名"这一实现层设计决策）；②发现 H-3 的真正形态是**文档-实现漂移**
  （register 漏切 argon2id），而非单纯轮数问题；③发现并修复 revoke_other_sessions
  在令牌哈希迁移中的自伤缺陷；④补齐第三轮缺失的构建/测试证据（其时无 cargo 工具链）。
