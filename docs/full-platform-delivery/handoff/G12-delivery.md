# G12 交付说明（Flutter 核心界面与原生桥）

日期：2026-10-24　工作树：`D:/mimo/test/wxlike-g12`　基线：`origin/main@9d00452`
组跟踪 [#93](https://github.com/bear20252026/wxlike/issues/93)；总跟踪 [#80](https://github.com/bear20252026/wxlike/issues/80)。

六张卡按编号串行，分支 stacked（01 → 06）；每张卡只改 G12 归属路径，
`lib/main.dart`、`apps/mobile/android/**`、`apps/mobile/ios/**` 与上游契约的改动全部登记在
[`integration-requests/G12/`](../../../integration-requests/G12/README.md)。

## 一、逐卡交付

| 卡 | 分支 | 内容 | 状态 |
|---|---|---|---|
| G12-01 #163 | `feat/FP-G12-01` | 产品壳、设计 token、底部栏/路由栈/系统返回、应用锁、深链、账号与手机号挑战状态机 | 待评审 |
| G12-02 #165 | `feat/FP-G12-02` | 会话/时间线投影、反向分页、未读定位、前后台与网络恢复、草稿/附件落盘 | 待评审 |
| G12-03 #166 | `feat/FP-G12-03` | 输入器、语音按住取消、附件分阶段失败与重试、长按菜单、红包入口 | 待评审 |
| G12-04 #169 | `feat/FP-G12-04` | 通讯录分页/搜索/审批、资料页权限区分、群成员与角色、越权拒绝 | 待评审 |
| G12-05 #171 | `feat/FP-G12-05` | MethodChannel 平台桥：权限/采集/录音/定位/安全存储/网络/深链/推送 | 待评审 |
| G12-06 #173 | `feat/FP-G12-06` | 验证码登录与扫码授权页、协议版本可审计、批准只影响目标桌面 | 待评审 |

## 二、关键设计决定

1. **界面不判断业务事实**。消息去重/排序/未读、关系版本、权限拒绝全部来自端口返回值；
   Dart 只做渲染与导航，错误统一映射成 `G12Failure`，页面不解析异常字符串。
2. **未接线就报错**。身份域、关系域、媒体、红包、消息订阅尚未进入 FFI 的入口使用
   `Unavailable*Port` / `PLATFORM_UNWIRED`，绝不用本地假数据或 toast 冒充成功。
3. **草稿与待发附件跨进程存活**。文字与附件元数据写入应用私有目录的 `g12_drafts.json`
   （原子 rename），切后台/退出时 flush；附件保留"已上传 media_id"，重试只重做失败阶段。
   消息事实仍在 Rust outbox，这份文件只恢复界面输入。
4. **重试必须重查**。时间线 `retry` 后重新查询历史与 outbox；好友审批/群操作成功后重新
   查询上游，不本地拼一个"已通过"。
5. **应用锁是设备锁，不是 token 校验**。冷启动恢复 PIN 后先上锁；后台超过 60 秒重新上锁；
   PIN 存 OS 安全存储（G12-05 通道）。Dart 不持有 token。
6. **平台桥零插件**。用 MethodChannel/EventChannel 定义固定协议，宿主实现由 G00 落地；
   通道缺失时每个入口返回明确原因，测试用 mock 通道钉住参数形状。
7. **深链是外部输入**。只认 `wxlike://` 的 chat/qr/contacts 三种形状，其余明确拒绝；
   未登录收到扫码链接先暂存，登录后继续确认。

## 三、验证结果（本机实测）

```text
cd apps/mobile
flutter analyze
  No issues found!

flutter test
  59 tests, All tests passed
  其中 G12 新增 50 个：
    auth_test            9
    shell_test           7
    timeline_test        7
    composer_test        9
    contacts_test        9
    platform_test        6
    qr_approval_test     3
```

覆盖：成功、空结果、参数错误、未授权/越权、超时/断网、取消、重复提交；
写操作幂等键复用与冲突刷新；附件上传/发送分阶段重试；进程重启恢复草稿/附件。

本机限制（如实声明）：

- **没有 Android/iOS 真机、签名与 APNs/FCM 凭据**：平台通道宿主侧、权限、推送、深链
  冷启动均未在真机执行，状态是 BLOCKED，不是 PASS。
- **身份域/关系域/消息订阅/媒体/红包命令尚未进入 FFI**：对应页面用替身验证状态机与
  失败分类；这些页面不能据此标 ACCEPTED。
- 深色 token 是原型缺失下的实现补全（5px 气泡圆角按原型 `--color-bubble` 区块修正），
  已在 G12-01-R3 请求设计确认。
- 六卡是 stacked PR；本机只在 tip 上跑完整 analyze/test，未对中间分支单独重跑。

## 四、数据与回滚

- 无数据库迁移；Dart 侧新增唯一持久化是 `g12_drafts.json`（应用私有目录），
  回滚后残留文件无副作用，可直接删除。
- 应用锁 PIN 存 OS 安全存储单个 key `g12.lock.pin`；回滚/退出不涉及服务端数据。
- 未删除旧 `screens/*.dart`，入口切换由 G00 执行，避免同一 PR 既换入口又删旧页面。

## 五、未完成与归属

| 项 | 负责方 | 说明 |
|---|---|---|
| 入口挂载与深链声明 | G00 | `integration-requests/G12/G12-01.yaml`、`G12-06.yaml` |
| 事件订阅 / first_unread_seq | G01、G03 | `G12-02.yaml`；当前轮询是降级路径 |
| 媒体/语音/转发/撤回/引用/红包动作 | G03、G04、G06 | `G12-03.yaml` |
| 关系域、资料域命令进入 FFI | G01、G02 | `G12-04.yaml` |
| Kotlin/Swift 通道、清单权限、APNs/FCM | G00、G16 | `G12-05.yaml`；真机证据只能在 CI/设备产生 |
| 协议文档查询与版本审计 | G01、G02 | `G12-06.yaml`；文档未加载时登录按钮不可用 |
| 深色 token 正式化 | G00/设计 | `G12-01.yaml` R3 |

按任务单：合入并通过云 CI/真机验收后才关闭子 Issue；本组当前为**待评审**。
`blocked` 不等于 `pass`。
