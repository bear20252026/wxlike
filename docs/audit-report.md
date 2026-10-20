# wxlike 已完成项审计报告

> 日期：2026-10-19 · 审计基线：`main @ 4d1d4a0`（Merge branch 'feat/p3-mirror'）
> 方法：三路并行全量阅读（docs 语料 14 份 + 16 个 crate 全部源码 + server/ui/deploy/scripts）＋ 关键声明逐条人工复核 ＋ 本机实测（`cargo test` / `cargo clippy` / `cargo fmt` / git 取证）
> 视角：任务书 §0.1 架构铁律——**一切业务逻辑必须在 Rust**，前端 TS 只许 UI 渲染与调 Rust command
> 前置输入：微信多平台源码恢复（`D:\mimo\test\FINAL_REPORT.md`，四样本全解，Java 32 万 / smali 43 万 / Ghidra C 93 MB / WMPF 8162 模块）

**审计期间基线前移说明**：本次审计开始时 HEAD 为 `a1a9f15`（mailbox over HTTP）。审计过程中另一开发会话将 P3 步骤 1（direct/group 镜像进信箱）以 `c2ff005` 提交并合并为 `4d1d4a0`（2026-10-19 07:40）。本报告以 `4d1d4a0` 为准；实测（test/clippy/fmt）在提交后重跑，结果覆盖该增量。

---

## 一、实测结论（DoD 三关 + 桩清点）

| 检查 | 命令 | 结果 |
|---|---|---|
| 测试 | `cargo test --workspace` | **94 套件，517 通过 / 0 失败 / 4 ignored**，退出码 0 |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | 通过（0 warning） |
| 格式 | `cargo fmt --check` | 通过 |
| 桩 | `grep -r "todo!\|unimplemented!"` crates/server/ui src | **0 处** |
| unsafe | crates + server src | **0 处** |

- 4 个 ignored 为按约定门控的真外部依赖项（TURN 真服务器、ffmpeg 等，需 `--ignored` 显式跑）。
- ui 为独立 workspace（根 Cargo.toml `exclude`），另有 16 项 Tauri 侧测试（ui_commands 12 + p2_messages 4），不在本次 `--workspace` 实测范围内，属报告值。
- 合同第五条测试下限合计 ≥100：workspace 实测 517 + ui 16 ≈ **533**，远超下限；且绝大多数断言**行为**（乱序恢复、守恒、伪造拒绝、真 DTLS 回环、真 SIGTERM），非冒烟。

## 二、源码恢复 → wxlike：成熟经验的迁移映射

源码恢复工程（4 样本、8 条技术路线、9 份阶段报告）沉淀的方法论，与 wxlike 现行实践的对照：

| 源码恢复的成熟经验 | wxlike 对应实践 | 评价 |
|---|---|---|
| 分阶段报告 + 每阶段可量化产物（RESTORE/ENHANCE/PHASE3/4） | p3-mailbox / p4-report / p5-report + docs/bench 四份基准 | ✅ 已制度化 |
| 映射表是一等公民（Tiny v2 映射、99.8% 落地率） | THIRD-PARTY-NOTICES.md（来源/位置/许可）+ reference-mirror.md（41 仓库 Tier 分层） | ✅ 已制度化 |
| 多锚点交叉验证命名（import/@Metadata/日志标签/调用图） | borrow-first：开工先查 Tier-0 表 + 41 镜像；tweb vendoring 头注保留适配清单 | ✅ |
| **诚实记录不可逆边界**（R8 方法名/局部变量/注释不可恢复；C++ 仅到伪代码） | p5-report 显式声明"只交付第一增量，不做已全部完成表述"；各 crate 模块头写明"不做什么"（APNs 不做、真实支付不接、演示词表） | ✅ 罕见的诚实度 |
| 上游对齐验证（glass-easel 60/75 归属矩阵） | 与参考实现的行为对齐测试（str0m Event 匹配、FCM 真线格式本地回放） | ✅ |
| 出错即留档（maptest 实验、纠错报告） | **回滚闭环**：`45ddbc3` 回滚重复实现 → p3-mailbox.md 全文复盘 → 提炼"第 0 步：先 grep 本仓库"纪律 | ✅ 已转化为纪律 |

