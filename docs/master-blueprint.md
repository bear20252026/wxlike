# wxlike 总蓝图（汇总版）：技术方案 × 全功能对照 × 参考项目 × 路线图

> 2026-10-16。**本文件是全部讨论的唯一汇总入口**，汇总五轮内容：
> ① 任务书（WORK_BREAKDOWN.md，14 WP）→ ② feature-comparison.md（WP 视角对照，149 行）
> → ③ feature-gap-matrix.md（四大产品九域差距 + P0–P6 路线图，148 行）
> → ④ 第二轮补充（Rust 技术栈选型 / 万人群读扩散数据模型 / 前后端功能服务清单 /
>    支付与小程序解耦 / 七端策略 / WuKongIM·OpenIM·goim 等新参考系）
> → ⑤ 第三轮补充（**P0 决策全部确认**：云同步=TG 式、移动端=Flutter 基座；
>    支付账本四层架构 cala-ledger/dtmrs/Hyperswitch 范式/obix）
> → ⑥ 第四轮补充（**ADR-13 直播=频道级能力**：Discord 语言分层启示、
>    Rust 媒体面选型 Xiu/mediasoup/atm0s、弹幕复用读扩散管道、
>    Flutter-OH 主仓库在 GitCode 不在 GitHub）
> → ⑦ 第五轮补充（**ADR-14 小程序自研运行时**：Rong 多引擎逻辑层 + LingXia/Wry 视图层 +
>    Wasmtime 沙箱 + 自研 publish/invoke Bridge 契约，非微信兼容路线；
>    详见 [miniprogram-blueprint.md](miniprogram-blueprint.md)）
> → ⑧ 第六轮补充（**ADR-15 运营与合规体系**：审计日志/RBAC+MFA/内容审核管线/防泄密、
>    频道分组与频道级角色、话题订阅、全局搜索本地+云端、撤回跨端状态同步、
>    Bot 账号体系/开放 API/Webhook；新参考系 Stoat/Bonfire/Zulip/JuggleIM/
>    LiveKit/OpenVidu/adminx/Meilisearch/Tantivy/Gorush 等）
> → ⑨ 第七轮终版收敛（**产品功能树 + 技术架构树 + 决策速查 + 九大不变式**：
>    社交三支柱并列、消息类型注册表三通道、三账本命名空间、通知决策/传输分离、
>    风控/审核/举报三层、media/video 分离、推荐搜索发现三层分离、
>    陌生人消息请求、多租户预留——见第十一/十二/十三部分）。
> 已有文档全部保留未删改；本文负责**合并、去重、映射到 wxlike 现有代码、并补充遗漏项**。
> 图例：✅ 已实现有验证 · ⚠️ 部分 · ❌ 未实现 · 🔶 需按新决策重做 · ➖ 排除。

---

## 第一部分：项目现状快照（2026-10-16 实测）

| 维度 | 现状 |
|---|---|
| 代码量 | 生产 Rust ~11,500 行 + 测试 Rust ~8,200 行 + UI ~2,500 行 ≈ 22,000 行 |
| 测试 | workspace 30 套件 247 测试 + ui 7 测试，全绿；fmt/clippy(-D warnings)/build/doc 四关过 |
| 已完成 WP | 01/02/04/05/07/09/10/12/13 完整；03 缺转发·合并转发·收藏·正在输入·搜索五件套；06 按契约（FCM 真机缺凭据）；08 库就绪缺真机双机验收与 UI 接入；11 代码完成缺真机演示 |
| 已知硬缺口 | 多设备同时在线（hub 单连接 last-wins）· 手机端不存在 · 历史消息 UI 未接 · E2EE 未接客户端消息路径 |
| 基准 | 单 hub 10k 连接 / 1k msg·s / 60s：0 断连，p99 5ms；200 人群 p99 11ms |
| 部署 | docker compose（redis+server+SeaweedFS S3）已验证 |

---

## 第二部分：架构决策记录（ADR，含新决策）

