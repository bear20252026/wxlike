# 软件开发外包合同（wxlike 类微信即时通讯平台）

> 文档版本 2026-10-18 v1.1。**本文件为外包合同草案模板，签署前须经法律专业人员审阅修订。**
>
> **修订记录**：v1.1（2026-10-18）—— 第 8.1 条平台开源许可由 **GPL-3.0 改为 AGPL-3.0**
> （甲方决定；理由：Tier 0 参照项目中 5 个为 AGPL-3.0，统一许可后可直接复用其代码，
> 且本平台属网络服务，AGPL 的源码提供义务与项目定位一致）。第七条参考表与许可义务不变。
> 配套技术文档作为合同附件，与本正文具有同等效力（效力顺序见第二条）。

**合同编号**：WXLIKE-DEV-2026-001
**甲方（委托方）**：________________________（统一社会信用代码：________）
**乙方（受托方）**：________________________（统一社会信用代码：________）
**签署日期**：2026 年 __ 月 __ 日

鉴于甲方拟建设"wxlike"即时通讯平台（以下简称"本平台"），乙方具备 Rust 后端与跨平台客户端开发能力，双方根据《中华人民共和国民法典》《计算机软件保护条例》及相关法律法规，经平等协商达成如下协议。

---

## 第一条 项目标的与总体要求

1.1 乙方按照本合同及其附件所规定的产品功能、技术架构、验收标准与里程碑，完成本平台的设计、开发、测试、交付与技术文档编写。

1.2 **产品定位**：类微信/类 Discord 的即时通讯平台——支持单聊、群组、万人社区（频道/话题）、音视频通话、直播频道、短视频点播、虚拟钱包与红包、自研小程序容器、自建支付账本、信息流广告，覆盖七端客户端（Windows/macOS/Linux/iOS/Android/鸿蒙/Web）与 Rust 后端。

1.3 **总体技术路线（已锁定，乙方不得单方变更）**：
- Rust 后端：Tokio + Axum，模块化单体起步（按技术树划 crate 边界），按负载分阶段拆分独立服务；
- 实时传输：桌面/移动 QUIC（TOFU 证书钉扎），Web 端 WebSocket（Transport trait 抽象，双实现）；
- 消息架构：三支柱（单聊 direct / 群组 group / 频道 channel）**统一读扩散信箱**（owner_type 区分、序列号独立、游标同步）；
- 数据：PostgreSQL（核心）+ Redis（缓存/游标/原子操作）+ 对象存储（S3 兼容）+ NATS（实时通道）+ Meilisearch/Tantivy（搜索）；
- 端到端加密：vodozemac（Olm/Megolm），范围=1:1 秘密聊天（可选模式）；
- 移动端：Flutter 统一基座（鸿蒙走 Flutter-OH + ArkTS 桥接），Rust core 经 UniFFI/cargo-ndk 出绑定；
- 桌面端：Tauri 2；Web：独立前端工程；支付：独立 payment-service + 复式记账账本内核（cala-ledger 模式），渠道适配器可插拔；
- 小程序：自研运行时（Rong 逻辑层 + Wry/LingXia 视图层 + Wasmtime 沙箱 + publish/invoke Bridge），非微信兼容路线。

1.4 **项目性质**：本项目为**甲方内部测试项目**，不对外发布、不分发、不商用。据此，本合同第七条的许可合规义务按"内部测试模式"执行（全部代码来源均可接纳，详见 7.2）；并含**再激活条款**：若未来对外发布、开源或商业化，许可合规义务自动恢复，届时须补齐第三方许可核验（第七条 7.2/7.3）。

1.5 **合规边界**：本项目为虚拟记账与研究定位（详见任务书 WP-09 边界声明）；真实支付收单、内容审核资质、电信业务经营许可等属甲方运营责任，乙方负责为实现上述合规要求预留并实现技术能力（第九条、第五条 P4/P6）。

---

## 第二条 定义与文档层级

2.1 **附件（与本合同正文具同等效力，冲突时效力顺序如下）**：
| 序号 | 文档 | 内容 |
|---|---|---|
| 附件一 | `docs/WORK_BREAKDOWN.md` | 原始任务书（WP-01~13 工作包、DoD、验收流程） |
| 附件二 | `docs/master-blueprint.md` | 总蓝图（ADR-1~24 架构决策、产品功能树、技术架构树、九大不变式、路线表详细版） |
| 附件三 | `docs/miniprogram-blueprint.md` | 自研小程序运行时详细方案（ADR-14） |
| 附件四 | `docs/feature-gap-matrix.md` + `docs/feature-comparison.md` | 功能差距对照（vs 微信/QQ/Telegram/WhatsApp） |
| 附件五 | `docs/threat-model.md` | 威胁模型 |
| 附件六 | 本合同第三条（功能需求清单）、第五条（验收标准明细）、第七条（参考项目与许可表） |