**可再借鉴的一点**：源码恢复用"落地率 99.8/99.9%"量化还原质量；wxlike 的 P3 接线同样可以量化（如"信箱覆盖率：3/3 属主类型已镜像，平行机制删除 0/1"），建议阶段报告统一带一张进度数字表。

## 三、Rust 主架构审计

### 3.1 分层与依赖方向（实测无环）

```
core（零内部依赖） ← message ← net ← push
core ←─────────────────── rtc ← sfu
risk / moderation / report / search / dpage / ledger（纯独立）
core+message+risk+moderation+report+rtc ← platform（业务数据面，26 模块）
core+net+mq+platform+message+...+video+search ← server（组合根/装配层）
```

- `net dev-deps platform`、`platform deps rtc` 不构成环（dev-only）。
- **依赖倒置是这个库最突出的工程习惯**：`EventSink`（platform 事件反向注入 hub，`platform/src/lib.rs:144`）、`Membership`、`DeliveryPolicy`（fail-open 三段裁决）、`OfflinePush`、`PushProvider`、`MediaStore` 全部 trait 注入——platform 可完全脱离 QUIC 单测。
- 传输与密码学选型全部"借审计过的轮子"：vodozemac 0.11（Olm/Megolm，core/Cargo.toml 注释明示不用手搓 ratchet）、quinn 0.11 + rustls（QUIC+TLS1.3+ALPN `wxlike/1`）、str0m 0.9（sans-I/O WebRTC，任务书禁 libwebrtc）、Tantivy 0.22（ngram 解 CJK）、argon2/totp-rs（P4）。自研部分集中在**编排层**（128 条乱序窗口、重放拒绝、pre-key 补充、hub 裁决、信箱语义）——边界划得对。

### 3.2 与"逻辑必须 Rust"铁律的偏离点（3 处，均有改进路径）

1. **`server/src/lib.rs`（734 行）承载两个带鉴权的业务端点** `/api/history`、`/api/search/messages` + `authorize_conv` 鉴权（server/src/lib.rs:381-403）——业务逻辑长在 crate 体系之外。应下沉 platform。
2. **`ui/src-tauri/src/lib.rs`（2080 行）是整套手写 HTTP API 客户端**（register/friends/wallet/media… 约 70 个 command）——客户端协议层与 crate 零复用，等于协议实现存在两份（hub 的 Rust 端 + Tauri 端 ureq）。建议抽 `wxlike-client` crate 供 Tauri 与未来 Flutter/UniFFI 共用。
3. `core/src/crypto.rs` 静态-静态 scaffold 与 `e2ee.rs` 并存的双轨遗留（威胁模型已计划 1.0 移除，回退路径打 warn 不静默——处理正确，属有台账的债）。

前端侧铁律执行良好：ui 仅依赖 jotai，62 处 `invoke` 封装（ui/src/api.ts），**0 处直接 HTTP/QUIC**；通话引擎走专用 OS 线程（callrtc.rs:1-25）避免 tokio 节拍拖歪。

## 四、已完成项核对（计划 vs 实证）

### 4.1 按阶段

