# wxlike 小程序自研方案详细说明（Rong + LingXia/Wry + Wasmtime）

> 2026-10-16。第五轮补充的完整展开，对应总蓝图 [master-blueprint.md](master-blueprint.md) 的
> **ADR-14**。路线决策：**不走微信兼容路线**（不逆向 WXML/WXSS 语义），自定义 DSL 与运行时
> 契约，用 Rust 组件拼装"逻辑执行、沙箱隔离、视图渲染"三层分离的原生宿主体系。
>
> **与现有资产的衔接（实证）**：workspace 里的 `container` crate（engine.rs = Boa 0.20 JS
> 引擎宿主、bridge.rs = wxhost 桥、Container 生命周期 load/start/page/logs）与 `dpage`
> crate（DPG1 magic+版本+id+长度桩头 + zstd 载荷 + manifest.json）**就是本方案的雏形**：
> 逻辑层引擎、桥、包格式三个位置都已占位。本方案是把它们从"研究级"升级为"产品级"，
> 并补上缺失的视图层。

---

## 一、分层架构总览

| 层级 | 职责 | 选型 | 现状 |
|---|---|---|---|
| **编译层（可选）** | 把自定义 DSL（或选定前端语法）编译为平台无关运行时资源包 | 参考 Dimina DMCC 思路；自定义 DSL 用 `langkit`/`dsl-kit`；**包格式直接复用自有 DPG1（补签名字段）** | ⚠️ dpage 已有桩头+zstd+manifest，缺签名与产物编译 |
| **逻辑执行层** | 执行小程序业务逻辑，暴露宿主 API | **Rong**（一套 Rust API 驱动 QuickJS / JavaScriptCore / ArkJS，构建时选引擎）；纯 Rust 备选 **Boa**（已是 container 现役，Test262 ≈94%，性能弱于 Rong 所驱动的引擎） | ⚠️ Boa 单引擎（engine.rs） |
| **沙箱隔离层** | 隔离第三方不可信代码，限 CPU/内存 | **Wasmtime**（fuel/epoch 中断 + 线性内存限制 + host functions 显式授权）；JS 层用 Rong Worker 有界执行兜底 | ❌ |
| **视图渲染层** | 渲染 UI，接收逻辑层数据驱动更新 | **Wry**（Tauri 同源跨平台 WebView，支持 `build_as_child` 子面板嵌入）或 **LingXia 的 lingxia-webview**（已抹平 Android/iOS/HarmonyOS/Windows 差异） | ❌ 完全缺失 |
| **桥接路由层** | 转发逻辑层↔视图层消息，代理 IM 原生能力 | **自研 Rust Bridge**（container/bridge.rs 升级：参照 LingXia 的 setData/streams/channels 模型 + Dimina 的 publish/invoke 二分法） | ⚠️ wxhost 桥只有 log/setTitle/store/nowMs |

---

## 二、核心组件详解

### 2.1 Rong —— 逻辑层执行引擎（核心）

一套 Rust API 驱动三种 JS 引擎，**构建时选定**，业务代码零引擎分支：桌面/Android 用 QuickJS，iOS/macOS 用系统 JavaScriptCore，鸿蒙用 ArkJS。对宿主场景的四个关键能力：

1. **声明式 Rust-JS 绑定**：`#[js_class]` / `#[js_method]` 把 Rust 结构体直接暴露给 JS，构造/getter/setter/静态方法全部由类型系统校验。**IM 能力（`im.sendMessage`、`im.getConversation`、`payment.request`）封装成 Rust 类，直接成为小程序可调用的 JS API**。
2. **Async/await 一等公民**：Promise 与 Rust future 原生交织，跨引擎边界无回调地狱。
3. **Worker 池与有界执行**：`shared()`（无状态分发）/ `pinned()`（有状态固定运行时）两种模型，可对排队和执行阶段设**任务截止时间**，引擎支持时**中断非让渡的 JS**、超时后安全复用 Worker——防止单个小程序死循环拖垮宿主的第一道防线。
4. **内置模块**：Timer/HTTP/FS/SQLite/Redis/Worker 已提供，宿主不必从零造基础能力。

