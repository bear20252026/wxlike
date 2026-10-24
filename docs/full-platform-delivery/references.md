# 成熟项目复用指南：先选组件，再移植业务模式

检索日期：2026-10-24。完整远端HEAD见upstream_manifest.json。下列路径是定位入口；路径存在不等于已审完每个文件。禁止从HEAD直接生成生产依赖版本建议，开发时固定经过测试的发行版或提交。用户提供的refs镜像未在本环境找到，因此本次使用联网仓库元数据、目录和许可证样本，不声称完成本地八仓逐行审计。

## 1. 复用政策与移植流程

优先级：已有wxlike能力 → 维护活跃且许可兼容的成熟库直接依赖 → 独立模块带出处移植 → 参考交互/状态机后用Rust适配。不能为“尽量搬”同时引入三套消息SDK、两个RTC房间系统和两份交易真相。TS/Python/Go参考项目的业务不能原封并入后就宣称同一Rust核心；可搬UI组件和测试场景，服务端业务经Rust用例/ports适配。

每次移植PR必须附：上游仓库、完整SHA、具体文件永久链接、原始许可与版权、NOTICE、例外目录检查、引入方式（依赖/复制/参考）、本地适配差异、上游测试改编、安全公告检索日、后续更新owner。示例链接格式`https://github.com/<repo>/blob/<manifest中的sha>/<path>`。只有目录级研究的候选必须先完成文件级审查，不能拿目录说明充当复制批准。

Apache-2.0保留版权和NOTICE、标明修改；MIT保留许可与版权；MPL-2.0按文件级义务处理；AGPL复用必须保留版权、许可及适用源码提供要求，G00/G17按本项目既有AGPL策略核查。当前工程准入：wxlike已核实为AGPL-3.0-only；兼容的AGPL代码可列入移植候选，由G17核对版本、文件许可及依赖。若项目决定接受相应开源义务，可另建有明确许可边界的集成方案，不能通过“改变量名”规避。商业/enterprise目录、商标、图片、字体、模型权重都另核。

## 2. 用户指定8仓：具体可以借什么