| 阶段 | 计划要点 | 实证状态 | 判定 |
|---|---|---|---|
| P0 决策锁定 | ADR-1~24 + 九不变式 + Q1-Q8 | master-blueprint 第十二部分全部拍板落档 | ✅ |
| P1 五件套+E2EE 接线 | ≥10 测试 | 转发/合并转发/收藏/typing/搜索 + 秘密聊天（ui_commands 16 项覆盖）；p2_regression 留痕 | ✅ 通过 |
| P2 真机+富消息 | ≥8 测试 | 语音/文件/贴纸/视频首帧/本地库/头像（p2_messages 4 项 + platform 213 项中相关面）；bench p2 回归 PASS | ✅ 通过 |
| P3 读扩散核心 | ≥47 测试；万人群信箱压测 | **刚闭环**：信箱库（message 23 测）+ HTTP 暴露（`4ceb59c`）+ direct/group 镜像（`c2ff005`，mailbox_mirror 2 项真 QUIC+HTTP）+ WS 网关多端 + 社区/频道/线程 + 拉黑/禁言/陌生人闸门（policy_integration 4 项）+ Tantivy + conv_owner 映射。**未完**：平行机制删除（core::Store 仍承担扇出与序列号，双写过渡态）、device_cursor 迁移、万人群**全链路**压测（现有证据=存储面 33k msg/s + 网关面 p99 9ms 拼合，非单报告） | 🔶 核心 80%，收尾未完 |
| P4 运营合规+账本+通话房间 | ≥18 测试 | 报告 58 项（三层治理 26 + 房间 13 + 对账 4 + 找回 5 + 秘密聊天 2 + 红包回写 3 + 2FA 6）；ledger 复式记账守恒深测；**风控 fail-open、挂起无 NACK 帧**为已声明边界 | ✅ 通过（遗留 5 项归 P5/P6，有台账） |
| P5 移动端+直播+小程序 | ≥15 测试 + 三端安装包 + 弹幕压测 + 小程序端到端 | **仅第一增量**（17 项）：直播控制面/RTMP 凭据/弹幕路由/video 阶梯/DPG 签名/权限矩阵/信令/媒体协商/TURN/mesh/服务端 SFU。⛔ Flutter 三端、Web 工程、直播媒体面、小程序 M1-M4、多语言、厂商推送均未开始 | 🔶 1/6 增量 |
| P6 发布工程 | ≥5 测试 + 部署演练 | 部分：launchd/systemd/Docker 三态部署 + SIGTERM 优雅关闭（真进程测试）+ `--check` 自检 + Prometheus /metrics。adminx、TLS 反代、FCM 文档、安装包未做 | 🔶 |

### 4.2 性能基线（合同 4.4，全部 PASS 留痕于 docs/bench/）

| 项 | 指标 | 门槛 | 结果 |
|---|---|---|---|
| wp13 hub | 10k 连接/1k msg/s/60s | p99<50ms，断连 0 | p99 **5ms**，断连 0 |
| wp04 群直投 | 200 人群×250 条 | release p99<100ms | **11ms**（并留档修掉的 hub 串行扇出 bug：336ms→11ms） |
| p2 回归 | 全量档 | 同上 | p99 9ms |
| p3 信箱写入 | 16 写者 20k 条 | 无洞无重 | 33,174 msg/s，0 洞 0 重 |

### 4.3 回滚事件闭环（本次审计重点核查）

`45ddbc3` 回滚 platform 层重复 read_cursor → p3-mailbox.md 复盘 → `4ceb59c` 以正确方式暴露 HTTP → `c2ff005` 完成镜像。**核查结论：平台层无平行游标残留**（platform/src/read.rs 头注释明示"不新建表、不新建游标"，仅做 conv_id→属主映射）；真正的平行机制只剩 `core::Store`（有台账，p3-mailbox 步骤 3 待办）。

## 五、问题清单（分级）

### P1 级（P6 扩展前必须处理）

1. **async 路径上的同步 SQLite**：platform 全部 SQL 在 axum handler 内同步执行（db.rs:1-6 只保证守卫不跨 await）；`SqliteOffline` async 方法内同步 rusqlite（net/offline.rs:123-159）；hub async 路径同步调 `Store`/`MessageStore`（mq_hub.rs）。单机内部测试可辩护（且注释自知），但与"10k 连接"目标组合时 SQL 会占满 tokio worker。**注意**：wp13 压测跑的是 memory MQ + 路由热路径，未含 SQL 真负载——报告口径已说明，但扩展前需补含 SQL 的全链路基线。建议：spawn_blocking 封装或专用 SQL 线程 + 通道。
2. **P3 双写过渡态**：Store 与信箱双份真相（撤回已联动，append 已镜像），但序列号/扇出仍在 Store。按 p3-mailbox 步骤 3/4 收尾前，多设备语义仍非"唯一真相"。有台账、有顺序，属**按计划的债**，非失控。

