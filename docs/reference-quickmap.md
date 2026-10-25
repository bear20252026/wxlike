# borrow-first 按组速查表（2026-10-24）

> 目的：每个组开工/评审前**先查本表对应行**，再动手写代码。
> 完整政策见 [THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md) 与
> [reference-mirror.md](reference-mirror.md)（Tier 0 总表）、
> [full-platform-delivery/references.md](full-platform-delivery/references.md)（移植流程与许可义务）。
> 镜像统一在仓库外 `D:\mimo\test\refs\`（浅克隆）。

## 怎么借（四步，缺一不可）

1. **查本表** → 找到本组优先参照与入口目录；
2. **选引入方式**：同语言能整段照搬就 vendoring/依赖；不能就直接依赖 crate/npm；
   语言不同就**按原语境重写**（保留分层与边界条件），绝不逐行翻译；
3. **移植 PR 七要素**：上游仓库 + 完整 SHA + 文件级许可 + NOTICE + 引入方式 +
   本地适配差异 + 上游测试改编（references.md 第 1 节）；
4. **G17-01 来源审查**：SBOM 注册表登记，CI 有零 diff 门禁。

红线：反编译代码（微信/腾讯产物）一行不进分发物；sourcemap 找回的原始源码只当地图。

## 按组对照

| 组 | 跟踪 | 卡域 | 优先参照（refs/ 名称 · 许可 · 看哪里） |
|---|---|---|---|
| G00 | #81 | 地基/契约/拆分 | **tauri**(MIT/Apache·capabilities 最小授权)；**uniffi-rs**(MPL·bindgen) |
| G01 | #82 | 共享核心/三端绑定 | **uniffi-rs**·`uniffi_bindgen/`+`examples/`；**matrix-rust-sdk**(Apache·`crates/matrix-sdk-{base,sqlite,indexeddb}` 存储抽象/跨端缓存)；**element-web**·`shared-types/`+`shared-utils/`（分包边界） |
| G02 | #83 | 账号/设备/加密/关系 | **vodozemac**(Apache·Olm/Megolm，**不要重写密码原语**)；**Signal-Desktop**(AGPL·`ts/state/` 设备变更/安全编号)；**synapse**(AGPL·`synapse/state/`+`event_auth.py` 授权测试模型)；**Rocket.Chat**(MIT·权限矩阵) |
| G03 | #84 | 消息事务/同步/离线 | **zulip**(Apache·`web/src/echo*.ts`+`local_message.ts`+`message_events.ts` 回显→确认；`list_cursor.ts` 稳定游标)；**matrix-rust-sdk**·滑动同步；**mars**(MIT·心跳/退避参数表，已投递 #223)；**box-im**(已读/撤回/去重) |
| G04 | #85 | 上传/对象/文件中心 | **OpenDAL**(Apache·`core/` 统一对象存储，包在 MediaStore 后)；**Signal-Desktop**(AGPL·附件分片/断点续传/发送队列) |
| G05 | #86 | 朋友圈/相册/公众号文章 | **false-world**+**hpf-circle**(GPL/学习·九宫格性能/交互清单)；**ammonia**(MIT/Apache·文章富文本净化)+**DOMPurify**(客户端净化) |
| G06 | #87 | 账本/支付/红包/卡券 | **cala**(Rust 复式账本·不可变账本/资金账户对照)；**hyperswitch**(Apache·支付意图/回调/对账流程)；验收规则已投递 #225 |
| G07 | #88 | 二维码/OCR/模型服务 | **zxing-cpp**(Apache·`core/`+`wrappers/`) 或 **rqrr**(MIT/Apache·纯 Rust 利于 WASM)——二选一做实测对比；OCR/翻译/语音无 Tier0 对照，自研+外部 API 边界 |
| G08 | #89 | 小程序容器全链 | **glass-easel**(MIT·组件内核，已投递 #150 **认领 G08-05 前必读**)；**dimina**(Apache·滴滴四端容器整装参照)；**glass-easel-analyzer**(MIT·工具链) |
| G09 | #90 | RTC/信令/直播 | **TRTC_Web**(开源·房间状态机/信令语义，已投递 #153)；**spreed**(AGPL·CallView/会话设置/信令)；**atm0s-media-server**(Rust+str0m·SFU 分层)；**Signal-Desktop**·CallScreen(设备选择) |
| G10 | #91 | 桌面壳/聊天闭环 | **element-web**(房间列表虚拟化/E2EE 设备验证 UI)；**zulip**(消息菜单/编辑历史)；**TanStack Query+Virtual**(MIT·缓存投影/长列表)；**HuLa-IM-Tauri**(Tauri IM 竖屏参照) |
| G11 | #92 | UI v2 五面 | **TanStack Query/Virtual**；**element-web**·`shared-components/`；**spreed**·`src/components/CallView/`；**glass-easel**(小程序页)；**zxing**(扫码页) |
| G12 | #93 | 移动壳/实时消息/投影 | **Signal-Android**(AGPL·`app/src/main/java/.../{calls,chats,messages,components}/` 会话状态机/通知聚合/媒体压缩)；**uniffi-rs**(Dart 绑定)；**matrix-rust-sdk**·`matrix-sdk-sqlite` 本地投影 |
| G13 | #94 | 内容面/视频号/直播 | **spreed**(通话/直播面板)；**Signal-Android**(通知聚合)；**blivedm_rs**(直播弹幕流 Rust)；**false-world**(卡片内容交互) |
| G14 | #95 | adminx/RBAC/审计 | **mattermost**(AGPL+Apache webapp·`server/` 群角色/管理操作/审计；webapp Apache 部分可直接看)；**Rocket.Chat**(MIT·用户/群管理/审计交互) |
| G15 | #96 | MCP/CLI/AI 命令面 | 待评审 #212–#216；`rmcp` 官方接入与 `agent-policy` 抽取已登记 `integration-requests/G15/`，无新增参照 |
| G16 | #97 | CI/构建/发布 | **mattermost**(部署测试矩阵)；**tauri**(跨平台构建/更新通道) |
| G17 | #98 | 来源审查/威胁模型 | 本组就是门禁；**ammonia** 选型对比；mmtls 公开文献已投递 `integration-requests/G17/borrow-public-research.yaml` |

## 本轮新增镜像（2026-10-24）

- `refs/glass-easel`（MIT）· `refs/TRTC_Web` · `refs/dimina` 已存在（Apache-2.0，四端小程序容器）
- 申请 G00 增补 Tier 0 表：`integration-requests/G00/reference-tier0-additions.yaml`