### 2.2 LingXia —— 现成跨平台宿主框架（完整骨架路线）

构建在 Rong 之上，覆盖 Android/iOS/HarmonyOS/macOS/Windows，三层与本项目**高度同构**：View（WebView 内 React/Vue/HTML 渲染）/ Logic（原生 JS 或 Rust 宿主，持状态调平台 API）/ Bridge（Rust 运行时，转发 setData/流/通道/原生调用）。CLI 可直接创建 native host app + lxapp（小程序形态）；Rust 扩展经 `#[lingxia::native]` + `HostAddon` 注册；`lingxia-webview` 已抹平四平台 WebView 后端差异（Windows 用 WebView2）。

### 2.3 Wry —— 轻量视图容器（不想要完整框架时）

Tauri 同源的跨平台 WebView 库（Windows/macOS/Linux/iOS/Android），**`build_as_child` 子 WebView** 允许把小程序视图作为 IM 主界面的一个子面板嵌入而非全屏替换。配合 Rong 做逻辑层，中间自研 Bridge。

### 2.4 Wasmtime —— 第三方不可信代码的沙箱（可选层）

Bytecode Alliance 维护，默认近零权限：fuel/epoch 中断卡死循环、线性内存限额、宿主经 host functions **显式授权**文件/网络访问。第三方上传的逻辑编译为 Wasm、在独立 Wasmtime 实例执行，与 JS 逻辑层隔离——无限循环或越权尝试都无法突破。生产成熟度：24/7 fuzzing + 正式安全策略。

### 2.5 Boa —— 纯 Rust 引擎（已有资产）

container 现役引擎，纯 Rust 无 C 依赖，Test262 ≈94%。过渡期继续服务研究装载器；产品化后逻辑层主力切 Rong（多引擎），Boa 保留为纯 Rust 环境的备选。

---

## 三、Bridge 协议契约（自研方案的核心产出）

宿主内部 wire format，**不对外公开**。三类消息，`bridgeId` 贯穿路由：

```text
// ① 数据同步通道（setData 语义）：逻辑层推状态快照，视图层只读快照
{ "bridgeId": "lxapp-7f3a/page-1", "type": "publish", "kind": "setData", "payload": { ... } }

// ② 事件上行：视图层用户事件 → 逻辑层
{ "bridgeId": "lxapp-7f3a/page-1", "type": "publish", "kind": "event",
  "name": "onTapSubmit", "payload": { ... } }

// ③ 能力调用（invoke 请求/响应）：Bridge 拦截 → 权限校验 + 频率限制 → 转发
{ "bridgeId": "lxapp-7f3a/page-1", "type": "invoke", "id": "req-42",
  "api": "im.sendMessage", "payload": { "convId": "...", "text": "..." } }
→ { "type": "invokeResult", "id": "req-42", "ok": true, "result": { ... } }

// ④ 生命周期与路由标识
{ "type": "lifecycle", "event": "page.load|page.unload|app.background",
  "bridgeId": "lxapp-7f3a/page-1" }
```

**协议规则**（采纳 Dimina 二分法 + LingXia 模型）：
- `publish` 走**异步帧批处理**（setData 合帧下发，视图不逐字段拉取逻辑层内部状态）；
- `invoke` 走**请求/响应**模型（id 配对），Bridge 统一做权限校验中间件与频率限制；
- `bridgeId` 编码 `lxapp实例/页面` 两级；页面栈、打开/关闭、异步回调的标识规则**必须显式定义**——这是桥接协议最容易漏的部分；
- 序列化用 JSON 起步，高频通道可切 Protobuf（内部契约，随时可换）。

---

## 四、与 IM 后端的集成边界