2.2 **效力顺序**：本合同正文 > 第三/五/七条 > 附件二 > 附件三 > 附件一 > 附件四/五。附件一任务书中与附件二总蓝图冲突的条款（如群扩散模型由写扩散修订为三支柱统一读扩散），以附件二为准。

2.3 **已确认的架构决策**：甲方已于 2026-10-16 确认总蓝图第十二部分 Q1–Q8 全部按受托方建议方案执行（三支柱统一读扩散信箱、新建 message-service、渠道开放账本自建、E2EE 秘密聊天保留、群接龙先于群收款、video-service 最小版提前至 P2、模块化单体、推送厂商账号即日启动）。

---

## 第三条 交付范围：功能需求清单（产品功能树全量）

> 以下为甲方应付验收的完整功能范围。标注【已交付】的项已含前期交付并纳入本期验收回归范围；标注阶段编号的为本合同期内交付。

### 3.1 账号与身份
- 用户名+密码注册/登录/登出【已交付】；可选邮箱验证（找回/防滥用）【P4】；恢复码【P4】
- 多设备同时登录与设备管理（设备列表、远程下线）【P3】；2FA（TOTP）【P4】
- 个人资料（头像接媒体上传、昵称、简介、状态）【头像 P2】；二维码名片（承载 username/user_id）【P5】
- 账号注销与数据导出【P6】；防批量注册（captcha+IP/设备限流）【P4】

### 3.2 社交关系（支柱 A：好友+单聊）
- 好友申请/同意/拒绝/备注/删除/增量同步【已交付】；星标、标签、分组【P3】
- 拉黑/屏蔽【P3】；共同群聊/共同好友展示【P3】；好友推荐（recommendation-service）【P6+】
- **陌生人消息请求**：非好友首条消息进请求箱，接受/拒绝/屏蔽，可配置开关【P3】
- 单聊会话：全消息类型、引用/转发/撤回/编辑/搜索/置顶/标记未读、已读回执（可选）、正在输入、一键撤回本次全部【P1–P4 分项见第五条】

### 3.3 群组（支柱 B）
- 建群/邀请/踢人/退群/转让/解散【已交付】；成员上限可配置（默认 500）【已交付】
- 群公告（多媒体置顶）【P3】；禁言（全员/单人，定时解除）【P3】；入群审批+群二维码（invite_token 带过期）【P3】
- 群收款【P5】、群接龙【P4】、群文件【P5】；@所有人（owner/admin 限定）【P3】
- 群昵称（成员维度）【已交付】；折叠/保存到通讯录/免打扰【P3】

### 3.4 社区/频道/话题（支柱 C，万人规模）
- 社区 Community（member_limit 万人）：公告、社区级角色、成员管理（审批/禁言/踢出/封禁）、发现页入口【P3】
- 频道 Channel：四类型（文字/公告只读/语音/直播）；频道级权限；频道邀请链接；频道分组；频道级角色（社群管理员/频道管理员分离）【P3】
- 话题 Thread：对任意频道消息发起；回复聚合不占主序列；订阅/取消订阅；话题内成员列表【P3】

### 3.5 会话与消息
- 统一会话列表（三支柱聚合、最后消息排序、未读数、免打扰/置顶/归档）【P3】
- 全局搜索（消息/群组/频道/社区/用户/话题；本地+云端；时间范围筛选）【P3】
- 消息类型注册表+三通道（normal/high_priority/ephemeral）【P3】
- 消息操作：撤回（含一键撤回本次全部与跨端状态同步）、编辑、置顶、标记未读、翻译（P5+）【撤回跨端 P3，其余 P4/P5】

### 3.6 消息类型
- 文本【已交付】；图片（缩略图/原图/断点续传）【已交付】；语音条（录制/波形/转文字占位）【P2】；视频消息（首帧缩略图+倍速）【P2】；文件（任意类型+断点续传）【P2】；贴纸/GIF【P2】；位置/名片【P5】
- 引用回复【已交付】；@提醒（含 @所有人）【已交付】；已读回执【协议已交付，UI P1】

### 3.7 音视频与直播
- 1:1 语音/视频通话（WebRTC，真机验收）【P2】；群通话房间（独立调度，静音/踢人/锁定/录制）【P4】
- 直播频道（频道类型之一）：RTMP ingest（OBS 兼容）、WebRTC/HLS 播放、连麦、弹幕（复用读扩散+分层推送）【P5】
- 屏幕共享（桌面原生采集；移动端录屏权限桥接）【P5】
- NAT 穿透：内置 STUN；TURN 为部署接口（甲方环境自备）

