# UP-LIVEKIT-RUSTSDK · `livekit/src/room/participant/mod.rs` → `crates/rtc/src/room.rs`

- 主上游：`livekit/rust-sdks` @ `052f089166b6864bc5f7e92906f6a7897b9f546c`
  永久链接：<https://github.com/livekit/rust-sdks/blob/052f089166b6864bc5f7e92906f6a7897b9f546c/livekit/src/room/participant/mod.rs>
  同仓还借了三处：`livekit/src/room/mod.rs`（`ConnectionState`、`update_connection_state`、
  参与者信息批处理）、`livekit-protocol/src/livekit.rs`（`participant_info::State` 与
  `DisconnectReason` 的 proto 数值/稳定名）、以及 `RoomEvent` 的名册分支（`room/mod.rs:107`）。
- 交叉核对源（服务端，另一条 upstream `UP-LIVEKIT-SERVER`）：
  `livekit/livekit` @ `94edd5da75246bd3c04becf88e3fda563e3d6e1c`
  <https://github.com/livekit/livekit/blob/94edd5da75246bd3c04becf88e3fda563e3d6e1c/pkg/rtc/room.go>、
  同 commit 的 `pkg/rtc/participant.go`、`pkg/rtc/types/interfaces.go`。
  本文件里"只前进的状态表"与"房间关闭"两组行为只在 Go 侧存在，Rust SDK 侧没有对应实现，
  所以行号引的是 Go。
- 许可：Apache-2.0（许可不阻挡本项目，但来源必须可复查）
- 本地 sha256：`00495cebb615ffb97daa657171b55f740c0e9de23d6bc2876c461878113c0602`
  （按 Git LF 字节计算；本仓库 `core.autocrlf=true`，未钉 LF 的文件在 Windows 重新 checkout
  会变成 CRLF 从而指纹漂移——`sfu.rs` 就踩过这个坑，`.gitattributes` 现已把
  `crates/rtc/src/room.rs` 与 `crates/rtc/src/token.rs` 一并钉成 `text eol=lf`）
- 形态：**adapted**（逐段照抄行为/状态表/枚举语义，Rust 化改写 + 去依赖 + 换错误类型；
  不是 copy-paste 改 import，也不是"参考思路"）
- 需求：#153 / ADR-25（媒体面 LiveKit sidecar；信令、成员裁决、房间审计留 Rust）

## 拿了什么（逐段对应，行号已逐个回读上游原文核对）