| # | 决策 | 结论 | wxlike 证据 / 改动量 |
|---|---|---|---|
| ADR-1 | **消息云同步路线** | **Telegram 式服务端全量存储**（维持现状）：普通聊天云存可搜可漫游；E2EE 做"秘密聊天"可选模式。拒绝 WhatsApp 式全量 E2EE（会同时杀死服务端搜索/全量历史/已读统计） | 存储层已就位（`storage: Some(store)` + messages 表）；缺 UI 历史拉取（P1） |
| ADR-2 | **群扩散架构** | **双轨并存**：≤500 人小群保留写扩散（已验证）；万人群/频道/弹幕走**读扩散**新建管道（第四部分数据模型）。两者长期共存（Discord/TG 同构） | 现写扩散 1000 上限保留；读扩散为新建，不可原地迁移（P3） |
| ADR-3 | **账号锚点** | **用户名+密码**（现状），补：恢复码/邮箱找回、captcha+IP 限流、argon2id 可选升级、2FA | WP-01 已有 UNIQUE+PBKDF2-100k（P4 补三件） |
| ADR-4 | **多设备策略** | **多设备同时在线必须做**：hub 连接表 `user → Vec<SendStream>` 改造 + 路由表加设备维度 + 设备管理页。实证缺口：当前后到连接静默顶掉先到 | mq_hub.rs:363（P3，与读扩散同批） |
| ADR-5 | **支付与 IM 的边界** | **资金逻辑独立 payment-service**：IM 只承载消息通道与状态广播。红包卡片走"消息变更"模式更新状态（未领取→已领取→已领完），支付回调与信息流只在回调点交汇。现有 wallet 在 platform 内，独立服务化排在 P4+ | wallet.rs 已有事务守恒；**缺"消息变更"广播**（实证：claim 只发新消息，原卡片不回写）（P4） |
| ADR-6 | **小程序容器** | **沙箱与 IM 进程隔离**（三明治架构）：Rust 后端只管包元数据/鉴权/回调转发，不介入执行。现有 `container` crate（Boa JS 宿主）+ `dpage`（DPG1 包格式）是雏形，产品化排 P5+ | 已有研究级实现 |
| ADR-7 | **音视频与消息管道的关系** | **通话房间独立于群消息管道**：1:1 走现有 rtc crate（str0m）；群语音/视频做成独立"通话房间"能力，从群/频道入口进入，不占用群信箱 | rtc crate 就绪未接 UI（P2）；房间制排 P4 |
| ADR-8 | **传输层抽象（新识别）** | 长连接入口需**同时支持 QUIC（桌面/移动）与 WebSocket（Web/浏览器）**。当前 `Client` 结构体直接持有 `quinn::SendStream`，QUIC 紧耦合——需要抽 `Transport` trait（framing 已独立，改造成本可控） | client.rs:15-16（P3，Web 端前置） |
| ADR-9 | **网关无状态化** | 连接状态外移 Redis（设备→网关→最后路由），网关进程可水平扩展。当前 mq_hub 内存态 + 路由表 Redis，部分达标 | P3/P6 |
| ADR-10 | **认证模型** | 维持 DB session token（支持即时吊销），**网关层加 JWT(RS256) 校验可选**——JWT 无状态的代价是吊销需黑名单，当前规模 DB token 更简单。两者可并存（JWT 给网关，DB token 给敏感操作） | auth.rs（P4 评估） |
| **ADR-11 移动端基座（第三轮新增）** | **Flutter 统一 iOS/Android/鸿蒙**：鸿蒙走 Flutter-OH 社区适配 + ArkTS 桥接系统能力（通知栏/后台保活/分布式调度）；ArkUI-X 降为"鸿蒙战略优先"备选（版本碎片化：HarmonyOS 3.0/4.0/NEXT 的 API 行为不一致，@State/@Builder 兼容成本持续存在）；KMP-CMP（2026-09 首版）与 RN-OH 排除。桌面 Tauri 2 默认、复杂富媒体消息流可切 Flutter Desktop；Web 独立工程 | 依据：壹信 IM 生产案例（Flutter 3.22 + 自研鸿蒙插件，市级政务平台日均 180 万条消息、滚动 60FPS、鸿蒙 NEXT Beta 兼容认证）（P5） |
| **ADR-12 支付账本架构（第三轮新增）** | **四层解耦**：①账本核心=事件溯源+复式记账（`cala-ledger` 嵌入 PG：余额=重放分录、禁止原地 UPDATE、persistent outbox）；②交易协调=FSM（Initialized→Authorized→Settled→Failed）+幂等键+`dtmrs` Saga 编排（补偿替代全局原子）；③渠道适配=`ChannelConnector` trait（Hyperswitch 范式，对接自有银行/清算接口，**适配器只发送划拨指令、绝不碰账本**）；④对账审计=独立服务消费 outbox+渠道回执双向核对（长短款进人工队列）+maker-checker 双人复核。与 ADR-5 衔接：现有 wallet.rs 为演示级单式账本，接真实渠道时迁移 cala 双式账本，`money_supply` 守恒断言保留为对账基线。高性能备选：ousia-ledger 两阶段执行 / TigerBeetle | P4（真实渠道阶段）；虚拟钱包演示期不动 |
| **ADR-13 直播=频道级能力（第四轮新增）** | 参照 Discord：**频道即形态**（文字频道/语音频道/直播频道），直播不是独立模块。直播频道内部 **SFU 架构**（主播推一路流、SFU 选择性转发、观众拉流——非 MCU 混流）；**弹幕/实时评论复用读扩散消息管道**（绝不另建一套消息系统）；**直播信令（加入/离开/切流）走独立高优先级通道**，不与聊天流混排。语言分层启示（Discord=Elixir/OTP 网关 + Rust 消息与媒体代理 + 25,000 SFU）：我们坚持全 Rust = Tokio 网关 + Rust SFU，代价是自行实现 BEAM 级监督与背压（P3 慢消费者隔离已列） | 媒体面选型：**Xiu**（纯 Rust 协议最全：RTMP 集群/RTSP/WebRTC WHIP-WHEP/HTTP-FLV/HLS，OBS 可推流）或 mediasoup Rust crate（SFU 语义 Simulcast/SVC 最成熟）；atm0s-media-server 重构期只观察。成熟产品的形态差异：微信=小程序直播/视频号（电商工具）、TG=群通话 RTMP+WebRTC 双模式、Discord=语音频道延伸（P5） |
| **ADR-14 小程序自研运行时（第五轮新增）** | **不走微信兼容路线**：自定义 DSL 与运行时契约，四层拼装——逻辑执行=**Rong**（一套 Rust API 驱动 QuickJS/JSC/ArkJS，`#[js_class]` 绑定 + Worker 池有界执行 + 内置 Timer/HTTP/FS/SQLite 模块）；视图=**Wry**（子 WebView 子面板嵌入 IM，互不侵入渲染栈）或 **LingXia**（完整宿主骨架，lingxia-webview 已抹平四平台差异）；沙箱=**Wasmtime**（fuel/epoch + host functions，第三方不可信代码两级信任模型）；Bridge=自研 **publish/invoke 双通道契约**（setData 异步合帧 + 能力调用请求响应 + bridgeId 路由，权限中间件统一）。**包管理在 Rust 后端**（签名/摘要/体积限制，格式复用 DPG1 补签名）；**payment.request 只转发独立 payment-service**。与现有资产衔接：container（Boa 引擎/wxhost 桥）+ dpage（DPG1）即雏形，升级路径 Boa→Rong、wxhost→双通道 Bridge、DPG1+签名 | 详见 [miniprogram-blueprint.md](miniprogram-blueprint.md)（含 M1–M5 分阶段与全量参考链接表）（P5+ 独立轨道） |
| **ADR-16 社交三支柱 + 统一读扩散信箱（第七轮终版，修订 ADR-2）** | **三根并列支柱**：好友+单聊（direct，owner=规范化用户对）/ 群组（group，owner=group_id）/ 社区频道话题（channel，owner=channel_id）——三者**共用同一套读扩散信箱基础设施**，message-service 按 `owner_type` 区分，不写三套投递逻辑；序列号各 owner 独立生成；万人规模是 community 的 member_limit，群组默认上限另设。**此决策修订 ADR-2 的"小群保留写扩散"条款**；现 mq_hub 的写扇出转过渡实现（见第十二部分 Q1/Q2） | 技术树 C 消息与同步（P3 核心） |
| **ADR-17 消息类型注册表 + 三通道分级（第七轮终版）** | 消息类型统一注册（type_id/payload_schema/render_target/lifecycle/channel_class），投递分三通道：**normal**（群信箱+占用序列号+持久化：文本/图/视频引用/文件/小程序卡片/广告卡片/红包卡片）、**high_priority**（独立 NATS 主题，不占序列号只投在线：直播信令/正在输入/实时弹幕）、**ephemeral**（落库不进时间线，只更新既有卡片：红包领取状态/广告回执/消息编辑状态） | message-service（P3） |
| **ADR-18 三账本账户命名空间（第七轮终版）** | 单一 ledger-service 复式记账内核（cala-ledger），**wallet / advertiser / platform_marketing / platform_revenue 四类 account_type 隔离**——不是三个账本引擎。广告红包闭环=ad-account 扣费 → marketing 中转 → 用户钱包，**两笔独立分录**。新增 ad-account-service 与 marketing-account-service | 6 支付树（P4） |
| **ADR-19 通知决策与传输分离（第七轮终版）** | **notification-service**（决策层：查在线状态只推离线设备、多设备去重只推最近活跃、聚合策略）产出 PushTask；**push-gateway**（传输层：APNs/FCM/华为/小米/OPPO/vivo）只负责发出去，**不知道免打扰与聚合** | 通知树（P3 决策层 / P5 厂商通道） |
| **ADR-20 风控/审核/举报三层分离（第七轮终版）** | **risk-service**（前置毫秒级实时拦截：放行/限流/挑战/拒绝，不调用审核服务）· **moderation-service**（后置内容判定 API 能力层，供举报与发送流程调用）· **report-service**（举报工单流程层，状态流转+处置动作回调各业务服务） | 安全治理树（P4 管线 / P6 完整） |
| **ADR-21 media-service 与 video-service 分离（第七轮终版）** | **media-service**（实时：WebRTC SFU、1:1 通话、群房间、直播信令与转发——有状态按房间调度需边缘节点）与 **video-service**（点播：上传→FFprobe→FFmpeg 多分辨率 HLS→缩略图→对象存储——无状态任务队列可水平扩展）分离；完成后发事件由 message-service 构造视频引用消息，**视频二进制不经过消息网关** | 媒体树（P2 真机 / P4 房间 / P5 点播） |
| **ADR-22 推荐/搜索/发现三层分离（第七轮终版）** | **发现页**（展示层不持索引，community-service 聚合活跃度排序）· **search-service**（主动查询：Meilisearch/Tantivy，索引消息/群组/频道/社区/用户/话题，事件订阅更新）· **recommendation-service**（被动分发：消费行为事件流，双塔召回+Faiss、Wide&Deep 排序，产出推荐频道/视频/广告/好友） | 内容分发树（P3 搜索 / P6+ 推荐） |
| **ADR-23 多租户预留（第七轮终版）** | 所有核心表带 `tenant_id`（默认 default），username 唯一索引为 (tenant_id, username)，ledger 分录 tenant_id 入分区键，鉴权中间件预留 tenant_id 提取——**当前不实现租户隔离逻辑**，是字段预留不是功能 | 全树（P3 建表时带上） |
| **ADR-24 陌生人消息请求（第七轮终版）** | 非好友首条单聊进入 **message_requests 请求箱**：接受→正常会话，拒绝→屏蔽；"是否允许陌生人单聊"为产品配置。**流程必须在架构中存在，不可绕过关系层直入会话**（实证：现 mq_hub 入站无任何关系层拦截，是真新增） | relation-service（P3，与拉黑同批） |
| **ADR-15 运营与合规体系（第六轮新增）** | 面向公众运营的三件套**以独立服务叠加，不推翻消息管道**：①**审计日志**——登录/发消息/文件下载/管理后台操作全量记录，按时间/用户/操作类型检索（安全事件唯一可追溯依据）；②**内容审核管线**——文本敏感词+图片/音频/视频多模态，先审后发/先发后审按场景配置，举报→审核→处置闭环（Lexfence 多模态微服务模式，REST API 调用）；③**RBAC+MFA 管理后台**——adminx（纯 Rust）直接嵌 Axum：RBAC/审计日志/TOTP/全文搜索；提现审核/封禁/数据导出等敏感操作 MFA 或双人复核（与 ADR-12 maker-checker 同源）。按场景可选防泄密：水印/防截屏/防转发/会话锁定；存储层加密 AES-256 或国密 SM 系列 | P4 起分批（审计日志随读扩散；完整运营面板 P6） |

---

## 第三部分：技术栈选型总表（现状 → 目标）

| 层 | 目标选型（第二轮建议） | wxlike 现状 | 差距 / 迁移 |
|---|---|---|---|
| 异步运行时 | Tokio (full) | ✅ Tokio | 无 |
| 业务 API | Axum | ✅ Axum 0.8 | 无 |
| 长连接网关 | 独立网关进程（无状态）+ WS/QUIC 双协议 | ⚠️ server 单进程内嵌 QUIC hub | ADR-8/9：Transport trait + 网关拆分（P3/P6） |
| 实时传输 | WS 为主（Web），QUIC（桌面/移动弱网） | ✅ QUIC（客户端紧耦合） | Transport trait（P3） |
| 服务间通信 | tonic gRPC | ❌ 单体内函数调用 | 服务拆分时（P4+） |
| 消息管道 | NATS（在线实时）/ Kafka（离线审计） | ⚠️ MQ trait（内存 / Redis Streams at-least-once+巡检） | **`MessageQueue` trait 加 NATS 后端成本低**（P3）；Kafka 远期 |
| 关系库 | PostgreSQL（分区表） | ❌ SQLite（单机） | P6 迁移（rusqlite→sqlx，schema 大体平移） |
| 消息存储 | PG 分区表 + S3 | ⚠️ SQLite + LocalDirStore（S3 trait 已留） | P6；S3 实现挂 SeaweedFS/MinIO |
| 缓存/在线态/未读 | Redis ZSET/INCR | ⚠️ Redis 仅作 MQ backend；在线态在内存路由表 | 未读 ZSET 方案随读扩散（P3） |
| 对象存储 | S3 兼容 | ⚠️ trait + LocalDirStore | compose 里 SeaweedFS 即用（P2） |
| 认证 | JWT RS256（网关无状态） | ⚠️ DB session token（可即时吊销） | ADR-10：并存评估（P4） |
| 支付账本 | cala-ledger（嵌入式复式记账+事件溯源）+ dtmrs Saga + obix outbox | ❌ wallet.rs 为演示级单式账本（有守恒断言） | ADR-12：真实渠道阶段迁移（P4） |
| 音视频 | str0m（已选）/ rvoip | ✅ str0m + 状态机 + 回环验证 | UI 接入 + 真机（P2），房间制（P4） |
| 直播媒体面（SFU） | Xiu（RTMP/RTSP/WebRTC/HTTP-FLV/HLS）或 mediasoup Rust crate（Simulcast/SVC） | ❌ 无 | ADR-13：P5 直播频道 |
| 小程序运行时 | Rong（多引擎逻辑层）+ Wry/LingXia（视图）+ Wasmtime（沙箱）+ 自研 Bridge | ⚠️ container（Boa 宿主/wxhost 桥）+ dpage（DPG1 包格式）研究级雏形 | ADR-14：P5+ 独立轨道，见 miniprogram-blueprint.md |
| 全文搜索 | Meilisearch（独立服务）或 Tantivy（嵌入式库，Rust 原生） | ❌ | 第六轮：读扩散下的服务端搜索必选（P3） |
| 内容审核 | Lexfence 模式独立微服务（文本敏感词+图/音/视频多模态） | ❌ | 运营前置（P4 管线 / P6 完整） |
| 管理后台 | adminx（纯 Rust 嵌 Axum：RBAC/审计/TOTP/全文搜索） | ❌ | P6（最小 CLI 先行） |
| 推送网关 | Gorush 模式 + 国内厂商通道（野火 push_server 参考） | ⚠️ FCM 实现完成，聚合未做 | P3 聚合 / P5 厂商通道 |
| 开放 API 网关 | Apache ShenYu / Otoroshi（OAuth2/JWT/Webhook） | ❌ | P6+ 生态阶段 |
| 容器化 | Docker Compose → k8s | ✅ compose | k8s 远期（P6） |
| 密码哈希 | argon2id | ✅ PBKDF2-100k | 可选升级（P4） |