| 集成点 | 规则 |
|---|---|
| **包管理（Rust 后端）** | 小程序包上传 → **签名校验、内容摘要比对、解包体积限制** → 才允许下发。包格式复用 DPG1（补签名字段与 manifest 扩展：name/main_js/声明的 capabilities）；文件布局是内部契约，不兼容微信。参考 Quick App RPK 签名与 Dimina 产物结构 |
| **能力暴露（宿主侧）** | IM 能力经 Rong `#[js_class]` / LingXia `HostAddon` 封装为 JS API；**权限声明存 Rust 后端**：小程序在平台声明所需能力，Bridge 转发 `invoke` 时查询，未授权直接拒绝 |
| **支付（独立服务）** | `payment.request` 只**转发给独立 payment-service**（ADR-12 四层），资金逻辑绝不进入小程序运行时或 IM 消息管道；支付结果以自定义消息卡片回聊天窗口 |
| **渲染共存** | 小程序视图 = IM 主界面中的**子面板/独立页**（Wry 子 WebView / LingXia 容器）；聊天列表与消息流仍由 Flutter/Tauri 原生渲染，两者经 Bridge 通信、互不侵入渲染栈 |
| **沙箱分级** | 第一方小程序：Rong JS + Worker 有界执行即可；**第三方小程序：Wasmtime 隔离 + 包签名强制**，两级信任模型 |

---

## 五、分阶段落地（并入总路线图 P5+ 独立轨道）

| 阶段 | 目标 | 产出 / 验收 |
|---|---|---|
| **M1 逻辑层跑通** | Rong + QuickJS 在 Rust 宿主执行 JS，`#[js_class]` 暴露模拟 `sendMessage` | 验证引擎绑定/异步/Worker 池基本形状 |
| **M2 视图与 Bridge 闭环** | 接 Wry（或 LingXia WebView），定义 setData+事件通道，JS 驱动 UI 更新、点击回逻辑层 | 验证 Bridge 路由、bridgeId 管理、序列化 |
| **M3 多引擎多平台** | Rong 切引擎 QuickJS → JSC → ArkJS，同一份 Rust 绑定行为一致 | iOS/Android/鸿蒙逻辑层复用确认 |
| **M4 沙箱与第三方** | Wasmtime 隔离不可信模块；Rong 有界执行+Worker 超时 | 资源限制与越权拦截验证 |
| **M5 与 IM 后端集成** | 后端包管理/签名/权限配置；Bridge 接 IM 能力代理与支付转发 | 端到端："打开小程序 → 调 IM 发消息 → 支付卡片回传" |

---

## 第六部分：与现有 wxlike 资产的映射（升级路径）

| 现有资产 | 现状 | 升级动作 |
|---|---|---|
| `container`（engine.rs / bridge.rs / Container 生命周期） | Boa 单引擎 + wxhost 极简桥（log/setTitle/store/nowMs）+ 生命周期钩子 | engine.rs → Rong 多引擎；bridge.rs → publish/invoke 双通道协议；保留生命周期模型 |
| `dpage`（DPG1 桩头 + zstd + manifest） | 自有包格式已可用 | 补：签名字段、capabilities 声明、产物编译层（可选 DSL） |
| `platform`（能力 API 面） | 好友/群/媒体/钱包 HTTP API | 小程序 `invoke` 的能力清单从这里收敛，权限表新建 |
| ADR-12 payment-service | 独立资金服务规划中 | 小程序 `payment.request` 只转发，回执以消息卡片回会话 |

---

## 第七部分：参考链接总表（全轮次合并，统一格式）

> 格式：项目 | 栈 | 链接 | 许可 | 借什么。按架构角色分组；⭕=本项目直接组件，📐=架构参考，⚠️待核=使用前先核 LICENSE。

### 7-A 小程序运行时（本方案组件）