| 上游出处 | 借的行为 / 状态 / 枚举语义 | 本地落点 |
|---|---|---|
| `participant/mod.rs:39-45` `enum ParticipantState` | 参与者状态四值**及声明顺序** Joining/Joined/Active/Disconnected | `SidecarParticipantState`：同名四变体、同顺序；额外派生 `PartialOrd/Ord/Hash/Default(=Joining)` |
| `livekit-protocol/src/livekit.rs:378-387` `participant_info::State` | `Joining=0 … Disconnected=3`：数值序**就是**进度序（proto 注释"websocket' connected, but not offered yet"等四条即本文档注释的原文摘要） | `SidecarParticipantState::tag()` 逐值照抄；`PartialOrd` 与 tag 同序不是装饰，是 `updateState` 判定的载体 |
| `livekit-protocol/src/livekit.rs:393-409` `as_str_name`/`from_str_name` | proto 线上名；"值不做变换、视为稳定"；认不出来即 `None` | 两个同名方法，字面量逐条一致；不做大小写宽松、不猜 |
| `pkg/rtc/participant.go:2333-2358` `ParticipantImpl.updateState` | ① `state <= oldState` → 只打 debug 日志、直接 return，**状态一律不改**；② CAS 重试循环；③ 仅 `ACTIVE→DISCONNECTED` 才记一次会话时长 | ①→ `advance()`（纯函数）+ `report_sidecar_state()` 的 `applied:false` 分支；②→ 见"改了什么"4；③→ `is_clean_disconnect_from()`，落到审计 `detail="active_to_disconnected"` |
| `pkg/rtc/participant.go:443` `p.state.Store(ParticipantInfo_JOINING)` | 参与者初始态是 JOINING | `cmd_join()` 建席位时 `sidecar: SidecarParticipantState::Joining`；`DeviceSeat::default()` 同值；枚举 `#[default]` |
| `pkg/rtc/participant.go:629-641` `IsReady()` | 非迁移分支：`JOINED \|\| ACTIVE` 才算 ready（"因为有 JoinResponse，状态走 JOINING→JOINED→ACTIVE"） | `is_ready()` 只取这一支；迁移分支不借（见"没拿什么"4） |
| `pkg/rtc/participant.go:615-622` `IsDependent()` | agent/egress/recorder 类连接**不决定房间生死** | `Room::mark_dependent()`/`is_dependent()` + `close_if_empty()` 的跳过规则；判据由 `crate::token::AccessClaims::is_dependent` 喂进来（token.rs 那份 patchlog） |
| `pkg/rtc/room.go:136` `closed chan struct{}` | 房间关闭是一个**单向、不可逆**的闸 | `closed: Option<RoomCloseReason>`（channel 只表达"关没关"，我们顺手把原因并进去） |
| `pkg/rtc/room.go:754-761` `IsClosed()` | 关闭状态可查询、无副作用 | `is_closed()` / `close_reason()` |
| `pkg/rtc/room.go:800-822` `Room.Close(reason)` | ① 先 `select { case <-r.closed: return }`——重复关闭是 no-op；② 对每个在场参与者 `p.Close(true, reason.ToParticipantCloseReason(), false)`；③ 最后 `onClose(reason)` | `Room::close()`：①幂等返回 `false` 且不产生第二份审计；②逐个席位落 `Left`+`Disconnected`，原因**取自同一张表**，每个参与者一条 `MemberChanged` 审计（值是参与侧投影）；③`onClose` 回调换成 `RoomEnded` 审计行 |
| `pkg/rtc/room.go:764-798` `Room.CloseIfEmpty()` | 判定四步照抄：已关→直接回；**还有一个独立成员**→直接回；计时起点二选一（`FirstJoinedAt>0 && LastLeftAt>0` 用 `departure_timeout` 从离席起算，否则 `empty_timeout` 从建房起算）；`elapsed >= timeout` 才关，原因 `IdleTimeout` | `close_if_empty()` 同序同判据；两个默认值 `DEFAULT_EMPTY_TIMEOUT_SECS=300` / `DEFAULT_DEPARTURE_TIMEOUT_SECS=20` 取自服务端 `config-sample.yaml:246/248`；单位仍是**秒**（proto 字段就是秒），上游那条 "closing idle room" 日志换成 `departure_timeout`/`empty_timeout` 审计 detail |
| `pkg/rtc/room.go:414-420` `FirstJoinedAt()`/`LastLeftAt()` + `469-471`、`1426` 的写入点 | 两个计时锚点（离开时刻每次移除都写） | `first_joined_at_ms` / `last_left_at_ms`（起点选取与上游有一处等价性偏离，见"刻意的偏离"6） |
| `pkg/rtc/room.go:1227-1281` `Room.onStateChange` | `ACTIVE` → 先 `subscribeToExistingTracks` 再 `telemetry.ParticipantActive`（顺序即语义）；`DISCONNECTED` → `go r.RemoveParticipant(...)`（`1279-1280`） | `SidecarStateEffect{SubscribeExistingTracks, ReportActive, BeginReconnectGrace}`：前两条按上游顺序成对返回，第三条是**刻意替换**（见"刻意的偏离"1） |
| `pkg/rtc/types/interfaces.go:311-320` `RoomCloseReason` iota | 六个房间关闭原因及顺序 | `RoomCloseReason`：`Unknown/ApiDelete/IdleTimeout/ServerShutdown/Superseded/OpenFailed` |
| 同文件 `318-336` `RoomCloseReason.String()` | `UNKNOWN / API_DELETE / IDLE_TIMEOUT / SERVER_SHUTDOWN / SUPERSEDED / OPEN_FAILED` | `as_str_name()` 六条字面量一致；`from_str_name()` 补了上游没有的反解析（上游 `String()` 对越界值回退成十进制数字，我们不猜，未知即 `None`） |
| 同文件 `359-375` `ToParticipantCloseReason()` | 注释原文"so the two can never disagree"：`APIDelete→ServiceRequestDeleteRoom`、`IdleTimeout/Superseded→RoomClosed`、`ServerShutdown→RoomManagerStop`、其余→`None` | `to_disconnect_reason()` 四行表；目标枚举换成 Rust SDK 真正使用的 `DisconnectReason`（`RoomDeleted/RoomClosed/ServerShutdown/UnknownReason`），因为 Go 那侧的 `ParticipantCloseReason` 在线上不存在 |
| `livekit/src/room/mod.rs:290-295` `enum ConnectionState` | **客户端侧**连接状态只有三值，且与房间状态/参与者状态是三张表不能混用 | `SidecarConnectionState{Disconnected,Connected,Reconnecting}` |
| `livekit/src/room/mod.rs:1224-1236` `update_connection_state` | ① 同值 → `return false` 不发事件（上游靠这条把重复 `Reconnected` 折成一条）；② `if info.state == state \|\| self.disconnected.load() { return false }` → 进入 Disconnected 即**终态锁存**，之后任何状态都不再上报 | `ConnectionStateTracker::update()` 两条规则逐条落地（`disconnected` 从 `AtomicBool` 变成 `bool`，见"改了什么"4） |
| `livekit-protocol/src/livekit.rs:2118-2152` `enum DisconnectReason` | 17 个断开原因、`=0..=16` 的编号、`as_str_name`/`from_str_name`、每条变体的注释语义（"the client initiated the disconnect"…） | `DisconnectReason`：17 个同名变体 + `tag()` + 两个名字方法；注释是原文中文摘要。同一枚举在 SDK 侧也有一份无 tag 版本（`participant/mod.rs:68-87`），变体集合一致，我们取带数值的那份，因为审计要与 sidecar/遥测按数字对表 |
| `livekit/src/room/mod.rs:1292-1350` 参与者信息批处理 | 四条**实测出来的**事件序规则：① 未认识 + `DISCONNECTED` → "Ignore, just received the ParticipantInfo but the participant is already disconnected"，什么都不发；已认识 → 先 `update_info`（"need to update to get the correct disconnect reason"）再派发断开，且那一支**没有** push 进 `participants`，所以批尾事件也不发；② 已认识 → 先记 `already_active` 再 `update_info`，只在"非 Active→Active"的**边沿**发 `ParticipantActive`；③ 新人 → **先** `ParticipantConnected`，**再**（若已 ACTIVE）**补一条** `ParticipantActive`，然后才 `update_info` 补轨道；④ 整批结束才发**一条** `ParticipantsUpdated` | `plan_participant_events()`（纯函数）+ `SidecarRoomEvent` 五个名册分支。规则①②③完整照抄；④只落地"每参与者至多一条"的局部形式，批合并归调用方（见"刻意的偏离"4） |
| `livekit/src/room/mod.rs:107` `enum RoomEvent` | 名册相关事件这一小撮（`ParticipantConnected/Active/Disconnected`、`ParticipantsUpdated`、`ConnectionStateChanged`） | `SidecarRoomEvent` 只取这五支；其余分支不派发（见"没拿什么"6） |

