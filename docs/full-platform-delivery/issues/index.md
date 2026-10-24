# GitHub全平台交付任务索引

总跟踪：[Issue #80](https://github.com/bear20252026/wxlike/issues/80)。本轮实际创建 **119 个Issues = 1 总跟踪 + 18 组跟踪 + 100 开发任务**。

最新设计：桌面10页、手机27页；331个onClick位置为待展开的入口种子。全部新任务当前均为PLANNED，不代表功能已实现；完成度以实际Issue、CI与真机证据为准。

## 使用方法

1. 按下表进入自己的职责组，认领独立任务。实际GitHub账号未知，未虚构assignee。
2. 先读独占路径和硬依赖；同组卡默认按序串行。公共锁/router/迁移编号交G00，绑定交G01。
3. 一任务一分支一PR，PR引用任务Issue；合入并通过对应CI/设备/供应商验收后才关闭。
4. group跟踪中的任务列表显示子Issue状态；组长核对关闭条件和进度。Issue索引是发布快照，线上Issue状态为实时真相。

## 18组总览

| 职责组 | 负责人职责 | 组跟踪 | 子任务 | 筛选本组待办 |
|---|---|---|---:|---|
| [G00](#g00) | 全局架构与串行集成 | [#81](https://github.com/bear20252026/wxlike/issues/81) | 5 | [查看](https://github.com/bear20252026/wxlike/issues?q=is%3Aissue%20is%3Aopen%20label%3A%22delivery%3Aui-v2%22%20label%3A%22type%3Atask%22%20label%3A%22group%3AG00%22) |
| [G01](#g01) | Rust共享核心与绑定 | [#82](https://github.com/bear20252026/wxlike/issues/82) | 5 | [查看](https://github.com/bear20252026/wxlike/issues?q=is%3Aissue%20is%3Aopen%20label%3A%22delivery%3Aui-v2%22%20label%3A%22type%3Atask%22%20label%3A%22group%3AG01%22) |
| [G02](#g02) | 身份、关系、设备与加密安全 | [#83](https://github.com/bear20252026/wxlike/issues/83) | 7 | [查看](https://github.com/bear20252026/wxlike/issues?q=is%3Aissue%20is%3Aopen%20label%3A%22delivery%3Aui-v2%22%20label%3A%22type%3Atask%22%20label%3A%22group%3AG02%22) |
| [G03](#g03) | 消息、同步、离线与推送 | [#84](https://github.com/bear20252026/wxlike/issues/84) | 5 | [查看](https://github.com/bear20252026/wxlike/issues?q=is%3Aissue%20is%3Aopen%20label%3A%22delivery%3Aui-v2%22%20label%3A%22type%3Atask%22%20label%3A%22group%3AG03%22) |
| [G04](#g04) | 对象、文件、转码与备份 | [#85](https://github.com/bear20252026/wxlike/issues/85) | 5 | [查看](https://github.com/bear20252026/wxlike/issues?q=is%3Aissue%20is%3Aopen%20label%3A%22delivery%3Aui-v2%22%20label%3A%22type%3Atask%22%20label%3A%22group%3AG04%22) |
| [G05](#g05) | 内容、社交、收藏与检索 | [#86](https://github.com/bear20252026/wxlike/issues/86) | 6 | [查看](https://github.com/bear20252026/wxlike/issues?q=is%3Aissue%20is%3Aopen%20label%3A%22delivery%3Aui-v2%22%20label%3A%22type%3Atask%22%20label%3A%22group%3AG05%22) |
| [G06](#g06) | 账本、支付、红包、卡券 | [#87](https://github.com/bear20252026/wxlike/issues/87) | 5 | [查看](https://github.com/bear20252026/wxlike/issues?q=is%3Aissue%20is%3Aopen%20label%3A%22delivery%3Aui-v2%22%20label%3A%22type%3Atask%22%20label%3A%22group%3AG06%22) |
| [G07](#g07) | 扫码、发现、模型与服务聚合 | [#88](https://github.com/bear20252026/wxlike/issues/88) | 9 | [查看](https://github.com/bear20252026/wxlike/issues?q=is%3Aissue%20is%3Aopen%20label%3A%22delivery%3Aui-v2%22%20label%3A%22type%3Atask%22%20label%3A%22group%3AG07%22) |
| [G08](#g08) | 小程序容器、应用目录与SDK | [#89](https://github.com/bear20252026/wxlike/issues/89) | 5 | [查看](https://github.com/bear20252026/wxlike/issues?q=is%3Aissue%20is%3Aopen%20label%3A%22delivery%3Aui-v2%22%20label%3A%22type%3Atask%22%20label%3A%22group%3AG08%22) |
| [G09](#g09) | 通话、音视频与直播 | [#90](https://github.com/bear20252026/wxlike/issues/90) | 5 | [查看](https://github.com/bear20252026/wxlike/issues?q=is%3Aissue%20is%3Aopen%20label%3A%22delivery%3Aui-v2%22%20label%3A%22type%3Atask%22%20label%3A%22group%3AG09%22) |
| [G10](#g10) | React桌面与网页核心界面 | [#91](https://github.com/bear20252026/wxlike/issues/91) | 6 | [查看](https://github.com/bear20252026/wxlike/issues?q=is%3Aissue%20is%3Aopen%20label%3A%22delivery%3Aui-v2%22%20label%3A%22type%3Atask%22%20label%3A%22group%3AG10%22) |
| [G11](#g11) | React扩展界面与服务入口 | [#92](https://github.com/bear20252026/wxlike/issues/92) | 5 | [查看](https://github.com/bear20252026/wxlike/issues?q=is%3Aissue%20is%3Aopen%20label%3A%22delivery%3Aui-v2%22%20label%3A%22type%3Atask%22%20label%3A%22group%3AG11%22) |
| [G12](#g12) | Flutter核心界面与原生桥 | [#93](https://github.com/bear20252026/wxlike/issues/93) | 6 | [查看](https://github.com/bear20252026/wxlike/issues?q=is%3Aissue%20is%3Aopen%20label%3A%22delivery%3Aui-v2%22%20label%3A%22type%3Atask%22%20label%3A%22group%3AG12%22) |
| [G13](#g13) | Flutter扩展页面与全入口 | [#94](https://github.com/bear20252026/wxlike/issues/94) | 6 | [查看](https://github.com/bear20252026/wxlike/issues?q=is%3Aissue%20is%3Aopen%20label%3A%22delivery%3Aui-v2%22%20label%3A%22type%3Atask%22%20label%3A%22group%3AG13%22) |
| [G14](#g14) | 管理后台与运营闭环 | [#95](https://github.com/bear20252026/wxlike/issues/95) | 5 | [查看](https://github.com/bear20252026/wxlike/issues?q=is%3Aissue%20is%3Aopen%20label%3A%22delivery%3Aui-v2%22%20label%3A%22type%3Atask%22%20label%3A%22group%3AG14%22) |
| [G15](#g15) | MCP、CLI与AI授权命令面 | [#96](https://github.com/bear20252026/wxlike/issues/96) | 5 | [查看](https://github.com/bear20252026/wxlike/issues?q=is%3Aissue%20is%3Aopen%20label%3A%22delivery%3Aui-v2%22%20label%3A%22type%3Atask%22%20label%3A%22group%3AG15%22) |
| [G16](#g16) | 云CI、发布、部署与独立验收 | [#97](https://github.com/bear20252026/wxlike/issues/97) | 5 | [查看](https://github.com/bear20252026/wxlike/issues?q=is%3Aissue%20is%3Aopen%20label%3A%22delivery%3Aui-v2%22%20label%3A%22type%3Atask%22%20label%3A%22group%3AG16%22) |
| [G17](#g17) | 安全、来源许可与独立评审 | [#98](https://github.com/bear20252026/wxlike/issues/98) | 5 | [查看](https://github.com/bear20252026/wxlike/issues?q=is%3Aissue%20is%3Aopen%20label%3A%22delivery%3Aui-v2%22%20label%3A%22type%3Atask%22%20label%3A%22group%3AG17%22) |

## G00

全局架构与串行集成；[完整任务单](../teams/G00.md)。

| 任务 | 独立Issue | 波次 | 硬依赖 |
|---|---|---|---|
| G00-01 入口总账与范围冻结 | [#99](https://github.com/bear20252026/wxlike/issues/99) | W0 | 首先启动 |
| G00-02 纯移动拆分与文件所有权 | [#100](https://github.com/bear20252026/wxlike/issues/100) | W0 | [G00-01 #99](https://github.com/bear20252026/wxlike/issues/99) |
| G00-03 契约版本与组合根冻结 | [#101](https://github.com/bear20252026/wxlike/issues/101) | W0 | [G00-02 #100](https://github.com/bear20252026/wxlike/issues/100) |
| G00-04 服务端数据库生产迁移 | [#104](https://github.com/bear20252026/wxlike/issues/104) | W2 | [G00-03 #101](https://github.com/bear20252026/wxlike/issues/101)、[G01-01 #102](https://github.com/bear20252026/wxlike/issues/102) |
| G00-05 集成节奏、容量与发布标准 | [#107](https://github.com/bear20252026/wxlike/issues/107) | W0–W5 continuous | [G00-04 #104](https://github.com/bear20252026/wxlike/issues/104) |

集成验收依赖：


## G01

Rust共享核心与绑定；[完整任务单](../teams/G01.md)。

| 任务 | 独立Issue | 波次 | 硬依赖 |
|---|---|---|---|
| G01-01 共享类型、领域不变量与ports | [#102](https://github.com/bear20252026/wxlike/issues/102) | W1 | [G00-02 #100](https://github.com/bear20252026/wxlike/issues/100)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101) |
| G01-02 原生核心状态机与缓存适配 | [#105](https://github.com/bear20252026/wxlike/issues/105) | W1 | [G01-01 #102](https://github.com/bear20252026/wxlike/issues/102) |
| G01-03 Rust WASM网页可运行闭环 | [#106](https://github.com/bear20252026/wxlike/issues/106) | W1 | [G01-02 #105](https://github.com/bear20252026/wxlike/issues/105) |
| G01-04 UniFFI异步与移动生命周期 | [#109](https://github.com/bear20252026/wxlike/issues/109) | W1 | [G01-03 #106](https://github.com/bear20252026/wxlike/issues/106) |
| G01-05 统一生成、兼容包与契约验证 | [#110](https://github.com/bear20252026/wxlike/issues/110) | W1 | [G01-04 #109](https://github.com/bear20252026/wxlike/issues/109) |

集成验收依赖：

- G01-01：[G02-03 #111](https://github.com/bear20252026/wxlike/issues/111)、[G03-02 #118](https://github.com/bear20252026/wxlike/issues/118)。
- G01-02：[G02-03 #111](https://github.com/bear20252026/wxlike/issues/111)、[G03-02 #118](https://github.com/bear20252026/wxlike/issues/118)。
- G01-03：[G02-03 #111](https://github.com/bear20252026/wxlike/issues/111)、[G03-02 #118](https://github.com/bear20252026/wxlike/issues/118)。
- G01-04：[G02-03 #111](https://github.com/bear20252026/wxlike/issues/111)、[G03-02 #118](https://github.com/bear20252026/wxlike/issues/118)。
- G01-05：[G02-03 #111](https://github.com/bear20252026/wxlike/issues/111)、[G03-02 #118](https://github.com/bear20252026/wxlike/issues/118)。

## G02

身份、关系、设备与加密安全；[完整任务单](../teams/G02.md)。

| 任务 | 独立Issue | 波次 | 硬依赖 |
|---|---|---|---|
| G02-01 账号会话与设备生命周期 | [#103](https://github.com/bear20252026/wxlike/issues/103) | W1 | [G00-02 #100](https://github.com/bear20252026/wxlike/issues/100)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101) |
| G02-02 好友、新朋友、群与隐私规则 | [#108](https://github.com/bear20252026/wxlike/issues/108) | W1 | [G02-01 #103](https://github.com/bear20252026/wxlike/issues/103) |
| G02-03 多设备E2EE与密钥分发 | [#111](https://github.com/bear20252026/wxlike/issues/111) | W1 | [G02-02 #108](https://github.com/bear20252026/wxlike/issues/108) |
| G02-04 本地SecretStore与口令快照接线 | [#113](https://github.com/bear20252026/wxlike/issues/113) | W1 | [G02-03 #111](https://github.com/bear20252026/wxlike/issues/111) |
| G02-05 设置、账户数据生命周期与通知偏好 | [#114](https://github.com/bear20252026/wxlike/issues/114) | W1 | [G02-04 #113](https://github.com/bear20252026/wxlike/issues/113) |
| G02-06 手机号验证码、登录方式与协议同意记录 | [#112](https://github.com/bear20252026/wxlike/issues/112) | W2/W3 | [G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101) |
| G02-07 桌面扫码登录、手机授权与设备信任 | [#115](https://github.com/bear20252026/wxlike/issues/115) | W2/W3 | [G02-06 #112](https://github.com/bear20252026/wxlike/issues/112) |

集成验收依赖：

- G02-01：[G01-01 #102](https://github.com/bear20252026/wxlike/issues/102)。
- G02-02：[G01-01 #102](https://github.com/bear20252026/wxlike/issues/102)。
- G02-03：[G01-01 #102](https://github.com/bear20252026/wxlike/issues/102)。
- G02-04：[G01-01 #102](https://github.com/bear20252026/wxlike/issues/102)。
- G02-05：[G01-01 #102](https://github.com/bear20252026/wxlike/issues/102)。
- G02-06：[G12-06 #173](https://github.com/bear20252026/wxlike/issues/173)、[G10-06 #160](https://github.com/bear20252026/wxlike/issues/160)。
- G02-07：[G07-01 #136](https://github.com/bear20252026/wxlike/issues/136)、[G10-06 #160](https://github.com/bear20252026/wxlike/issues/160)、[G12-06 #173](https://github.com/bear20252026/wxlike/issues/173)。

## G03

消息、同步、离线与推送；[完整任务单](../teams/G03.md)。

| 任务 | 独立Issue | 波次 | 硬依赖 |
|---|---|---|---|
| G03-01 消息提交事务与幂等 | [#116](https://github.com/bear20252026/wxlike/issues/116) | W1/W2 | [G00-02 #100](https://github.com/bear20252026/wxlike/issues/100)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101) |
| G03-02 设备持久化确认与离线队列 | [#118](https://github.com/bear20252026/wxlike/issues/118) | W1/W2 | [G03-01 #116](https://github.com/bear20252026/wxlike/issues/116) |
| G03-03 会话投影、未读与跨设备偏好 | [#121](https://github.com/bear20252026/wxlike/issues/121) | W1/W2 | [G03-02 #118](https://github.com/bear20252026/wxlike/issues/118) |
| G03-04 路由租约、重连与协议兼容 | [#123](https://github.com/bear20252026/wxlike/issues/123) | W1/W2 | [G03-03 #121](https://github.com/bear20252026/wxlike/issues/121) |
| G03-05 移动推送与同步调度 | [#124](https://github.com/bear20252026/wxlike/issues/124) | W1/W2 | [G03-04 #123](https://github.com/bear20252026/wxlike/issues/123) |

集成验收依赖：

- G03-01：[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G02-02 #108](https://github.com/bear20252026/wxlike/issues/108)、[G02-03 #111](https://github.com/bear20252026/wxlike/issues/111)、[G01-02 #105](https://github.com/bear20252026/wxlike/issues/105)、[G00-04 #104](https://github.com/bear20252026/wxlike/issues/104)。
- G03-02：[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G02-02 #108](https://github.com/bear20252026/wxlike/issues/108)、[G02-03 #111](https://github.com/bear20252026/wxlike/issues/111)、[G01-02 #105](https://github.com/bear20252026/wxlike/issues/105)、[G00-04 #104](https://github.com/bear20252026/wxlike/issues/104)。
- G03-03：[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G02-02 #108](https://github.com/bear20252026/wxlike/issues/108)、[G02-03 #111](https://github.com/bear20252026/wxlike/issues/111)、[G01-02 #105](https://github.com/bear20252026/wxlike/issues/105)、[G00-04 #104](https://github.com/bear20252026/wxlike/issues/104)。
- G03-04：[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G02-02 #108](https://github.com/bear20252026/wxlike/issues/108)、[G02-03 #111](https://github.com/bear20252026/wxlike/issues/111)、[G01-02 #105](https://github.com/bear20252026/wxlike/issues/105)、[G00-04 #104](https://github.com/bear20252026/wxlike/issues/104)。
- G03-05：[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G02-02 #108](https://github.com/bear20252026/wxlike/issues/108)、[G02-03 #111](https://github.com/bear20252026/wxlike/issues/111)、[G01-02 #105](https://github.com/bear20252026/wxlike/issues/105)、[G00-04 #104](https://github.com/bear20252026/wxlike/issues/104)。

## G04

对象、文件、转码与备份；[完整任务单](../teams/G04.md)。

| 任务 | 独立Issue | 波次 | 硬依赖 |
|---|---|---|---|
| G04-01 上传会话、对象权限与下载 | [#117](https://github.com/bear20252026/wxlike/issues/117) | W1/W2 | [G00-02 #100](https://github.com/bear20252026/wxlike/issues/100)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101) |
| G04-02 文件中心与消息附件 | [#119](https://github.com/bear20252026/wxlike/issues/119) | W1/W2 | [G04-01 #117](https://github.com/bear20252026/wxlike/issues/117) |
| G04-03 音视频解码与隔离转码 | [#120](https://github.com/bear20252026/wxlike/issues/120) | W1/W2 | [G04-02 #119](https://github.com/bear20252026/wxlike/issues/119) |
| G04-04 聊天迁移、备份恢复与空间清理 | [#122](https://github.com/bear20252026/wxlike/issues/122) | W1/W2 | [G04-03 #120](https://github.com/bear20252026/wxlike/issues/120) |
| G04-05 平台媒体能力接口与资源预算 | [#125](https://github.com/bear20252026/wxlike/issues/125) | W1/W2 | [G04-04 #122](https://github.com/bear20252026/wxlike/issues/122) |

集成验收依赖：

- G04-01：[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G00-04 #104](https://github.com/bear20252026/wxlike/issues/104)。
- G04-02：[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G00-04 #104](https://github.com/bear20252026/wxlike/issues/104)。
- G04-03：[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G00-04 #104](https://github.com/bear20252026/wxlike/issues/104)。
- G04-04：[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G00-04 #104](https://github.com/bear20252026/wxlike/issues/104)。
- G04-05：[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G00-04 #104](https://github.com/bear20252026/wxlike/issues/104)。

## G05

内容、社交、收藏与检索；[完整任务单](../teams/G05.md)。

| 任务 | 独立Issue | 波次 | 硬依赖 |
|---|---|---|---|
| G05-01 朋友圈、相册与可见性 | [#126](https://github.com/bear20252026/wxlike/issues/126) | W2 | [G00-02 #100](https://github.com/bear20252026/wxlike/issues/100)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101) |
| G05-02 自主公众号与文章订阅 | [#128](https://github.com/bear20252026/wxlike/issues/128) | W2 | [G05-01 #126](https://github.com/bear20252026/wxlike/issues/126) |
| G05-03 收藏、笔记与表情包 | [#131](https://github.com/bear20252026/wxlike/issues/131) | W2 | [G05-02 #128](https://github.com/bear20252026/wxlike/issues/128) |
| G05-04 视频号与创作者内容流 | [#132](https://github.com/bear20252026/wxlike/issues/132) | W2 | [G05-03 #131](https://github.com/bear20252026/wxlike/issues/131) |
| G05-05 统一授权搜索、热搜与通知 | [#135](https://github.com/bear20252026/wxlike/issues/135) | W2 | [G05-04 #132](https://github.com/bear20252026/wxlike/issues/132) |
| G05-06 我的状态、好友状态、背景与到期 | [#129](https://github.com/bear20252026/wxlike/issues/129) | W2/W3 | [G05-01 #126](https://github.com/bear20252026/wxlike/issues/126) |

集成验收依赖：

- G05-01：[G02-02 #108](https://github.com/bear20252026/wxlike/issues/108)、[G04-01 #117](https://github.com/bear20252026/wxlike/issues/117)、[G00-04 #104](https://github.com/bear20252026/wxlike/issues/104)。
- G05-02：[G02-02 #108](https://github.com/bear20252026/wxlike/issues/108)、[G04-01 #117](https://github.com/bear20252026/wxlike/issues/117)、[G00-04 #104](https://github.com/bear20252026/wxlike/issues/104)。
- G05-03：[G02-02 #108](https://github.com/bear20252026/wxlike/issues/108)、[G04-01 #117](https://github.com/bear20252026/wxlike/issues/117)、[G00-04 #104](https://github.com/bear20252026/wxlike/issues/104)。
- G05-04：[G02-02 #108](https://github.com/bear20252026/wxlike/issues/108)、[G04-01 #117](https://github.com/bear20252026/wxlike/issues/117)、[G00-04 #104](https://github.com/bear20252026/wxlike/issues/104)。
- G05-05：[G02-02 #108](https://github.com/bear20252026/wxlike/issues/108)、[G04-01 #117](https://github.com/bear20252026/wxlike/issues/117)、[G00-04 #104](https://github.com/bear20252026/wxlike/issues/104)。
- G05-06：[G02-02 #108](https://github.com/bear20252026/wxlike/issues/108)、[G04-01 #117](https://github.com/bear20252026/wxlike/issues/117)、[G13-06 #174](https://github.com/bear20252026/wxlike/issues/174)。

## G06

账本、支付、红包、卡券；[完整任务单](../teams/G06.md)。

| 任务 | 独立Issue | 波次 | 硬依赖 |
|---|---|---|---|
| G06-01 不可变账本与资金账户 | [#127](https://github.com/bear20252026/wxlike/issues/127) | W2 | [G00-02 #100](https://github.com/bear20252026/wxlike/issues/100)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101) |
| G06-02 支付意图、绑卡与回调 | [#130](https://github.com/bear20252026/wxlike/issues/130) | W2 | [G06-01 #127](https://github.com/bear20252026/wxlike/issues/127) |
| G06-03 充值提现退款与对账 | [#133](https://github.com/bear20252026/wxlike/issues/133) | W2 | [G06-02 #130](https://github.com/bear20252026/wxlike/issues/130) |
| G06-04 转账、红包与领取并发 | [#134](https://github.com/bear20252026/wxlike/issues/134) | W2 | [G06-03 #133](https://github.com/bear20252026/wxlike/issues/133) |
| G06-05 会员卡、券、票证与订单接口 | [#137](https://github.com/bear20252026/wxlike/issues/137) | W2 | [G06-04 #134](https://github.com/bear20252026/wxlike/issues/134) |

集成验收依赖：

- G06-01：[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G00-04 #104](https://github.com/bear20252026/wxlike/issues/104)。
- G06-02：[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G00-04 #104](https://github.com/bear20252026/wxlike/issues/104)。
- G06-03：[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G00-04 #104](https://github.com/bear20252026/wxlike/issues/104)。
- G06-04：[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G00-04 #104](https://github.com/bear20252026/wxlike/issues/104)。
- G06-05：[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G00-04 #104](https://github.com/bear20252026/wxlike/issues/104)。

## G07

扫码、发现、模型与服务聚合；[完整任务单](../teams/G07.md)。

| 任务 | 独立Issue | 波次 | 硬依赖 |
|---|---|---|---|
| G07-01 二维码解码与安全深链 | [#136](https://github.com/bear20252026/wxlike/issues/136) | W2 | [G00-02 #100](https://github.com/bear20252026/wxlike/issues/100)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101) |
| G07-02 识物、OCR、翻译与语音模型服务 | [#139](https://github.com/bear20252026/wxlike/issues/139) | W2 | [G07-01 #136](https://github.com/bear20252026/wxlike/issues/136) |
| G07-03 位置、附近与摇一摇匹配 | [#140](https://github.com/bear20252026/wxlike/issues/140) | W2 | [G07-02 #139](https://github.com/bear20252026/wxlike/issues/139) |
| G07-04 服务目录、报价与统一供应商订单 | [#143](https://github.com/bear20252026/wxlike/issues/143) | W2 | [G07-03 #140](https://github.com/bear20252026/wxlike/issues/140) |
| G07-05 充值、生活缴费与公益服务适配 | [#144](https://github.com/bear20252026/wxlike/issues/144) | W2 | [G07-04 #143](https://github.com/bear20252026/wxlike/issues/143) |
| G07-06 出行酒店电影票务与医疗适配 | [#146](https://github.com/bear20252026/wxlike/issues/146) | W2 | [G07-05 #144](https://github.com/bear20252026/wxlike/issues/144) |
| G07-07 购物、游戏与应用服务路由 | [#148](https://github.com/bear20252026/wxlike/issues/148) | W2 | [G07-06 #146](https://github.com/bear20252026/wxlike/issues/146) |
| G07-08 位置消息、POI、导航与实时位置共享 | [#142](https://github.com/bear20252026/wxlike/issues/142) | W2/W3 | [G07-03 #140](https://github.com/bear20252026/wxlike/issues/140)、[G03-01 #116](https://github.com/bear20252026/wxlike/issues/116) |
| G07-09 个人名片二维码、样式、保存分享与撤销 | [#138](https://github.com/bear20252026/wxlike/issues/138) | W2/W3 | [G07-01 #136](https://github.com/bear20252026/wxlike/issues/136) |

集成验收依赖：

- G07-01：[G04-01 #117](https://github.com/bear20252026/wxlike/issues/117)、[G06-02 #130](https://github.com/bear20252026/wxlike/issues/130)、[G06-03 #133](https://github.com/bear20252026/wxlike/issues/133)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)。
- G07-02：[G04-01 #117](https://github.com/bear20252026/wxlike/issues/117)、[G06-02 #130](https://github.com/bear20252026/wxlike/issues/130)、[G06-03 #133](https://github.com/bear20252026/wxlike/issues/133)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)。
- G07-03：[G04-01 #117](https://github.com/bear20252026/wxlike/issues/117)、[G06-02 #130](https://github.com/bear20252026/wxlike/issues/130)、[G06-03 #133](https://github.com/bear20252026/wxlike/issues/133)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)。
- G07-04：[G04-01 #117](https://github.com/bear20252026/wxlike/issues/117)、[G06-02 #130](https://github.com/bear20252026/wxlike/issues/130)、[G06-03 #133](https://github.com/bear20252026/wxlike/issues/133)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)。
- G07-05：[G04-01 #117](https://github.com/bear20252026/wxlike/issues/117)、[G06-02 #130](https://github.com/bear20252026/wxlike/issues/130)、[G06-03 #133](https://github.com/bear20252026/wxlike/issues/133)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)。
- G07-06：[G04-01 #117](https://github.com/bear20252026/wxlike/issues/117)、[G06-02 #130](https://github.com/bear20252026/wxlike/issues/130)、[G06-03 #133](https://github.com/bear20252026/wxlike/issues/133)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)。
- G07-07：[G04-01 #117](https://github.com/bear20252026/wxlike/issues/117)、[G06-02 #130](https://github.com/bear20252026/wxlike/issues/130)、[G06-03 #133](https://github.com/bear20252026/wxlike/issues/133)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)。
- G07-08：[G12-05 #171](https://github.com/bear20252026/wxlike/issues/171)、[G13-06 #174](https://github.com/bear20252026/wxlike/issues/174)、[G04-05 #125](https://github.com/bear20252026/wxlike/issues/125)。
- G07-09：[G02-02 #108](https://github.com/bear20252026/wxlike/issues/108)、[G04-05 #125](https://github.com/bear20252026/wxlike/issues/125)、[G13-06 #174](https://github.com/bear20252026/wxlike/issues/174)。

## G08

小程序容器、应用目录与SDK；[完整任务单](../teams/G08.md)。

| 任务 | 独立Issue | 波次 | 硬依赖 |
|---|---|---|---|
| G08-01 包格式、签名与发布供应链 | [#141](https://github.com/bear20252026/wxlike/issues/141) | W2 | [G00-02 #100](https://github.com/bear20252026/wxlike/issues/100)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101) |
| G08-02 跨端隔离运行与预算 | [#145](https://github.com/bear20252026/wxlike/issues/145) | W2 | [G08-01 #141](https://github.com/bear20252026/wxlike/issues/141) |
| G08-03 宿主权限broker与能力SDK | [#147](https://github.com/bear20252026/wxlike/issues/147) | W2 | [G08-02 #145](https://github.com/bear20252026/wxlike/issues/145) |
| G08-04 目录、最近使用与升级撤销 | [#149](https://github.com/bear20252026/wxlike/issues/149) | W2 | [G08-03 #147](https://github.com/bear20252026/wxlike/issues/147) |
| G08-05 可运行示例与开发工具链 | [#150](https://github.com/bear20252026/wxlike/issues/150) | W2 | [G08-04 #149](https://github.com/bear20252026/wxlike/issues/149) |

集成验收依赖：

- G08-01：[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G04-01 #117](https://github.com/bear20252026/wxlike/issues/117)、[G01-01 #102](https://github.com/bear20252026/wxlike/issues/102)。
- G08-02：[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G04-01 #117](https://github.com/bear20252026/wxlike/issues/117)、[G01-01 #102](https://github.com/bear20252026/wxlike/issues/102)。
- G08-03：[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G04-01 #117](https://github.com/bear20252026/wxlike/issues/117)、[G01-01 #102](https://github.com/bear20252026/wxlike/issues/102)。
- G08-04：[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G04-01 #117](https://github.com/bear20252026/wxlike/issues/117)、[G01-01 #102](https://github.com/bear20252026/wxlike/issues/102)。
- G08-05：[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G04-01 #117](https://github.com/bear20252026/wxlike/issues/117)、[G01-01 #102](https://github.com/bear20252026/wxlike/issues/102)。

## G09

通话、音视频与直播；[完整任务单](../teams/G09.md)。

| 任务 | 独立Issue | 波次 | 硬依赖 |
|---|---|---|---|
| G09-01 唯一RTC引擎可行性门禁 | [#151](https://github.com/bear20252026/wxlike/issues/151) | W2 | [G00-02 #100](https://github.com/bear20252026/wxlike/issues/100)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101) |
| G09-02 呼叫信令与成员状态机 | [#153](https://github.com/bear20252026/wxlike/issues/153) | W2 | [G09-01 #151](https://github.com/bear20252026/wxlike/issues/151) |
| G09-03 媒体轨、设备与TURN | [#155](https://github.com/bear20252026/wxlike/issues/155) | W2 | [G09-02 #153](https://github.com/bear20252026/wxlike/issues/153) |
| G09-04 媒体E2EE、录制与权限 | [#156](https://github.com/bear20252026/wxlike/issues/156) | W2 | [G09-03 #155](https://github.com/bear20252026/wxlike/issues/155) |
| G09-05 直播、观看与恢复 | [#159](https://github.com/bear20252026/wxlike/issues/159) | W2 | [G09-04 #156](https://github.com/bear20252026/wxlike/issues/156) |

集成验收依赖：

- G09-01：[G02-02 #108](https://github.com/bear20252026/wxlike/issues/108)、[G02-03 #111](https://github.com/bear20252026/wxlike/issues/111)、[G03-04 #123](https://github.com/bear20252026/wxlike/issues/123)、[G04-03 #120](https://github.com/bear20252026/wxlike/issues/120)。
- G09-02：[G02-02 #108](https://github.com/bear20252026/wxlike/issues/108)、[G02-03 #111](https://github.com/bear20252026/wxlike/issues/111)、[G03-04 #123](https://github.com/bear20252026/wxlike/issues/123)、[G04-03 #120](https://github.com/bear20252026/wxlike/issues/120)。
- G09-03：[G02-02 #108](https://github.com/bear20252026/wxlike/issues/108)、[G02-03 #111](https://github.com/bear20252026/wxlike/issues/111)、[G03-04 #123](https://github.com/bear20252026/wxlike/issues/123)、[G04-03 #120](https://github.com/bear20252026/wxlike/issues/120)。
- G09-04：[G02-02 #108](https://github.com/bear20252026/wxlike/issues/108)、[G02-03 #111](https://github.com/bear20252026/wxlike/issues/111)、[G03-04 #123](https://github.com/bear20252026/wxlike/issues/123)、[G04-03 #120](https://github.com/bear20252026/wxlike/issues/120)。
- G09-05：[G02-02 #108](https://github.com/bear20252026/wxlike/issues/108)、[G02-03 #111](https://github.com/bear20252026/wxlike/issues/111)、[G03-04 #123](https://github.com/bear20252026/wxlike/issues/123)、[G04-03 #120](https://github.com/bear20252026/wxlike/issues/120)。

## G10

React桌面与网页核心界面；[完整任务单](../teams/G10.md)。

| 任务 | 独立Issue | 波次 | 硬依赖 |
|---|---|---|---|
| G10-01 从展示稿拆分产品壳与登录 | [#152](https://github.com/bear20252026/wxlike/issues/152) | W3 | [G00-02 #100](https://github.com/bear20252026/wxlike/issues/100)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101) |
| G10-02 聊天闭环与消息操作 | [#154](https://github.com/bear20252026/wxlike/issues/154) | W3 | [G10-01 #152](https://github.com/bear20252026/wxlike/issues/152) |
| G10-03 联系人、新朋友与群管理 | [#157](https://github.com/bear20252026/wxlike/issues/157) | W3 | [G10-02 #154](https://github.com/bear20252026/wxlike/issues/154) |
| G10-04 桌面系统与网页能力适配 | [#158](https://github.com/bear20252026/wxlike/issues/158) | W3 | [G10-03 #157](https://github.com/bear20252026/wxlike/issues/157) |
| G10-05 搜索、设置与本地安全界面 | [#161](https://github.com/bear20252026/wxlike/issues/161) | W3 | [G10-04 #158](https://github.com/bear20252026/wxlike/issues/158) |
| G10-06 桌面扫码登录与新版界面字面承诺接线 | [#160](https://github.com/bear20252026/wxlike/issues/160) | W2/W3 | [G10-01 #152](https://github.com/bear20252026/wxlike/issues/152) |

集成验收依赖：

- G10-01：[G01-02 #105](https://github.com/bear20252026/wxlike/issues/105)、[G01-03 #106](https://github.com/bear20252026/wxlike/issues/106)、[G02-05 #114](https://github.com/bear20252026/wxlike/issues/114)、[G03-05 #124](https://github.com/bear20252026/wxlike/issues/124)、[G04-05 #125](https://github.com/bear20252026/wxlike/issues/125)。
- G10-02：[G01-02 #105](https://github.com/bear20252026/wxlike/issues/105)、[G01-03 #106](https://github.com/bear20252026/wxlike/issues/106)、[G02-05 #114](https://github.com/bear20252026/wxlike/issues/114)、[G03-05 #124](https://github.com/bear20252026/wxlike/issues/124)、[G04-05 #125](https://github.com/bear20252026/wxlike/issues/125)。
- G10-03：[G01-02 #105](https://github.com/bear20252026/wxlike/issues/105)、[G01-03 #106](https://github.com/bear20252026/wxlike/issues/106)、[G02-05 #114](https://github.com/bear20252026/wxlike/issues/114)、[G03-05 #124](https://github.com/bear20252026/wxlike/issues/124)、[G04-05 #125](https://github.com/bear20252026/wxlike/issues/125)。
- G10-04：[G01-02 #105](https://github.com/bear20252026/wxlike/issues/105)、[G01-03 #106](https://github.com/bear20252026/wxlike/issues/106)、[G02-05 #114](https://github.com/bear20252026/wxlike/issues/114)、[G03-05 #124](https://github.com/bear20252026/wxlike/issues/124)、[G04-05 #125](https://github.com/bear20252026/wxlike/issues/125)。
- G10-05：[G01-02 #105](https://github.com/bear20252026/wxlike/issues/105)、[G01-03 #106](https://github.com/bear20252026/wxlike/issues/106)、[G02-05 #114](https://github.com/bear20252026/wxlike/issues/114)、[G03-05 #124](https://github.com/bear20252026/wxlike/issues/124)、[G04-05 #125](https://github.com/bear20252026/wxlike/issues/125)。
- G10-06：[G02-07 #115](https://github.com/bear20252026/wxlike/issues/115)、[G01-02 #105](https://github.com/bear20252026/wxlike/issues/105)、[G01-03 #106](https://github.com/bear20252026/wxlike/issues/106)、[G04-01 #117](https://github.com/bear20252026/wxlike/issues/117)、[G16-04 #192](https://github.com/bear20252026/wxlike/issues/192)。

## G11

React扩展界面与服务入口；[完整任务单](../teams/G11.md)。

| 任务 | 独立Issue | 波次 | 硬依赖 |
|---|---|---|---|
| G11-01 朋友圈、收藏、文章与表情 | [#162](https://github.com/bear20252026/wxlike/issues/162) | W3 | [G00-02 #100](https://github.com/bear20252026/wxlike/issues/100)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101) |
| G11-02 文件中心与传输管理 | [#164](https://github.com/bear20252026/wxlike/issues/164) | W3 | [G11-01 #162](https://github.com/bear20252026/wxlike/issues/162) |
| G11-03 视频流、通话与直播面板 | [#167](https://github.com/bear20252026/wxlike/issues/167) | W3 | [G11-02 #164](https://github.com/bear20252026/wxlike/issues/164) |
| G11-04 钱包、收付款、红包与卡包 | [#168](https://github.com/bear20252026/wxlike/issues/168) | W3 | [G11-03 #167](https://github.com/bear20252026/wxlike/issues/167) |
| G11-05 小程序、扫码与服务目录 | [#170](https://github.com/bear20252026/wxlike/issues/170) | W3 | [G11-04 #168](https://github.com/bear20252026/wxlike/issues/168) |

集成验收依赖：

- G11-01：[G04-04 #122](https://github.com/bear20252026/wxlike/issues/122)、[G05-05 #135](https://github.com/bear20252026/wxlike/issues/135)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)、[G07-07 #148](https://github.com/bear20252026/wxlike/issues/148)、[G08-05 #150](https://github.com/bear20252026/wxlike/issues/150)、[G09-05 #159](https://github.com/bear20252026/wxlike/issues/159)、[G10-01 #152](https://github.com/bear20252026/wxlike/issues/152)。
- G11-02：[G04-04 #122](https://github.com/bear20252026/wxlike/issues/122)、[G05-05 #135](https://github.com/bear20252026/wxlike/issues/135)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)、[G07-07 #148](https://github.com/bear20252026/wxlike/issues/148)、[G08-05 #150](https://github.com/bear20252026/wxlike/issues/150)、[G09-05 #159](https://github.com/bear20252026/wxlike/issues/159)、[G10-01 #152](https://github.com/bear20252026/wxlike/issues/152)。
- G11-03：[G04-04 #122](https://github.com/bear20252026/wxlike/issues/122)、[G05-05 #135](https://github.com/bear20252026/wxlike/issues/135)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)、[G07-07 #148](https://github.com/bear20252026/wxlike/issues/148)、[G08-05 #150](https://github.com/bear20252026/wxlike/issues/150)、[G09-05 #159](https://github.com/bear20252026/wxlike/issues/159)、[G10-01 #152](https://github.com/bear20252026/wxlike/issues/152)。
- G11-04：[G04-04 #122](https://github.com/bear20252026/wxlike/issues/122)、[G05-05 #135](https://github.com/bear20252026/wxlike/issues/135)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)、[G07-07 #148](https://github.com/bear20252026/wxlike/issues/148)、[G08-05 #150](https://github.com/bear20252026/wxlike/issues/150)、[G09-05 #159](https://github.com/bear20252026/wxlike/issues/159)、[G10-01 #152](https://github.com/bear20252026/wxlike/issues/152)。
- G11-05：[G04-04 #122](https://github.com/bear20252026/wxlike/issues/122)、[G05-05 #135](https://github.com/bear20252026/wxlike/issues/135)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)、[G07-07 #148](https://github.com/bear20252026/wxlike/issues/148)、[G08-05 #150](https://github.com/bear20252026/wxlike/issues/150)、[G09-05 #159](https://github.com/bear20252026/wxlike/issues/159)、[G10-01 #152](https://github.com/bear20252026/wxlike/issues/152)。

## G12

Flutter核心界面与原生桥；[完整任务单](../teams/G12.md)。

| 任务 | 独立Issue | 波次 | 硬依赖 |
|---|---|---|---|
| G12-01 移动产品壳、主题与账号 | [#163](https://github.com/bear20252026/wxlike/issues/163) | W3 | [G00-02 #100](https://github.com/bear20252026/wxlike/issues/100)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101) |
| G12-02 实时消息页与本地投影 | [#165](https://github.com/bear20252026/wxlike/issues/165) | W3 | [G12-01 #163](https://github.com/bear20252026/wxlike/issues/163) |
| G12-03 输入器、附件与消息菜单 | [#166](https://github.com/bear20252026/wxlike/issues/166) | W3 | [G12-02 #165](https://github.com/bear20252026/wxlike/issues/165) |
| G12-04 联系人、新朋友、群与资料 | [#169](https://github.com/bear20252026/wxlike/issues/169) | W3 | [G12-03 #166](https://github.com/bear20252026/wxlike/issues/166) |
| G12-05 Android/iOS原生平台能力与后台 | [#171](https://github.com/bear20252026/wxlike/issues/171) | W3 | [G12-04 #169](https://github.com/bear20252026/wxlike/issues/169) |
| G12-06 手机号登录、扫码授权与移动协议界面 | [#173](https://github.com/bear20252026/wxlike/issues/173) | W2/W3 | [G12-01 #163](https://github.com/bear20252026/wxlike/issues/163) |

集成验收依赖：

- G12-01：[G01-04 #109](https://github.com/bear20252026/wxlike/issues/109)、[G02-05 #114](https://github.com/bear20252026/wxlike/issues/114)、[G03-05 #124](https://github.com/bear20252026/wxlike/issues/124)、[G04-05 #125](https://github.com/bear20252026/wxlike/issues/125)。
- G12-02：[G01-04 #109](https://github.com/bear20252026/wxlike/issues/109)、[G02-05 #114](https://github.com/bear20252026/wxlike/issues/114)、[G03-05 #124](https://github.com/bear20252026/wxlike/issues/124)、[G04-05 #125](https://github.com/bear20252026/wxlike/issues/125)。
- G12-03：[G01-04 #109](https://github.com/bear20252026/wxlike/issues/109)、[G02-05 #114](https://github.com/bear20252026/wxlike/issues/114)、[G03-05 #124](https://github.com/bear20252026/wxlike/issues/124)、[G04-05 #125](https://github.com/bear20252026/wxlike/issues/125)。
- G12-04：[G01-04 #109](https://github.com/bear20252026/wxlike/issues/109)、[G02-05 #114](https://github.com/bear20252026/wxlike/issues/114)、[G03-05 #124](https://github.com/bear20252026/wxlike/issues/124)、[G04-05 #125](https://github.com/bear20252026/wxlike/issues/125)。
- G12-05：[G01-04 #109](https://github.com/bear20252026/wxlike/issues/109)、[G02-05 #114](https://github.com/bear20252026/wxlike/issues/114)、[G03-05 #124](https://github.com/bear20252026/wxlike/issues/124)、[G04-05 #125](https://github.com/bear20252026/wxlike/issues/125)。
- G12-06：[G02-06 #112](https://github.com/bear20252026/wxlike/issues/112)、[G02-07 #115](https://github.com/bear20252026/wxlike/issues/115)、[G07-01 #136](https://github.com/bear20252026/wxlike/issues/136)。

## G13

Flutter扩展页面与全入口；[完整任务单](../teams/G13.md)。

| 任务 | 独立Issue | 波次 | 硬依赖 |
|---|---|---|---|
| G13-01 朋友圈、公众号、收藏与卡片内容 | [#172](https://github.com/bear20252026/wxlike/issues/172) | W3 | [G00-02 #100](https://github.com/bear20252026/wxlike/issues/100)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101) |
| G13-02 视频号、语音视频通话与直播 | [#175](https://github.com/bear20252026/wxlike/issues/175) | W3 | [G13-01 #172](https://github.com/bear20252026/wxlike/issues/172) |
| G13-03 钱包、银行卡、红包、收付款与卡包 | [#177](https://github.com/bear20252026/wxlike/issues/177) | W3 | [G13-02 #175](https://github.com/bear20252026/wxlike/issues/175) |
| G13-04 发现、扫码、识物翻译与小程序 | [#179](https://github.com/bear20252026/wxlike/issues/179) | W3 | [G13-03 #177](https://github.com/bear20252026/wxlike/issues/177) |
| G13-05 搜索、设置、我的资料与账号安全 | [#181](https://github.com/bear20252026/wxlike/issues/181) | W3 | [G13-04 #179](https://github.com/bear20252026/wxlike/issues/179) |
| G13-06 新版状态、位置共享、名片码与订阅号整合 | [#174](https://github.com/bear20252026/wxlike/issues/174) | W2/W3 | [G13-01 #172](https://github.com/bear20252026/wxlike/issues/172) |

集成验收依赖：

- G13-01：[G12-05 #171](https://github.com/bear20252026/wxlike/issues/171)、[G05-05 #135](https://github.com/bear20252026/wxlike/issues/135)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)、[G07-07 #148](https://github.com/bear20252026/wxlike/issues/148)、[G08-05 #150](https://github.com/bear20252026/wxlike/issues/150)、[G09-05 #159](https://github.com/bear20252026/wxlike/issues/159)。
- G13-02：[G12-05 #171](https://github.com/bear20252026/wxlike/issues/171)、[G05-05 #135](https://github.com/bear20252026/wxlike/issues/135)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)、[G07-07 #148](https://github.com/bear20252026/wxlike/issues/148)、[G08-05 #150](https://github.com/bear20252026/wxlike/issues/150)、[G09-05 #159](https://github.com/bear20252026/wxlike/issues/159)。
- G13-03：[G12-05 #171](https://github.com/bear20252026/wxlike/issues/171)、[G05-05 #135](https://github.com/bear20252026/wxlike/issues/135)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)、[G07-07 #148](https://github.com/bear20252026/wxlike/issues/148)、[G08-05 #150](https://github.com/bear20252026/wxlike/issues/150)、[G09-05 #159](https://github.com/bear20252026/wxlike/issues/159)。
- G13-04：[G12-05 #171](https://github.com/bear20252026/wxlike/issues/171)、[G05-05 #135](https://github.com/bear20252026/wxlike/issues/135)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)、[G07-07 #148](https://github.com/bear20252026/wxlike/issues/148)、[G08-05 #150](https://github.com/bear20252026/wxlike/issues/150)、[G09-05 #159](https://github.com/bear20252026/wxlike/issues/159)。
- G13-05：[G12-05 #171](https://github.com/bear20252026/wxlike/issues/171)、[G05-05 #135](https://github.com/bear20252026/wxlike/issues/135)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)、[G07-07 #148](https://github.com/bear20252026/wxlike/issues/148)、[G08-05 #150](https://github.com/bear20252026/wxlike/issues/150)、[G09-05 #159](https://github.com/bear20252026/wxlike/issues/159)。
- G13-06：[G05-06 #129](https://github.com/bear20252026/wxlike/issues/129)、[G07-08 #142](https://github.com/bear20252026/wxlike/issues/142)、[G07-09 #138](https://github.com/bear20252026/wxlike/issues/138)、[G05-02 #128](https://github.com/bear20252026/wxlike/issues/128)、[G12-05 #171](https://github.com/bear20252026/wxlike/issues/171)、[G06-04 #134](https://github.com/bear20252026/wxlike/issues/134)。

## G14

管理后台与运营闭环；[完整任务单](../teams/G14.md)。

| 任务 | 独立Issue | 波次 | 硬依赖 |
|---|---|---|---|
| G14-01 管理身份、RBAC与审计骨架 | [#176](https://github.com/bear20252026/wxlike/issues/176) | W3 | [G00-02 #100](https://github.com/bear20252026/wxlike/issues/100)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101) |
| G14-02 用户、设备、群与申诉管理 | [#178](https://github.com/bear20252026/wxlike/issues/178) | W3 | [G14-01 #176](https://github.com/bear20252026/wxlike/issues/176) |
| G14-03 内容、小程序与服务发布审核 | [#180](https://github.com/bear20252026/wxlike/issues/180) | W3 | [G14-02 #178](https://github.com/bear20252026/wxlike/issues/178) |
| G14-04 商户、卡券、资金与对账台 | [#182](https://github.com/bear20252026/wxlike/issues/182) | W3 | [G14-03 #180](https://github.com/bear20252026/wxlike/issues/180) |
| G14-05 运营作业、隐私请求与可观测入口 | [#185](https://github.com/bear20252026/wxlike/issues/185) | W3 | [G14-04 #182](https://github.com/bear20252026/wxlike/issues/182) |

集成验收依赖：

- G14-01：[G02-05 #114](https://github.com/bear20252026/wxlike/issues/114)、[G05-05 #135](https://github.com/bear20252026/wxlike/issues/135)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)、[G07-07 #148](https://github.com/bear20252026/wxlike/issues/148)、[G08-04 #149](https://github.com/bear20252026/wxlike/issues/149)。
- G14-02：[G02-05 #114](https://github.com/bear20252026/wxlike/issues/114)、[G05-05 #135](https://github.com/bear20252026/wxlike/issues/135)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)、[G07-07 #148](https://github.com/bear20252026/wxlike/issues/148)、[G08-04 #149](https://github.com/bear20252026/wxlike/issues/149)。
- G14-03：[G02-05 #114](https://github.com/bear20252026/wxlike/issues/114)、[G05-05 #135](https://github.com/bear20252026/wxlike/issues/135)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)、[G07-07 #148](https://github.com/bear20252026/wxlike/issues/148)、[G08-04 #149](https://github.com/bear20252026/wxlike/issues/149)。
- G14-04：[G02-05 #114](https://github.com/bear20252026/wxlike/issues/114)、[G05-05 #135](https://github.com/bear20252026/wxlike/issues/135)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)、[G07-07 #148](https://github.com/bear20252026/wxlike/issues/148)、[G08-04 #149](https://github.com/bear20252026/wxlike/issues/149)。
- G14-05：[G02-05 #114](https://github.com/bear20252026/wxlike/issues/114)、[G05-05 #135](https://github.com/bear20252026/wxlike/issues/135)、[G06-05 #137](https://github.com/bear20252026/wxlike/issues/137)、[G07-07 #148](https://github.com/bear20252026/wxlike/issues/148)、[G08-04 #149](https://github.com/bear20252026/wxlike/issues/149)。

## G15

MCP、CLI与AI授权命令面；[完整任务单](../teams/G15.md)。

| 任务 | 独立Issue | 波次 | 硬依赖 |
|---|---|---|---|
| G15-01 动作目录与协议SDK适配 | [#183](https://github.com/bear20252026/wxlike/issues/183) | W3 | [G00-02 #100](https://github.com/bear20252026/wxlike/issues/100)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101) |
| G15-02 会话授权、资源scope与代理隔离 | [#184](https://github.com/bear20252026/wxlike/issues/184) | W3 | [G15-01 #183](https://github.com/bear20252026/wxlike/issues/183) |
| G15-03 全业务命令与能力差异映射 | [#186](https://github.com/bear20252026/wxlike/issues/186) | W3 | [G15-02 #184](https://github.com/bear20252026/wxlike/issues/184) |
| G15-04 危险动作确认、幂等与作业控制 | [#188](https://github.com/bear20252026/wxlike/issues/188) | W3 | [G15-03 #186](https://github.com/bear20252026/wxlike/issues/186) |
| G15-05 协议兼容、诊断与可审查脚本 | [#190](https://github.com/bear20252026/wxlike/issues/190) | W3 | [G15-04 #188](https://github.com/bear20252026/wxlike/issues/188) |

集成验收依赖：

- G15-01：[G01-05 #110](https://github.com/bear20252026/wxlike/issues/110)、[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101)。
- G15-02：[G01-05 #110](https://github.com/bear20252026/wxlike/issues/110)、[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101)。
- G15-03：[G01-05 #110](https://github.com/bear20252026/wxlike/issues/110)、[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101)。
- G15-04：[G01-05 #110](https://github.com/bear20252026/wxlike/issues/110)、[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101)。
- G15-05：[G01-05 #110](https://github.com/bear20252026/wxlike/issues/110)、[G02-01 #103](https://github.com/bear20252026/wxlike/issues/103)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101)。

## G16

云CI、发布、部署与独立验收；[完整任务单](../teams/G16.md)。

| 任务 | 独立Issue | 波次 | 硬依赖 |
|---|---|---|---|
| G16-01 统一云CI门禁与缓存 | [#187](https://github.com/bear20252026/wxlike/issues/187) | W0/W1 | [G00-02 #100](https://github.com/bear20252026/wxlike/issues/100)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101) |
| G16-02 服务端和桌面跨系统构建 | [#189](https://github.com/bear20252026/wxlike/issues/189) | W4/W5 | [G16-01 #187](https://github.com/bear20252026/wxlike/issues/187) |
| G16-03 Android/iOS云构建、签名与设备证据 | [#191](https://github.com/bear20252026/wxlike/issues/191) | W4/W5 | [G16-02 #189](https://github.com/bear20252026/wxlike/issues/189) |
| G16-04 端到端、故障与容量门禁 | [#192](https://github.com/bear20252026/wxlike/issues/192) | W4/W5 | [G16-03 #191](https://github.com/bear20252026/wxlike/issues/191) |
| G16-05 生产可观测、恢复与发布演练 | [#194](https://github.com/bear20252026/wxlike/issues/194) | W4/W5 | [G16-04 #192](https://github.com/bear20252026/wxlike/issues/192) |

集成验收依赖：


## G17

安全、来源许可与独立评审；[完整任务单](../teams/G17.md)。

| 任务 | 独立Issue | 波次 | 硬依赖 |
|---|---|---|---|
| G17-01 成熟代码来源审查与SBOM | [#193](https://github.com/bear20252026/wxlike/issues/193) | W0/W1 | [G00-02 #100](https://github.com/bear20252026/wxlike/issues/100)、[G00-03 #101](https://github.com/bear20252026/wxlike/issues/101) |
| G17-02 近期漏洞与依赖准入门禁 | [#195](https://github.com/bear20252026/wxlike/issues/195) | W0/W1 | [G17-01 #193](https://github.com/bear20252026/wxlike/issues/193) |
| G17-03 威胁模型与越权/滥用回归 | [#196](https://github.com/bear20252026/wxlike/issues/196) | W4/W5 | [G17-02 #195](https://github.com/bear20252026/wxlike/issues/195) |
| G17-04 解码、XSS、SSRF与沙箱模糊测试 | [#197](https://github.com/bear20252026/wxlike/issues/197) | W4/W5 | [G17-03 #196](https://github.com/bear20252026/wxlike/issues/196) |
| G17-05 独立发布评审与事实核对 | [#198](https://github.com/bear20252026/wxlike/issues/198) | W4/W5 | [G17-04 #197](https://github.com/bear20252026/wxlike/issues/197) |

集成验收依赖：

