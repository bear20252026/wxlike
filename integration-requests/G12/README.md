# G12 集成请求（交 G00 串行执行）

本目录是 G12「Flutter 核心界面与原生桥」的**跨组改动清单**。G12 只写：

- `apps/mobile/lib/shell/**`
- `apps/mobile/lib/features/auth/**`
- `apps/mobile/lib/features/chat/**`
- `apps/mobile/lib/features/contacts/**`
- `apps/mobile/lib/platform/**`
- `integration-requests/G12/**`

凡是要改 `lib/main.dart`、`apps/mobile/android/**`、`apps/mobile/ios/**`、根依赖、
设计 token 或上游域契约的，一律登记在这里。`status` 取值与 G01 一致：

| status | 含义 |
|---|---|
| `requested` | 尚未执行，等 owner |
| `applied-pending-ratification` | 为让验证能跑，已临时落地；请 owner 复核 |
| `done` | owner 已执行 |
| `no-change` | 本卡确认不需要改；保留一行说明，便于审计 |

## 逐卡缺口

| 卡 | 主要请求 | owner |
|---|---|---|
| G12-01 | 入口挂载、深链声明、深色 token 确认 | G00 |
| G12-02 | 事件订阅绑定、first_unread_seq | G01、G03 |
| G12-03 | msg.send_media/send_voice/forward/recall/quote、media.upload/play、red_packet.current | G03、G04、G06 |
| G12-04 | 关系域与资料域命令进入 FFI、分页/版本字段 | G01、G02 |
| G12-05 | Kotlin/Swift 通道实现、清单权限、APNs/FCM 凭据 | G00、G16 |
| G12-06 | identity.login.* / qr_login.* 绑定、协议文档查询与版本审计 | G01、G02 |

## 当前证据边界

- 本机没有 Android/iOS 真机与签名环境：平台通道、权限、推送、深链的**宿主侧**未实测。
- G02 关系域、G02 身份登录、G03 消息订阅/媒体、G04 上传、G06 红包命令尚未进入 FFI；
  对应页面在端口层明确返回 `UPSTREAM_NOT_WIRED`，测试使用替身验证状态机与失败分类。
- 因此六卡状态是**待评审**，不得标 ACCEPTED；`blocked` 不等于 `pass`。