## 改了什么

1. **类型与表示**：Go 的 `int` iota + `atomic.Value`、proto 的 `i32` + prost 派生，统一换成手写
   Rust 枚举；`String()`/`as_str_name()` 从生成码变成显式 `match`。`closed chan struct{}` 换成
   `Option<RoomCloseReason>`（原因与状态同处一格，关闭后仍查得到为什么关）。所有借来的枚举都补了
   `serde` 派生——上游不需要序列化，我们要落库并走信令快照；`#[serde(rename_all)]` 的选法跟着
   线上名走（`SidecarParticipantState` 用 `SCREAMING_SNAKE_CASE` 与 proto 名同形）。
   `advance()` 的形状与上游相反：上游是 `void` + 早退，我们是
   `Option<SidecarParticipantState>`（`Some(old)` = 该生效，`None` = 乱序），
   目的是让调用方能在不复制状态表的前提下区分"没变"和"被忽略"。
2. **错误处理换成 thiserror**：上游 `updateState`/`Close` 没有返回值，"被忽略"只留一行 debug 日志。
   这里改成 `#[derive(thiserror::Error)]` 的 `MemberError`（`RoomClosed`/`OutOfOrderSidecarState`/
   `NoSuchDevice`/`NotMember`/`StaleRevision{seen,current}`…，全部 `PartialEq + Eq`，测试能逐条断言
   结论而不是 `is_err()`），并用 `MemberErrorCode::as_str()` 输出稳定结论码进审计表。
   既有的 `RoomError`（ADR-7 管控原语）通过 `room_to_member()` 收敛到 `MemberError`，对外只有一套
   错误。**行为等价、可观测性等价，签名不等价**：乱序不再是静默忽略，而是 `Ok(applied:false)` +
   一条 `out_of_order_sidecar_state` 审计；乱序**不改状态**这条硬语义与上游完全一致（水印 `revision`
   都不许推进，由测试钉住）。
