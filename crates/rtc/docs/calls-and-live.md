# G09 运行手册：通话、媒体控制、E2EE/录制与直播

本文件覆盖 G09-02 ~ G09-05 的服务端契约与运维常量。所有状态机都是纯
Rust、可脱离网络测试；真实媒体路径与 SFU 行为在
`crates/rtc/tests/*`、`crates/sfu/tests/*` 中用真 UDP 验收。

## 1. 呼叫状态机（G09-02）

```text
RINGING ──accept(第一个设备胜出)──► ACCEPTED ──begin_connecting──► CONNECTING
   │                                    │                              │
   │                                    └──────────media_connected─────┤
   ▼                                                                   ▼
ENDED(Rejected | Cancelled | HungUp | Busy | Timeout | Failed | AuthorizationRevoked)
```

* `RING_TIMEOUT_MS = 30_000`：无人接听自动 `Cancelled`。
* `CONNECT_TIMEOUT_MS = 30_000`：接了但媒体不通 → `Failed`。
* 忙线索引由服务端维护：被叫已在任何通话中，第二通直接 `Busy`。
* 多设备接听：第一个 `accept` 胜出；同设备重试幂等；其他设备得到
  `AnsweredElsewhere(winner_device)`。
* 退群/封禁调用 `revoke_member`：立即结束通话、代次前滚、所有旧令牌失效。
* 结束是幂等的，且只写一条通话记录（`CallRecordSink`）。

### 媒体令牌

格式 `v1.<hex(claims)>.<hex(HMAC-SHA256)>`，claims 绑定
`call + user + device + scope + generation + issued_at + expires_at`。

* `MediaScope::Call / LivePublish / LiveWatch / LiveCohost` 权限面分离。
* 令牌寿命 `MEDIA_TOKEN_TTL_MS = 120_000`，客户端到期前续签。
* 撤销靠代次：成员变更/停播/重连都会 `generation += 1`，旧令牌整代失效。
* 设备绑定：只有赢下接听竞争的设备能换媒体令牌（`active_device_ok`）。
* 算法与密钥：HMAC-SHA256，密钥最少 32 字节；`TokenSigner` 的 Debug
  只输出 `[redacted]`，不打印密钥。

### 通话记录与迁移

表结构见 `crates/rtc/schema/calls.sql`，正式迁移编号由 G00 分配
（提案见 `schema/proposals/G09/`）。只存参与人/设备 id、时间、视频标志、
代次与结束原因；**不存 SDP、IP、媒体内容**。

回滚：本迁移只新增表与索引，不修改既有表。回滚即
`DROP INDEX idx_calls_callee, idx_calls_caller, idx_calls_conv; DROP TABLE calls;`
旧版本进程不读取该表，因此可以安全回退。

## 2. 媒体轨与设备（G09-03）

* 静音是编码器出口开关：`write_audio` / `write_video` 在 muted 时**不产生
  任何包**（不是只改 UI 图标）。测试 `media_controls.rs` 断言远端计数为 0。
* 摄像头/屏幕切换：`select_video_source` + `write_video_from(source, frame)`；
  切换后旧源的帧返回 `false` 并被丢弃。
* 设备拔出：`DeviceRegistry::unplug` 停止采集并选择回退设备，但**不自动
  开始采集**；权限被拒时 `deny_permission` 立即清空采集。
* ICE 恢复：`IceMonitor` 默认 2 s 降级、5 s 触发重启、10 s 冷却、最多 3 次；
  `MediaSession::restart_ice()` 产出新 offer 并等待新的 `Connected`。
* SFU 侧 `set_published_muted` 做服务端静音：客户端继续发包也不会被转发。

### TURN

coturn REST 凭据（`crates/rtc/src/turn_auth.rs`）：

```text
username = "<expiry_unix>:<user>:<device>"
password = base64(HMAC-SHA1(secret, username))
```

* 密钥 `TURN_SECRET` 最少 16 字节，`ttl` 默认 3600 s、上限 21600 s。
* `TurnQuota` 默认每用户 2 路并发、每小时 2 GiB，滚动窗口重置。
* 凭据含 user/device，泄给别人也只能在有效期内冒充该设备；日志不得打印
  password（`TurnCredentials` 只暴露 username/expiry）。
