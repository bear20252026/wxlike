# 参考仓库本地镜像清单（合同第七条配套）

> 2026-10-17 起，合同 7.1 参考表中的重点仓库已浅克隆（`--depth 1`）到仓库外
> **`D:\mimo\test\refs\`**，供 P3–P6 直接对照源码移植。本清单记录：镜像位置、
> 用途、许可；**borrow-first 政策见 [THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md)**。
> 刷新单个仓库：`cd /d/mimo/test/refs/<repo> && git pull --ff-only`。

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
| `atm0s-media-server/` | 8xFF/atm0s-media-server | 去中心 SFU（观察，重构期不用） |
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