3. **去掉对上游 crate 的运行时依赖**：没有引入 `livekit`、`livekit-protocol`、`prost`、`tonic`，
   也没有 `livekit-ffi`。所有借来的语义都是手写字面量 + 纯 Rust 计算；proto 的数值/名字表靠
   `tag()`/`as_str_name()`/`from_str_name()` 与 sidecar 对表，而不是把 protobuf 类型搬进状态机。
   根 `Cargo.toml` / `Cargo.lock` 未改动（新增依赖必须走 integration-request）。
4. **并发模型**：上游用 CAS 自旋 + `atomic.Int32/Bool` + `chan struct{}`（`p.state`、
   `p.disconnected`、`r.holds`、`r.closed`），因为 Go 侧一个 participant 会被多个 goroutine 同时摸；
   Rust 侧 `Room` 是单所有权、所有变更走 `&mut self`，串行化由类型系统保证，所以
   `advance()` 是无共享状态的纯函数、`ConnectionStateTracker` 是普通 `struct`（不再 `Arc<AtomicBool>`）。
   `EngineGate` 是唯一保留的锁，且只用 `std::sync::Mutex` 保护一个枚举值，不跨 await 持锁（ADR-28）。
   **上游没有**的跨请求竞态（同时加入、离席后的迟到信令、重放旧快照）改由客户端带上来的
   `MemberCommand::expected_revision` 水印裁决——这一条是自研，见下一节第 5 项。
5. **借来的表被接到我们自己的状态机上**：`SidecarParticipantState`（sidecar 报到哪一步）与
   `MemberState`（我们的房间决定到哪一步）是两格，靠 `to_member_state()`/`from_member_state()`
   显式投影；上游只有一个状态格，不存在这层映射。

## 刻意的偏离（不是移植失真，理由都写在代码注释里）

1. `DISCONNECTED` 不立即 `RemoveParticipant`（上游 `room.go:1279-1280`），而是落到
   `MemberState::Reconnecting` + `grace_until_ms`，由 `Room::tick()` 到点回收。理由：客户端网络抖一下
   不该把名册打散，更不该让 `generation` 前滚一轮从而把全员凭证作废。
2. `SidecarParticipantState::Joined` 折叠进 `MemberState::Joining`（`to_member_state`）：成员状态里
   没有"收到 offer 但 ICE 未通"这一档，凭证面只关心 `holds_media()`。
3. `MemberState::Removed`（被踢/撤销）反向映射到 `Disconnected`：上游参与者状态表里根本没有
   "被踢"这一档，只有 `DISCONNECTED` + 关闭原因（`DisconnectReason::ParticipantRemoved`）。
4. `plan_participant_events()` 的断开分支写回 `DisconnectReason::UnknownReason`。上游那一支先
   `update_info(pi)` 正是为了拿到 sidecar 给的**真实**原因，而那个原因住在 protobuf
   `ParticipantInfo` 里，Rust 侧不解析 protobuf，所以现在只能落 UNKNOWN。这是**已知缺口**，
   `participant_event_order_matches_the_upstream_client` 第 6 条钉的是当前行为，不是理想行为。
5. 同上，规则④"整批一条 `ParticipantsUpdated`"只实现到"每参与者至多一条"：函数按单个参与者签名，
   批合并责任交给调用方，而**当前没有生产调用方**（见下一节的落地状态）。