| 项目 | 栈 | 链接 | 许可 | 角色 |
|---|---|---|---|---|
| Rong | Rust | https://github.com/LingXia-Dev/Rong | ⚠️待核 | ⭕ 逻辑层：多引擎（QuickJS/JSC/ArkJS）+ `#[js_class]` 绑定 + Worker 池 + 有界执行 |
| LingXia | Rust | https://github.com/LingXia-Dev/LingXia | ⚠️待核 | ⭕ 完整宿主骨架：View/Logic/Bridge 三层 + CLI + `HostAddon` + lingxia-webview（Android/iOS/HarmonyOS/macOS/Windows） |
| Wry | Rust | https://github.com/tauri-apps/wry | ✅ MIT/Apache-2.0 | ⭕ 轻量视图容器（`build_as_child` 子面板嵌入 IM） |
| Wasmtime | Rust | https://github.com/bytecodealliance/wasmtime | ✅ Apache-2.0（LLVM 例外） | ⭕ 第三方不可信代码沙箱（fuel/epoch + host functions） |
| Boa | Rust | https://github.com/boa-dev/boa | ✅ MIT/Apache-2.0 | ⭕ 纯 Rust JS 引擎（现役，Test262 ≈94%） |
| Dimina（星河） | TS | https://github.com/didi/dimina · 架构文档 https://github.com/didi/dimina/blob/main/docs/Architecture-Details.md | ⚠️待核（滴滴开源） | 📐 DMCC 编译器、逻辑/视图分离（QuickJS+WebView / JSC+WKWebView / 鸿蒙）、publish/invoke Bridge 二分法 |

### 7-B 万人群 / 弹幕 / 后端架构

| 项目 | 栈 | 链接 | 许可 | 角色 |
|---|---|---|---|---|
| WuKongIM | Go | https://github.com/WuKongIM/WuKongIM | ⚠️待核 | 📐 频道订阅模型、无上限群、1 字节心跳、自包含存储 |
| OpenIM | Go | https://github.com/openimsdk/open-im-server · SDK https://github.com/openimsdk | ⚠️待核 | 📐 五层成员同步契约（判断→增量→兜底→批量→懒加载） |
| goim (Bilibili) | Go | https://github.com/Terry-Mao/goim | ⚠️待核 | 📐 分层推送（活跃实时/非活跃聚合）、千万长连接 |
| only-talk-rs | Rust | https://github.com/nicolastinger/only-talk-rs | ⚠️待核 | 📐 工程骨架（Actix+QUIC+PG+Redis+S3+JWT 分层） |
| ISM | Rust | https://github.com/JrTimha/ISM | ⚠️待核 | 📐 消息正文与元数据分离存储 |
| flare-core | Rust | https://github.com/flare-im/flare-core | ⚠️待核 | 📐 传输层：心跳/重连/序列化协商/慢消费者隔离 |

### 7-C 企业级 / 全球平台