---

## 第四部分：万人群读扩散数据模型（P3 设计规范）

> 来源：第二轮补充 + WuKongIM（频道订阅模型）/ OpenIM（五层同步）/ goim（分层推送）。
> 与 ADR-2 对应：写扩散（≤500 人小群，已实现）之外的新管道。

### 4.1 必须的三张关系表（第一天就建，不是优化项）

```sql
-- ① 群消息已读游标：公共信箱不记个人进度，未读数靠游标差值计算
CREATE TABLE group_read_cursor (
    group_id        TEXT NOT NULL,
    user_id         TEXT NOT NULL,
    last_ack_seq    INTEGER NOT NULL,   -- 该成员读到哪条
    PRIMARY KEY (group_id, user_id)
);

-- ② 消息用户状态表：删除/置顶/标记只对自己生效，不动物理信箱
CREATE TABLE msg_user_state (
    group_id    TEXT NOT NULL,
    seq         INTEGER NOT NULL,
    user_id     TEXT NOT NULL,
    state_type  TEXT NOT NULL,          -- deleted | pinned | unread
    PRIMARY KEY (group_id, seq, user_id, state_type)
);

-- ③ 群成员版本差分表：万人群不做全量成员拉取，客户端按版本增量同步
CREATE TABLE group_member_log (
    group_id     TEXT NOT NULL,
    version      INTEGER NOT NULL,      -- 单调递增（对应消息 seq 或独立序列）
    change_type  TEXT NOT NULL,         -- join | leave | role_change | mute
    user_id      TEXT NOT NULL,
    payload      TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (group_id, version)
);
```

**同步协议**（OpenIM 五层，直接作为客户端-服务端契约）：摘要判断（版本号对比）→ 增量拉取（版本差分）→ 全量兜底（版本链断裂）→ 批量阈值收口 → 成员列表懒加载（分页按需）。

### 4.2 未读数：ZSET 排名差，不为万人各写计数器

- 群消息按写入顺序存 Redis ZSET（member=msg_id, score=seq）；
- 成员未读数 = `ZREVRANK`（最新 seq）− `ZSCORE`（该成员游标 seq），一次 O(log N)；
- 已读 = 更新 `group_read_cursor`，未读数即免维护。

### 4.3 有序性：每群一个单调序列号

- 序列号集中在**单点原子生成**（PG sequence 或 Redis INCR），禁止多写节点各自生成；
- 客户端按 `seq > last` 拉取与补洞；空洞超过阈值触发兜底全量；
- 与现有 `Envelope.seq` 语义对齐（写扩散小群已有 per-conv seq）。

### 4.4 对应新建 Rust 模块

| 模块 | 职责 | 依赖现状 |
|---|---|---|
| `mailbox`（群信箱服务） | 有序写入、按 seq 拉取、补洞 | 新建；存储走 PG 分区（P6 前 SQLite） |
| `sync`（同步服务） | 成员/权限/话题列表版本差分，五层策略 | 新建 |
| `presence/read`（已读未读服务） | 游标更新、ZSET 未读数、已读回执开关（万人群默认只做未读） | 新建 |
| 网关改造 | 多设备连接表 + Transport trait + 慢消费者隔离（flare-core 思路） | mq_hub.rs 改造 |
| 弹幕分层推送 | 活跃成员实时、非活跃聚合降频（goim 思路） | push crate 扩展 |

---

## 第五部分：全功能对照总表（合并四款产品矩阵，含第二轮新增项）

> 与 feature-gap-matrix.md 的九域表互补：此处按"功能 → wxlike → 借鉴 → 阶段"压缩呈现，
> ✅/⚠️/❌ 含义相同。完整四产品逐列对比见 feature-gap-matrix.md，不重复。

### 5.1 账号（详见 ADR-3/10）
✅ 用户名注册/登录/登出/资料/公开页 · ✅ PBKDF2 · ⚠️ 头像文本字段 · ❌ 找回 · ❌ captcha/限流 · ❌ 2FA · ❌ 设备管理 · ❌ 注销 · ❌ 多账号 · ❌ 手机号/邮箱/OAuth/扫码（按决策排除或远期）

### 5.2 关系链
✅ 申请/同意/拒绝/备注/删除/增量同步 · ❌ 搜索用户 · ❌ 拉黑（P3 与读扩散收件过滤同批） · ❌ 分组标签/添加方式权限/二维码 · ➖ 附近的人（隐私排除）

### 5.3 消息基础与操作
✅ 文本/离线落盘/去重/顺序/多端增量协议/引用/@/撤回（无时限+占位）/已读回执协议 · ⚠️ 云同步（存储✅ UI 拉取❌）· ⚠️ E2EE（库✅ 接线❌）· ❌ **转发/合并转发/收藏/正在输入/搜索（五件套，P1）** · ❌ 语音条（含波形/语音转文字）/视频（首帧+倍速）/文件消息/位置/名片/贴纸/GIF（P2 起逐步） · ❌ 编辑/置顶/翻译/定时/自毁（P5+）· ⚠️ 撤回跨端状态同步（读扩散下=逻辑删除+信箱变更事件广播，离线端重连补同步——P3）· ❌ 富文本/Markdown · ❌ 投票 · ❌ 消息固定/保存

### 5.4 群组 / 频道 / 话题
✅ 管理（建邀踢退/角色/1000 上限/转让/my_remark）/写扩散跨 hub/离线补收/@改写 · ❌ **万人群读扩散管道（P3 全套：信箱/游标/版本差分/ZSET 未读）** · ❌ 频道层/话题层/社区（P3）· ❌ 禁言/公告/审批/邀请链接/慢速模式/成员搜索（P3）· ❌ 投票/群文件库/机器人（P5+）

### 5.5 朋友圈 / 动态
✅ 发布/好友圈读时判定/赞/评论树/软删/分页 · ❌ 视频号/24h 动态/可见范围细分（P5+）

### 5.6 音视频
⚠️ 1:1 语音/视频（str0m 库+回环验证，缺 UI 接入与双机真机）· ❌ 群通话房间（ADR-7，P4）· ❌ 多人/屏幕共享/通话记录 · ⚠️ STUN 内置+TURN 接口
· ❌ **直播频道（ADR-13，P5：SFU + 弹幕复用读扩散 + 信令独立通道）** · ❌ RTMP ingest / OBS 推流（Xiu 覆盖）· ❌ 屏幕共享直播（Discord Go Live 形态）· ❌ 会议管控（静音/踢人/锁定/录制，P4 房间制）· ❌ 连麦 · ❌ 屏幕共享（桌面原生采集 + 移动端录屏权限经 Flutter 平台通道桥接）

### 5.7 支付 / 小程序
✅ 虚拟转账/红包/流水（事务守恒）· ⚠️ 红包卡片"消息变更"回写缺失（ADR-5，P4）· ❌ 充值/提现/支付密码/对账（持牌边界内，P4）· ⚠️ 小程序容器雏形（Boa+DPG1）→ **ADR-14 自研运行时（Rong/LingXia/Wry/Wasmtime + publish-invoke Bridge，包格式复用 DPG1 补签名），详见 miniprogram-blueprint.md（P5+）**

**支付四层架构（ADR-12，真实渠道阶段落地）**：账本核心（cala-ledger 事件溯源+复式记账——余额永远由重放分录得出，任何并发与守恒问题收敛在这层解决）→ 交易协调（FSM 状态机+幂等键防双重扣款，dtmrs 嵌入式 Saga 用"扣款撤销"补偿收敛跨服务一致性，选编排 Orchestration 而非编舞以保审计路径）→ 渠道适配（`ChannelConnector` trait 逐渠道实现认证/请求/解析，对接自有银行企业网银/银企直连/内部清算文件接口——这是"完全自建渠道"的真正工作量所在）→ 对账审计（obix outbox 可靠流出账本事件，与渠道结算文件双向核对，长短款人工队列；提现/调账 maker-checker 双人复核）。边界铁律：**消息网关与群信箱永不执行资金操作；红包在聊天里只是一条自定义消息卡片**。

### 5.8 推送
✅ 框架+FCM 实现+设备表 · ⚠️ FCM 真机 · ❌ APNs（接口位）· ❌ **通知聚合**（万人群必做，goim 分层推送思路，P3）· ❌ 厂商通道（国内安卓）

### 5.9 客户端工程（七端）
✅ Windows 桌面（深色/动效/九宫格/红包 UI）· ⚠️ macOS/Linux 未验 · ❌ **Android/iOS（P5，Flutter + UniFFI core）** · ❌ **鸿蒙（P5 决策：ArkUI-X 战略优先 or Flutter 社区适配+ArkTS 桥接）** · ❌ Web（独立工程，复用 API+WS 协议；Transport trait 是前置）· ❌ 虚拟列表/历史加载（P1/P2）· ❌ 多语言/无障碍/自动更新/崩溃上报（P5/P6）