6. `first_joined_at_ms` 在建房时即置为 `now_ms`，上游是在"第一个非依附参与者真正加入"时才写
   （`room.go:469-471`）。因为我们的 host 在建房那一刻就是独立成员，两者取值等价，但后果是上游
   `FirstJoinedAt() > 0` 这半边守卫实际退化成了 `last_left_at_ms != 0` 的判断。照实记录，不谎称等价。
7. 上游 `CloseIfEmpty` 的 `r.holds.Load() > 0` 前置守卫（`room.go:767`，`holds` 是对"正在进行中的
   房间操作"的引用计数，见 `434/439`）没有移植——我们没有等价的进行中操作计数，因此也没有
   "操作期间不关房"这条保护。
8. `Room::close()` 里额外前滚 `generation` 并 `bump()` 水印：上游关房不动这两者（它没有这两个概念），
   我们关房必须让已签发的凭证整代失效。

## 没拿什么（以及为什么）

1. **protobuf / prost / `livekit-protocol` 的消息类型**（`ParticipantInfo`、`RoomConfiguration`、
   `DataPacket`、`AddParticipantRequest` 等）：借的只有这些消息里**枚举的数值与稳定名**（proto 生成码
   的 `State`/`DisconnectReason`），不是消息本身。线格式留在 sidecar 那一侧。
2. **FFI 与上游 SDK 的运行时**：`livekit-ffi/`、`livekit/src/room/mod.rs` 里
   `Room::connect()`/`RoomOptions`/`Room::from_ffi`/sanitizer worker/receiver 那一整套
   client↔server protobuf-over-WebSocket 协议**一律不借**。我们与 sidecar 的边界是自己的信令信封。
3. **服务端内部结构**：`pkg/rtc/types` 里 `Participant`/`LocalParticipant` 那两个 Go interface
   （`interfaces.go` 中 `Participant` 单是接口体就有 36 个方法，加上 `LocalParticipant` 的扩展与
   counterfeiter 生成的假件）、`Room` struct 的其余字段
   （`batchedUpdates`、`trailer`、`agentDispatches`、`hasPublished`、`participantRequestSources`、
   `trackEgresses`）、`roomutils`/`connectionquality`/`types.ProtocolVersion`：只取上表列出的状态片段。
4. **连接迁移**：`IsReady()` 的 `p.params.Migration` 分支、`MigrationState`、`migrateState`、
   `reconcile_absent_participants`（`room/mod.rs:1243-…`，重连后按服务端名册合成缺失参与者断开）、
   `DisconnectReason::Migration` 的产生路径。我们不迁移连接，只保留该枚举值以便与 sidecar 对表。
5. **遥测与指标**：`telemetry.ParticipantActive(...)`、`roomobs.ParticipantSessionTx`
   （`ReportClientConnectTime`/`ReportConnectResult`/`ReportConnectionType`）、
   `prometheus.RecordSessionDuration`、`livekit.RoomEndReason`（`ToProto()`）：借的是"ACTIVE 要上报"
   与"只对 ACTIVE→DISCONNECTED 记完整会话"这两条判据，载体换成房间审计行，不引入指标面。
6. **非名册事件**：轨道上下线、data channel、chat、`E2EE`、`SIP`、`Agent` 那几支 `RoomEvent` 与
   `ParticipantKind/KindDetail` 的完整分派。ADR-25 下同一件事只能有一个订阅者，Rust 侧重复派发
   等于两份房间事实。
7. **多节点与服务端控制面**：`rtcNode`、Redis/`MessageSuite`、`SignalNext` 转发、
   `RoomService`/`SIPService` handler、`config-sample.yaml` 除两个超时以外的配置面、
   `RemoveParticipant` 的 `hidden`/`NumParticipants` 记账。
8. **服务端 Egress**：上游 `StartEgress`/`EgressStart` 语义与 `ParticipantCloseReason` 的 recorder 分支
   都不借；ADR-25 把服务端录制关死，本地只留 `EgressError::Disabled` 一条恒定拒绝。

## 判定为自研的部分（明确**不是**从 LiveKit 来的，不为凑数挂名）