### 3.8 短视频点播
- video-service：上传→FFprobe 元数据→FFmpeg 多分辨率 HLS→缩略图→对象存储；最小版（单分辨率）【P2】，完整版【P5】
- 视频消息以结构化引用进会话（video_id/playlist_url/时长），二进制不经过消息网关【P2/P5】

### 3.9 朋友圈/动态
- 发布（文本+≤9 图）/好友圈读时判定/点赞/评论树/删除【已交付】；24h 动态、可见范围细分【P6+ 可选】

### 3.10 钱包与支付
- 虚拟钱包（转账/红包/流水，事务守恒）【已交付】；红包卡片状态变更回写【P4】
- 三账本账户体系（user_wallet/advertiser/platform_marketing/platform_revenue）+ 复式记账内核【P4】
- 充值渠道适配（渠道清单由甲方书面确认；微信虚拟支付可作为渠道适配器之一）、支付密码、对账任务【P4】
- 群收款（依赖 3.3 + 账本）【P5】；广告红包（ad-account → marketing → 用户钱包，两笔分录）【P6】

### 3.11 小程序（自研运行时，非微信兼容）
- 分五里程碑交付（详见附件三）：逻辑层跑通→视图 Bridge 闭环→多引擎多平台→Wasmtime 沙箱→与 IM 后端集成（包管理/签名/权限/支付转发）【P5 并行轨道】
- 聊天内小程序卡片（normal 通道）；host_bridge + im_bridge（权限白名单）

### 3.12 广告
- 信息流广告（ad_card 消息入频道时间线）、会话列表广告、小程序广告【P6】
- 广告主后台（活动管理/定向/eCPM 排序/频次控制/预算平滑/报表）【P6】
- 广告红包/任务（ad-account → marketing → 用户钱包）【P6】
- 广告标识合规（"广告"字样显著标识，符合《互联网广告管理办法》）【P6】

### 3.13 推送通知
- 离线推送框架+FCM【已交付】；通知决策/传输分离（notification-service + push-gateway）【P3】
- 通知聚合+多级免打扰（会话/频道/关键词）【P3】；APNs【P5】；国内厂商通道（华为/小米/OPPO/vivo）【P5】

### 3.14 安全与治理
- 审计日志（登录/发消息/文件下载/管理操作，按时间/用户/类型检索，留存≥6 个月）【P3 起步，P6 完整】
- 风控（注册/发送/登录三层实时拦截）【P4】；内容审核管线（多模态+先审后发/先发后审配置）【P4】
- 举报-审核-处置-反馈闭环【P4】；防泄密选配（水印/防截屏/防转发/会话锁定）【P5+ 选配】

### 3.15 后台管理与运营
- 管理后台（adminx：RBAC/MFA/审计检索/用户管理/封禁/工单/提现双人复核）【P6】
- 运营数据看板（DAU/MAU/消息量/频道活跃/留存/推送到达率/账本对账异常）【P6】

### 3.16 客户端（七端）
- Windows 桌面【已交付，P2 验收】；macOS/Linux【P2 验证】；Android/iOS（Flutter + UniFFI）【P5】；鸿蒙（Flutter-OH + ArkTS 桥接）【P5】；Web（独立工程）【P5】
- 全端共性：深色模式（三态）、消息列表虚拟化、本地消息库、多语言、无障碍（WCAG AA/200% 字号/读屏）、崩溃上报（Sentry）、自动更新【P2/P5/P6 分项】

### 3.17 运维与可观测
- /metrics + OpenTelemetry 追踪 + Loki 日志 + 配置中心（灰度/分桶）【P3/P6】
- 压测基准（保持 10k 连接 p99<50ms 达标回归）【每阶段回归】

---

## 第四条 技术要求（架构约束）

4.1 **架构决策**：乙方交付必须符合附件二总蓝图 ADR-1~24 全部决策与"九大锁定不变式"（不变式任何一条被违反即验收不通过）。核心约束摘录：
1. 三支柱共用读扩散信箱，`owner_type` 区分，不写三套投递逻辑；
2. 各 owner 序列号独立、单点原子生成；
3. 仅 normal 通道占用序列号进时间线；
4. 资金变动只经 ledger-service 复式记账接口，四类 account_type 命名空间隔离；
5. push-gateway 无通知决策逻辑；
6. 小程序仅经白名单调 im_bridge 受限子集；
7. 审计日志（PG）与运行日志（Loki）永不混存；
8. 全表 tenant_id 预留（当前不实现隔离）；
9. 陌生人单聊必经 message_requests 流程。