### 5.10 服务端基础设施
✅ MQ trait/多节点路由/崩溃恢复/可观测/compose/压测基准 · ❌ PG+分片（P6）· ❌ 管理后台（用户/群管理/审核/封禁/举报，P6）· ❌ 备份多机房（P6）

### 5.11 运营 / 合规 / 生态（第六轮新增域）

✅ 钱包对账断言 · ⚠️ FCM/推送框架（聚合与厂商通道未做）· 其余全 ❌，按第六轮清单补：
- **审计日志**：登录/发消息/文件下载/管理操作全量，按时间/用户/操作类型检索（P3 随读扩散起步）
- **RBAC+MFA 后台**：adminx 嵌 Axum；敏感操作（提现审核/封禁/导出）MFA 或双人复核（P4/P6）
- **内容审核管线**：多模态+先审后发/先发后审配置+举报-审核-处置闭环（P4 管线 / P6 完整）
- **防泄密**：水印/防截屏/防转发/会话锁定（按目标场景选配，P5+）
- **存储加密**：AES-256 或国密 SM 系列（合规按需，P6）
- **Bot 账号体系**：机器人入频道/发结构化卡片/响应 @——与 ADR-14 小程序桥接**共享同一套权限校验和能力路由**（P6）
- **开放 API/Webhook**：消息发送/用户管理/群组频道管理/事件订阅（P6+）
- **SSO/组织架构同步**：企业场景预留接口（P6+）
- **后台运营**：批量导入/封禁、社群强制解散/转让、举报工单、数据看板（DAU/MAU/消息量/频道活跃/留存/万人群增流失/推送到达率/账本对账异常）（P6）
- **体验与无障碍**：主题三模式（浅/深/跟随系统——深色已有，补跟随系统）· i18n 文案分离 · WCAG AA 对比度 + 字号 200% + 读屏 · 通知多级过滤（会话/频道/关键词）（P3 起逐步）

---

## 第六部分：七端客户端策略（收敛结论）

| 端 | 选型 | 关键约束 | 阶段 |
|---|---|---|---|
| Windows / macOS / Linux | **Tauri 2（现状）** | 与 Rust 同栈、包体 ~10MB；WebView 渲染差异与 IPC 性能需验证；文字/图片/文件消息为主 → 合理 | ✅ Win 已有 / mac·Linux P2 |
| Android / iOS / 鸿蒙 | **Flutter 统一基座（ADR-11 ✅ 已确认）**：iOS/Android 原生 Flutter + Rust core（UniFFI/cargo-ndk 绑定）；鸿蒙走 Flutter-OH 社区适配 + ArkTS 桥接（通知/后台保活/分布式调度）。备选：鸿蒙战略优先时切 ArkUI-X（接受版本碎片化成本） | Flutter-OH 有 IM 生产案例背书 | P5 |
| Web | **独立前端工程**（复用后端 API + WS 协议） | 浏览器无 QUIC（WebTransport 未普及）；**Transport trait 是前置**（ADR-8） | P5 |
| 本地消息库（全端） | 客户端 SQLite（打开即渲染，不等服务器）；万人群只缓存近期窗口+主动漫游区间 | 现有 UI 无本地库 | P2 |
**Flutter-OH 仓库地址勘误（第四轮实证）**：主仓库**不在 GitHub**，2026 年已整体迁移至 **GitCode `openharmony-tpc/flutter_flutter`**（AtomGit 镜像同步；组织 CPF-Flutter）。生产用 **3.27.4 稳定分支**，新特性 `oh-3.35.7-dev`，3.41 已发布；兼容库在 `openharmony-tpc/flutter_packages`。在 GitHub 上找会扑空。

---

## 第七部分：参考项目总表（统一格式，全四轮合并）

> 格式：项目 | 栈 | 链接 | 许可状态 | 借什么 | 用在 wxlike 哪里。
> 许可：✅已确认可借（搬代码保留声明）· ⚠️待核（核后定） · ❌无 LICENSE 不可抄（只学思想） · ➖闭源/不适用。

### 7.1 万人群 / 弹幕 / IM 后端

| 项目 | 栈 | 链接 | 许可 | 借什么 | 用在哪 |
|---|---|---|---|---|---|
| WuKongIM | Go | https://github.com/WuKongIM/WuKongIM | ⚠️待核 | 频道订阅发布模型、群成员无上限约束、1 字节心跳/二进制协议、自包含存储倾向 | P3 读扩散管道 + 频道模型 |
| OpenIM | Go | https://github.com/openimsdk/open-im-server （SDK：https://github.com/openimsdk） | ⚠️待核（社区普遍 Apache-2.0） | **判断→增量→兜底→批量→懒加载**五层成员同步契约 | P3 sync 服务协议规范 |
| goim (Bilibili) | Go | https://github.com/Terry-Mao/goim | ⚠️待核 | 分层推送（活跃实时/非活跃聚合）、房间管理、千万长连接 | P3 弹幕/高频频道推送 |
| only-talk-rs | Rust | https://github.com/nicolastinger/only-talk-rs | ⚠️待核 | 工程骨架（Actix+QUIC+PG+Redis+S3+JWT 分层） | 网关/服务拆分参考 |
| ISM | Rust | https://github.com/JrTimha/ISM | ⚠️待核 | 消息正文（NoSQL）与元数据（PG）分离 | P6 存储演进 |
| flare-core | Rust | https://github.com/flare-im/flare-core | ⚠️待核 | WS/QUIC/TCP 传输层：心跳/重连/序列化协商/慢消费者隔离 | ADR-8 Transport trait 参考 |

### 7.2 企业级 / 全球平台

| 项目 | 栈 | 链接 | 许可 | 借什么 | 用在哪 |
|---|---|---|---|---|---|
| Rocket.Chat | JS | https://github.com/RocketChat/Rocket.Chat | ✅ MIT（待核复核） | 私有化部署形态、频道/角色模型 | P3 频道权限 |
| Mattermost | Go | https://github.com/mattermost/mattermost | ⚠️MIT 待核 | 企业级群管理、合规 | P6 管理后台 |
| Element Web | TS | https://github.com/element-hq/element-web | ✅ Apache-2.0 | Matrix 客户端 E2EE 交互（密钥验证 UI） | P4 秘密聊天 |
| Synapse | Python | https://github.com/element-hq/synapse | ✅ AGPL-3.0（与本项目同许可，可直接复用） | 联邦/房间模型 | 参考 |
| Telegram Android | Java/Kotlin | https://github.com/DrKLO/Telegram | ✅ GPL-3.0 | 消息列表回收复用、弱网策略 | P2 虚拟列表 |
| Telegram iOS | Swift | https://github.com/TelegramMessenger/Telegram-iOS | ✅ GPL-3.0 | 移动端内存管理 | P5 |
| Telegram Desktop | C++/Qt | https://github.com/telegramdesktop/tdesktop | ✅ GPL-3.0 | 桌面网络层 | P2 |
| Telegram Web | TS | https://github.com/morethanwords/tweb（官方 Web） | ✅ GPL-3.0 | Web 端 WS 架构 | P5 Web |
| TDLib | C++ | https://github.com/tdlib/td | ✅ Boost 1.0 | **跨平台核心库设计范式（= 我们 core 的目标形态）** | 全局 |
| teamgram-server | Go | https://github.com/teamgram/teamgram-server | ⚠️待核 | MTProto 服务端分层 | 协议参考 |
| Gramsrv / MyTelegram | Go / C# | https://github.com/iamxvbaba/gramsrv · https://github.com/loyldg/mytelegram | ⚠️待核 | 协议实现参考 | 参考 |
| hungry-rs / grammers / ferogram / tuigram / telegram_rust | Rust | https://github.com/dimentyy/hungry-rs · https://github.com/Lonami/grammers · https://github.com/ankit-chaubey/ferogram · https://github.com/erictran308/tuigram · https://github.com/oovets/telegram_rust | ⚠️待核（grammers MIT） | Rust 处理复杂加密协议/异步架构 | core 演进 |

### 7.3 国内开源 IM

| 项目 | 栈 | 链接 | 许可 | 借什么 | 用在哪 |
|---|---|---|---|---|---|
| 唐僧叨叨 | Go+Vue+Android | Server: https://github.com/TangSengDaoDao/TangSengDaoDaoServer · Web: https://github.com/TangSengDaoDao/TangSengDaoDaoWeb · Android: https://github.com/TangSengDaoDao/TangSengDaoDaoAndroid | ✅ Apache-2.0（任务书已核） | **微信还原度最高**：消息操作五件套模型、UI 交互 | P1 五件套 / P2 UI |
| Startalk | 多端 | https://github.com/startalkIM | ⚠️待核 | Docker 部署方案 | P6 |
| XyTalk | Java 等 | https://github.com/xy-Group/xyTalk-pc （iOS 在 Gitee） | ⚠️待核 | 企业场景参考 | 参考 |
| box-im | Java+Vue+UniApp | https://github.com/bluexsx/box-im | ✅ MIT（任务书已核） | 全栈仿微信（MQ 设计已借） | P1/P2 |
| HuLa（+ HuLa-Server） | Rust+Tauri2+Vue3 | https://github.com/HuLaSpark/HuLa · https://github.com/HulaSpark/HuLa-Server | ✅ Apache-2.0（任务书已核） | **前端多端工程组织、UI 组件、Tauri 移动配置**；注意 Web 端不支持 | P2 UI 打磨 / P5 移动 |

### 7.4 微信高仿（多数无 LICENSE——只学思想，不抄代码）