### P2 级（尽快）

3. **本机工作副本 CRLF 残留（实测比子审计更广）**：`deploy/linux/install.sh`（84 行 CR）、`deploy/macos/install.sh`（88）、`deploy/linux/wxlike-server.service`（31）、`ui/scripts/check-msgindex.sh`（57）在本工作副本均为 CRLF。`.gitattributes`（9c9af0）已强制 `*.sh`/`deploy/**` LF 且索引为 LF（fresh clone / CI 不受影响），但**从本机直接拷脚本到 Linux 会 bad interpreter**。修复：`git rm --cached` 后重新检出，或 `git add --renormalize .` + 重检出这四个文件。
4. `mq/memory.rs` 10 处 `lock().unwrap()`（锁中毒即 panic）；全库其余 Mutex 均用 `PoisonError::into_inner` 恢复——统一成后者。
5. `server/src/lib.rs` 的 history/search 业务端点与 `ui/src-tauri/src/lib.rs` 2080 行协议客户端（见 §3.2 偏离点 1/2）。
6. `mq_hub.rs:514` `is_disconnect` 靠错误字符串匹配——脆，换错误分类。
7. `platform/lib.rs:134` `live_ingest_secret` 默认 `"dev-insecure-live-secret"`（需 env 覆盖——部署文档应加粗强调；`--check` 已能打印配置）。
8. 风控 **fail-open**（查询失败放行）：已声明的取舍，但作为对抗项意味着"让 SQL 报错"即可绕过拦截。内部测试可接受；P6 建议加熔断计数 + 告警，而非改 fail-closed。

### P3 级（观察项）

9. `sfu` crate 只转发音频（`video` 字段 `#[allow(dead_code)]`）；`CallRegistry` 进程内、重启即散（lib.rs:204 自认）。
10. `ui/src-tauri/data/media/` 4 个内容寻址媒体文件连仓库提交（开发残留，应 gitignore）。
11. README 个别数字滞后（"ui_commands 7 项"实为 16 项——测试增长快于文档，方向无害）。
12. 无 git tag；本地 36 分支 vs 远端 3（建议按阶段打 tag：p1…p5-d1）。
13. 两套里程碑口径并存（WORK_BREAKDOWN §5 的 M1-M4 vs 合同第十条 P1-P6）——文档已注明以合同为准，建议在 WORK_BREAKDOWN 加一行删除线引用，避免新读者混淆。

## 六、借码与许可合规

- **实际复制（vendored）**：tweb（GPL-3.0）`slicedArray.ts`/`listenerSetter.ts` 两文件，头部保留原许可 + 适配清单（合规范本）；atm0s-media-server（MIT）SFU 出口码率分配**跨库重写**并登记。
- **设计移植未抄码**：box-im（MQ/跨节点路由）、OpenIM（五层同步）、唐僧叨叨（communities 表形）、Rocket.Chat（权限矩阵）、Zulip（topic 语义）、goim（推送聚合分层）——均登记于 THIRD-PARTY-NOTICES.md。
- 许可链 MIT→GPL-3.0（44e3d3a）→**AGPL-3.0**（90d5bc3）：与 Tier-0 五个 AGPL 参照 + tweb GPL 兼容，策略自洽。
- 合同 7.2 内部测试模式：全部来源可接纳，唯一持续义务 = THIRD-PARTY-NOTICES 记录（已满足）。**再激活条款提醒**：一旦对外发布/商用，待核清单（flare-core、cala、dtmrs 等十余项）需补核验。
- refs/ 41 仓库浅克隆与 reference-mirror.md 清单一致（P3/P4/P5 分期 + 有意暂缓名单 + 引入纪律三条）。**未发现任何未声明的代码复制**（crates 内无携带他项目许可头的 .rs 文件）。

## 七、审计结论