4.2 **工程纪律**：禁止 `unwrap()/expect()` 出现于非测试代码路径（expect 仅限启动期配置）；错误处理库用 thiserror/anyhow 分层；SQL 全参数化；依赖新增须在 PR 说明理由；**禁止引入闭源 IM SDK 与云厂商绑定 SDK**。

4.3 **完成定义（DoD，每个合并必过）**：`cargo build --workspace` 0 error；`cargo clippy --workspace --all-targets -- -D warnings` 0 warning；`cargo test --workspace` 全绿且本阶段新增测试 ≥ 第五条约定的数量下限；`cargo fmt --check` 无 diff；`cargo doc --no-deps` 无 broken link；前端 `tsc --noEmit` 通过。

4.4 **性能基线（回归门槛）**：单 hub ≥10,000 并发连接、1,000 msg/s 持续 60s，断连 0，投递 p99 < 50ms（release）；200 人群直投 p99 < 100ms（release）。每阶段合并回归一次，报告落 `docs/bench/`。

---

## 第五条 阶段划分、交付物与验收标准

> 工期合计约 16–26 周（按 Q7 模块化单体与并行度）。每阶段交付物含：代码（分支+PR）、测试、演示命令序列、基准报告（如适用）、文档更新。人日为估算上限，乙方自行调配人力。

### P1 体验补齐（2 周）
| 交付 | 验收标准（可执行） |
|---|---|
| E2EE 接入客户端消息路径（秘密聊天雏形+回退日志） | `cargo test -p wxlike-core --test e2ee` 全绿 + UI 秘密会话演示 |
| 消息操作五件套（转发/合并转发/收藏/正在输入/搜索协议） | 新增测试 ≥10；UI 可操作演示 |
| UI 历史消息加载（分页拉取） | UI 演示 + 集成测试 |
| 已读回执 UI | UI 演示 |

### P2 真机验收 + 客户端体验（2 周）
| 交付 | 验收标准 |
|---|---|
| WP-08 双机音视频验收（语音 ≥60s 不断流 + 视频连续） | 双机实录 + 指标输出（MOS 主观记录） |
| WP-11 桌面端真机演示（注册→加好友→单聊含图→建群→群聊→红包，无控制台报错） | 甲方现场或录屏验收 |
| WP-10 Redis 真机测试 | `REDIS_URL=... cargo test -p wxlike-mq --features redis -- --ignored` 全绿 |
| 语音条（波形+转文字占位）/文件消息/贴纸/视频首帧+倍速 | 新增测试 ≥8 + UI 演示 |
| video-service 最小版（单分辨率 HLS+缩略图） | 上传→播放闭环演示 |
| 消息列表虚拟化/本地消息库/头像接媒体/mac·Linux 构建验证 | 构建产物 + 演示 |

### P3 读扩散核心 + 多设备 + 社群层（6 周，关键路径）
| 交付 | 验收标准 |
|---|---|
| message-service（owner_type 三态信箱/序列号原子分配/类型注册表/三通道/撤回变更事件/批量撤回） | 新增测试 ≥15；万人群信箱写入压测报告 |
| sync-service（五层差分）/ read-service（游标+ZSET 未读；万人群回执默认关） | 新增测试 ≥8 |
| Transport trait + WS 网关 + 多设备多连接（ADR-4/8） | 多设备同时收发测试 ≥3 |
| 社区/频道四类型/频道权限/分组/频道级角色/话题订阅 | 新增测试 ≥12 |
| relation 增强（拉黑/message_requests/星标/标签/共同聚合） | 新增测试 ≥6 |
| 群管理补齐（公告/禁言/审批/邀请 token/慢速/@所有人） | 新增测试 ≥6 |
| 通知决策层+聚合+多级过滤 / NATS backend / 审计日志 / Meilisearch 接入 / tenant_id 建表 | 测试+检索演示 |

### P4 运营合规 + 资金账本 + 通话（4 周）
| 交付 | 验收标准 |
|---|---|
| 支付四层（ledger 四类 account_type/obix/dtmrs/ChannelConnector） | 账本守恒测试（100 并发转账分毫不差）+ 对账演示 |
| 风控/审核/举报三层（risk/moderation/report） | 管线演示 + 测试 ≥6 |
| 红包卡片变更回写/支付密码/对账任务/找回+captcha/2FA/秘密聊天/设备管理页 | 逐项演示 + 测试 ≥8 |
| 通话房间制+会议管控（静音/踢人/锁定/录制） | 房间演示 + 测试 ≥4 |