| 项目 | 栈 | 链接 | 许可 | 借什么 |
|---|---|---|---|---|
| LQRWeChat | Android | https://github.com/GitLqr/LQRWeChat | ❌无 LICENSE | MVP 架构思想 |
| vue-WeChat | Vue2 | https://github.com/arden-kfc/vue-WeChat | ❌待核 | WeUI 交互 |
| CoderMikeHe/WeChat | iOS | https://github.com/CoderMikeHe/WeChat | ❌待核 | MVVM 组织 |
| SwiftUI-WeChat | SwiftUI | https://github.com/wxxsw/SwiftUI-WeChat | ⚠️MIT 待核 | 声明式 UI 写法 |
| fake-world | React | https://github.com/shanyu-e/fake-world | ⚠️待核 | 聊天记录数据模型 |
| WeChatArkTS | ArkTS | https://github.com/xiaowuDev/Wechat_ArktsDevelop | ❌待核 | 鸿蒙 UI 思路 |

### 7.5 基础设施 / 跨平台框架

| 项目 | 栈 | 链接 | 许可 | 借什么 | 用在哪 |
|---|---|---|---|---|---|
| sandcat-backend | Rust 微服务 | https://github.com/Xu-Mj/sandcat-backend | ⚠️待核 | 服务拆分、PG/Mongo 混合分片 | P6 存储演进 |
| Mimir | Rust+UniFFI | https://github.com/Mimir-IM/mimir | ⚠️待核 | **UniFFI 跨平台绑定范式** | P5 移动端 |
| iroh | Rust | https://github.com/n0-computer/iroh | ✅ Apache-2.0/MIT | P2P/NAT（远期去中心化选项） | 参考 |
| PaperPhone-Plus | React+Axum | https://github.com/619dev/Paperphone-plus | ⚠️待核 | 微信风格 E2EE 全栈 | P4 参考 |
| Tauri | Rust | https://github.com/tauri-apps/tauri | ✅ MIT/Apache-2.0 | 桌面/移动容器（现役） | 全端 |
| Flutter | Dart | https://github.com/flutter/flutter | ✅ BSD-3 | 移动端 UI（P5） | P5 |
| ArkUI-X | ArkTS | https://github.com/arkui-x （主仓 GitCode/Gitee） | ✅ Apache-2.0 | 鸿蒙三端（P5 备选） | P5 |

### 7.6 支付基础设施（第三轮新增，ADR-12 配套）

| 项目 | 栈 | 链接 | 许可 | 借什么 | 用在哪 |
|---|---|---|---|---|---|
| Cala (cala-ledger) | Rust | https://github.com/GaloyMoney/cala · 文档 https://docs.rs/cala-ledger | ⚠️待核（Galoy 系通常 Apache-2.0） | 嵌入式复式记账+事件溯源+persistent outbox（Galoy 曾支撑国家级钱包运营，ISO 27001） | P4 账本核心 |
| ousia-ledger | Rust | https://github.com/TheOusia/ousia | ⚠️待核 | 两阶段执行（内存规划+单事务微秒级锁）、不可变 ValueObject（Alive→Reserved/Burned 单向转换） | 备选账本引擎 |
| TigerBeetle / tb-rs | Zig / Rust 客户端 | https://github.com/tigerbeetle/tigerbeetle · https://github.com/dsp/tb | ⚠️待核（TigerBeetle Apache-2.0） | 专用金融账本数据库（PG 吞吐瓶颈时的存储备选） | 远期备选 |
| dtmrs | Rust | https://github.com/jackwangfeng/dtmrs | ⚠️待核 | 嵌入式 Saga/TCC/二阶段消息/XA/workflow（分支可为本地函数，不强制 HTTP） | P4 交易协调 |
| Hyperswitch | Rust | https://github.com/juspay/hyperswitch · https://docs.hyperswitch.io | ✅ Apache-2.0 | `ConnectorIntegration` 渠道适配 trait 范式、幂等键、FSM 状态机 | P4 渠道适配层设计 |
| obix | Rust | https://github.com/GaloyMoney/obix | ⚠️待核 | 事务性 Outbox/Inbox（顺序持久存储+实时临时事件，pg_notify 防注入） | P4 账本事件流 |

**初始化顺序（第三轮建议，采纳）**：先嵌 cala-ledger 跑通复式记账+outbox → 引 obix 消费账本事件 → 跨服务补偿叠加 dtmrs → 真正写银行/清算适配器时再对照 Hyperswitch 的 trait 模式，避免渠道复杂度过早侵入账本核心。

### 7.7 直播 / 媒体面 / 弹幕（第四轮新增，ADR-13 配套）

| 项目 | 栈 | 链接 | 许可 | 借什么 | 用在哪 |
|---|---|---|---|---|---|
| Xiu | Rust | https://github.com/harlanc/xiu | ⚠️待核 | 纯 Rust 直播服务器，协议最全：RTMP（含集群）/RTSP/WebRTC（WHIP/WHEP）/HTTP-FLV/HLS；OBS 可推流；cargo install/Docker 皆可 | P5 直播频道媒体面**首选** |
| atm0s-media-server | Rust | https://github.com/8xFF/atm0s-media-server（RTMP 需 legacy 分支 `tree/legacy`） | ⚠️待核 | 去中心化全球 SFU（WebRTC/WHIP/WHEP/RTMP/SIP；atm0s-sdn + Quinn 虚拟网络）。**重构期警告：sans-io-runtime 重写中，RTMP/SIP 未完成** | 长期观察，暂不作生产依赖 |
| mediasoup（Rust crate） | Rust/C++ | https://github.com/versatica/mediasoup （crate 在 `v3/rust/`；docs.rs/mediasoup） | ✅ ISC（待核复核） | SFU 语义最成熟：Simulcast/SVC/选择性转发（57 个版本） | P5 复杂自适应转发评估 |
| discord/sorted_set_nif | Rust | https://github.com/discord/sorted_set_nif | ⚠️待核 | **Discord 官方开源**的 Rustler NIF：有序集合（成员列表）高性能维护——看 Discord 怎么用 Rust 增强 Elixir 网关 | 网关数据结构参考 |
| Discord 架构文档（社区整理） | 文档 | https://github.com/babushkai/system-design-patterns/blob/codex/article-reading-redesign/08-case-studies/08-discord.md | ➖ | Elixir/OTP 网关 + Rust 消息存储与媒体代理 + Cassandra（消息）/ScyllaDB（附件）/Redis（会话·PubSub）分治；sub-100ms 投递；10 万成员 mega-guild 设计目标 | ADR-13 全局参考 |
| blivedm_rs | Rust | https://github.com/isomoes/blivedm_rs（备选 fork：https://github.com/jiahaoxiang2000/blivedm_rs，使用前对比 commit 活跃度） | ⚠️待核 | B 站直播间弹幕 WS 客户端（Tokio 高并发、TTS、Cookie 检测） | 弹幕协议/高频消息参考 |

**数据点（第四轮）**：Rust 弹幕场景压测 **QPS ≈ 45 万/s，p99 ≈ 9.8ms**——弹幕/信令层选 Rust 有数据支撑，代价是工程迭代速度慢于 Go，纳入权衡。

---

## 第八部分：第二轮之外的新增补充（此前所有轮次都没覆盖到的）

1. **红包卡片"消息变更"回写缺失（实证）**：领取红包当前只发一条新 `claim` 消息，原卡片不更新。需按腾讯范式补"消息变更广播"（`msg_id` + 新 payload），UI 收到后原地更新卡片状态。→ P4
2. **Transport trait（传输层抽象，实证）**：`Client` 直接持有 `quinn::SendStream`，Web 端（浏览器无 QUIC）接入前必须抽象出 `Transport`（framing 已经独立，改造成本可控），WS 与 QUIC 双实现。→ P3 前置
3. **本地消息库（客户端 SQLite）**：所有成熟端都是"打开即本地渲染"，当前 UI 每次进会话都是空窗口。万人群只缓存近期窗口 + 主动漫游。→ P2
4. **慢消费者隔离**：flare-core 的思路——fanout 时对写不动的连接丢弃/降级而非阻塞全局（与 goim 分层推送互补）。→ P3 网关改造一并
5. **通知聚合**：万人群不能每条一推；按"群维度摘要 + 频次窗口"聚合后走厂商通道。→ P3 push 扩展
6. **钱包对账任务**：有 ledger 无对账。定时任务核对 `sum(balances)+escrow == money_supply`，不一致告警。→ P4
7. **支付密码**：红包/转账的二次确认因子（虚拟闭环也需要，防"借用电脑"误操作）。→ P4
8. **管理后台**：用户/群/频道管理、封禁、举报处理、内容审核入口——成熟产品全有，当前为零。→ P6（先做最小 CLI 工具）
9. **会话级 Pin/免打扰/归档的协议字段**：客户端列表体验的地基，服务端要有 per-user 会话属性表。→ P3
10. **SLO 定义**：把 p99<50ms（投递）、断连率 0、可用性目标写成 docs/slo.md 并挂告警——压测已达标但没固化成 SLO。→ P6
11. **直播信令独立通道（第四轮）**：直播的加入/离开/切流信令走独立高优先级通道，绝不混入普通聊天消息流——聊天管道再快也不能让信令排队。→ P5
12. **弹幕不单独建消息系统（第四轮）**：弹幕就是读扩散管道里的高频消息，P3 分层推送（goim 思路）直接承载；Rust 弹幕压测 45 万 QPS / p99 9.8ms 证明该层选 Rust 有数据优势。→ P3/P5
13. **语言分层认知（Discord 启示，第四轮）**：长连接网关与媒体转发对语言特性的要求完全不同（BEAM 轻进程容错 vs 无 GC 媒体帧处理）；全 Rust 路线的代价是自行实现监督与背压——P3 网关改造的慢消费者隔离要按这个标准设计。→ P3
14. **atm0s-media-server 重构期纪律（第四轮）**：sans-io-runtime 重写分支的 RTMP/SIP 未完成；需要 RTMP 时切 legacy 分支，生产媒体面首选 Xiu。→ P5 选型纪律
15. **小程序包格式复用 DPG1（第五轮）**：仓库已有自有包格式（magic+版本+id+zstd+manifest），做小程序只需补签名字段与 capabilities 声明，**无需兼容微信也无需新造格式**——此前各轮未点明的现成资产。→ ADR-14
16. **小程序与 IM 渲染栈隔离（第五轮）**：小程序视图以 Wry 子 WebView 子面板存在，聊天列表/消息流仍由 Flutter/Tauri 原生渲染，两者只经 Bridge 通信——小程序崩溃不拖垮 IM 主界面。→ ADR-14
17. **撤回的跨端状态同步是读扩散特有问题（第六轮）**：写扩散下撤回随信箱走；读扩散下撤回=逻辑删除+**信箱变更事件广播**，离线端重连补同步——变更事件通道必须在 P3 管道设计时预留。→ P3
18. **万人群已读回执默认关闭、只做未读（显式入册）**：已读回执在万人规模下是通知与存储炸弹；未读数走 ZSET 排名差。回执仅保留在 ≤500 人小群与 1:1。→ P3
19. **通知多级过滤（第六轮）**：按会话/频道/关键词的多级免打扰策略，是万人群高频消息→通知轰炸的解药，与推送聚合配套。→ P3
20. **消息消费细节决定基础体验（第六轮）**：语音波形+转文字、视频首帧+倍速播放——用户预期最高的两类细节，随语音条/视频消息一并做（P2）。

