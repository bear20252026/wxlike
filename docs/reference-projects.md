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