### P5 移动端 + 直播 + 小程序 + 点播（8 周，可与 P3/P4 部分并行）
| 交付 | 验收标准 |
|---|---|
| Flutter Android/iOS + 鸿蒙 Flutter-OH（Rust core UniFFI 绑定） | 三端安装包 + 演示 |
| 直播频道（Xiu RTMP ingest + WebRTC/HLS 播放 + 信令独立通道 + 弹幕） | 弹幕压测报告 + 演示 |
| 小程序 M1–M5（Rong 逻辑层→Bridge 闭环→多引擎→沙箱→后端集成） | 端到端演示："打开小程序→调 IM 发消息→支付卡片回传" |
| video-service 完整版（多分辨率）/ Web 独立工程 / 多语言 / 数据导出 / 国内厂商推送通道 | 逐项演示 |

### P6 发布工程 + 规模（4 周）
| 交付 | 验收标准 |
|---|---|
| 安装包（Tauri bundle 多平台）/ release CI / TLS 反代部署文档 / TURN 部署文档 | 甲方环境部署演练 |
| adminx 管理后台（RBAC/MFA/审计检索/工单/提现双人复核） | 后台演示 + 测试 ≥5 |
| 运营看板 / 审核完整闭环 / Bot+开放 API/Webhook / 存储加密 / 无障碍 / 多语言 | 逐项演示 |
| PG 迁移 / k8s 就绪 / 备份方案 / SLO 文档 | 文档+演练 |

5.x **每阶段通用验收门槛**：DoD（4.3）全绿；性能基线（4.4）回归达标；交付物三件套齐全（代码+测试+演示命令）；文档同步更新。**测试数量下限合计 ≥100 项新增**（P1≥10、P2≥8、P3≥47、P4≥18、P5≥15、P6≥5，可由乙方在阶段细则微调、甲方确认）。

---

## 第六条 验收流程

6.1 **三件套交付**：每个功能=代码+测试+演示命令，缺一不验收。
6.2 **DoD 机器强制**：CI（GitHub Actions）四关不绿不得合并；进度以 git 提交为准，口头汇报无效。
6.3 **对抗测试集**：甲方持有黑盒对抗测试集（针对断线、重连、重复消息、超长字段、并发、错误口令、越权 token、伪造撤回等边界），随里程碑更新、不提前公开。乙方自测全绿但对抗集不通过=本阶段验收不通过。
6.4 **验收期限**：每阶段交付后甲方 7 个工作日内完成验收并书面反馈；逾期未反馈视为通过（甲方主动放弃该权利的除外，需书面说明）。验收不通过的，乙方 5 个工作日内修复后重新提交。
6.5 **度量留痕**：涉及性能的阶段必须附基准脚本与机器配置说明，数字不达标即不通过。

---

## 第七条 参考项目与许可合规义务

7.1 乙方**允许并鼓励**参考下列开源项目（架构思想、代码、组件）。许可列仅作来源追溯记录；按 1.4 内部测试模式，**全部来源均可接纳**：

**Rust 生态（后端骨架/传输/搜索/账本）**
| 项目 | 链接 | 许可 |
|---|---|---|
| flare-core | https://github.com/flare-im/flare-core | 待核 |
| only-talk-rs | https://github.com/nicolastinger/only-talk-rs | 待核 |
| ISM | https://github.com/JrTimha/ISM | 待核 |
| Meilisearch | https://github.com/meilisearch/MeiliSearch | MIT |
| Tantivy | https://github.com/quickwit-oss/tantivy | MIT |
| cala-ledger | https://github.com/GaloyMoney/cala （docs.rs/cala-ledger） | 待核（Galoy 系通常 Apache-2.0） |
| ousia-ledger | https://github.com/TheOusia/ousia | 待核 |
| TigerBeetle / tb-rs | https://github.com/tigerbeetle/tigerbeetle · https://github.com/dsp/tb | 待核（通常 Apache-2.0） |
| dtmrs | https://github.com/jackwangfeng/dtmrs | 待核 |
| Hyperswitch | https://github.com/juspay/hyperswitch | Apache-2.0 |
| obix | https://github.com/GaloyMoney/obix | 待核 |
| iroh | https://github.com/n0-computer/iroh | Apache-2.0/MIT |
| adminx | https://crates.io/crates/adminx | 待核 |
| str0m | https://github.com/algesten/str0m | ISC/Apache-2.0（现役依赖） |

**万人群/弹幕/IM 后端（Go 为主，架构参考）**
| 项目 | 链接 | 许可 |
|---|---|---|
| WuKongIM | https://github.com/WuKongIM/WuKongIM | 待核 |
| OpenIM | https://github.com/openimsdk/open-im-server · https://github.com/openimsdk | 待核 |
| goim (Bilibili) | https://github.com/Terry-Mao/goim | 待核 |
| JuggleIM | https://github.com/juggleim/im-server | 待核 |