* 强制 TURN 验收需要真实 coturn 部署；本机只有算法与端口逻辑测试，
  已在待验清单登记。

## 3. E2EE 与录制（G09-04）

### 媒体 E2EE

* 算法 ChaCha20-Poly1305；每 `(epoch, sender)` 用 HKDF-SHA256 派生独立密钥
  （info `wxlike-media-e2ee-v1|<epoch>|<sender>`），nonce 由发送序号构造。
* 帧头含 version/epoch/sender，作为 AAD；`ReplayWindow`（128 帧）拒绝重放。
* 入会即拿当前 epoch 的密钥；成员被移除/主动轮换时 `install_rotation`
  升 epoch 并只发给剩余成员。被移除者解不开新代次。
* SFU/中继不持有密钥：它只转发密文；录制端没有密钥时
  `record_frame(None, …)` 返回 `E2eeRequired`。
* `MediaKeyRing` 的 Debug 输出密钥为 `[redacted]`；密钥不落日志、不写库。

### 知情同意录制

* `RecordingSession`：发起人视为同意，其余参与者必须显式同意；任一人拒绝
  → `Rejected`，存储端口从未 `begin`，不会产生文件。
* 只有发起人能 `stop`；重复 `stop` 返回同一 artifact，不重复封存。
* 开启 E2EE 时录制端必须先解出明文再交给 `RecordingSink`（G04 端口）。
* 录制文件只经端口交给 G04；本模块不写磁盘、不存 URL。

### 权限与日志

* 通话记录、令牌 claims、TURN 凭据只含必要 id；无手机号/密码/SDP。
* `TokenSigner`、`MediaKeyRing` 的 Debug 脱敏由单元测试锁定。
* 错误响应区分 `NotInvited` / `Busy` / `Ended`，但房间不存在与无权限在
  SFU 入会处统一 404，避免探测。

## 4. 直播（G09-05）

直播复用 SFU，但权限面与通话分离：

| 角色 | scope | 上行 | 说明 |
|---|---|---|---|
| 主播 | `LivePublish` | 有 | 每路直播只有一人 |
| 观众 | `LiveWatch` | **无** | SFU `join_viewer`，发布者过滤兜底 |
| 连麦者 | `LiveCohost` | 有 | 主播审批后签发，取消即失效 |

* 观众计数按 **user** 去重，多设备只增加连接数。
* 主播断网：`host_offline` 进入 `Reconnecting`，宽限
  `RECONNECT_GRACE_MS = 60_000`；`host_reconnect` 代次前滚并换发推流令牌；
  `tick` 超时 → `Ended(HostLost)` 并清空观看者。
* 停播顺序：先 `source_revoked`（G05 Feed 立即撤源），再 `feed_ended`，
  最后 `archive_stopped`（若开着 G04 归档）。`stop` 幂等；治理侧可
  `force_stop`。
* 连麦：观众 `request_cohost` → 主播 `approve_cohost`；取消连麦不需要
  代次前滚，校验时查当前连麦名单。
* 举报：同一举报人对同一路直播只计一次；主播可 `ban`，立即断开其所有
  设备、移出连麦名单，且该用户不能再看。
* 播放状态：`Buffering / Playing / Stalled / AwaitingGesture`。浏览器自动
  播放受限时停在 `AwaitingGesture`，只有 `Playing` 算在播；弱网卡顿显示
  `Stalled`，恢复必须经过 `Buffering`。
* 归档开关 `set_archive` 仅主播可用，重复设置不重复调用 G04 端口。

## 5. 已知边界

* 直播状态在进程内；持久化由 G05 Feed 行与 G04 归档任务承担，本卡不新增
  live 数据库表。
* 多节点 SFU 级联不在本卡范围；单节点容量与转发规则已固定。
* 真实设备、公网 TURN、后台来电、系统级音频路由见
  `engine-decision.md` 待验清单。