| 项目与入口 | 许可核查与引入方式 | 建议参考代码位置 | 对应卡与应搬出的能力 |
|---|---|---|---|
| [Zulip](https://github.com/zulip/zulip) | Apache-2.0；优先UI/测试与算法适配 | `web/src/echo.ts`、`echo_state.ts`、`local_message.ts`、`message_events.ts`、`message_actions_popover.ts`、`list_cursor.ts` | G03/G10/G12：本地回显→服务端确认、消息菜单、稳定列表游标；不要照搬Python后端作为第二业务核心 |
| [Rocket.Chat](https://github.com/RocketChat/Rocket.Chat) | 根LICENSE为混合边界，非ee代码通常MIT；排除`ee/`及`apps/meteor/ee/`，仍逐文件核查 | `apps/meteor/client/`、`apps/meteor/server/` | G02/G03/G10/G14：会话订阅、权限矩阵、用户/群管理与审计交互；Meteor方法不能绕开Rust用例 |
| [Mattermost](https://github.com/mattermost/mattermost) | 不是“一仓纯AGPL”这么简单；源码AGPL及例外/商业目录，部分admin/config/webapp条款不同，按LICENSE逐文件，兼容AGPL部分可审核移植 | `server/`与根`LICENSE.txt` | G02/G03/G14/G16：频道/群角色、通知、管理操作、后台作业与部署测试；不能将发行二进制许可推到所有源码 |
| [Signal Desktop](https://github.com/signalapp/Signal-Desktop) | AGPL-3.0；可按本项目兼容许可策略审核移植 | `ts/components/`、`ts/messages/`、`ts/state/` | G02/G09/G10：发送状态、设备变更、安全编号、CallScreen与设备选择交互；libsignal本身另核，不能假定MIT |
| [Signal Android](https://github.com/signalapp/Signal-Android) | AGPL-3.0；可按本项目兼容许可策略审核移植 | `app/src/main/java/org/thoughtcrime/securesms/{calls,chats,messages,components,crypto}/` | G02/G12/G13：Android生命周期、来电/录音、消息恢复、密钥安全交互；Kotlin代码仅平台桥可考虑合规移植 |
| [Element Web](https://github.com/element-hq/element-web) | 当前源码含AGPL/GPL/商业许可边界；可按兼容AGPL部分审核移植，勿套历史Apache印象 | `packages/shared-components/`、`shared-types/`、`shared-utils/` | G01/G10/G11：状态投影、虚拟时间线、共享组件分包；不把Matrix业务SDK塞入wxlike自有协议 |
| [Synapse](https://github.com/element-hq/synapse) | 当前AGPL/商业路径；兼容AGPL部分可审核移植 | `synapse/state/`、`events/`、`event_auth.py`、`crypto/` | G02/G03：事件授权、成员状态、设备密钥与历史可见性测试模型；不抄Python系统再维护两套真相 |
| [Nextcloud Talk](https://github.com/nextcloud/spreed) | AGPL-3.0；可按本项目兼容许可策略审核移植 | `src/components/{CallView,ConversationSettings,MessagesList,NewMessage}/` | G09/G11/G13：通话控件、设备选择、会话设置、消息输入状态；PHP/Nextcloud权限不得直接替代Rust服务授权 |

## 3. 新增候选：更适合直接拼装的基础组件

| 候选 | 许可/直接依赖边界 | 定位入口与用途 | 负责人与适配要求 |
|---|---|---|---|
| [Tauri](https://github.com/tauri-apps/tauri) | MIT/Apache-2.0，插件单独核 | `crates/`、官方capabilities文档；现有桌面宿主 | G00/G10：沿用已有Tauri，按窗口最小授权；guest无主IPC |
| [UniFFI](https://github.com/mozilla/uniffi-rs) | MPL-2.0 | `uniffi_bindgen/`、`examples/`；原生绑定与异步对象 | G01：优先依赖生成器，Dart后端支持按现有实现测试，不假定上游所有语言支持相同 |
| [matrix-rust-sdk](https://github.com/matrix-org/matrix-rust-sdk) | Apache-2.0，依赖另核 | `crates/{matrix-sdk-base,matrix-sdk-crypto,matrix-sdk-indexeddb,matrix-sdk-sqlite,matrix-sdk-store-encryption,matrix-sdk-ui}/` | G01/G02/G03：参考存储抽象、滑动同步、跨端缓存和加密测试；协议不同，不整包替换当前客户端 |
| [vodozemac](https://github.com/matrix-org/vodozemac) | Apache-2.0 | crate源码与测试；Olm/Megolm | G02：沿用现有加密路径，补设备认证与多设备协议；不要重写密码原语 |
| [OpenDAL](https://github.com/apache/opendal) | Apache-2.0 | `core/`及服务backend；统一对象存储 | G04：包在MediaStore后，统一ACL/引用由wxlike负责，库不代替业务授权 |
| [ZXing-C++](https://github.com/zxing-cpp/zxing-cpp) | Apache-2.0，第三方/文件例外核查 | `core/`、`wrappers/`；二维码编码解码 | G07/G12：Rust/FFI或平台桥封装，绑定版本与解码预算，不引入完整第二套界面 |
| [rqrr](https://github.com/WanzenBug/rqrr) | `(MIT OR Apache-2.0) AND ISC`；锁定时核LICENSE | Rust二维码解码替代候选，未在HEAD manifest中固定 | G07：与ZXing二选一做真实图像识别比较，纯Rust有利WASM；不要求重复安装两个引擎 |
| [ammonia](https://github.com/rust-ammonia/ammonia) | MIT/Apache-2.0；以文件为准 | Rust HTML sanitizer crate | G05/G17：选已修复版本，默认禁style/外部资源；净化+CSP并用，不能单独保证XSS全消失 |
| [DOMPurify](https://github.com/cure53/DOMPurify) | Apache-2.0/MPL-2.0选择及文件条款核查 | `src/`；浏览器富文本防御 | G10/G11/G14：补客户端净化，服务端Rust仍强制净化；Trusted Types/CSP按目标浏览器验证 |
| [TanStack Query](https://github.com/TanStack/query) | MIT | `packages/react-query/`；React请求状态 | G10/G11/G14：仅缓存Rust SDK投影与请求，不新增消息outbox/财务规则；事件到来按明确key失效 |
| [TanStack Virtual](https://github.com/TanStack/virtual) | MIT | `packages/react-virtual/`；长列表 | G10/G11：稳定消息key、测量高度、分页锚点，不能简单换offset分页 |
| [Tiptap](https://github.com/ueberdosis/tiptap) | MIT核心；Pro/Cloud不在免费复制范围 | `packages/core/`及已核许可extension | G11：收藏笔记/文章编辑器，文档schema服务端验证，禁任意HTML注入 |
| [react-admin](https://github.com/marmelab/react-admin) | 社区MIT；Enterprise另付费/许可 | `packages/ra-core/`、`ra-ui-materialui/` | G14：快速管理壳、DataProvider/AuthProvider接Rust管理API；复杂权限仍由服务端执行 |
| [Medusa](https://github.com/medusajs/medusa) | 根MIT但明确排除enterprise文件，须读ENTERPRISE_LICENSE | `packages/modules/{payment,promotion,order,cart,auth}/` | G06/G07/G14：订单/支付Provider接口与促销测试模式可适配Rust；不部署Node为第二业务核心，不搬enterprise |
| [LiveKit](https://github.com/livekit/livekit) | Apache-2.0；各客户端SDK另核 | `pkg/`、官方客户端SDK；RTC候选与架构参考 | G09：仅可行性对比；当前首选str0m不能同时保留两套房间authority。Go基础设施若改选需全局ADR，仍不搬账号/交易业务 |
| [coturn](https://github.com/coturn/coturn) | BSD-3-Clause，根LICENSE已核 | `src/`；成熟TURN服务 | G09/G16：作为网络基础设施容器部署，锁digest、短期凭据、端口与出口限制，不能开匿名中继 |
| [Wasmtime](https://github.com/bytecodealliance/wasmtime) | Apache-2.0 with LLVM exceptions等按LICENSE | `crates/wasmtime/`；可选WASM执行隔离 | G08：只可选服务器/桌面，先补公告交集；iOS兼容和JIT不能从库存在推断，首发小程序不依赖它 |
| [Candle](https://github.com/huggingface/candle) | MIT/Apache-2.0；模型权重另核 | `candle-core/`、`candle-transformers/`、`candle-examples/` | G07：优先服务端识别/翻译推理，模型隔离与有界内存；手机不常驻全部权重 |
| [whisper.cpp](https://github.com/ggml-org/whisper.cpp) | MIT；权重、示例媒体另核 | `src/`、`examples/`；语音识别 | G07/G04：任务化转写，可取消；C/C++是通用模型引擎，不复制业务规则，Rust统一调度 |
| [OpenAPI Generator](https://github.com/OpenAPITools/openapi-generator) | Apache-2.0；生成模板及runtime核查 | `modules/openapi-generator/`；类型/客户端生成候选 | G01：已有工具若够用不新增Java负担；只生成边界类型和transport，不生成另一套用例 |
| [官方Rust MCP SDK](https://github.com/modelcontextprotocol/rust-sdk) | 当前新贡献Apache-2.0、旧代码MIT、文档CC-BY-4.0，不能笼统标MIT | `crates/`、`examples/`；rmcp适配 | G15：优先官方实现，抽象后逐步替换旧代码；协议版本与transport选型按现有门禁冻结 |
| [rust-mcp-sdk替代实现](https://github.com/rust-mcp-stack/rust-mcp-sdk) | MIT | Rust MCP server/client实现候选 | G15：作为对比而非同时引入；若选官方SDK则本项仅研究，不拉两个同功能运行库 |

## 4. 每组开工应阅读与复制优先次序

- G01：matrix-rust-sdk缓存/IndexedDB/SQLite结构、UniFFI生成与异步句柄；优先抽现有wxlike SDK。
- G02/G03：vodozemac依赖、Zulip本地回显与事件测试；Signal/Synapse可按AGPL兼容策略移植组件/测试，Python/TS业务仍适配Rust。
- G04：OpenDAL依赖、成熟FFmpeg基础设施；用户数据解码必须限资源。
- G05/G10/G11/G13：Zulip宽松许可UI、TanStack虚拟列表、Tiptap核心、Rust ammonia + DOMPurify防御。
- G06/G07：Medusa社区模块的订单/支付Provider设计、ZXing/rqrr真实码；交易账本与幂等由Rust实现。
- G08：Tauri能力边界、WebView/Web origin隔离、可选Wasmtime；不要把一个JS解释器当完整小程序平台。
- G09：先现有RTC证明可行，LiveKit/Nextcloud/Signal参考交互；coturn直接用成熟部署。
- G14：react-admin社区UI框架；管理操作必须落域API。
- G15：官方rmcp + 本项目Executor，不给模型任意数据库或shell。
- G16/G17：上游测试场景和安全策略可按许可适配，扫描实际锁/镜像/运行feature，不只查顶层版本。

## 5. 官方安全与平台文档

近期漏洞及适用性详见主手册第6节。实现者在依赖冻结当日重新查询，不从本清单断言永久安全。

- [RustSec公告](https://rustsec.org/advisories/)：RUSTSEC-2026-0373、0379、0380、0381、0382、0385。
- [coturn安全公告](https://github.com/coturn/coturn/security/advisories)：GHSA-f6hc-79w3-p8pq、GHSA-847g-qmc6-6m4r。
- [plugin-rsc公告](https://github.com/vitejs/vite-plugin-react/security/advisories/GHSA-59pf-q495-3c99)：核对是否实际引入RSC，普通SPA不据此误报。
- [Tauri Capabilities](https://v2.tauri.app/security/capabilities/)：逐窗口/命令最小授权。
- [MCP Security Best Practices](https://modelcontextprotocol.io/docs/tutorials/security/security_best_practices)：token audience、禁止token passthrough、会话与origin安全。
- [Wasmtime平台支持](https://docs.wasmtime.dev/stability-platform-support.html)：解释器/JIT/平台支持与实际iOS部署分开验证。
- [Apple审核指南](https://developer.apple.com/app-store/review/guidelines/)：小程序、支付、下载内容和分发依提交时现行条款专项验证。


## 固定提交快速链接

- [signalapp/Signal-Android](https://github.com/signalapp/Signal-Android/tree/16f138410a4ab940864a2791ae281dd1b2318611) — `16f138410a4ab940864a2791ae281dd1b2318611`
- [zulip/zulip](https://github.com/zulip/zulip/tree/6bef8cb6bdc0029f98bcc14d6ca58b8285d06234) — `6bef8cb6bdc0029f98bcc14d6ca58b8285d06234`
- [signalapp/Signal-Desktop](https://github.com/signalapp/Signal-Desktop/tree/17502b9d1a9e34af689f31508c4ac4ccee5b5cb7) — `17502b9d1a9e34af689f31508c4ac4ccee5b5cb7`
- [RocketChat/Rocket.Chat](https://github.com/RocketChat/Rocket.Chat/tree/1826a86264c14ebb8b6130a5b89ed8fc37cb2ef9) — `1826a86264c14ebb8b6130a5b89ed8fc37cb2ef9`
- [mattermost/mattermost](https://github.com/mattermost/mattermost/tree/c2f2eff3b2684e1be94a24bbc48848bb437389dd) — `c2f2eff3b2684e1be94a24bbc48848bb437389dd`
- [nextcloud/spreed](https://github.com/nextcloud/spreed/tree/1fd0f2133833ecafcaf97bf4e3cda4056e44d44d) — `1fd0f2133833ecafcaf97bf4e3cda4056e44d44d`
- [element-hq/synapse](https://github.com/element-hq/synapse/tree/8d60450b336fecf629cd55aafe3ccb122dc11ada) — `8d60450b336fecf629cd55aafe3ccb122dc11ada`
- [element-hq/element-web](https://github.com/element-hq/element-web/tree/15f63d5959697796e9d30d507e75513b6c97e34f) — `15f63d5959697796e9d30d507e75513b6c97e34f`
- [cure53/DOMPurify](https://github.com/cure53/DOMPurify/tree/b7e0edc1293359d6ab54dc0ccc9cf9bbdd72aff7) — `b7e0edc1293359d6ab54dc0ccc9cf9bbdd72aff7`
- [mozilla/uniffi-rs](https://github.com/mozilla/uniffi-rs/tree/c31f4bc025c7a31eae56b49107f50e6d38e76bc7) — `c31f4bc025c7a31eae56b49107f50e6d38e76bc7`
- [marmelab/react-admin](https://github.com/marmelab/react-admin/tree/089ad1bf6d55500eab228f3833d12a5dbaf530cb) — `089ad1bf6d55500eab228f3833d12a5dbaf530cb`
- [huggingface/candle](https://github.com/huggingface/candle/tree/12bb5d1f6350f4ea9827d9086f213feec1f0af6a) — `12bb5d1f6350f4ea9827d9086f213feec1f0af6a`
- [OpenAPITools/openapi-generator](https://github.com/OpenAPITools/openapi-generator/tree/f822c4feac9ee6fc8244925f893e4cdd342d7c7a) — `f822c4feac9ee6fc8244925f893e4cdd342d7c7a`
- [coturn/coturn](https://github.com/coturn/coturn/tree/596c48bff27e82ecfb15f42327688348f3b49f23) — `596c48bff27e82ecfb15f42327688348f3b49f23`
- [ggml-org/whisper.cpp](https://github.com/ggml-org/whisper.cpp/tree/231638a0717b7822784297e5980965ffcec05810) — `231638a0717b7822784297e5980965ffcec05810`
- [matrix-org/matrix-rust-sdk](https://github.com/matrix-org/matrix-rust-sdk/tree/8de215a0aa0026afa5754ae5f05a6d168d1a1d60) — `8de215a0aa0026afa5754ae5f05a6d168d1a1d60`
- [rust-ammonia/ammonia](https://github.com/rust-ammonia/ammonia/tree/72d56debf499cc88adc253c326d05a3563b8ee06) — `72d56debf499cc88adc253c326d05a3563b8ee06`
- [tauri-apps/tauri](https://github.com/tauri-apps/tauri/tree/c464ef928c35d7eedd64bcd5df16b62b3393b7fa) — `c464ef928c35d7eedd64bcd5df16b62b3393b7fa`
- [livekit/livekit](https://github.com/livekit/livekit/tree/9644b99247f5bf770e83d61d186315588372e408) — `9644b99247f5bf770e83d61d186315588372e408`
- [zxing-cpp/zxing-cpp](https://github.com/zxing-cpp/zxing-cpp/tree/2c3dcfeffa7878950a58c1703c86774a428b0c91) — `2c3dcfeffa7878950a58c1703c86774a428b0c91`
- [matrix-org/vodozemac](https://github.com/matrix-org/vodozemac/tree/70ad23e8cbb1c7c0ab5884553f49a4e4f05af80b) — `70ad23e8cbb1c7c0ab5884553f49a4e4f05af80b`
- [TanStack/query](https://github.com/TanStack/query/tree/9e4253766f8015d1372bcd515f50103fd067a42d) — `9e4253766f8015d1372bcd515f50103fd067a42d`
- [ueberdosis/tiptap](https://github.com/ueberdosis/tiptap/tree/b1f54b402b1b8b535763bfd38b30ae3ddde1104d) — `b1f54b402b1b8b535763bfd38b30ae3ddde1104d`
- [medusajs/medusa](https://github.com/medusajs/medusa/tree/029fb31fd077ab9c6d6fd9d68c676c17c71294c7) — `029fb31fd077ab9c6d6fd9d68c676c17c71294c7`
- [TanStack/virtual](https://github.com/TanStack/virtual/tree/c462c6141ea434fdbc52b5dd866bb8db7e4fd07e) — `c462c6141ea434fdbc52b5dd866bb8db7e4fd07e`
- [bytecodealliance/wasmtime](https://github.com/bytecodealliance/wasmtime/tree/ffff2e2db24a291f02429f123aba1631fa8eea67) — `ffff2e2db24a291f02429f123aba1631fa8eea67`
- [rust-mcp-stack/rust-mcp-sdk](https://github.com/rust-mcp-stack/rust-mcp-sdk/tree/d18de2c19ccfdaa249db56137ff5268e2b193f2f) — `d18de2c19ccfdaa249db56137ff5268e2b193f2f`
- [apache/opendal](https://github.com/apache/opendal/tree/19ba33da9e46738fd4cad29c7ebfd5f3e5ed2cfc) — `19ba33da9e46738fd4cad29c7ebfd5f3e5ed2cfc`
- [modelcontextprotocol/rust-sdk](https://github.com/modelcontextprotocol/rust-sdk/tree/8aec3891dd07fc537b21c2eb2596235640de9025) — `8aec3891dd07fc537b21c2eb2596235640de9025`
