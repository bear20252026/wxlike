# wxlike 总蓝图（汇总版）：技术方案 × 全功能对照 × 参考项目 × 路线图

> 2026-10-16。**本文件是全部讨论的唯一汇总入口**，汇总四轮内容：
> ① 任务书（WORK_BREAKDOWN.md，14 WP）→ ② feature-comparison.md（WP 视角对照，149 行）
> → ③ feature-gap-matrix.md（四大产品九域差距 + P0–P6 路线图，148 行）
> → ④ 第二轮补充（Rust 技术栈选型 / 万人群读扩散数据模型 / 前后端功能服务清单 /
>    支付与小程序解耦 / 七端策略 / WuKongIM·OpenIM·goim 等新参考系）。
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
| 音视频 | str0m（已选）/ rvoip | ✅ str0m + 状态机 + 回环验证 | UI 接入 + 真机（P2），房间制（P4） |
| 搜索 | PG 全文 → Meilisearch/ES | ❌ | P3（读扩散下必须服务端搜） |
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
✅ 文本/离线落盘/去重/顺序/多端增量协议/引用/@/撤回（无时限+占位）/已读回执协议 · ⚠️ 云同步（存储✅ UI 拉取❌）· ⚠️ E2EE（库✅ 接线❌）· ❌ **转发/合并转发/收藏/正在输入/搜索（五件套，P1）** · ❌ 语音条/视频/文件消息/位置/名片/贴纸/GIF（P2 起逐步） · ❌ 编辑/置顶/翻译/定时/自毁（P5+）

### 5.4 群组 / 频道 / 话题
✅ 管理（建邀踢退/角色/1000 上限/转让/my_remark）/写扩散跨 hub/离线补收/@改写 · ❌ **万人群读扩散管道（P3 全套：信箱/游标/版本差分/ZSET 未读）** · ❌ 频道层/话题层/社区（P3）· ❌ 禁言/公告/审批/邀请链接/慢速模式/成员搜索（P3）· ❌ 投票/群文件库/机器人（P5+）

### 5.5 朋友圈 / 动态
✅ 发布/好友圈读时判定/赞/评论树/软删/分页 · ❌ 视频号/24h 动态/可见范围细分（P5+）

### 5.6 音视频
⚠️ 1:1 语音/视频（str0m 库+回环验证，缺 UI 接入与双机真机）· ❌ 群通话房间（ADR-7，P4）· ❌ 多人/屏幕共享/通话记录 · ⚠️ STUN 内置+TURN 接口

### 5.7 支付 / 小程序
✅ 虚拟转账/红包/流水（事务守恒）· ⚠️ 红包卡片"消息变更"回写缺失（ADR-5，P4）· ❌ 充值/提现/支付密码/对账（持牌边界内，P4）· ⚠️ 小程序容器雏形（Boa+DPG1）→ 产品化（包管理/审核/JSBridge/回调转发，P5+）

### 5.8 推送
✅ 框架+FCM 实现+设备表 · ⚠️ FCM 真机 · ❌ APNs（接口位）· ❌ **通知聚合**（万人群必做，goim 分层推送思路，P3）· ❌ 厂商通道（国内安卓）

### 5.9 客户端工程（七端）
✅ Windows 桌面（深色/动效/九宫格/红包 UI）· ⚠️ macOS/Linux 未验 · ❌ **Android/iOS（P5，Flutter + UniFFI core）** · ❌ **鸿蒙（P5 决策：ArkUI-X 战略优先 or Flutter 社区适配+ArkTS 桥接）** · ❌ Web（独立工程，复用 API+WS 协议；Transport trait 是前置）· ❌ 虚拟列表/历史加载（P1/P2）· ❌ 多语言/无障碍/自动更新/崩溃上报（P5/P6）

### 5.10 服务端基础设施
✅ MQ trait/多节点路由/崩溃恢复/可观测/compose/压测基准 · ❌ PG+分片（P6）· ❌ 管理后台（用户/群管理/审核/封禁/举报，P6）· ❌ 备份多机房（P6）

---

## 第六部分：七端客户端策略（收敛结论）