---

## 第九部分：合并路线图（终版；在 P0–P6 基础上吸收第二轮"第 0–5 步"，不删旧项）

| 阶段 | 内容（新增项加粗） | 规模 |
|---|---|---|
| **P0 决策锁定 ✅（2026-10-16 三项全部确认）** | ① 云同步=Telegram 式服务端全量存储（ADR-1）② 移动端=Flutter 统一基座，鸿蒙 Flutter-OH+ArkTS 桥接（ADR-11）③ 读扩散立项（ADR-2→ADR-16 修订为三支柱统一信箱） | 已锁定，P1 起全面可执行 |
| **P1 已采纳第一批** | E2EE 接入客户端（回退日志+秘密聊天雏形）· 消息操作五件套 · **UI 历史消息加载** · 已读回执 UI | 1–2 周 |
| **P2 真机验收 + 体验** | 双机音视频验收 · tauri dev 演示 · Redis 真机 · **语音条/文件消息/贴纸 · 消息列表虚拟化 · 本地消息库 · 头像接媒体 · mac/Linux 构建验证** | 1–2 周 |
| **P3 万人群新架构** | 读扩散管道（信箱/游标/版本差分/ZSET 未读，五层同步契约）· **Transport trait + WS 网关** · **多设备多连接改造** · 频道层 · 话题层 · 群管理补齐（禁言/公告/审批/邀请链接/慢速/成员搜索）· 拉黑 · 服务端未读态 · **通知聚合（+多级免打扰过滤） + 慢消费者隔离 · 会话 Pin/免打扰/归档 · NATS backend（MQ trait 新实现）· 弹幕式高频消息（读扩散承载+分层推送）· 撤回跨端状态同步（信箱变更事件）· 频道分组/频道级角色/话题订阅 · 审计日志起步 · Meilisearch/Tantivy 搜索接入** | 4–6 周 |
| **P4 账号 / 隐私 / 资金** | 找回+captcha/限流 · argon2id（可选）· 2FA · 秘密聊天完整+消失消息 · 红包卡片消息变更回写 · 支付密码 · 钱包对账 · 通话房间制 · 设备管理页 · **支付四层架构落地（ADR-12：cala-ledger 账本核心 → obix 事件流 → dtmrs Saga → ChannelConnector 渠道适配，初始化顺序照第三轮建议）· 内容审核管线（Lexfence 模式微服务）· 举报工单闭环 · 语音转文字/波形/视频首帧 · 会议管控（静音/踢人/锁定/录制）** | 3–4 周 |
| **P5 移动端与多语言 + 直播** | **Flutter 统一基座（ADR-11）**：Android · iOS · 鸿蒙（Flutter-OH + ArkTS 桥接，后台保活专项；仓库在 GitCode）· Rust core 经 UniFFI/cargo-ndk 出绑定 · Web 独立工程 · 多语言 · 数据导出 · **直播频道（ADR-13：Xiu RTMP ingest + WebRTC 播放闭环起步；mediasoup 评估 Simulcast；信令独立通道；弹幕复用读扩散）** · **小程序 M1–M5（ADR-14：Rong 逻辑层 → Wry/LingXia 视图 Bridge 闭环 → 多引擎多平台 → Wasmtime 沙箱 → 包管理/签名/权限接后端）** | 4–8 周（小程序可与移动端并行推进，独立轨道） |
| **P6 发布工程与规模** | 安装包/bundle · release CI · TLS 反代 · TURN/FCM 部署文档 · **PG 迁移+分片 · 管理后台（adminx：RBAC/MFA/审计检索）· 运营数据看板 · 内容审核完整闭环 · Bot 账号体系+开放 API/Webhook · SSO/组织架构预留 · 存储加密（AES-256/国密按合规）· 无障碍（WCAG AA/200% 字号/读屏）· 多语言 · k8s · 备份 · SLO 固化** | 2–4 周 |

### 第六轮优先级矩阵 → 本路线图映射

| 第六轮优先级 | 功能簇 | 落入 |
|---|---|---|
| P0（万人群管道强绑定） | 频道/话题订阅与权限 · 全局搜索（本地+云端）· 撤回跨端同步 · 审计日志 | **本表 P3**（读扩散批次） |
| P1（运营与安全底线） | RBAC+MFA · 内容审核管线 · 举报工单 · 后台数据看板 | **本表 P4/P6** |
| P2（体验与增长） | 音视频/屏幕共享 · Bot 体系 · AI 助手 · 深色/国际化/无障碍 | **本表 P2/P5/P6** |
| P3（生态延伸） | 开放 API/Webhook · SSO/组织架构 | **本表 P6+** |

---

## 第十部分：一句话总纲

**wxlike 已经有的是一个经过对抗测试的 Rust IM"引擎层"**（协议/加密/传输/存储/群路由/通话内核，约 2.2 万行、254 测试）；要成为七端、万人群、带支付与小程序的完整产品，缺的是**读扩散管道、多设备、频道话题（P3）+ 移动端（P5）+ 产品化外壳（P1/P2）**。参考系已齐：WuKongIM 给万人群架构答案、OpenIM 给同步契约、goim 给弹幕推送、HuLa/唐僧叨叨给前端与功能模型、TDLib 给核心库范式、**cala/dtmrs/Hyperswitch/obix 给支付账本四层**——全部是"重新实现于 Rust 之上的参考"，没有一个是拿来即用；这正是路线图把 P3（读扩散）和 P5（移动端）定为最大工程块的原因。

**P0 已于 2026-10-16 全部锁定（云同步=Telegram 式、移动端=Flutter 基座、读扩散立项），支付账本架构（ADR-12）与移动端基座（ADR-11）同步入册——路线图自 P1 起全面可执行，无阻塞项。第四轮补充直播为频道级能力（ADR-13）：工程量集中在媒体面（Xiu/mediasoup），弹幕与互动几乎免费搭读扩散管道的车——这是把直播排在 P5（读扩散落地之后）而非单独立项的原因。第六轮补上最后一块拼图：**运营与合规体系（ADR-15）**——审计日志/RBAC+MFA/审核管线/防泄密以独立服务叠加，消息管道不需要回头推翻。至此架构蓝图收敛：引擎层（已建成）→ 读扩散+多设备+频道话题（P3）→ 运营合规（P4/P6）→ 移动端与直播（P5）→ 发布工程（P6）。**

---

## 第十一部分：终版架构树（第七轮收敛，权威基线）

> 以下两棵树 + 决策速查 + 九大不变式为最终收敛结果，**作为服务拆分、schema 设计、
> API 边界、客户端会话列表聚合的基线**。与前面各部分冲突时，以本部分为准
> （冲突点已标入 ADR-16：ADR-2 的"小群保留写扩散"条款被三支柱统一读扩散取代）。

### 11.1 产品功能树（用户视角，最终版）

