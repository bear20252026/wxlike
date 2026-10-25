# G13 集成请求（交 G00 串行执行）

本目录是 G13（Flutter 扩展页面与全入口）的**跨组改动清单**。G13 只写
`apps/mobile/lib/features/{content,video,commerce,mini,discover,settings,calls,search}`
与 `apps/mobile/test/g13`；凡需要改根导航 / 生成绑定 / 平台桥 / 构建脚本 /
schema 编号的，登记在这里由 G00 或者对应 owner 执行。

| 文件 | 任务 | 主要请求 |
|---|---|---|
| G13-01.yaml | #172 朋友圈、公众号、收藏与卡片内容 | ContentPort/PublisherPort/FormatPort 绑定、发现/我入口 |
| G13-02.yaml | #175 视频号、通话、直播 | Video/Live/Call 绑定、播放器与音视频平台桥、来电恢复 |
| G13-03.yaml | #177 钱包、银行卡、红包、收付款、卡包 | Commerce 绑定、卡令牌/码图渲染/相册、转账回会话 |
| G13-04.yaml | #179 发现、扫码、识物翻译、小程序 | Discovery/Mini 绑定、相机/定位/小程序宿主、发现入口 |
| G13-05.yaml | #181 搜索、设置、我的资料、账号安全 | Search/Profile/Preferences 绑定、退出登录动作、链接打开、版本注入 |
| G13-06.yaml | #174 状态、位置共享、名片码、订阅号 | Status/Location/Qr/Publisher 绑定、地图/导航/二维码渲染、回会话 |

## 状态说明

- 全部请求当前为 `requested`：G13 未修改根导航、未新增依赖、未申请 schema 编号。
- 页面在端口未接线时使用 `Unavailable*Port`，显示"上游生成绑定未接入"，
  不返回静态样例、不用 toast 冒充完成（见 `apps/mobile/lib/features/content/g13/`）。
- 因此 **G13 的六个任务均未达到 ACCEPTED**；需要等上表绑定与平台桥合入后，
  按各 YAML 的 verification 在真机复验。

## 测试口径

- `apps/mobile/test/g13/` 的替身只替换尚未合入的生成绑定，用于并行开发阶段
  验证三态、参数校验、幂等键复用与防重；它不证明真实域实现正确。
- 最终集成验收必须换成真实端口，并覆盖成功/空/参数错误/未授权/超时或断网/
  取消，以及写操作重复提交。

## 契约版本

`g13/1`（定义见 `apps/mobile/lib/features/content/g13/core.dart`）。升级需与
`docs/full-platform-delivery/01_contracts_and_data.md` 中对应域契约同步。
