# 参考项目调研（高仿微信开源生态）

> 调研日期 2026-10-16。目的：为 wxlike 提取可借鉴的架构、功能清单与交互设计。
> 注意：部分链接与 star 数经核实与传闻有出入，见各节勘误。

## 一览

| 项目 | 端 | 栈 | 真实 star* | 对 wxlike 的价值 |
|------|----|----|-----------|------------------|
| [LQRWeChat](https://github.com/GitLqr/LQRWeChat) | Android | RxJava+Retrofit+MVP+融云SDK | ~3.4k ✓ | ★★★ 消息类型全集、自定义消息实现红包 |
| [vue-WeChat](https://github.com/zhaohaodang/vue-WeChat) | Web | Vue2+Vuex+WeUI（arden-kfc 是 fork） | ~2.1k（62 commits，mock 纯前端） | ★★ UI 微交互验收清单（滑动/搜索动画/转场）；Vue2 技术栈本身不参考 |
| [CoderMikeHe/WeChat](https://github.com/CoderMikeHe/WeChat) | iOS | OC+MVVM+RAC | ~1.2k ✓ | ★★ ViewModel-Based Navigation、文档化风格 |
| [SwiftUI-WeChat](https://github.com/wxxsw/SwiftUI-WeChat) | iOS | SwiftUI 2 | ~1.2k ✓ | ★★ 朋友圈九宫格、导航栏滚动联动、深色模式 |
| [box-im](https://github.com/bluexsx/box-im) | 全栈 | SpringBoot+Netty / Vue3+uniapp | **696**（非"暂缺"） | ★★★★★ 唯一生产级全栈：业务/推送分离、Redis 路由、已读/多端同步 |
| [fake-world](https://github.com/LiangNiang/fake-world) | Web | React+Jotai+Slate+Bun（monorepo，后端可选） | **153**（65 commits，小而精；shanyu-e 链接是其 0-star fork） | ★★★ 设计决策 > 代码量：本地优先、编辑/运行态同构、AI 可选后端。GPL-3.0：只学决策不碰代码 |
| [Wechat_ArktsDevelop](https://github.com/xiaowuDev/Wechat_ArktsDevelop) | 鸿蒙 | ArkTS/API12 | **13**（非高热度项目） | ★ 多端参考、明暗主题 |

\* 为 2026-10-16 实测。

## 核心结论

**功能仿制类项目（1-4、7）参考 UI 与交互；生产可用参考只有 box-im（后端协议设计）；架构思想参考 fake-world（本地优先）。**

> 2026-10-16 修正：fake-world 原项目实测 153★/65 commits，规模小但架构决策密度高（本地优先、Shift+Z 编辑态/查看态同构、AI 后端可选解耦）——学决策不学代码。vue-WeChat 原项目 2.1★/62 commits，纯 mock 无后端，当 UI 微交互清单用，Vue2 栈已 EOL 不参考。

## 对 wxlike 各 crate 的行动映射

### 1. server：拆分业务与推送（学 box-im）—— ✅ MQ 路由已落地（2026-10-16）

**已实现**：`crates/mq`（MessageQueue/RouteTable trait，Memory + Redis 双后端）、`net/mq_hub`（按收件人路由：本地直投/MQ 转发/广播扇出）、hub 队列 `wxlike:msg:hub:{id}`、路由表 `用户→hub`。集成测试验证双 hub 经 MQ 交换定向消息与广播。

剩余（原计划保留）：platform 业务面（登录/资料/离线暂存）、store-and-forward、Redis Streams 升级 at-least-once。

box-im 的 `im-platform`（业务）/`im-server`（推送）/`im-client`(SDK) 三分结构是对的方向：

- `server/` 拆为 `hub`（纯推送，保留现有 QUIC hub）+ `platform`（登录/资料/会话元数据/离线暂存）
- box-im 用 Redis 记录 `用户 → serverId` 连接路由 + Redis List 按 serverId 分队列；wxlike 对应实现：
  - 单节点：tokio mpsc + `HashMap<UserId, HubConn>`（现在已有雏形）
  - 多节点：路由表落 Redis（`user:{id} -> hub_id`），hub 间经队列转发——**接口先留好，实现后补**
- 学它的"避免引入 MQ"的克制：中小规模用 Redis List 即可，不上 Kafka

### 2. core/types：消息模型对齐（学 LQRWeChat + box-im）

当前 `MsgKind::{Text,Blob,System}` 太薄，按两个项目的功能清单扩展：

```rust
pub enum MsgBody {
    Text(String),
    Bytes(Vec<u8>),
    Notice(String),
    // 新增（LQRWeChat 的消息类型全集）:
    Custom { kind: String, payload: Vec<u8> },  // 红包/位置/名片/引用/撤回指令等
}
pub struct Envelope {
    // 新增（box-im 已读/撤回/去重）:
    pub msg_id: [u8; 16],       // uuid，客户端生成，hub 去重
    pub quote_msg_id: Option<[u8; 16]>, // 消息引用
    pub read_receipt_to: Option<[u8; 16]>, // 已读回执
}
```

存储侧（core/store）同步加：`msg_id UNIQUE`、`read_at`、`withdrawn` 列。

### 3. 多端同步（学 box-im）

- 设备级会话游标：每设备记录 `per_conv_last_seq`，登录后只补增量——微信的多端同在线语义
- 本地 SQLite 已是我们的 source of truth（core/store），补一个同步协议命令即可

### 4. UI（Tauri2 前端）

- 朋友圈九宫格布局（SwiftUI-WeChat 的图片数动态布局）、导航栏滚动联动、全量深色模式——做成 dist 前端的组件清单
- 消息列表滑动操作、搜索动画（vue-WeChat）
- 富文本/引用编辑选型 **Slate**（fake-world 验证过与 React 配合）
- 状态管理选 Jotai 风格的原子化（fake-world）

### 5. 离线优先原则（学 fake-world）

明确架构原则：**本地 store 是 source of truth，hub 只是同步通道**。断网时一切功能可用（除收发），上线后按游标同步。这同时是 fake-world "数据不出浏览器"的隐私卖点。

### 6. 差异化功能：AI 生成聊天记录（学 fake-world）

fake-world 的核心卖点——AI 生成聊天记录+所见即所得编辑——很适合 wxlike 的演示模式：
- `wxlike-demo`：用 LLM 批量生成 Envelope 流灌入 store，产生逼真的演示数据
- 实现成本低（复用 Envelope + store），演示效果高

### 7. 远期：音视频通话（学 box-im）

box-im 用原生 WebRTC 摆脱收费 SDK。wxlike 对应：`str0m`/`webrtc-rs` 纯 Rust WebRTC 栈，或网关侧 liberlive/mediasoup。排期最后。

## 风险与边界备忘

- LQRWeChat 依赖融云 SDK（服务端是融云开源的 Node "嗨豹"）——wxlike **不引入任何收费/闭源 IM SDK**，box-im 的自研路线一致
- fake-world 是 **GPL v3**：只借鉴思想，不复制代码，否则 wxlike 被传染
- WeChatArkTS 主要是 UI 仿制，鸿蒙端若做，参考其页面清单即可

---

## 全球参考项目（第二批，已核实 2026-10-16，全部真实存在）

### 核实结果速览

| 项目 | 实测 | 声称 | 许可证 | 结论 |
|------|------|------|--------|------|
| [HuLa](https://github.com/HuLaSpark/HuLa) | **7.8k★**，1226 commits | 7.7k+ | **Apache-2.0** | ✅ 完整跨平台 IM 客户端（非 UI 演示） |
| [iroh](https://github.com/n0-computer/iroh) | **12.7k★**，n0-computer 出品 | — | **Apache-2.0 OR MIT** | ✅ 生产级 P2P 网络库（公共中继跑自家代码） |
| [teamgram-server](https://github.com/teamgram/teamgram-server) | 2.3k★，Go，MTProto API 228 | — | Apache-2.0 | ✅ 真实，但社区版仅私聊+基础群；频道/通话是**付费企业版** |
| [grammers](https://github.com/Lonami/grammers) | 815★，Rust MTProto | — | MIT/Apache 双许可 | ⚠️ GitHub 已归档（2026-02），迁往 Codeberg；未做安全审计 |
| [sandcat-backend](https://github.com/Xu-Mj/sandcat-backend) | 97★，513 commits | — | MIT | ✅ 小而真：Kafka+MongoDB+PG+Redis+MinIO 微服务 |
| [PaperPhone-Plus](https://github.com/619dev/Paperphone-plus) | 126★ | — | **AGPL-3.0** | ⚠️ 真，但网络服务也要开源修改——**严格只学思想** |
| [Mimir](https://github.com/Mimir-IM/mimir) | 39★，19 commits | — | MPL-2.0 | ⚠️ 早期项目，但 UniFFI 绑定（Android JNI/iOS Swift）模式可学 |
| [hungry-rs](https://github.com/dimentyy/hungry-rs) | **3★**，WIP | — | MIT | ❌ 作者计划完工后重建仓库——无参考价值 |

TDLib 相关（未重新核实，公知事实）：TDLib 为 Boost 1.0 宽松许可（"单实例 2.5 万活跃 bot"出自其 README，属实）；Telegram 官方客户端（DrKLO/Telegram、Telegram-iOS、tdesktop）均为 **GPL-3.0**。

### 对 wxlike 工作包的直接影响

1. **WP-11 UI：HuLa 是最大加速器（Apache-2.0，可合法借用代码）**
   - Vue3+Tauri 覆盖五端，已实现：撤回/@/已读/回复/表情包/链接预览/消息点赞/群管理/在线状态/音视频通话/多窗口/截图/自动更新/i18n——正是我们 WP-11 的 15 人日清单，且远超
   - ⚠️ 架构不学：HuLa 的业务逻辑在 TS/Vue 层，违反我们"逻辑必须 Rust"的铁律。**借 UI/交互与代码（保留 Apache 声明），架构坚持 wxlike 分层**
   - 附带 HuLa-Server（独立服务端仓库）可作对照

2. **WP-08 音视频：iroh 直改技术选型（12.7k★，生产级）**
   - 原计划：str0m + 自建 STUN/TURN。现在：**iroh 打洞 + 公共中继回退**，砍掉 TURN 基础设施建设，且 Apache/MIT 双许可
   - 附带能力：`iroh-blobs`（BLAKE3 内容寻址大文件传输）可作 WP-05 媒体传输的 P2P 通道；`iroh-gossip` 可作群消息广播备选
   - 信令仍走我们的 QUIC 长连接（`MsgBody::Custom{kind:"rtc-signal"}`）不变

3. **移动端 FFI：UniFFI 路线确立（Mimir/TDLib 双重先例）**
   - Mimir 验证了 UniFFI → Kotlin/Swift 绑定的可行性；TDLib 本身就是"核心库+原生壳"模式的祖师爷（C++ 版）
   - 原计划 cargo-ndk+手写 JNI → 改为 **UniFFI 生成绑定**，手写 JNI 面大幅缩减

4. **协议设计阅读材料（只读不抄）**
   - MTProto 分层（传输/加密/API）与二进制序列化：grammers（Rust，用 Codeberg 上游）源码是最佳读物；TDLib 的 API 对象体系是设计范本
   - teamgram-server：MTProto 服务端语义参考——但注意其核心功能与付费版的分界恰好说明"群聊/通话"是 IM 后端的主要工作量所在（与我们任务书 WP-04/08 的预估互相印证）

5. **明确排除**
   - ~~Telegram GPL-3.0 客户端代码：不进仓库~~ **已解锁（2026-10-16 项目转 GPL-3.0）**：交互范式与性能优化代码可合法借鉴，按文件保留 GPL 声明
   - PaperPhone-Plus（AGPL-3.0）：X25519+ML-KEM-768 抗量子混合加密升格为 WP-12 远期可引入项（AGPL→GPL 组合依 GPLv3 §13 可行），引入时保留 AGPL 模块头单独评审
   - hungry-rs：3★ WIP，跳过
   - sandcat-backend：仅作"SQLite 不够用之后"的 MongoDB 分片策略阅读材料（其 30-50% 提升是 README 自述，未经独立验证）

### 对任务书估计的修订

- WP-08a 语音：iroh 替代自建 TURN → **25 人日下调至 18–20 人日**（打洞/中继/信令封装仍需做）
- WP-11 UI：HuLa 交互代码可借鉴 → **15 人日下调至 10–12 人日**（Rust command 层不可省）
- 移动端壳（原 WP 清单外的后续项）：UniFFI 确立后，Android/iOS 各 8–10 人日（原手写 JNI 估 15+）

---

## 企业级与国内 IM（第三批，已核实 2026-10-16）

### 核实结果

| 项目 | 实测 | 许可证 | 状态 | 对 wxlike 的价值 |
|------|------|--------|------|------------------|
| [Rocket.Chat](https://github.com/RocketChat/Rocket.Chat) | 46.3k★ | **MIT 核心**（ee/ 目录企业功能分离） | 极活跃（30k+ commits） | 消息/频道数据模型、联邦、WebRTC 通话——MIT 可借 |
| [Mattermost](https://github.com/mattermost/mattermost) | 39.3k★ | **MIT 核心** + LICENSE.enterprise（open core） | 极活跃（月度发版） | Go+PostgreSQL 单二进制、插件体系、通话——架构与 schema 参考 |
| [Element Web](https://github.com/element-hq/element-web) | 13.6k★ | **AGPL-3.0**（多许可，含商业） | 极活跃 | Matrix 客户端交互与 E2EE UX——只学设计 |
| [Synapse](https://github.com/element-hq/synapse) | 4.7k★ | **AGPL-3.0 或商业**（已从 Apache-2.0 改！） | 极活跃 | Matrix homeserver 语义参考——只学设计 |
| [唐僧叨叨 Server](https://github.com/TangSengDaoDao/TangSengDaoDaoServer) | 3.6k★ | **Apache-2.0** | 活跃（529 commits） | ★★★ 与 wxlike 同构的"通讯层/业务层"分离，微信还原度最高的中文全栈参考 |
| [WuKongIM](https://github.com/WuKongIM/WuKongIM) | 5.0k★ | **Apache-2.0** | 活跃（v3 beta，5.6k commits） | Go 自研消息引擎（自带存储/复制/集群哈希槽）——hub 设计对照物 |
| [Startalk](https://github.com/startalkIM) | 主仓库 104★，组件个位数 | GPL-3.0/MIT 混杂 | **已停维**（核心 2021-2023） | ❌ 价值有限 |
| [XyTalk-pc](https://github.com/xy-Group/xyTalk-pc) | 58★ | **AGPL-3.0** | 业余练手，低活跃 | ❌ 且是 Java Swing（非 Erlang），跳过 |

### 关键修正（相对常见认知）

1. **Synapse 与 Element Web 已不再是 Apache-2.0**：Element 于 2024-11 起改为 AGPL-3.0/商业双许可。引用 Matrix 生态时必须区分：**Matrix 协议规范是 Apache-2.0 可自由实现**；Element 的具体代码是 AGPL——学规范与 Megolm 设计，不搬代码。
2. **Startalk（360 星语）已停维**，"适合学习参考"的说法不成立——核心 ejabberd 定制停更于 2021。
3. **XyTalk 是 Java Swing 桌面 + Tigase/XMPP**，作者自述练手作品，非生产参考。

### 对 wxlike 工作包的映射

1. **WP-03/04 消息与群模型：唐僧叨叨是最好的中文对照系（Apache-2.0 可借）**
   - 其功能清单与我们 WP-03 的扩展方向几乎一一对应：撤回/回复/转发/合并转发/收藏/@提醒/正在输入/未读数/多端同步/消息搜索
   - 群：成员管理/群公告/群二维码/群昵称/群投诉——直接补进 WP-04 验收清单
   - **架构互证**：它 = 通讯层（WuKongIM 引擎，长连接/投递/存储）+ 业务层（好友/群组，经 Webhook/GRPC 交互）——与我们 hub（推送）/platform（业务）分离完全同构，且验证了"引擎不管业务"的正确性
2. **WP-12 群端到端加密：Matrix 的 Megolm/Olm 设计**
   - 群 E2EE 不能简单 per-pair ratchet（N 人 O(N²)），Matrix 的 Megolm 用 sender-key + ratchet 是标准答案；**Matrix 协议规范 Apache-2.0 可自由实现**（Rust 参考实现 vodozemac，引入时核对其许可证）
   - 这解决我们 WP-12 只规划了单聊 ratchet、未规划群组 E2EE 的缺口——补进 WP-12 验收
3. **WP-13 运维：Mattermost 的"单二进制 + PostgreSQL + 月度发版"模式**是自托管产品形态的最小复杂度范本；Rocket.Chat 的 monorepo/turborepo 工程化可参考
4. **WP-06 推送**：Rocket.Chat/Mattermost 的 push gateway（自建推送代理转发 APNs/FCM）模式与我们 push crate 设计一致
5. **长期路线图参照**：这批项目的功能边界（TSDaoDao 缺朋友圈/红包/通话；Mattermost 通话靠插件）再次印证任务书的判断——**聊天内核是 30%，朋友圈/支付/音视频才是工作量大头**