| 项目 | 栈 | 链接 | 许可 | 角色 |
|---|---|---|---|---|
| Rocket.Chat | JS | https://github.com/RocketChat/Rocket.Chat | ✅ MIT（待核复核） | 📐 私有化部署、频道/角色 |
| Mattermost | Go | https://github.com/mattermost/mattermost | ⚠️MIT 待核 | 📐 企业群管理、合规 |
| Element Web | TS | https://github.com/element-hq/element-web | ✅ Apache-2.0 | 📐 E2EE 密钥验证交互 |
| Synapse | Python | https://github.com/element-hq/synapse | ✅ AGPL-3.0（同许可，可直接复用） | 📐 联邦/房间模型 |
| Telegram Android | Java/Kotlin | https://github.com/DrKLO/Telegram | ✅ GPL-3.0 | 📐 消息列表回收复用、弱网 |
| Telegram iOS | Swift | https://github.com/TelegramMessenger/Telegram-iOS | ✅ GPL-3.0 | 📐 移动端内存管理 |
| Telegram Desktop | C++/Qt | https://github.com/telegramdesktop/tdesktop | ✅ GPL-3.0 | 📐 桌面网络层 |
| Telegram Web (tweb) | TS | https://github.com/morethanwords/tweb | ✅ GPL-3.0 | 📐 Web 端 WS 架构 |
| TDLib | C++ | https://github.com/tdlib/td | ✅ Boost 1.0 | 📐 跨平台核心库范式（core 目标形态） |
| teamgram-server | Go | https://github.com/teamgram/teamgram-server | ⚠️待核 | 📐 MTProto 服务端分层 |
| Gramsrv / MyTelegram | Go / C# | https://github.com/iamxvbaba/gramsrv · https://github.com/loyldg/mytelegram | ⚠️待核 | 📐 协议实现 |
| hungry-rs / grammers / ferogram / tuigram / telegram_rust | Rust | https://github.com/dimentyy/hungry-rs · https://github.com/Lonami/grammers · https://github.com/ankit-chaubey/ferogram · https://github.com/erictran308/tuigram · https://github.com/oovets/telegram_rust | ⚠️待核（grammers MIT） | 📐 Rust 加密协议/异步架构 |

### 7-D 国内开源 IM

| 项目 | 栈 | 链接 | 许可 | 角色 |
|---|---|---|---|---|
| 唐僧叨叨 | Go+Vue+Android | Server https://github.com/TangSengDaoDao/TangSengDaoDaoServer · Web https://github.com/TangSengDaoDao/TangSengDaoDaoWeb · Android https://github.com/TangSengDaoDao/TangSengDaoDaoAndroid | ✅ Apache-2.0 | 📐 微信还原度最高：消息操作五件套模型、UI 交互 |
| Startalk | 多端 | https://github.com/startalkIM | ⚠️待核 | 📐 Docker 部署 |
| XyTalk | Java | https://github.com/xy-Group/xyTalk-pc | ⚠️待核 | 📐 企业场景 |
| box-im | Java+Vue+UniApp | https://github.com/bluexsx/box-im | ✅ MIT | 📐 全栈仿微信（MQ 设计已借） |
| HuLa（+Server） | Rust+Tauri2+Vue3 | https://github.com/HuLaSpark/HuLa · https://github.com/HulaSpark/HuLa-Server | ✅ Apache-2.0 | 📐 前端多端工程组织、UI 组件、Tauri 移动配置（Web 不支持注意） |

### 7-E 微信高仿（多数无 LICENSE——只学思想）

| 项目 | 栈 | 链接 | 许可 | 角色 |
|---|---|---|---|---|
| LQRWeChat | Android | https://github.com/GitLqr/LQRWeChat | ❌无 LICENSE | 思想 |
| vue-WeChat | Vue2 | https://github.com/arden-kfc/vue-WeChat | ⚠️待核 | WeUI 交互 |
| CoderMikeHe/WeChat | iOS | https://github.com/CoderMikeHe/WeChat | ❌待核 | MVVM 组织 |
| SwiftUI-WeChat | SwiftUI | https://github.com/wxxsw/SwiftUI-WeChat | ⚠️MIT 待核 | 声明式写法 |
| fake-world | React | https://github.com/shanyu-e/fake-world | ⚠️待核 | 聊天记录数据模型 |
| WeChatArkTS | ArkTS | https://github.com/xiaowuDev/Wechat_ArktsDevelop | ❌待核 | 鸿蒙 UI 思想 |

### 7-F 基础设施 / 跨平台