```text
类微信/类Discord平台
├── 1. 账号与身份
│   ├── 用户名+密码注册（不强制手机验证）
│   ├── 可选邮箱验证（找回/防滥用）· 恢复码 · 多设备登录管理（远程下线）· 2FA(TOTP)
│   ├── 个人资料（头像/昵称/简介/状态）· 二维码名片（username/user_id）
│   └── 账号注销 / 数据导出
├── 2. 社交结构（三大并列支柱）【ADR-16】
│   ├── 2A. 好友关系+单聊 ★支柱A
│   │   ├── 申请/新的朋友 · 备注/标签/分组 · 星标 · 拉黑 · 通讯录同步（可选）
│   │   ├── 好友推荐（recommendation-service）· 共同群聊/共同好友
│   │   ├── 单聊会话（owner=规范化用户对）：全消息类型/引用/转发/撤回/编辑/搜索/
│   │   │   置顶/标记未读/已读回执(可选)/正在输入/消失消息(可选)/一键撤回本次全部
│   │   └── 陌生人消息请求（ADR-24）：请求箱→接受/拒绝→屏蔽；可配置开关
│   ├── 2B. 群组 Group ★支柱B
│   │   ├── 成员上限可配置(默认500) · 群主/管理员(≤10) · 公告 · 禁言(定时解除)
│   │   ├── 入群确认(40人+默认) · 群二维码(invite_token) · 群收款/群接龙 · 群文件
│   │   ├── 群昵称 · @所有人(owner/admin) · 转让/解散 · 折叠/保存到通讯录/免打扰
│   │   └── 群消息（owner=group_id，写扩散过渡→读扩散）
│   └── 2C. 社区/频道/话题 ★支柱C
│       ├── 社区 Community（万人 member_limit）：公告/社区级角色/成员管理/发现页入口
│       ├── 频道 Channel：类型=文字/公告只读/语音/直播；频道级权限；邀请链接；
│       │   消息（owner=channel_id，序列号独立）
│       └── 话题 Thread：任意频道消息发起；回复聚合于父消息下不占主序列；
│           订阅/取消订阅；话题内成员列表
├── 3. 会话与消息
│   ├── 统一会话列表（direct/group/channel 聚合，按最后消息时间）
│   ├── 未读数（read-service 统一提供）· 免打扰/置顶/归档
│   ├── 全局搜索（search-service：消息/群组/频道/社区/用户/话题）
│   └── 消息类型注册表+三通道分级【ADR-17】
│       ├── normal：文本/图/视频引用/文件/小程序卡片/广告卡片/红包卡片 → 信箱+序列号+持久化
│       ├── high_priority：直播信令/正在输入/实时弹幕 → 独立 NATS 主题，不占序列号只投在线
│       └── ephemeral：红包领取状态/广告回执/编辑状态 → 落库不进时间线，只更新卡片
├── 4. 音视频与直播（独立媒体服务）【ADR-21】
│   ├── 1:1 通话(WebRTC) · 群通话房间（独立于消息管道）· 直播频道（频道的一种类型）
│   ├── RTMP ingest(OBS) · WebRTC/HLS 播放 · 连麦/桌面共享 · 屏幕共享
│   └── 短视频（video-service 点播 HLS；消息只传结构化引用）
├── 5. 内容与发现（三层分离）【ADR-22】
│   ├── 发现页（展示层：分类/热门/新晋/标签——community-service 聚合排序）
│   ├── 搜索（search-service：本地+云端+时间范围）
│   ├── 推荐（recommendation-service：频道/视频/广告/好友）
│   ├── 朋友圈/状态/动态（可选 feed-service/status-service）
│   └── 邀请与裂变（链接/邀请码/奖励→余额体系）
├── 6. 支付与钱包（三账本分离）【ADR-18】
│   ├── wallet-service：充值/余额支付/红包/群收款/转账/支付密码/流水/提现冻结
│   ├── ad-account-service：预付款/扣费/信用额度/退款
│   ├── marketing-account-service：红包补贴/邀请奖励/广告红包（两笔独立分录）
│   └── ledger-service：复式记账内核（account_type 隔离）
├── 7. 小程序（容器统一，Bridge 分层）【ADR-14】
│   ├── 入口/最近使用 · 聊天内小程序卡片（normal 通道）
│   ├── host_bridge（生命周期/存储/网络/UI）· im_bridge（受限白名单：发消息/会话/拉起支付）
│   └── 支付回调转发 → payment-service（不碰账本）
├── 8. 广告
│   ├── 信息流广告（ad_card 消息，normal 通道）· 会话列表广告 · 小程序广告
│   ├── 广告红包/任务（ad-account → marketing → 用户钱包）
│   └── 广告主后台（活动/定向/出价/预算/报表/CTR 预测）
├── 9. 安全与治理（三层分离）【ADR-20】
│   ├── risk-service（前置实时拦截：注册/发送/登录风控）
│   ├── moderation-service（后置多模态判定，先审后发/先发后审）
│   ├── report-service（举报工单：待处理→审核中→已处置→已反馈）
│   ├── 隐私设置/聊天锁/隐藏会话 · 合规（青少年模式/日志留存/广告标识/协议）
├── 10. 设置与辅助
│   ├── 深色/主题/字号 · 多语言 · 无障碍（对比度/缩放/读屏）
│   ├── 通知自定义（会话/关键词）· 数据导出 · 账号注销
└── 11. 客服与申诉
    ├── 应用内工单 · 封禁申诉 · 帮助中心/FAQ · 支付争议（联动 ledger 流水）
```

### 11.2 技术架构树（服务视角，最终版）

```text
平台技术架构
├── A. 客户端层
│   ├── 移动端：Flutter + Flutter-OH（ADR-11）· 桌面：Tauri 2 / Flutter Desktop
│   ├── Web：独立工程（复用 API/WS 协议）
│   └── 小程序容器：LingXia/Wry + 分层 Bridge（host_bridge + im_bridge 白名单）
├── B. 接入层
│   ├── WebSocket 网关（Tokio 无状态，连接态存 Redis）
│   ├── HTTP API 网关（Axum；开放 API 可选 ShenYu/Otoroshi）· CDN
│   └── push-gateway（APNs/FCM/华为/小米/OPPO/vivo——只发出去，不做决策）【ADR-19】
├── C. 业务服务层
│   ├── 社交三支柱【ADR-16】
│   │   ├── relation-service：friendships/requests/blocklist/message_requests/saved_groups/共同聚合
│   │   ├── group-service：CRUD/成员角色/公告/禁言/审批/邀请token/收款/接龙/文件/@校验/转让解散
│   │   └── community-service + thread-service：社区/频道四类型/频道权限/成员/话题/订阅/发现聚合
│   ├── 消息与同步【ADR-16/17】
│   │   ├── message-service：读扩散信箱（owner_type+owner_id 分区，序列号独立）/
│   │   │   类型注册表+三通道/撤回编辑（逻辑删除+变更广播）/批量撤回
│   │   ├── sync-service：成员/权限/话题版本差分，五层策略
│   │   └── read-service：已读游标(PG)/未读 ZSET/已读回执（可选，万人群默认关闭）
│   ├── 通知与推送【ADR-19】
│   │   ├── notification-service：在线查询/多设备去重/聚合 → PushTask
│   │   └── push-gateway：传输层
│   ├── 支付与账本【ADR-12/18】
│   │   ├── ledger-service（cala 嵌 PG：四类 account_type/不可变分录/余额重放/obix outbox）
│   │   ├── wallet-service · ad-account-service · marketing-account-service
│   │   └── payment-service（渠道适配，不碰账本）
│   ├── 内容分发【ADR-22】
│   │   ├── recommendation-service（行为事件流→双塔+Faiss 召回→Wide&Deep 排序）
│   │   ├── search-service（Meilisearch/Tantivy，事件订阅更新索引）
│   │   └── feed-service/status-service（可选）
│   ├── 媒体【ADR-21】
│   │   ├── media-service（实时 SFU：通话/房间/直播信令与转发——有状态）
│   │   └── video-service（点播：FFprobe/FFmpeg→HLS→缩略图——无状态任务队列）
│   ├── 平台化
│   │   ├── mini-app-service（包管理/签名/灰度/权限白名单/支付回调转发——不执行代码）
│   │   └── ad-service（Ad Server：活动/定向/eCPM/频控/预算平滑 + 计费信标 + 扣费指令→ad-account）
│   └── 安全治理【ADR-20】
│       ├── risk-service（前置毫秒级拦截）· moderation-service（后置判定 API）
│       └── report-service（工单流程，处置回调各服务）
├── D. 数据层
│   ├── PostgreSQL：用户/关系/群/社区/频道/话题/游标/账本分录/广告/审计日志(≥6个月)/全表 tenant_id【ADR-23】
│   ├── Redis：在线态/设备映射/未读 ZSET/红包原子/频控滑窗
│   ├── 队列：NATS（实时路由/high_priority/状态变更）· Kafka（离线审计/推送触发/埋点/索引更新）
│   ├── 对象存储：图/音/文件 · media/live/{room} · video/{id} · mini-app/{app}/{ver}
│   ├── 搜索索引（Meilisearch/Tantivy）· 账本存储（cala 嵌 PG）· 分析库（可选 ClickHouse）
├── E. 基础设施层
│   ├── OpenTelemetry 追踪（Jaeger/Tempo 7-30天）· Prometheus+Grafana · Loki（30-90天）
│   ├── 配置中心（动态开关/灰度/分桶）· 管理后台（adminx：RBAC/MFA/工单/提现双人复核/只读 audit_log）
│   ├── CI/CD（Rust workspace + Flutter 多端 + Tauri 打包）· 密钥管理
└── F. 跨切面能力
    ├── 事件埋点管道（三端统一 schema → ClickHouse/推荐特征；广告归因链路）
    ├── 分层权限模型（平台 RBAC/社区角色/群角色/频道权限/小程序白名单/广告主权限）
    └── 多租户预留【ADR-23】
```

### 11.3 决策落点速查

| # | 决策 | 产品树位置 | 技术树位置 |
|---|---|---|---|
| 1 | 社交三支柱并列（ADR-16） | 2A/2B/2C | relation/group/community+thread-service |
| 2 | 消息类型注册表+三通道（ADR-17） | 3 | message-service |
| 3 | 三账本命名空间（ADR-18） | 6 | ledger+wallet+ad-account+marketing-account |
| 4 | 推荐/搜索/发现三层分离（ADR-22） | 5 | recommendation/search/community 聚合 |
| 5 | 风控/审核/举报三层分离（ADR-20） | 9 | risk/moderation/report-service |
| 6 | media/video 分离（ADR-21） | 4 | media-service/video-service |
| 7 | 通知决策/传输分离（ADR-19） | 3 通知 | notification-service/push-gateway |
| 8 | 审计入 PG、运行日志入 Loki（ADR-15） | 9/10 | audit_log 表 / Loki / OTel |
| 9 | 小程序 Bridge 分层（ADR-14） | 7 | host_bridge + im_bridge 白名单 |
| 10 | 多租户预留（ADR-23） | 全树 | 全表 tenant_id + 鉴权中间件 |

### 11.4 九大锁定不变式