1. `Role{Member,Moderator,Host}` 与三档权限矩阵（静音他人 / 踢人 / 锁门 / 录制旗标 / 转让主持）。
   LiveKit 的角色是服务端可配置的字符串 + `VideoGrant` 位，没有这套三档，`participant/mod.rs` 里
   也没有 `Role` 类型。出处是合同 §3.7。
2. `Track{Audio,Video}` 静音维度与 `audio_muted/video_muted` 两个旗标。上游 rust-sdks 全文 grep
   不到 `MutedState`（零命中），轨道级 mute 走的是 publication 上的 `is_muted()`
   （`local_participant.rs:418`、`remote_participant.rs:172`），与我们的"按轨别记在服务端名册上"
   不是一回事。`canPublishSources` 只作为 protobuf 字段存在于生成码里
   （`livekit-api/src/public_api/generated.rs:4144`、`livekit-protocol/src/livekit.serde.rs`），
   判定实现未随本 checkout 提供（真正执行它的是 Go 模块 `protocol/auth`）——同 token.rs 那份
   patchlog 的结论：**没有源可抄的判定不写**。
3. `MemberState{Joining,Active,Reconnecting,Left,Removed}` 这套**成员**状态机（含
   `holds_media()`/`may_receive_token()`/`as_str()`）。借来的 `SidecarParticipantState` 是它的输入
   之一，不是它的别名。
4. `DeviceSeat` 与 user+device 复合席位（含 `revision`、`grace_until_ms`、聚合函数 `aggregate()`）。
   LiveKit 的身份是 `ParticipantIdentity`/`ParticipantID`，没有"同一用户多个设备席位"这一层。
5. `MemberCommand::expected_revision` 的 CAS 水印、`revision` 与 `generation` 双计数、
   `MemberAction`/`MemberOutcome`。上游的 CAS 是**同一状态格内部**的原子重试，不是客户端带上来的
   快照版本比对；跨请求的"重放旧状态"裁决是我们自己的。
6. `EngineGate`/`MediaEngine`（单在线引擎、str0m 冷备不得拥有在服务中的房间）与 `EngineError`。
   纯 ADR-25/ADR-28 的本地部署事实，上游无此概念。
7. 房间审计整块：`RoomAuditEntry`/`RoomAuditAction`/`MemberErrorCode`/`take_audit()`/`audit_tail()`/
   `AUDIT_QUEUE_CAP` 溢出计数、个人数据最小化（只存 id 与结论码，不存 SDP/IP/密钥）。
   上游对应物是 telemetry/roomobs，形态完全不同。
8. `sidecar_room_name()`（`wxlike-<scope>-<conv>-<id>` 的稳定房间名）与 `ACCESS_TTL_MS`/
   `RECONNECT_GRACE_MS` 两个本地策略常量。
9. `room_to_member()` 错误收敛、`prepare_access()` 的引擎不符即拒签、`may_publish()` 的
   "服务端静音体现在下一次凭证的 `canPublish`"这条路径。
10. 出口码率相关（`EgressPlan`/`allocate_egress`/`allocators`）不属于本条上游：那是
    `UP-ATM0S`（`crates/rtc/src/sfu.rs`，另有 patchlog）。此处点名是为了避免"同文件即同源"的误读。

**落地状态（照实说）**：上面借来的公开接口——`report_sidecar_state`、`close_if_empty`、
`plan_participant_events`、`ConnectionStateTracker`、`SidecarRoomEvent`、`is_ready()`、
`to_member_state()`、`EngineGate`、`take_audit()`——目前**只被测试调用**，`apps/` 与
`crates/rtc/src/call_session.rs` 里没有任何引用（已 grep 确认）。registry.json 的
`used_by` 写成 "crates/rtc/src/{call_session,room}.rs"，对本条 entry 而言**偏高**：
`call_session.rs` 只用 token.rs 的签发面，不用本文件的状态机。接线属于 sidecar 集成卡，
不在本卡范围；在接上之前，借来的语义是"已实现、已验收、未上路"。

## 本地测试覆盖（`crates/rtc/tests/room_sidecar_state.rs`，16 条，全部通过）