| 项目 | 栈 | 链接 | 许可 | 角色 |
|---|---|---|---|---|
| sandcat-backend | Rust 微服务 | https://github.com/Xu-Mj/sandcat-backend | ⚠️待核 | 📐 PG/Mongo 分片、服务拆分 |
| Mimir | Rust+UniFFI | https://github.com/Mimir-IM/mimir | ⚠️待核 | 📐 UniFFI 绑定范式（P5 移动端） |
| iroh | Rust | https://github.com/n0-computer/iroh | ✅ Apache-2.0/MIT | 📐 P2P/NAT（远期） |
| PaperPhone-Plus | React+Axum | https://github.com/619dev/Paperphone-plus | ⚠️待核 | 📐 微信风格 E2EE 全栈 |
| Tauri | Rust | https://github.com/tauri-apps/tauri | ✅ MIT/Apache-2.0 | ⭕ 桌面容器（现役） |
| Flutter | Dart | https://github.com/flutter/flutter | ✅ BSD-3 | ⭕ 移动端 UI（P5，ADR-11） |
| ArkUI-X | ArkTS | https://github.com/arkui-x（主仓 GitCode/Gitee） | ✅ Apache-2.0 | 鸿蒙备选（ADR-11） |
| Flutter-OH | Dart/OH | 主仓 **https://gitcode.com/openharmony-tpc/flutter_flutter**（不在 GitHub）· 镜像 https://atomgit.com/openharmony-tpc/flutter_flutter · 组织 https://atomgit.com/CPF-Flutter · 兼容库 https://atomgit.com/openharmony-tpc/flutter_packages | ⚠️BSD-3 社区版 | ⭕ 鸿蒙适配（生产分支 3.27.4） |

### 7-G 直播 / 媒体面 / 弹幕

| 项目 | 栈 | 链接 | 许可 | 角色 |
|---|---|---|---|---|
| Xiu | Rust | https://github.com/harlanc/xiu | ⚠️待核 | ⭕ 直播媒体面首选（RTMP 集群/RTSP/WebRTC/HTTP-FLV/HLS） |
| atm0s-media-server | Rust | https://github.com/8xFF/atm0s-media-server（RTMP 走 legacy 分支） | ⚠️待核 | 📐 去中心化 SFU（重构期观察） |
| mediasoup | Rust/C++ | https://github.com/versatica/mediasoup（crate `v3/rust/`） | ✅ ISC | 📐 SFU 语义（Simulcast/SVC） |
| sorted_set_nif | Rust | https://github.com/discord/sorted_set_nif | ⚠️待核 | 📐 Discord 官方 Rust NIF（有序集合） |
| Discord 架构文档 | 文档 | https://github.com/babushkai/system-design-patterns/blob/codex/article-reading-redesign/08-case-studies/08-discord.md | ➖ | 📐 Elixir+Rust+SFU 分层全参考 |
| blivedm_rs | Rust | https://github.com/isomoes/blivedm_rs（备选 https://github.com/jiahaoxiang2000/blivedm_rs） | ⚠️待核 | 📐 弹幕 WS 客户端（Tokio） |

### 7-H 支付基础设施（ADR-12 配套）

| 项目 | 栈 | 链接 | 许可 | 角色 |
|---|---|---|---|---|
| Cala (cala-ledger) | Rust | https://github.com/GaloyMoney/cala · https://docs.rs/cala-ledger | ⚠️待核 | ⭕ P4 账本核心（复式记账+事件溯源+outbox） |
| ousia-ledger | Rust | https://github.com/TheOusia/ousia | ⚠️待核 | 📐 两阶段执行备选 |
| TigerBeetle / tb-rs | Zig/Rust | https://github.com/tigerbeetle/tigerbeetle · https://github.com/dsp/tb | ⚠️待核 | 📐 账本存储备选 |
| dtmrs | Rust | https://github.com/jackwangfeng/dtmrs | ⚠️待核 | ⭕ P4 嵌入式 Saga/TCC |
| Hyperswitch | Rust | https://github.com/juspay/hyperswitch · https://docs.hyperswitch.io | ✅ Apache-2.0 | 📐 ChannelConnector trait 范式、幂等键、FSM |
| obix | Rust | https://github.com/GaloyMoney/obix | ⚠️待核 | ⭕ P4 Outbox/Inbox 事件流 |