**音视频 / 直播 / 媒体面**
| 项目 | 链接 | 许可 |
|---|---|---|
| LiveKit | https://github.com/livekit/livekit | Apache-2.0 |
| OpenVidu | https://github.com/OpenVidu/openvidu | Apache-2.0 |
| MiroTalk SFU | https://github.com/miroslavpejic95/mirotalksfu | 待核（可能 AGPL） |
| Xiu | https://github.com/harlanc/xiu | 待核 |
| atm0s-media-server（RTMP 需 legacy 分支） | https://github.com/8xFF/atm0s-media-server | 待核 |
| mediasoup（Rust crate 在 `v3/rust/`） | https://github.com/versatica/mediasoup | ISC |
| blivedm_rs | https://github.com/isomoes/blivedm_rs · https://github.com/jiahaoxiang2000/blivedm_rs | 待核 |

**企业级 / 全球平台**
| 项目 | 链接 | 许可 |
|---|---|---|
| Rocket.Chat | https://github.com/RocketChat/Rocket.Chat | MIT（待核复核） |
| Mattermost | https://github.com/mattermost/mattermost | MIT（待核复核） |
| Element Web | https://github.com/element-hq/element-web | Apache-2.0 |
| Synapse | https://github.com/element-hq/synapse | AGPL-3.0（与本平台同许可，可直接复用） |
| Telegram Android | https://github.com/DrKLO/Telegram | GPL-3.0 |
| Telegram iOS | https://github.com/TelegramMessenger/Telegram-iOS | GPL-3.0 |
| Telegram Desktop | https://github.com/telegramdesktop/tdesktop | GPL-3.0 |
| Telegram Web (tweb) | https://github.com/morethanwords/tweb | GPL-3.0 |
| TDLib | https://github.com/tdlib/td | Boost 1.0 |
| teamgram-server | https://github.com/teamgram/teamgram-server | 待核 |
| Gramsrv | https://github.com/iamxvbaba/gramsrv | 待核 |
| MyTelegram | https://github.com/loyldg/mytelegram | 待核 |
| hungry-rs | https://github.com/dimentyy/hungry-rs | 待核 |
| grammers | https://github.com/Lonami/grammers | MIT |
| ferogram | https://github.com/ankit-chaubey/ferogram | 待核 |
| tuigram | https://github.com/erictran308/tuigram | 待核 |
| telegram_rust | https://github.com/oovets/telegram_rust | 待核 |

**国内开源 IM**
| 项目 | 链接 | 许可 |
|---|---|---|
| 唐僧叨叨 Server | https://github.com/TangSengDaoDao/TangSengDaoDaoServer | Apache-2.0 |
| 唐僧叨叨 Web | https://github.com/TangSengDaoDao/TangSengDaoDaoWeb | Apache-2.0 |
| 唐僧叨叨 Android | https://github.com/TangSengDaoDao/TangSengDaoDaoAndroid | Apache-2.0 |
| Startalk | https://github.com/startalkIM | 待核 |
| XyTalk PC | https://github.com/xy-Group/xyTalk-pc | 待核 |
| box-im | https://github.com/bluexsx/box-im | MIT |
| HuLa | https://github.com/HuLaSpark/HuLa | Apache-2.0 |
| HuLa-Server | https://github.com/HulaSpark/HuLa-Server | 待核 |
| HuLa-IM-Tauri | https://github.com/HuLaSpark/HuLa-IM-Tauri | Apache-2.0 |
| 野火 push_server | https://github.com/wildfirechat/push_server | 待核 |

**微信高仿（**无 LICENSE 或待核——只学思想，禁止复制代码，待核前一律按❌处理**）**
| 项目 | 链接 | 许可 |
|---|---|---|
| LQRWeChat | https://github.com/GitLqr/LQRWeChat | 无 LICENSE |
| vue-WeChat | https://github.com/arden-kfc/vue-WeChat | 待核 |
| CoderMikeHe/WeChat | https://github.com/CoderMikeHe/WeChat | 待核 |
| SwiftUI-WeChat | https://github.com/wxxsw/SwiftUI-WeChat | MIT（待核） |
| fake-world | https://github.com/shanyu-e/fake-world | 待核 |
| WeChatArkTS | https://github.com/xiaowuDev/Wechat_ArktsDevelop | 待核 |