| 端 | 选型 | 关键约束 | 阶段 |
|---|---|---|---|
| Windows / macOS / Linux | **Tauri 2（现状）** | 与 Rust 同栈、包体 ~10MB；WebView 渲染差异与 IPC 性能需验证；文字/图片/文件消息为主 → 合理 | ✅ Win 已有 / mac·Linux P2 |
| Android / iOS | **Flutter + Rust core（UniFFI/cargo-ndk 绑定）** | 移动端最成熟；聊天列表渲染可控 | P5 |
| 鸿蒙 | 二选一：**ArkUI-X**（鸿蒙战略优先，一套 ArkTS 覆盖鸿蒙+Android+iOS）或 **Flutter 社区适配 + ArkTS 桥接**（复用移动端） | 无完美方案，按优先级取舍 | P5 决策 |
| Web | **独立前端工程**（复用后端 API + WS 协议） | 浏览器无 QUIC（WebTransport 未普及）；**Transport trait 是前置**（ADR-8） | P5 |
| 本地消息库（全端） | 客户端 SQLite（打开即渲染，不等服务器）；万人群只缓存近期窗口+主动漫游区间 | 现有 UI 无本地库 | P2 |

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
| Synapse | Python | https://github.com/element-hq/synapse | ⚠️ 已改 AGPL-3.0（按依赖政策模块级评审） | 联邦/房间模型 | 参考 |
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
| ArkUI-X | ArkTS | https://github.com/arkui-x （主仓 GitCode/Gitee） | ✅ Apache-2.0 | 鸿蒙三端（P5 决策） | P5 |

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

---

## 第九部分：合并路线图（终版；在 P0–P6 基础上吸收第二轮"第 0–5 步"，不删旧项）

| 阶段 | 内容（新增项加粗） | 规模 |
|---|---|---|
| **P0 决策锁定** | 云同步路线确认（建议维持 TG 式）· 读扩散立项 · **移动端优先级 + 鸿蒙取舍（ArkUI-X vs Flutter 适配）** | 口头确认 |
| **P1 已采纳第一批** | E2EE 接入客户端（回退日志+秘密聊天雏形）· 消息操作五件套 · **UI 历史消息加载** · 已读回执 UI | 1–2 周 |
| **P2 真机验收 + 体验** | 双机音视频验收 · tauri dev 演示 · Redis 真机 · **语音条/文件消息/贴纸 · 消息列表虚拟化 · 本地消息库 · 头像接媒体 · mac/Linux 构建验证** | 1–2 周 |
| **P3 万人群新架构** | 读扩散管道（信箱/游标/版本差分/ZSET 未读，五层同步契约）· **Transport trait + WS 网关** · **多设备多连接改造** · 频道层 · 话题层 · 群管理补齐（禁言/公告/审批/邀请链接/慢速/成员搜索）· 拉黑 · 服务端未读态 · **通知聚合 + 慢消费者隔离 · 会话 Pin/免打扰/归档 · NATS backend（MQ trait 新实现）** | 4–6 周 |
| **P4 账号 / 隐私 / 资金** | 找回+captcha/限流 · argon2id（可选）· 2FA · 秘密聊天完整+消失消息 · **红包卡片消息变更回写 · 支付密码 · 钱包对账 · 通话房间制** · 设备管理页 | 2–3 周 |
| **P5 移动端与多语言** | Android（Flutter + UniFFI core）· iOS · **鸿蒙（按 P0 决策）** · Web 独立工程 · 多语言 · 数据导出 | 4–8 周 |
| **P6 发布工程与规模** | 安装包/bundle · release CI · TLS 反代 · TURN/FCM 部署文档 · **PG 迁移+分片 · 管理后台（最小 CLI 起步）· k8s · 备份 · SLO 固化** | 2–4 周 |

---

## 第十部分：一句话总纲

**wxlike 已经有的是一个经过对抗测试的 Rust IM"引擎层"**（协议/加密/传输/存储/群路由/通话内核，约 2.2 万行、254 测试）；要成为七端、万人群、带支付与小程序的完整产品，缺的是**读扩散管道、多设备、频道话题（P3）+ 移动端（P5）+ 产品化外壳（P1/P2）**。参考系已齐：WuKongIM 给万人群架构答案、OpenIM 给同步契约、goim 给弹幕推送、HuLa/唐僧叨叨给前端与功能模型、TDLib 给核心库范式——**全部是"重新实现于 Rust 之上的参考"，没有一个是拿来即用**；这正是路线图把 P3（读扩散）和 P5（移动端）定为最大工程块的原因。