| 借来的行为 | 测试名 |
|---|---|
| 数值序即进度序、tag/as_str_name/from_str_name、`updateState` 的"只前进"、`IsReady()` 非迁移分支 | `sidecar_state_only_moves_forward` |
| 初始态 JOINING、乱序报告不改状态也不推水印、忽略要留痕、`ACTIVE` 副作用的先后、`BeginReconnectGrace` 取代立即移除、`is_clean_disconnect_from`、`to_member_state`/`from_member_state` | `late_sidecar_reports_are_ignored_without_touching_state` |
| 席位/成员不存在时分别给 `NoSuchDevice`/`NotMember`（不折成一个"失败"） | `unknown_sidecar_seat_and_missing_member_are_named_not_folded` |
| 重连必须把 sidecar 状态表拨回 JOINING，否则后续报告全被当乱序 | `reconnect_resets_the_sidecar_clock_so_rejoin_can_succeed` |
| 宽限期内不回收、到点由 `tick()` 回收并触发主持移交 | `expired_grace_reclaims_the_seat_through_tick` |
| `Close` 幂等（不产生第二份审计）、房间侧与参与侧原因同源（`to_disconnect_reason` 五行全查） | `room_close_is_idempotent_and_agrees_on_the_reason` |
| 关房后命令/签发/sidecar 回报一律 `RoomClosed`（含匿名 actor 先撞关闭闸）、结论码可反解 | `closed_room_refuses_commands_mints_and_sidecar_reports` |
| `CloseIfEmpty` 的计时起点二选一、`empty_timeout`/`departure_timeout` 两个默认值、已关房间不再关第二次 | `close_if_empty_prefers_departure_timeout_after_someone_left` |
| `IsDependent` 语义：依附者不留房、不存在的人按独立处理（宁可晚关不可错关） | `dependent_participants_do_not_keep_the_room_alive` |
| `room/mod.rs:1292-1350` 的四条事件序（6 个用例，含"未认识+DISCONNECTED 什么都不发"与"断开那一支不发批尾事件"） | `participant_event_order_matches_the_upstream_client` |
| `update_connection_state` 的同值不发 + 终态锁存 | `connection_state_tracker_latches_the_terminal_disconnect` |
| `DisconnectReason` 17 值的 tag 与稳定名往返、`RoomCloseReason` 6 名往返、未知即 `None` | `disconnect_reason_names_round_trip_and_tags_match_proto` |
| 借来的状态机与既有容量/席位裁决的竞态（不超卖、过期水印不误判成功） | `interleaved_join_and_late_report_cannot_oversell_a_seat` |
| 旧 `Room::leave` 路径不推动离席计时器（两条关房路径不等价的回归闸） | `legacy_leave_does_not_arm_the_departure_timer_by_stealth` |
| 录制闸与房间生命周期无关、依附标记换不来可签 kind | `egress_gate_is_independent_of_the_room_lifecycle` |
| 冷备房间签不出 LiveKit 令牌 | `prepare_access_still_refuses_a_non_livekit_room` |

既有 ADR-7 管控行为（自研那部分）由 `crates/rtc/tests/room.rs` 的 13 条覆盖
（`join_and_roster`、`capacity_is_enforced`、`locked_room_refuses_members`、
`only_host_can_lock_or_record`、`mute_self_is_allowed_but_muting_others_needs_role`、
`kick_removes_and_blocks_rejoin`、`moderator_cannot_kick_moderator`、
`host_leave_transfers_to_moderator_first`、`host_leave_without_moderator_falls_back_to_earliest_member`、
`last_participant_leaving_ends_the_room`、`transfer_host_demotes_previous_host`、
`hands_raise_in_join_order`、`bad_capacity_is_refused`）——它们与上游无关，改借来的语义不该红它们。

## 复核方式

```bash
cd /d/mimo/test/par/G09/wxlike
cargo test -p wxlike-rtc --test room_sidecar_state --test room   # 16 + 13
PYTHONPATH=tools/source-audit python -m sourceaudit provenance check
```

`crates/rtc/src/room.rs` 第 147-164 行的"本节四个类型是对 LiveKit 官方实现的移植"来源声明段
不得删除；每条借来的规则上方都注了上游文件与行号（已逐个回读核对，Go 侧行号以 commit
`94edd5da` 为准，Rust 侧以 `052f0891` 为准）。改上游行为等于改上面那 16 条测试。