**支付基础设施（ADR-12 配套）**
| 项目 | 链接 | 许可 |
|---|---|---|
| Cala (cala-ledger) | https://github.com/GaloyMoney/cala · https://docs.rs/cala-ledger | 待核（Galoy 系通常 Apache-2.0） |
| ousia-ledger | https://github.com/TheOusia/ousia | 待核 |
| TigerBeetle / tb-rs | https://github.com/tigerbeetle/tigerbeetle · https://github.com/dsp/tb | 待核（通常 Apache-2.0） |
| dtmrs | https://github.com/jackwangfeng/dtmrs | 待核 |
| Hyperswitch | https://github.com/juspay/hyperswitch · https://docs.hyperswitch.io | Apache-2.0 |
| obix | https://github.com/GaloyMoney/obix | 待核 |

**小程序运行时（ADR-14 配套）**
| 项目 | 链接 | 许可 |
|---|---|---|
| Rong | https://github.com/LingXia-Dev/Rong | 待核 |
| LingXia | https://github.com/LingXia-Dev/LingXia | 待核 |
| Wry | https://github.com/tauri-apps/wry | MIT/Apache-2.0 |
| Wasmtime | https://github.com/bytecodealliance/wasmtime | Apache-2.0（LLVM 例外） |
| Boa | https://github.com/boa-dev/boa | MIT/Apache-2.0 |
| Dimina（架构参考） | https://github.com/didi/dimina · 架构文档 https://github.com/didi/dimina/blob/main/docs/Architecture-Details.md | 待核 |

**推荐系统 / 算法**
| 项目 | 链接 | 许可 |
|---|---|---|
| PaddleRec | https://github.com/PaddlePaddle/PaddleRec | Apache-2.0 |
| Gorse | https://github.com/gorse-io/gorse | Apache-2.0 |
| Faiss | https://github.com/facebookresearch/faiss | MIT |
| ScaNN | https://github.com/google-research/google-research/tree/master/scann | Apache-2.0 |
| Twitter The Algorithm | https://github.com/twitter/the-algorithm | AGPL-3.0（同许可，可直接复用） |
| LightFM | https://github.com/lyst/lightfm | 待核 |

**推送 / 网关 / 多租户 / 前端**
| 项目 | 链接 | 许可 |
|---|---|---|
| Gorush | https://github.com/appleboy/gorush | MIT |
| 野火 push_server | https://github.com/wildfirechat/push_server | 待核 |
| Apache ShenYu | https://github.com/apache/shenyu | Apache-2.0 |
| Otoroshi | https://github.com/MAIF/otoroshi | 待核 |
| Vuestic UI | https://github.com/epicmaxco/vuestic-ui | MIT |
| Flutter | https://github.com/flutter/flutter | BSD-3 |
| ArkUI-X | https://github.com/arkui-x（主仓 GitCode/Gitee） | Apache-2.0 |
| Flutter-OH | 主仓 https://gitcode.com/openharmony-tpc/flutter_flutter · 镜像 https://atomgit.com/openharmony-tpc/flutter_flutter · 组织 https://atomgit.com/CPF-Flutter · 兼容库 https://atomgit.com/openharmony-tpc/flutter_packages | BSD-3 社区版（待核） |

**Discord 架构**
| 项目 | 链接 | 许可 |
|---|---|---|
| Discord 架构文档（社区整理） | https://github.com/babushkai/system-design-patterns/blob/codex/article-reading-redesign/08-case-studies/08-discord.md | ➖文档 |
| sorted_set_nif | https://github.com/discord/sorted_set_nif | 待核 |

7.2 **内部测试模式下的义务（替代严格合规清单）**：
- **全部代码来源均可接纳**：含 Apache/MIT/BSD/ISC/Boost/GPL/AGPL 及无 LICENSE 仓库，乙方可自由参考、复制、改造；
- 唯一持续要求：在 `THIRD-PARTY-NOTICES.md` 中**记录来源仓库与引用位置**（工程可追溯性，便于未来审计，非法律义务）；
- **再激活条款**：若本项目未来对外发布、开源、商用或分发，本条豁免自动失效，第七条 7.1 表中全部许可义务（含"无 LICENSE 不可分发"）即时恢复，乙方须配合甲方在发布前完成许可核验与必要的代码替换；
- 禁止引入闭源 IM SDK 的限制维持不变（该限制为架构铁律而非许可问题）。

7.3 **风险分配**：内部测试期间因代码来源产生的第三方主张（如有），按 8.2 条由乙方就其引入的具体代码承担责任；甲方已知悉无 LICENSE 仓库的风险并接受该风险于内部测试范围内。

---

## 第八条 知识产权

8.1 本合同项下全部交付物（含源代码、设计文档、测试、脚本）的**著作权及其他知识产权归甲方所有**。甲方选择以 **AGPL-3.0** 许可对外发布本平台（不影响甲乙双方之间的前述归属）。