1. **真实度**：这不是贴皮 demo。16 个 crate 全部真实实现（0 桩、0 unsafe、517 测试实测全绿），密码学/传输/媒体栈全部站在审计过的库上，自研集中在编排与语义层；每个"没做什么"都写在注释和报告里——与源码恢复工程"诚实记录不可逆边界"的纪律同源。
2. **Rust 主架构**：分层干净、依赖倒置彻底、无环；3 处偏离（server 端点、Tauri 协议层、crypto 双轨）均有明确改进路径，铁律在前端侧严格执行。
3. **合同进度判定**：P1 ✅ / P2 ✅ / P4 ✅（58/18）；**P3 🔶**（核心接线随 `c2ff005` 刚闭环，收尾两步未完）；**P5 🔶**（第一增量 17/15 达标，主体四块未开始）；P6 部分（部署面完成，发布面未做）。
4. **最高优先下一步**（按序）：① P3 步骤 3/4 收尾（删平行机制 + device_cursor 迁移）→ 信箱成为唯一真相；② 抽 `wxlike-client` crate 消解 2080 行协议代码；③ SQLite async 化定型（含 SQL 的全链路基线补测）；④ 修本机 CRLF 四文件；⑤ P5 环境受限项（Flutter 工具链）寻找 CI 云构建替代路径。

> 审计证据可复现：`cargo test --workspace`（517/0/4）、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --check`、`git log --oneline`（112→114 commits）、`git ls-files --eol`（CRLF 残留清单）。

---

## 八、处理记录（按本报告修改，2026-10-19）

| 编号 | 意见 | 处理 |
|---|---|---|
| P2-3 | 工作副本 CRLF（4 文件） | ✅ 已 `rm` + 检出重写，`git ls-files --eol` 复查 `w/crlf` 为 0；**根因是 Windows 下用文本模式写文件**，后续编辑 `deploy/**`、`*.sh` 一律 `newline='
'` |
| P2-4 | `mq/memory.rs` 10 处 `lock().unwrap()` | ✅ 全部改为 `unwrap_or_else(PoisonError::into_inner)`，与全库一致 |
| P2-7 | 直播推流密钥默认值 | ✅ 装配层支持 `WXLIKE_LIVE_INGEST_SECRET` 覆盖，未设置时 `tracing::warn!`；`--check` 打印是否已设置；`deploy/README.md` 加粗告警 |
| P3-10 | `ui/src-tauri/data/media/` 残留 | ✅ 核查为**未跟踪**（`.gitignore:9` 已覆盖 `ui/src-tauri/data/`），报告该项为误报，无需改动 |
| P3-11 | README 测试数滞后 | ✅ `ui_commands 7 项` → `16 项` |
| P3-13 | 两套里程碑口径 | ✅ `WORK_BREAKDOWN` 的 M1–M4 表前加口径说明，指向蓝图 P1–P6 |
| P1-1 | async 路径同步 SQLite | ⬜ 未做：需要专用 SQL 线程 + 通道，属架构级改动，单独一轮 |
| P1-2 | P3 双写过渡态 | ⬜ 未做：`core::Store` 退化为归档是 p3-mailbox 步骤 3，需与扇出改造一起做 |
| P2-5 | server 端点下沉 / 抽 `wxlike-client` | 🔶 **第一刀已落**：新建 `crates/client`（`wxlike-client`），把 HTTP 传输 + 鉴权头 + 错误消息提取 + 上传/下载 + `direct_conv_id` 搬出 Tauri，`ui/src-tauri/src/lib.rs` 2080 → 1930 行，`Http` 由 re-export 提供（60 处调用点零改动）；新增领域方法 `register/login/me/conv_unread/mark_conv_read/conv_sync`，`conv_unread` 命令已改走它。剩：其余约 60 个业务命令与本地库编排仍在壳里 |
| P2-6 | `is_disconnect` 字符串匹配 | ⬜ 未做：需先确认 quinn 的错误分类 API |
| P2-8 | 风控 fail-open | ⬜ 未做：报告本身建议 P6 加熔断计数而非改 fail-closed |
| P3-9 | SFU 只转发音频 | ⬜ 已知边界（`video` 字段标注），等视频采集接入 |
| P3-12 | 无 git tag | ⬜ 未做：建议按阶段补 `p1…p5-d1`，需要与发布节奏一起定 |
