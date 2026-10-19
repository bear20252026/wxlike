# 参考仓库本地镜像清单（合同第七条配套）

> 2026-10-17 起，合同 7.1 参考表中的重点仓库已浅克隆（`--depth 1`）到仓库外
> **`D:\mimo\test\refs\`**，供 P3–P6 直接对照源码移植。本清单记录：镜像位置、
> 用途、许可；**borrow-first 政策见 [THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md)**。
> 刷新单个仓库：`cd /d/mimo/test/refs/<repo> && git pull --ff-only`。

## Tier 0 — 最高优先级参照（用户 2026-10-18 指定，全局适用）

> **规则**：任何功能开工前，先查本表对应项目；能整段照搬就照搬（同语言时
> vendoring/依赖），搬不了就**按原语境重写**（保留其分层与边界条件）。
> 优先级：**Tier 0（本表）> Telegram 系（tweb/grammers/teamgram…）> 其余仓库**。
>
> **许可现实（2026-10-18 更新）**：本项目自身已改为 **AGPL-3.0**，因此
> **本表 5 个 AGPL-3.0 项目（Mattermost 源码 / Signal / Element / Synapse / spreed）
> 的代码可直接复制**，合并后的作品整体仍为 AGPL-3.0；GPL-3.0 的 tweb 同样可并入
> （GPLv3→AGPLv3 是兼容方向）。MIT/Apache 项目照旧直接复制。
> 唯一代价：本平台对外提供网络服务时须向用户提供源码（AGPL §13）。

| 项目 | 上游 | 语言 | 许可（实测） | 可搬点 → 我们的缺口 | 本地镜像 |
|---|---|---|---|---|---|
| **Zulip** | https://github.com/zulip/zulip | Python + TS | **Apache-2.0** ✅ | **主题化线程模型**（topic/recipient 双层）、消息编辑历史、presence → 我们的 Thread/话题与编辑 | ❌ 未镜像（633 MB） |
| **Rocket.Chat** | https://github.com/RocketChat/Rocket.Chat | TypeScript | **MIT**（`ee/`、`apps/meteor/ee/` 为企业许可） ✅ | 消息操作与权限矩阵、REST + Realtime API 形状、频道/团队模型 → 我们的权限与 API 契约 | ❌ 未镜像（2.1 GB） |
| **Mattermost** | https://github.com/mattermost/mattermost | Go + TS | 源码 **AGPL-3.0**（`webapp/`、admin tools 为 Apache-2.0；编译产物 MIT） ⚠️ | 团队/频道/线程模型、插件架构、Webapp 组件组织 → 我们的 adminx 与插件面 | ❌ 未镜像（1.3 GB） |
| **Signal-Desktop** | https://github.com/signalapp/Signal-Desktop | TypeScript | **AGPL-3.0** ⚠️ | 发送队列/重试、附件分片与断点、E2EE 会话 UI → 我们的媒体上传与秘密聊天 UI | ❌ 未镜像（1.2 GB） |
| **Signal-Android** | https://github.com/signalapp/Signal-Android | Kotlin | **AGPL-3.0** ⚠️ | 会话状态机、媒体转码/压缩、通知聚合 → Flutter 端与推送聚合 | ❌ 未镜像（2.5 GB） |
| **Element Web** | https://github.com/element-hq/element-web | TypeScript | **AGPL-3.0** ⚠️ | Matrix 客户端：房间列表虚拟化、E2EE 设备验证 UI、sliding sync → 我们的会话列表与设备页 | ✅ **`refs/element-web`** |
| **Synapse** | https://github.com/element-hq/synapse | Python | **AGPL-3.0** ⚠️ | 事件 DAG、房间状态解析、联邦 → 读扩散/多端同步的对照模型 | ❌ 未镜像（629 MB） |
| **Nextcloud Talk** | https://github.com/nextcloud/spreed | JS + PHP | **AGPL-3.0** ⚠️ | **群通话房间 UI**（参与者/静音/录制/举手/force-mute）、通话信令 → 我们的 `rtc::room` 与通话页 | ✅ **`refs/spreed`**（84 MB 浅克隆） |

### 按缺口查表（先查这里，再查 Telegram）

| 我们的缺口 | 先看 |
|---|---|
| 话题/线程、消息编辑历史 | **Zulip** |
| 权限矩阵、REST/Realtime API 形状 | **Rocket.Chat** |
| adminx 后台、插件面、团队/频道 | **Mattermost**（webapp Apache-2.0 部分可直接看） |
| 附件分片/断点/发送队列 | **Signal-Desktop** |
| 媒体压缩、通知聚合、移动端会话 | **Signal-Android** |
| 设备验证 UI、房间列表虚拟化 | **Element Web**（已镜像） |
| 同步/事件模型对照 | **Synapse** |
| 群通话房间 UI 与管控 | **Nextcloud Talk** |
| 多方通话（mesh 上限 / 何时上 SFU） | **Nextcloud Talk** `docs/scalability.md`：带宽 = 1 Mbps × (人数−1)（视频）/ ~50 kbps（音频），音频-only 可达 ~20 人 |
| SFU 架构（真要写的时候） | **`refs/atm0s-media-server`**：Rust + **str0m**（与我们同一套栈），分层 `transport_webrtc` / `media_core` / `media_connector` / `media_runner`，按层参照重写而非 vendoring |

## P3 读扩散核心（下一阶段直接使用）

| 目录 | 上游 | 许可 | 借什么 |
|---|---|---|---|
| `WuKongIM/` | WuKongIM/WuKongIM | 待核（README 未标注则只学设计） | 频道订阅模型、信箱/序列号设计、二进制协议 |
| `open-im-server/` | openimsdk/open-im-server | 待核（社区普遍 Apache-2.0） | **五层差分同步契约**（判断→增量→兜底→批量→懒加载），sync-service 蓝本 |
| `goim/` | Terry-Mao/goim | 待核 | 分层推送（活跃实时/非活跃聚合）、房间管理 |
| `flare-core/` | flare-im/flare-core | 待核 | Transport trait 心跳/重连/慢消费者隔离 |
| `tantivy/` | quickwit-oss/tantivy | MIT | **P3 搜索真依赖**（替换 P1 过渡层时的嵌入对象） |
| `HuLa/` · `HuLa-IM-Tauri/` | HuLaSpark | Apache-2.0 | 前端工程组织、Tauri 多端配置 |
| `TangSengDaoDaoServer/` · `TangSengDaoDaoWeb/` | TangSengDaoDao | Apache-2.0 | 微信还原度最高的消息操作/会话模型 |

## P4 支付账本 + 安全（嵌入对象，不手写）

| 目录 | 上游 | 用途 |
|---|---|---|
| `cala/` | GaloyMoney/cala | **cala-ledger 复式记账内核**（嵌入 PG） |
| `obix/` | GaloyMoney/obix | 事务性 Outbox/Inbox |
| `dtmrs/` | jackwangfeng/dtmrs | 嵌入式 Saga/TCC 交易协调 |
| `ousia/` | TheOusia/ousia | 备选账本引擎（两阶段执行） |
| `hyperswitch/` | juspay/hyperswitch | `ChannelConnector` 渠道适配 trait 范式 |
| `im-server/` | juggleim/im-server | 企业级 IM 对照 |

## P5 媒体面 / 小程序 / 移动端

| 目录 | 上游 | 用途 |
|---|---|---|
| `xiu/` | harlanc/xiu | RTMP/WebRTC/HLS 直播服务器（**外部进程部署**） |
| `Rong/` · `LingXia/` | LingXia-Dev | 小程序逻辑层引擎 / 宿主骨架（ADR-14 拼装件） |
| `dimina/` | didi/dimina | 小程序容器架构文档对照 |
| `blivedm_rs/` | isomoes/blivedm_rs | B 站弹幕 WS 客户端（弹幕协议参考） |
| `mimir/` | Mimir-IM/mimir | UniFFI 跨平台绑定范式 |
| `atm0s-media-server/` | 8xFF/atm0s-media-server | 去中心 SFU（**观察**：重构期不 vendoring，但它是唯一与我们同栈（Rust + str0m）的 SFU，做多人通话时按层参照） |
| `tweb/` | morethanwords/tweb | Telegram Web（GPL）——P5 Web 端架构参考 |

## 其余（骨架/协议/推送/设计研究）

`only-talk-rs/`（Rust 服务骨架）· `ISM/`（正文/元数据分离）· `teamgram-server/` ·
`grammers/`（MIT）· `hungry-rs/` · `telegram_rust/` · `tuigram/` · `ferogram/` ·
`gramsrv/` · `mytelegram/`（MTProto 系参考）· `box-im/`（MQ 设计已移植）·
`gorush/`（MIT，推送网关模式）· `push_server/`（野火推送）· `sorted_set_nif/`
（Discord 官方 NIF，网关数据结构）· `element-web/`（Apache-2.0，E2EE UI）·
`iroh/`（P2P 远期）· `HuLa-Server/` · `fake-world/`（聊天记录数据模型）。

## 有意暂缓（未镜像，原因如下）

- **体积/工具链类**：Flutter-OH（在 GitCode，经 flutter 工具链获取而非源码参照）、
  wasmtime/Boa/Flutter（cargo/npm 依赖，无需源码镜像）、Rocket.Chat、Mattermost、
  Telegram Android/iOS/Desktop、TDLib、mediasoup、OpenVidu（P6/P5 后段再按需浅克隆）。
- **无 LICENSE 微信高仿**（LQRWeChat/vue-WeChat/CoderMikeHe/WeChat/SwiftUI-WeChat/
  WeChatArkTS）：合同 7.1 定为"只学思想"；需要交互对照时再单独浅克隆并只读不抄。
- **P6+ 推荐系统**（PaddleRec/Gorse/Faiss/ScaNN/The-Algorithm/LightFM）：远期轨道。

## 引入纪律

从镜像借代码/嵌依赖时：同一提交内 ① 更新 `THIRD-PARTY-NOTICES.md`（来源仓库、
引用位置、原许可），② 被移植文件头部保留原许可声明（GPL/AGPL 级别的按模块隔离），
③ 在 PR 描述注明"借鉴范围"。内部测试模式下全部来源可接纳（合同 7.2）；再激活
条款触发时按此清单做许可核验。