8.2 乙方保证交付物为原创或已获合法授权。在**内部测试范围内**，乙方按第七条 7.2 内部测试模式执行（来源不限，`THIRD-PARTY-NOTICES.md` 记录来源）；若触发 7.2 再激活条款（对外发布/开源/商用），乙方按该条款配合完成许可核验。因乙方引入代码的第三方主张（发生于内部测试期间的），由乙方承担责任并使甲方免责。

8.3 乙方在项目前已拥有的背景知识产权（如通用工具库）归乙方所有，乙方授予甲方在本平台范围内永久免费的使用许可；背景知识产权清单应于交付时书面列明。

---

## 第九条 保密与数据安全

9.1 双方对在合作过程中知悉的对方商业秘密、技术资料、经营信息负有保密义务，保密期自合同终止后持续 2 年。

9.2 乙方在开发中接触的甲方用户数据、运营数据仅可用于合同目的；测试使用虚构数据。违反本条的，乙方承担违约责任并赔偿损失。

9.3 乙方人员签署个人保密承诺，泄露责任由乙方承担。

---

## 第十条 付款方式与节点

10.1 合同总价按人日结算：估算总人日 ______ 人日 × 单价 ______ 元/人日 = 合同总价 ______ 元（含税）。总价为固定上限，实际结算按第五条验收通过的阶段人日实报实销（超估需变更单）。

10.2 **付款节点**（均以对应阶段书面验收通过为前提）：

| 节点 | 付款比例 | 条件 |
|---|---|---|
| 签约预付款 | 20% | 合同签署后 7 个工作日内 |
| P1 验收通过 | 10% | 第五条 P1 全部验收标准 |
| P2 验收通过 | 5% | P2 验收（含三项真机验收） |
| P3 验收通过 | 20% | P3 验收（读扩散核心+压测报告） |
| P4 验收通过 | 15% | P4 验收（含账本守恒审计） |
| P5 验收通过 | 15% | P5 验收（三端安装包+直播+小程序端到端） |
| P6 验收通过（终验） | 10% | P6 验收 + 全量 DoD 回归 |
| 质保期满 | 5% | 终验后 3 个月质保期满无未修复严重缺陷 |

10.3 发票：乙方按付款节点开具增值税专用发票。

---

## 第十一条 变更管理

11.1 任何范围变更（新增/删除/修改功能）须以书面《变更单》确认，载明变更内容、工期影响、费用影响，双方签认后生效。

11.2 单次变更不影响关键路径且工作量 ≤3 人日的，乙方应无偿实施（累计不超过 5 次）。

---

## 第十二条 违约责任

12.1 乙方逾期交付：每逾期一周，按合同总价 0.5% 支付违约金，累计不超过 10%；逾期超 4 周甲方可解除合同并要求返还已付款项的未交付对应部分。

12.2 乙方交付与附件要求严重不符且经两次修复仍不达标的，甲方有权解除合同，乙方返还对应已收款项。

12.3 乙方违反第七条许可合规义务或第八条知识产权保证的，承担全部法律责任与赔偿。

12.4 甲方逾期付款：每逾期一周按应付金额 0.5% 支付违约金。

---

## 第十三条 质保

13.1 终验通过后质保期 3 个月：质保期内非甲方原因导致的缺陷，乙方免费修复（严重缺陷 24 小时响应、72 小时修复；一般缺陷 5 个工作日修复）。

13.2 质保期内修复不改变合同价款；新需求按第十一条变更管理执行。

---

## 第十四条 不可抗力

因不可抗力（自然灾害、政策变化、网络管制等）导致合同无法履行的，受影响方应在 7 日内书面通知对方并提供证明，双方协商变更或解除合同，互不承担违约责任。

---

## 第十五条 争议解决

本合同适用中华人民共和国法律。争议协商不成的，提交甲方所在地有管辖权的人民法院诉讼解决。

---

## 签署栏

| | 甲方 | 乙方 |
|---|---|---|
| 单位盖章 | | |
| 授权代表签字 | | |
| 日期 | | |

---

## 附件清单（与正文具同等效力）

1. 附件一 `docs/WORK_BREAKDOWN.md` —— 原始任务书（WP-01~13）
2. 附件二 `docs/master-blueprint.md` —— 总蓝图（ADR-1~24、产品/技术架构树、九大不变式、待确认疑问已全部拍板）
3. 附件三 `docs/miniprogram-blueprint.md` —— 小程序自研运行时方案（Rong/LingXia/Wry/Wasmtime）
4. 附件四 `docs/feature-gap-matrix.md` · `docs/feature-comparison.md` —— 功能差距对照
5. 附件五 `docs/threat-model.md` —— 威胁模型
6. 附件六 `docs/bench/` —— 基准报告目录