1. **社交三支柱并列**，共用读扩散信箱基础设施，`owner_type` 区分，不写三套投递逻辑；
2. **三种 owner 序列号独立生成**（direct=规范化用户对 / group / channel）；万人是 community 的 member_limit，不是群组默认上限；
3. **只有 normal 通道占用序列号并进时间线**（直播信令/正在输入/红包状态变更走独立通道）；
4. **任何资金变动只经 ledger-service 复式记账接口**（wallet/ad/marketing 是账户命名空间，不是三个引擎）；
5. **push-gateway 不知道免打扰与聚合**（只收已决策的 PushTask）；
6. **小程序只能经权限白名单调 im_bridge 受限子集**（不可直接触账本/消息管道/管理接口）；
7. **审计日志与运行日志永不混存**（管理后台操作记录来自 PG audit_log，故障日志来自 Loki）；
8. **全表 tenant_id 但当前不实现租户隔离**（字段预留不是功能）；
9. **陌生人单聊必须经 message_requests 流程**（是否允许是配置，流程必须存在）。

---

## 第十二部分：待确认疑问（✅ 已全部拍板——2026-10-16 按建议答案确认，并入外包合同）

> **确认记录**：Q1 三支柱统一读扩散（含单聊，ADR-2 双轨条款废止）· Q2 新建 message-service 模块（旧写扇出转过渡）· Q3 渠道开放账本自建（微信虚拟支付作为 ChannelConnector 渠道适配器之一，资金真相仍在自有账本）· Q4 E2EE 秘密聊天保留（P4，1:1 范围）· Q5 群接龙 P4 早 / 群收款 P4 末·P5 · Q6 video-service 最小版提前 P2（完整版 P5），recommendation/feed P6+ · Q7 模块化单体起步（push-gateway/media/video/search 四者天生独立进程）· Q8 国内推送厂商账号即日启动申请（P5 前置）。

原疑问表存档：

| # | 疑问 | 建议答案 |
|---|---|---|
| Q1 | 终版树规定**三支柱全部走读扩散信箱**（含单聊），取代 ADR-2 的"小群保留写扩散"条款。确认吗？ | 建议确认（单信箱+owner_type 比双轨简单且够用）；现 mq_hub 写扇出标记为过渡实现 |
| Q2 | 新 message-service 与现有 mq_hub 的关系：**新建独立服务逐步下线旧投递路径**，还是原地演进？ | 建议新建（读扩散数据模型与写扇出结构差异过大，原地演进会长期双轨并存） |
| Q3 | 产品树钱包"充值（人民币，**微信虚拟支付**或自建渠道）"与 ADR-12"完全自建渠道"是否矛盾？ | 需确认渠道清单：若允许微信虚拟支付，payment-service 增加该渠道适配器即可（与"自建"不互斥，只是渠道来源） |
| Q4 | 终版产品树**未出现 E2EE/秘密聊天**——保留（ADR-1，P4）还是移出范围？ | 建议保留（已建成 17 测试的库资产，弃用可惜）；树中补一行"秘密聊天（可选）" |
| Q5 | 群收款/群接龙的范围与优先级？ | 依赖钱包与群服务就绪（P4 后）；建议接龙先做（纯消息）、收款后做（涉及账本） |
| Q6 | recommendation/video/feed 三个重资源服务确认排 P6+/独立轨道？ | 建议确认（树内存在但非 MVP；video-service 因语音条/视频消息依赖，P5 先行） |
| Q7 | ~20 个服务的拆分节奏：**模块化单体起步按阶段拆分**，还是直接微服务化？ | 强烈建议前者：进程内按树分层（crate 边界），P3/P6 按负载拆出网关/信箱/账本——工数差数倍 |
| Q8 | 国内推送厂商通道（华为/小米/OPPO/vivo）需逐家开发者账号与审核——运营前置项确认 | 属实，P5 前置准备 |

---

## 第十三部分：路线表详细版（终版；每项标注现状/交付）

> 状态：✅ 已实现有测试 · ⚠️ 雏形/部分 · ❌ 未实现。服务名对应 11.2 技术树节点。

### P1 体验补齐（1–2 周，无阻塞）

| 项 | 现状 | 交付 |
|---|---|---|
| E2EE 接入客户端消息路径（秘密聊天雏形+回退日志） | ⚠️ 库✅ | core::e2ee → ui 发送路径 + 回退 warn |
| 消息操作五件套（转发/合并转发/收藏/正在输入/搜索协议） | ❌ | 协议+服务端+UI |
| UI 历史消息加载（云同步闭环） | ⚠️ 存储✅ | list_messages → UI 分页 |
| 已读回执 UI 呈现 | ⚠️ 协议✅ | 气泡已读标记 |

### P2 真机验收 + 客户端体验（1–2 周）

| 项 | 现状 | 交付 |
|---|---|---|
| WP-08 双机音视频验收 / WP-11 tauri dev 演示 / WP-10 Redis 真机 | ⚠️ | 三项验收记录 |
| 语音条（波形/转文字占位）/ 文件消息 / 贴纸 / 视频首帧+倍速 | ❌ | 消息类型注册表首批扩展 |
| 消息列表虚拟化 / 本地消息库（客户端 SQLite） | ❌ | 渲染与启动体验 |
| 头像接媒体 / mac·Linux 构建验证 | ⚠️ | 资料页闭环 |

### P3 读扩散核心 + 多设备 + 社群层（4–6 周，终版树主战场）

| 项 | 现状 | 交付 |
|---|---|---|
| message-service 新建（ADR-16/17：owner_type 三态信箱/序列号/类型注册表/三通道/撤回变更事件/批量撤回） | ❌（现写扇出转过渡） | 新 crate/服务 |
| sync-service（五层差分）/ read-service（游标+ZSET 未读，回执万人群默认关） | ⚠️ **库已有**：`crates/message` 有三态信箱、每属主序列号、`ack`/`unread`、五层 `sync_plan`；只接了 channel/wallet，direct/group 仍走 `core::Store` 扇出。缺的是接线，见 `docs/p3-mailbox.md` | 接线 + HTTP 暴露 |
| Transport trait + WS 网关（ADR-8）/ 多设备多连接（ADR-4） | ⚠️ QUIC 紧耦合 | 网关改造 |
| community/thread/频道权限/分组/频道级角色/话题订阅（ADR-16 支柱C） | ❌ | 新建 |
| relation 增强：拉黑/message_requests/星标/标签/共同聚合（ADR-24） | ⚠️ 基础好友✅ | 补齐 |
| 群管理补齐：公告/禁言/审批/邀请token/慢速/@所有人校验 | ⚠️ 基础✅ | 补齐 |
| 通知聚合+多级过滤（ADR-19 决策层）/ NATS backend / 弹幕高频 / 审计日志起步 / Meilisearch 接入 / tenant_id 建表 | ❌ | 按清单 |

### P4 运营合规 + 资金账本 + 通话（3–4 周）

| 项 | 现状 | 交付 |
|---|---|---|
| 支付四层（ADR-12/18：cala→obix→dtmrs→Connector；四类 account_type） | ⚠️ 演示级钱包 | ledger/wallet/ad-account/marketing/payment 五件 |
| 内容审核管线（risk/moderation/report 三层，ADR-20） | ❌ | 三服务 |
| 红包卡片变更回写 / 支付密码 / 对账 / 找回+captcha / 2FA / 秘密聊天完整 / 设备管理页 | ❌/⚠️ | 按清单 |
| 通话房间制 + 会议管控（ADR-7/21） | ⚠️ 1:1 库 | media-service 房间 |

### P5 移动端 + 直播 + 小程序 + 点播（4–8 周，可与 P3/P4 并行轨道）

| 项 | 现状 | 交付 |
|---|---|---|
| Flutter 移动端（ADR-11）+ UniFFI core 绑定 + 鸿蒙 Flutter-OH | ❌ | Android/iOS/鸿蒙 |
| 直播频道（ADR-13：Xiu ingest + 播放 + 信令独立通道） | ❌ | media-service 扩展 |
| 小程序 M1–M5（ADR-14：Rong→Bridge 闭环→多引擎→沙箱→后端集成） | ⚠️ container/dpage 雏形 | miniprogram-blueprint.md |
| video-service 点播 HLS（ADR-21）/ Web 独立工程 / 多语言 / 数据导出 / 厂商推送通道 | ❌ | 按清单 |

### P6 发布工程 + 规模（2–4 周）

| 项 | 现状 | 交付 |
|---|---|---|
| 安装包/bundle · release CI · TLS 反代 · TURN/FCM 部署 | ⚠️ **原生部署已可用**（macOS launchd / Linux systemd / Docker，见 `deploy/`）；安装包、TLS 反代、FCM 未做 | 发布链 |
| adminx 管理后台（RBAC/MFA/审计检索/工单/看板） | ❌ | 运营面板 |
| PG 迁移+分片 · 存储加密 · Bot+开放 API/Webhook · SSO 预留 · 无障碍 · k8s · 备份 · SLO | ❌ | 规模化 |

### P6+ 远期轨道

recommendation-service（双塔+Faiss+Wide&Deep）· feed-service/status-service · 群收款/群接龙 · 多租户激活 · 国密升级 · 鸿蒙 ArkUI-X 备选切换评估

---

## 第十四部分：执行状态总结

**已收敛**：引擎层 254 测试全绿在 main；架构蓝图 ADR-1~24、十四部分、九大不变式、P1–P6+ 详细路线表；四份专项文档（对照/差距/总蓝图/小程序）。
**无阻塞**：P1 清单完全在已有范围内（E2EE 接线/五件套/历史加载/回执 UI），不需要任何新决策。
**待拍板**：第十二部分 Q1–Q8（其中 Q1/Q2 影响读扩散实现方式，建议 P3 开工前确认；其余不阻塞 P1/P2）。
