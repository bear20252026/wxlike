//! 群通话房间制（ADR-7 / 合同 §3.7）：通话房间**独立于群消息管道**，
//! 不占用群信箱、不写序列号。房间是有状态的：谁在房里、谁能静音谁、
//! 锁门后谁能进——这些都不属于消息层。
//!
//! 与 1:1 通话（[`crate::call`]）的关系：房间复用同一套信令与媒体栈，
//! 只是把"两方状态机"换成"多方名册 + 管控动作"。所以本模块是纯状态机，
//! 不碰 str0m，也不碰网络——房间状态可以脱离媒体栈单独验收。
//!
//! ADR-25 之后本模块多了三件事，都是**服务端事实**，不外包给 sidecar：
//!
//! 1. [`EngineGate`] 与 [`MediaEngine`]：媒体引擎归属。线上只允许一个引擎在线，
//!    str0m 是冷备，冷备不得拥有正在服务的房间（否则就是两套房间事实）。
//! 2. [`Room::member_command`]：成员状态机的**唯一**变更入口，带 CAS
//!    （`expected_revision`）与设备席位，用来裁决同时加入、离席后的迟到信令、
//!    以及被重放的旧状态快照。
//! 3. [`Room::take_audit`]：房间审计。每个允许与每个拒绝都留痕，只存 id 与
//!    结论码，不存 SDP/IP/密钥。
//!
//! 权限模型（合同 §3.7「静音/踢人/锁定/录制」）：
//!
//! | 动作 | 主持人 | 管理员 | 成员 | 本人 |
//! |---|---|---|---|---|
//! | 静音自己 | ✅ | ✅ | ✅ | — |
//! | 静音他人 | ✅ | ✅ | ❌ | — |
//! | 踢人 | ✅ | ✅（不能踢主持人/管理员） | ❌ | — |
//! | 锁定房间 | ✅ | ❌ | ❌ | — |
//! | 客户端录制旗标 | ✅ | ❌ | ❌ | — |
//! | 服务端 Egress 录制 | ❌ | ❌ | ❌ | ❌（ADR-25 关闭） |
//! | 转让主持 | ✅ | ❌ | ❌ | — |
//!
//! 三条安全默认值：**锁门后成员进不来**（显式的管理员入房动作仍可进，便于
//! 处置）、**被踢者不可再进**（ban 名单随房间生命周期）、**主持人离开自动移交**
//! （移交给最早加入的管理员，没有则最早加入的成员；空房即结束）。

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::sfu::{EgressBitrateAllocator, TrackId, DEFAULT_MAX_EGRESS_BPS};
use crate::token::MediaScope;

/// 房间内角色。
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Member,
    Moderator,
    Host,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Moderator => "moderator",
            Self::Host => "host",
        }
    }

    fn can_moderate(&self) -> bool {
        matches!(self, Self::Host | Self::Moderator)
    }
}

/// 静音维度：音频与视频独立（只关摄像头仍可说话）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Track {
    Audio,
    Video,
}

/// 成员在房间里的状态。这是**服务端事实**：客户端只能报告自己看到的，
/// 最终以这里的值为准（合同 §3.7 的"名册由服务端裁决"）。
///
/// ```text
///  ──join──► Joining ──mark_active──► Active ──disconnect──► Reconnecting
///              │                       │                        │
///              │                       │                        ├─reconnect─► Joining
///              └──────leave/timeout────┴───────leave────────────┘──► 席位清空 ⇒ Left
///                                              kick/封禁 ⇒ Removed（ban 名单）
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberState {
    /// 席位已占住，凭证已签发或待签发，sidecar 还没回报连上。
    #[default]
    Joining,
    /// 已连上，允许收发（静音只影响上行，见 `Participant::audio_muted`）。
    Active,
    /// 掉线宽限期内：席位与名册保留，但旧令牌已不可信，需要重新入房。
    Reconnecting,
    /// 已离席（可再次加入，不进 ban 名单）。
    Left,
    /// 被踢或成员资格被撤销：房间存活期间不可再进。
    Removed,
}

impl MemberState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Joining => "joining",
            Self::Active => "active",
            Self::Reconnecting => "reconnecting",
            Self::Left => "left",
            Self::Removed => "removed",
        }
    }

    /// 该状态是否可以持有媒体席位（sidecar 连接是否算数）。
    pub fn holds_media(self) -> bool {
        matches!(self, Self::Joining | Self::Active | Self::Reconnecting)
    }

    /// 是否可以再签一枚入房凭证。`Removed` 永远不行。
    pub fn may_receive_token(self) -> bool {
        matches!(self, Self::Joining | Self::Active | Self::Reconnecting)
    }
}

/// 一个设备席位的状态。设备是入房凭证的绑定对象（user + device 才唯一）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeviceSeat {
    pub state: MemberState,
    /// 最近一次状态变更的房间水印（`Room::revision`），用于拒绝重放的旧快照。
    pub revision: u64,
    /// `Reconnecting` 的宽限截止时间（毫秒）；其它状态为 0。
    pub grace_until_ms: u64,
    /// sidecar（LiveKit）自己那套参与者状态。**只前进不后退**，规则见
    /// [`SidecarParticipantState::advance`]。与 [`DeviceSeat::state`] 是两回事：
    /// 前者是"sidecar 报告到哪一步"，后者是"我们的房间决定到哪一步"。
    #[serde(default)]
    pub sidecar: SidecarParticipantState,
}

impl Default for DeviceSeat {
    fn default() -> Self {
        Self {
            state: MemberState::Joining,
            revision: 0,
            grace_until_ms: 0,
            sidecar: SidecarParticipantState::default(),
        }
    }
}

// ==================== ADR-25：sidecar 侧状态机（借自 LiveKit） ====================
//
// 本节的四个类型是对 LiveKit 官方实现的**移植**，不是"参考思路"：
//
// * [`SidecarParticipantState`] ← `livekit/rust-sdks` 的
//   `livekit/src/room/participant/mod.rs:39-45`（枚举成员）与
//   `livekit-protocol/src/livekit.rs` 的 `participant_info::State`
//   （数值 tag 与 `as_str_name`/`from_str_name`）；
// * [`SidecarConnectionState`] ← 同库 `livekit/src/room/mod.rs:290-295`；
// * [`DisconnectReason`] ← `livekit-protocol/src/livekit.rs` 的 `DisconnectReason`；
// * [`RoomCloseReason`] 与其到 `DisconnectReason` 的映射 ← 服务端
//   `livekit/livekit` 的 `pkg/rtc/types/interfaces.go:311-375`；
// * 状态"只前进"的 CAS 规则 ← 服务端 `pkg/rtc/participant.go:2333-2358`
//   （`updateState`）；房间关闭/空房判定 ← `pkg/rtc/room.go:754-822`
//   （`IsClosed`/`CloseIfEmpty`/`Close`）；依附参与者 ← `pkg/rtc/participant.go:615-622`。
//
// 逐段差异、改动与"没拿什么"见
// `third_party/reviews/patches/UP-LIVEKIT-RUSTSDK-room-participant.md`。

/// 上游 `livekit.ParticipantInfo_State`：sidecar 眼里这个连接走到哪一步了。
///
/// 顺序即 proto 的数值顺序（`JOINING=0 → JOINED=1 → ACTIVE=2 → DISCONNECTED=3`）。
/// 上游 `ParticipantImpl.updateState`（`pkg/rtc/participant.go:2333`）依赖的正是
/// 这个数值序：`state <= oldState` 一律当"乱序"忽略。所以 `PartialOrd` 不是
/// 顺手加的装饰，而是那条规则的载体——**不要**调整成员顺序。
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SidecarParticipantState {
    /// websocket 已连，但 sidecar 还没收到 offer（proto `JOINING = 0`）。
    #[default]
    Joining,
    /// sidecar 收到了 client offer（proto `JOINED = 1`）。
    Joined,
    /// ICE 连通性建立（proto `ACTIVE = 2`）。
    Active,
    /// WS 断开（proto `DISCONNECTED = 3`）。
    Disconnected,
}

impl SidecarParticipantState {
    /// proto 的枚举数值。与 sidecar/遥测对表时用这个，不要用我们内部的排序。
    pub fn tag(self) -> i32 {
        match self {
            Self::Joining => 0,
            Self::Joined => 1,
            Self::Active => 2,
            Self::Disconnected => 3,
        }
    }

    /// 上游 proto 的 `State::as_str_name()`（`livekit.rs` participant_info）。
    pub fn as_str_name(self) -> &'static str {
        match self {
            Self::Joining => "JOINING",
            Self::Joined => "JOINED",
            Self::Active => "ACTIVE",
            Self::Disconnected => "DISCONNECTED",
        }
    }

    /// 上游 proto 的 `State::from_str_name()`：认不出来即 `None`，不猜。
    pub fn from_str_name(raw: &str) -> Option<Self> {
        match raw {
            "JOINING" => Some(Self::Joining),
            "JOINED" => Some(Self::Joined),
            "ACTIVE" => Some(Self::Active),
            "DISCONNECTED" => Some(Self::Disconnected),
            _ => None,
        }
    }

    /// 上游 `updateState` 的核心一条：`if state <= oldState { 忽略 }`。
    ///
    /// 返回 `Some(old)` 表示这次报告应当生效，`None` 表示是乱序/重复报告，
    /// 调用方必须**原样保留旧状态**（上游连日志都只打 debug，不改状态）。
    /// 注意 `DISCONNECTED` 之后任何报告都会被判为乱序——这与上游一致，
    /// 一个已经结束的会话不会被迟到信令复活。
    pub fn advance(current: Self, next: Self) -> Option<Self> {
        if next <= current {
            None
        } else {
            Some(current)
        }
    }

    /// 上游 `ParticipantImpl.IsReady()`（`pkg/rtc/participant.go:632-641`）的
    /// 非迁移分支："JOINED 或 ACTIVE 才算ready"。上游注释写明了这条迁移路径
    /// （`JOINING -> ACTIVE -> DISCONNECTED`，没有 JoinResponse）不在此函数覆盖内，
    /// 我们不做连接迁移，所以只取非迁移分支。
    pub fn is_ready(self) -> bool {
        matches!(self, Self::Joined | Self::Active)
    }

    pub fn is_disconnected(self) -> bool {
        self == Self::Disconnected
    }

    /// `DISCONNECTED` 且上一站是 `ACTIVE` —— 上游据此才记录会话时长
    /// （`participant.go:2354-2357`），从 `JOINING` 直接掉线的不算一次完整会话。
    pub fn is_clean_disconnect_from(self, old: Self) -> bool {
        self == Self::Disconnected && old == Self::Active
    }

    /// sidecar 状态 → 我们的成员状态。
    ///
    /// `Disconnected` 落到 [`MemberState::Reconnecting`]（带宽限期），**不是**
    /// 上游的"立即移除参与者"（`pkg/rtc/room.go:1279-1280` 直接
    /// `RemoveParticipant`）。这是 ADR-25 下刻意的偏离：客户端网络抖动不该把
    /// 名册打散、把 `generation` 前滚一轮；宽限期结束后由 [`Room::tick`] 回收。
    pub fn to_member_state(self) -> MemberState {
        match self {
            Self::Joining | Self::Joined => MemberState::Joining,
            Self::Active => MemberState::Active,
            Self::Disconnected => MemberState::Reconnecting,
        }
    }

    /// 反向：我们的成员状态映射回 sidecar 语义（`Removed` 无对应值，
    /// 上游没有"被踢"这个参与者状态，只有 `DISCONNECTED` + 关闭原因）。
    pub fn from_member_state(state: MemberState) -> Self {
        match state {
            MemberState::Joining => Self::Joining,
            MemberState::Active => Self::Active,
            MemberState::Reconnecting | MemberState::Removed => Self::Disconnected,
            MemberState::Left => Self::Disconnected,
        }
    }
}

/// 上游 `livekit.ConnectionState`（`livekit/src/room/mod.rs:290-295`）：
/// **客户端**侧的连接状态，只有三值。注意它没有 `Disconnected` 之外的粒度，
/// 房间状态与参与者状态是三套不同的表，不能混用。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidecarConnectionState {
    #[default]
    Disconnected,
    Connected,
    Reconnecting,
}

/// 上游 `Room::update_connection_state`（`livekit/src/room/mod.rs:1224-1236`）
/// 的两条规则，原样落地：
///
/// 1. 状态没变 → 返回 `false`，**不发事件**（上游靠这个把重复的
///    `Reconnected` 折成一条）；
/// 2. 一旦进入 `Disconnected` 即**终态锁存**：后续任何状态变化都不再上报
///    （上游 `if ... || self.disconnected.load() { return false }`）。
///    断线后还能"复活"等于两份房间事实并存。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConnectionStateTracker {
    state: SidecarConnectionState,
    disconnected: bool,
}

impl ConnectionStateTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> SidecarConnectionState {
        self.state
    }

    /// `true` = 状态确实变了（调用方据此派发 `ConnectionStateChanged`）。
    pub fn update(&mut self, next: SidecarConnectionState) -> bool {
        if self.state == next || self.disconnected {
            return false;
        }
        if next == SidecarConnectionState::Disconnected {
            self.disconnected = true;
        }
        self.state = next;
        true
    }

    pub fn is_disconnected(&self) -> bool {
        self.disconnected
    }
}

/// 上游 proto 的 `DisconnectReason`（`livekit-protocol/src/livekit.rs`，
/// tag 0..=16，注释即上游原文的中文摘要）。这是 sidecar 给出的**关闭原因**，
/// 我们把它原样记进房间审计：`Removed`/`Left` 这类内部状态说不清"为什么"，
/// 而排障要的正是这个。
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum DisconnectReason {
    #[default]
    UnknownReason,
    /// 客户端主动断开
    ClientInitiated,
    /// 同名 identity 的另一个参与者进来了（顶号）
    DuplicateIdentity,
    /// 服务实例正在下线
    ServerShutdown,
    /// RoomService.RemoveParticipant 被调用（即我们的 kick）
    ParticipantRemoved,
    /// RoomService.DeleteRoom 被调用
    RoomDeleted,
    /// 客户端想恢复一个服务端并不认识的会话
    StateMismatch,
    /// 客户端没能完成入房
    JoinFailure,
    /// 服务端要求把连接迁到别处（我们不迁移）
    Migration,
    /// 信令 WS 意外关闭
    SignalClose,
    /// 所有 Standard/Ingress 参与者都走了，房间被关闭
    RoomClosed,
    /// SIP 被叫没应答
    UserUnavailable,
    /// SIP 被叫拒接（忙线）
    UserRejected,
    /// SIP 信令失败或响应异常
    SipTrunkFailure,
    /// 服务端判定该会话超时
    ConnectionTimeout,
    /// 媒体流失败或媒体超时
    MediaFailure,
    /// agent 侧出错
    AgentError,
}

impl DisconnectReason {
    /// proto 数值（与 sidecar 对齐用）。
    pub fn tag(self) -> i32 {
        match self {
            Self::UnknownReason => 0,
            Self::ClientInitiated => 1,
            Self::DuplicateIdentity => 2,
            Self::ServerShutdown => 3,
            Self::ParticipantRemoved => 4,
            Self::RoomDeleted => 5,
            Self::StateMismatch => 6,
            Self::JoinFailure => 7,
            Self::Migration => 8,
            Self::SignalClose => 9,
            Self::RoomClosed => 10,
            Self::UserUnavailable => 11,
            Self::UserRejected => 12,
            Self::SipTrunkFailure => 13,
            Self::ConnectionTimeout => 14,
            Self::MediaFailure => 15,
            Self::AgentError => 16,
        }
    }

    /// 上游 proto 的 `as_str_name()`：稳定的 SCREAMING_SNAKE 字符串，
    /// 直接进审计表的 `code`/`detail` 列。
    pub fn as_str_name(self) -> &'static str {
        match self {
            Self::UnknownReason => "UNKNOWN_REASON",
            Self::ClientInitiated => "CLIENT_INITIATED",
            Self::DuplicateIdentity => "DUPLICATE_IDENTITY",
            Self::ServerShutdown => "SERVER_SHUTDOWN",
            Self::ParticipantRemoved => "PARTICIPANT_REMOVED",
            Self::RoomDeleted => "ROOM_DELETED",
            Self::StateMismatch => "STATE_MISMATCH",
            Self::JoinFailure => "JOIN_FAILURE",
            Self::Migration => "MIGRATION",
            Self::SignalClose => "SIGNAL_CLOSE",
            Self::RoomClosed => "ROOM_CLOSED",
            Self::UserUnavailable => "USER_UNAVAILABLE",
            Self::UserRejected => "USER_REJECTED",
            Self::SipTrunkFailure => "SIP_TRUNK_FAILURE",
            Self::ConnectionTimeout => "CONNECTION_TIMEOUT",
            Self::MediaFailure => "MEDIA_FAILURE",
            Self::AgentError => "AGENT_ERROR",
        }
    }

    /// 上游 proto 的 `from_str_name()`。未知值 → `None`（审计宁缺不猜）。
    pub fn from_str_name(raw: &str) -> Option<Self> {
        Some(match raw {
            "UNKNOWN_REASON" => Self::UnknownReason,
            "CLIENT_INITIATED" => Self::ClientInitiated,
            "DUPLICATE_IDENTITY" => Self::DuplicateIdentity,
            "SERVER_SHUTDOWN" => Self::ServerShutdown,
            "PARTICIPANT_REMOVED" => Self::ParticipantRemoved,
            "ROOM_DELETED" => Self::RoomDeleted,
            "STATE_MISMATCH" => Self::StateMismatch,
            "JOIN_FAILURE" => Self::JoinFailure,
            "MIGRATION" => Self::Migration,
            "SIGNAL_CLOSE" => Self::SignalClose,
            "ROOM_CLOSED" => Self::RoomClosed,
            "USER_UNAVAILABLE" => Self::UserUnavailable,
            "USER_REJECTED" => Self::UserRejected,
            "SIP_TRUNK_FAILURE" => Self::SipTrunkFailure,
            "CONNECTION_TIMEOUT" => Self::ConnectionTimeout,
            "MEDIA_FAILURE" => Self::MediaFailure,
            "AGENT_ERROR" => Self::AgentError,
            _ => return None,
        })
    }
}

/// 上游 `types.RoomCloseReason`（服务端 `pkg/rtc/types/interfaces.go:311-320`）。
/// 六个值、`String()` 的名字、以及"房间关闭原因 ⇒ 参与者关闭原因"那张表
/// （同文件 360-375 行，注释写明目的是"两边永远不能对不上"）都照抄。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RoomCloseReason {
    #[default]
    Unknown,
    /// RoomService.DeleteRoom
    ApiDelete,
    /// 空房超时（`empty_timeout` / `departure_timeout` 到点）
    IdleTimeout,
    /// 服务实例下线
    ServerShutdown,
    /// 被新的同名房间顶掉
    Superseded,
    /// 房间没能开起来
    OpenFailed,
}

impl RoomCloseReason {
    /// 上游 `RoomCloseReason.String()`。
    pub fn as_str_name(self) -> &'static str {
        match self {
            Self::Unknown => "UNKNOWN",
            Self::ApiDelete => "API_DELETE",
            Self::IdleTimeout => "IDLE_TIMEOUT",
            Self::ServerShutdown => "SERVER_SHUTDOWN",
            Self::Superseded => "SUPERSEDED",
            Self::OpenFailed => "OPEN_FAILED",
        }
    }

    pub fn from_str_name(raw: &str) -> Option<Self> {
        Some(match raw {
            "UNKNOWN" => Self::Unknown,
            "API_DELETE" => Self::ApiDelete,
            "IDLE_TIMEOUT" => Self::IdleTimeout,
            "SERVER_SHUTDOWN" => Self::ServerShutdown,
            "SUPERSEDED" => Self::Superseded,
            "OPEN_FAILED" => Self::OpenFailed,
            _ => return None,
        })
    }

    /// 上游 `RoomCloseReason.ToParticipantCloseReason()` 的等价表，落到
    /// Rust SDK 那一侧实际使用的 [`DisconnectReason`]（`Room::close(reason)`）：
    ///
    /// * `API_DELETE` → `ROOM_DELETED`（上游 `ServiceRequestDeleteRoom`）
    /// * `IDLE_TIMEOUT` / `SUPERSEDED` → `ROOM_CLOSED`（上游 `RoomClosed`）
    /// * `SERVER_SHUTDOWN` → `SERVER_SHUTDOWN`（上游 `RoomManagerStop`）
    /// * 其余 → `UNKNOWN_REASON`（上游 `ParticipantCloseReasonNone`）
    ///
    /// 这张表存在的意义：客户端只会看到一个原因，房间审计里是另一个，
    /// 两者必须是同一次决定的两个投影，不能各编各的。
    pub fn to_disconnect_reason(self) -> DisconnectReason {
        match self {
            Self::ApiDelete => DisconnectReason::RoomDeleted,
            Self::IdleTimeout | Self::Superseded => DisconnectReason::RoomClosed,
            Self::ServerShutdown => DisconnectReason::ServerShutdown,
            Self::Unknown | Self::OpenFailed => DisconnectReason::UnknownReason,
        }
    }
}

/// 上游 `Room.onStateChange`（服务端 `pkg/rtc/room.go:1227-1281`）在状态变更时
/// 做的两件副作用，移植成显式的结论而不是隐藏行为：
///
/// * `ACTIVE` → 订阅既有轨道 + 上报 `telemetry.ParticipantActive`；
/// * `DISCONNECTED` → `RemoveParticipant`。
///
/// 我们的 `DISCONNECTED` 分支改成宽限期回收（见
/// [`SidecarParticipantState::to_member_state`]），所以这里只保留"要做什么"
/// 的清单，具体动作由 [`Room::report_sidecar_state`] 的调用方执行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidecarStateEffect {
    /// 该参与者现在活了：把房间里已有的轨道订阅给它（上游 `subscribeToExistingTracks`）。
    SubscribeExistingTracks,
    /// 上报入房成功指标（上游 `telemetry.ParticipantActive`）。我们落到房间审计。
    ReportActive,
    /// 掉线：席位进入宽限期。
    BeginReconnectGrace,
}

/// 一次 sidecar 状态回报的结论（[`Room::report_sidecar_state`] 的返回）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidecarStateReport {
    /// `false` = 这条回报是乱序/重复，**没有**改动任何状态（上游同法）。
    pub applied: bool,
    pub previous: SidecarParticipantState,
    pub current: SidecarParticipantState,
    /// 房间侧要跟着做的事，顺序即上游 `Room.onStateChange` 里的先后。
    pub effects: Vec<SidecarStateEffect>,
}

/// sidecar 房间事件（上游 `livekit::RoomEvent` 里与"名册"有关的那一小撮，
/// `livekit/src/room/mod.rs:107-...`）。
///
/// 只移植名册相关分支：轨道/数据通道/chat/e2ee 那些事件归 sidecar 自己，
/// ADR-25 下 Rust 侧不重复派发（否则同一件事会有两个订阅者、两份事实）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidecarRoomEvent {
    /// 新参与者进房（上游 `ParticipantConnected`）。
    ParticipantConnected(String),
    /// 参与者变为可用、可以收数据（上游 `ParticipantActive`：**边沿**触发）。
    ParticipantActive(String),
    /// 参与者离房（上游 `ParticipantDisconnected`）。
    ParticipantDisconnected(String, DisconnectReason),
    /// 一批名册更新结束（上游 `ParticipantsUpdated`：**每批只发一次**）。
    ParticipantsUpdated(Vec<String>),
    /// 房间连接状态变化（上游 `ConnectionStateChanged`）。
    ConnectionStateChanged(SidecarConnectionState),
}

/// 上游 `Room` 名册更新的事件序（`livekit/src/room/mod.rs:1292-1350`）。
///
/// 这四条顺序规则是**实测出来的**，不是我们发明的：
///
/// 1. `state == DISCONNECTED` 且这个 sid 我们根本不认识 → 什么都不发
///    （上游注释："Ignore, just received the ParticipantInfo but the
///    participant is already disconnected"）；认识的话**只**发
///    `ParticipantDisconnected` 就返回 —— 上游那一支没有把参与者 push 进
///    `participants`，所以批尾的 `ParticipantsUpdated` 也不发；
/// 2. 已认识：**只在"从非 Active 变 Active"的边沿**发 `ParticipantActive`，
///    重复的 ACTIVE 不再发（上游先记 `already_active` 再 `update_info`）；
/// 3. 不认识：**先** `ParticipantConnected`，**再**（若已经是 ACTIVE）**补一条**
///    `ParticipantActive`，然后才 `update_info` 补轨道；
/// 4. 整批处理完才发**一条** `ParticipantsUpdated`，不是每人一条。
pub fn plan_participant_events(
    known: bool,
    already_active: bool,
    reported: SidecarParticipantState,
    identity: &str,
) -> Vec<SidecarRoomEvent> {
    let mut out = Vec::new();
    let active = reported == SidecarParticipantState::Active;
    if reported == SidecarParticipantState::Disconnected {
        if known {
            // 上游顺序：先 `update_info`（拿到正确的断开原因），再派发断开。
            out.push(SidecarRoomEvent::ParticipantDisconnected(
                identity.to_string(),
                DisconnectReason::UnknownReason,
            ));
        }
        return out;
    }
    if known {
        if !already_active && active {
            out.push(SidecarRoomEvent::ParticipantActive(identity.to_string()));
        }
    } else {
        out.push(SidecarRoomEvent::ParticipantConnected(identity.to_string()));
        if active {
            // 上游：新参与者一进来就是 ACTIVE，Connected 之后紧跟一条 Active。
            out.push(SidecarRoomEvent::ParticipantActive(identity.to_string()));
        }
    }
    out.push(SidecarRoomEvent::ParticipantsUpdated(vec![
        identity.to_string()
    ]));
    out
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Participant {
    pub user: String,
    pub role: Role,
    pub audio_muted: bool,
    pub video_muted: bool,
    pub hand_raised: bool,
    pub joined_at: u64,
    /// 设备席位表。旧的 str0m 入房路径（[`Room::join`]）不登记设备，
    /// 此时为空但 `state` 仍是 `Active`，保持既有路由行为不变。
    #[serde(default)]
    pub devices: std::collections::BTreeMap<String, DeviceSeat>,
    /// 由设备席位聚合出的用户级状态。
    #[serde(default)]
    pub state: MemberState,
}

impl Participant {
    fn new(user: &str, role: Role, now_ms: u64, state: MemberState) -> Self {
        Self {
            user: user.to_string(),
            role,
            audio_muted: false,
            video_muted: false,
            hand_raised: false,
            joined_at: now_ms,
            devices: Default::default(),
            state,
        }
    }

    /// 该用户在指定状态下是否还可以收发媒体（无设备席位时看用户级状态）。
    pub fn holds_media(&self, device: Option<&str>) -> bool {
        match device {
            None => self.state.holds_media(),
            Some(d) => self.devices.get(d).is_some_and(|s| s.state.holds_media()),
        }
    }

    /// 刷新用户级状态（由设备席位聚合）。
    fn refresh(&mut self) {
        self.state = aggregate(&self.devices, self.state);
    }
}

/// 席位聚合：按 Active > Joining > Reconnecting > Left/Removed 的优先级取，
/// 只要有任一设备还在房里，用户就算在房里。
fn aggregate(
    devices: &std::collections::BTreeMap<String, DeviceSeat>,
    fallback: MemberState,
) -> MemberState {
    if devices.is_empty() {
        return fallback;
    }
    for want in [
        MemberState::Active,
        MemberState::Joining,
        MemberState::Reconnecting,
    ] {
        if devices.values().any(|s| s.state == want) {
            return want;
        }
    }
    if devices.values().all(|s| s.state == MemberState::Removed) {
        MemberState::Removed
    } else {
        MemberState::Left
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RoomError {
    #[error("room is full ({0} seats)")]
    Full(usize),
    #[error("room is locked")]
    Locked,
    #[error("not a participant")]
    NotMember,
    #[error("already in the room")]
    AlreadyJoined,
    #[error("removed from this room earlier")]
    Banned,
    #[error("insufficient role")]
    Forbidden,
    #[error("no such participant")]
    NoSuchParticipant,
    #[error("cannot kick the host")]
    CannotKickHost,
    #[error("capacity must be between 2 and 1000")]
    BadCapacity,
}

pub type Result<T> = std::result::Result<T, RoomError>;

/// 离开房间的结果：主持人离开会移交，最后一人离开则房间结束。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaveOutcome {
    pub new_host: Option<String>,
    pub ended: bool,
}

/// ADR-25 的媒体引擎。`LiveKit` 是线上引擎（sidecar 独立进程）；
/// `Str0m` 是**冷备**——代码留在仓库里，但不许拥有正在服务的房间。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaEngine {
    /// 线上：媒体转发与 sidecar 侧房间状态归 LiveKit。
    #[default]
    LiveKit,
    /// 冷备：本仓库的 str0m/SFU 路径，只用于对照与回滚演练。
    Str0m,
}

impl MediaEngine {
    pub fn name(self) -> &'static str {
        match self {
            Self::LiveKit => "livekit",
            Self::Str0m => "str0m",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "livekit" => Some(Self::LiveKit),
            "str0m" => Some(Self::Str0m),
            _ => None,
        }
    }

    /// ADR-25 的默认线上引擎。
    pub fn production() -> Self {
        Self::LiveKit
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EngineError {
    #[error("engine `{0}` is cold standby under ADR-25; it may not own a live room")]
    ColdStandby(&'static str),
    #[error("only one engine may be online; `{0}` already owns this process")]
    AlreadyOnline(&'static str),
    #[error("unknown rtc engine `{0}`")]
    Unknown(String),
}

/// 单一在线引擎闸（ADR-25：「同一时刻只允许一个引擎在线，冷备不产生第二份
/// 房间事实」）。房间创建必须过 [`EngineGate::admit`]。
///
/// 用 `std::sync::Mutex` 而非 async 锁：这里只保护一个枚举值，且不得在
/// async handler 里持有跨 await 的锁（ADR-28）。
#[derive(Debug, Default)]
pub struct EngineGate {
    active: std::sync::Mutex<MediaEngine>,
}

impl EngineGate {
    /// 线上部署：LiveKit 在线，str0m 冷备。
    pub fn production() -> Self {
        Self {
            active: std::sync::Mutex::new(MediaEngine::LiveKit),
        }
    }

    /// 回滚演练：把冷备引擎升为在线引擎（需要显式运维动作），此时
    /// 不允许再创建 LiveKit 房间，避免两套房间事实并存。
    pub fn cold_drill() -> Self {
        Self {
            active: std::sync::Mutex::new(MediaEngine::Str0m),
        }
    }

    pub fn active(&self) -> MediaEngine {
        *self.active.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 该引擎能否在本进程里拥有房间。只有一个答案可以是"能"。
    pub fn admit(&self, want: MediaEngine) -> std::result::Result<MediaEngine, EngineError> {
        let active = self.active();
        if want == active {
            return Ok(want);
        }
        if want == MediaEngine::Str0m {
            return Err(EngineError::ColdStandby(want.name()));
        }
        Err(EngineError::AlreadyOnline(active.name()))
    }
}

/// 成员状态机的结论码。审计与错误信息共用同一套稳定字符串，
/// 前端与运维不必再猜"为什么被拒"。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberErrorCode {
    StaleRevision,
    NotMember,
    NoSuchDevice,
    Full,
    Locked,
    Banned,
    Forbidden,
    IllegalState,
    WrongEngine,
    BadInput,
    NotFound,
    /// 策略关闭（ADR-25 的服务端 Egress 录制）。
    Disabled,
    /// 房间已经关闭（上游 `Room.IsClosed()`）。关闭是终态，不再接受任何命令。
    RoomClosed,
    /// sidecar 报告乱序：`state <= current`，上游是**忽略**而不是回滚。
    OutOfOrderSidecarState,
}

impl MemberErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StaleRevision => "stale_revision",
            Self::NotMember => "not_member",
            Self::NoSuchDevice => "no_such_device",
            Self::Full => "room_full",
            Self::Locked => "room_locked",
            Self::Banned => "banned",
            Self::Forbidden => "forbidden",
            Self::IllegalState => "illegal_member_state",
            Self::WrongEngine => "engine_mismatch",
            Self::BadInput => "bad_input",
            Self::NotFound => "room_not_found",
            Self::Disabled => "policy_disabled",
            Self::RoomClosed => "room_closed",
            Self::OutOfOrderSidecarState => "out_of_order_sidecar_state",
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MemberError {
    #[error("room revision moved on: command saw {seen}, current is {current}")]
    StaleRevision { seen: u64, current: u64 },
    #[error("not a participant of this room")]
    NotMember,
    #[error("no such device seat: {0}")]
    NoSuchDevice(String),
    #[error("room is full ({0} seats)")]
    Full(usize),
    #[error("room is locked")]
    Locked,
    #[error("removed from this room earlier")]
    Banned,
    #[error("insufficient role")]
    Forbidden,
    #[error("cannot {action} while device seat is {state:?}")]
    IllegalState {
        action: &'static str,
        state: MemberState,
    },
    #[error("room engine is `{0}`, command needs `{1}`")]
    WrongEngine(&'static str, &'static str),
    #[error("invalid member command: {0}")]
    BadInput(&'static str),
    #[error("room is closed ({0:?})")]
    RoomClosed(RoomCloseReason),
    #[error("out-of-order sidecar state report: `{current:?}` already seen, ignoring `{next:?}`")]
    OutOfOrderSidecarState {
        current: SidecarParticipantState,
        next: SidecarParticipantState,
    },
}

impl MemberError {
    /// 稳定的结论码（进审计、给客户端；不要把内部错误文案当契约）。
    pub fn code(&self) -> MemberErrorCode {
        match self {
            Self::StaleRevision { .. } => MemberErrorCode::StaleRevision,
            Self::NotMember => MemberErrorCode::NotMember,
            Self::NoSuchDevice(_) => MemberErrorCode::NoSuchDevice,
            Self::Full(_) => MemberErrorCode::Full,
            Self::Locked => MemberErrorCode::Locked,
            Self::Banned => MemberErrorCode::Banned,
            Self::Forbidden => MemberErrorCode::Forbidden,
            Self::IllegalState { .. } => MemberErrorCode::IllegalState,
            Self::WrongEngine(..) => MemberErrorCode::WrongEngine,
            Self::BadInput(_) => MemberErrorCode::BadInput,
            Self::RoomClosed(_) => MemberErrorCode::RoomClosed,
            Self::OutOfOrderSidecarState { .. } => MemberErrorCode::OutOfOrderSidecarState,
        }
    }
}

/// 成员状态机的动作。全部由服务端裁决；客户端只上报"我想做"。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemberAction {
    /// 占一个设备席位（成员角色）。锁门时拒绝。
    Join { device: String },
    /// 以管理员/主持人身份入房（锁门仍可进，便于处置）。
    JoinAsModerator { device: String, role: Role },
    /// sidecar 回报该设备已连上：`Joining`/`Reconnecting` ⇒ `Active`。
    MarkActive { device: String },
    /// 连接掉了：`Active` ⇒ `Reconnecting`（席位保留到宽限期结束）。
    Disconnect { device: String },
    /// 重连回来了：重新签发凭证前的状态检查。
    Reconnect { device: String },
    /// 主动离席：席位清空即 `Left`，可以再加入。
    Leave { device: String },
    /// 静音/解除自己的一条轨。
    MuteSelf { track: Track, muted: bool },
    /// 主持人/管理员静音他人（服务端静音：下一次凭证签发即失去上行）。
    MuteOther {
        target: String,
        track: Track,
        muted: bool,
    },
    /// 举手/放下。
    Hand { raised: bool },
}

impl MemberAction {
    fn name(&self) -> &'static str {
        match self {
            Self::Join { .. } => "join",
            Self::JoinAsModerator { .. } => "join_moderator",
            Self::MarkActive { .. } => "mark_active",
            Self::Disconnect { .. } => "disconnect",
            Self::Reconnect { .. } => "reconnect",
            Self::Leave { .. } => "leave",
            Self::MuteSelf { .. } => "mute_self",
            Self::MuteOther { .. } => "mute_other",
            Self::Hand { .. } => "hand",
        }
    }

    /// 命令涉及的远端用户（只用于审计字段）。
    pub(crate) fn target_user(&self) -> Option<&str> {
        match self {
            Self::MuteOther { target, .. } => Some(target),
            _ => None,
        }
    }

    /// 命令涉及的设备席位（只用于审计字段）。
    pub(crate) fn device(&self) -> Option<&str> {
        match self {
            Self::Join { device }
            | Self::JoinAsModerator { device, .. }
            | Self::MarkActive { device }
            | Self::Disconnect { device }
            | Self::Reconnect { device }
            | Self::Leave { device } => Some(device),
            Self::MuteSelf { .. } | Self::MuteOther { .. } | Self::Hand { .. } => None,
        }
    }

    /// 是否会改变名册（改变即推进 `generation`，旧凭证整代失效）。
    ///
    /// 裁决点分散在各动作分支里（`cmd_join` / `Leave` / `Reconnect` 各自
    /// `generation += 1`），这里只把"哪些动作算改名册"这条规则留在原地，
    /// 供审计与切路由时对照——真正的判据是代次有没有前滚。
    #[allow(dead_code)]
    fn changes_membership(&self) -> bool {
        matches!(
            self,
            Self::Join { .. }
                | Self::JoinAsModerator { .. }
                | Self::Leave { .. }
                | Self::Reconnect { .. }
        )
    }
}

/// 一条成员命令。`expected_revision` 是 CAS 水印：客户端把自己看到的
/// 房间快照版本带上来，服务端只接受"没有落后"的命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberCommand {
    pub actor: String,
    pub action: MemberAction,
    /// 客户端看到的房间水印；`None` 表示不做 CAS（服务端内部动作）。
    pub expected_revision: Option<u64>,
    pub at_ms: u64,
}

/// 命令被接受后的结论。客户端据此更新本地快照，服务端据此广播。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberOutcome {
    pub user: String,
    pub device: Option<String>,
    pub state: MemberState,
    /// 命令生效后的房间水印。
    pub revision: u64,
    /// 命令生效后的成员代次（凭证与 E2EE epoch 绑定它）。
    pub generation: u64,
    pub room_ended: bool,
    pub new_host: Option<String>,
    /// 因为对象离席而被丢弃的积压信令条数。
    pub dropped_signals: usize,
    /// 是否需要重新签发入房凭证（席位变化/静音变化都为真）。
    pub needs_new_token: bool,
}

/// 一次出口分配的结果（每个观众一份）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct EgressPlan {
    /// 各订阅轨的目标码率（顺序 = 订阅顺序）。
    pub tracks: Vec<(TrackId, u64)>,
    /// (current, desired) —— 交给 BWE/pacer 的目标区间。
    pub bwe: (u64, u64),
}

/// 每个收件人的信令积压上限（ICE trickle 高峰够用，又不至于 OOM）。
pub const SIGNAL_QUEUE_CAP: usize = 64;

/// 一次入房凭证的默认寿命（毫秒）。到期必须回信令面重新入房。
pub const ACCESS_TTL_MS: u64 = 60_000;
/// 掉线宽限期：超过即释放席位（与直播侧 `RECONNECT_GRACE_MS` 同值）。
pub const RECONNECT_GRACE_MS: u64 = 60_000;
/// 待落库审计的排队上限。通话房间是短生命周期对象，超量说明有人在刷命令；
/// 溢出只丢最旧的、并计数上报，绝不静默丢弃整个房间。
pub const AUDIT_QUEUE_CAP: usize = 512;

/// 上游 proto `Room.empty_timeout` 的默认值（秒），取自服务端
/// `config-sample.yaml:246`（`#   empty_timeout: 300`）。
pub const DEFAULT_EMPTY_TIMEOUT_SECS: u64 = 300;
/// 上游 proto `Room.departure_timeout` 的默认值（秒），取自同文件 248 行
/// （`#   departure_timeout: 20`）。给"人刚走"留出重连窗口，比空房超时短得多。
pub const DEFAULT_DEPARTURE_TIMEOUT_SECS: u64 = 20;

/// 房间审计记下的动作类别。允许与拒绝都走同一张表，运维才看得出
/// "谁在试着干什么但没成功"。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomAuditAction {
    RoomCreated,
    EnginePinned,
    MemberChanged,
    MembershipRevoked,
    SeatExpired,
    TokenIssued,
    SignalPosted,
    SignalDropped,
    Mute,
    MuteAll,
    Kicked,
    Lock,
    RecordingFlag,
    /// 服务端 Egress 录制请求（ADR-25 关闭，永远是一条拒绝记录）。
    ServerEgress,
    HostTransferred,
    RoleChanged,
    Hand,
    RoomEnded,
    /// 引擎与信令类型不符（LiveKit 房间收到 str0m 的 ICE trickle）。
    EngineSignalMismatch,
}

impl RoomAuditAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RoomCreated => "room_created",
            Self::EnginePinned => "engine_pinned",
            Self::MemberChanged => "member_changed",
            Self::MembershipRevoked => "membership_revoked",
            Self::SeatExpired => "seat_expired",
            Self::TokenIssued => "token_issued",
            Self::SignalPosted => "signal_posted",
            Self::SignalDropped => "signal_dropped",
            Self::Mute => "mute",
            Self::MuteAll => "mute_all",
            Self::Kicked => "kicked",
            Self::Lock => "lock",
            Self::RecordingFlag => "recording_flag",
            Self::ServerEgress => "server_egress",
            Self::HostTransferred => "host_transferred",
            Self::RoleChanged => "role_changed",
            Self::Hand => "hand",
            Self::RoomEnded => "room_ended",
            Self::EngineSignalMismatch => "engine_signal_mismatch",
        }
    }

    /// 审计行从库里读回来时的反解析。未知值一律 `None`——审计表只由本模块
    /// 写入，读到没见过的动作说明有人在库里动手脚，不能猜。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "room_created" => Some(Self::RoomCreated),
            "engine_pinned" => Some(Self::EnginePinned),
            "member_changed" => Some(Self::MemberChanged),
            "membership_revoked" => Some(Self::MembershipRevoked),
            "seat_expired" => Some(Self::SeatExpired),
            "token_issued" => Some(Self::TokenIssued),
            "signal_posted" => Some(Self::SignalPosted),
            "signal_dropped" => Some(Self::SignalDropped),
            "mute" => Some(Self::Mute),
            "mute_all" => Some(Self::MuteAll),
            "kicked" => Some(Self::Kicked),
            "lock" => Some(Self::Lock),
            "recording_flag" => Some(Self::RecordingFlag),
            "server_egress" => Some(Self::ServerEgress),
            "host_transferred" => Some(Self::HostTransferred),
            "role_changed" => Some(Self::RoleChanged),
            "hand" => Some(Self::Hand),
            "room_ended" => Some(Self::RoomEnded),
            "engine_signal_mismatch" => Some(Self::EngineSignalMismatch),
            _ => None,
        }
    }
}

/// 一条房间审计。个人数据最小化：只有 id、时间、结论码与水印，
/// **不含** SDP、IP、设备指纹、密钥、媒体内容。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RoomAuditEntry {
    /// 房间内单调序号（与 `revision` 不同：审计包含被拒的命令）。
    pub seq: u64,
    pub at_ms: u64,
    pub room: String,
    /// sidecar 侧房间名（LiveKit 房间名）；冷备引擎下与 `room` 同值。
    pub sidecar_room: String,
    pub conv: String,
    pub actor: String,
    pub action: RoomAuditAction,
    pub target: Option<String>,
    pub device: Option<String>,
    pub revision: u64,
    pub generation: u64,
    /// `Some(..)` 表示这条动作被拒绝，值是稳定结论码。
    pub code: Option<String>,
    /// 上下文短码（`join` / `queue_overflow` / `adr25_disabled` …），
    /// 只有类别与 id，没有内容。
    pub detail: Option<String>,
    /// 引擎名（`livekit` / `str0m`）——"哪个引擎的房间事实"必须可查。
    pub engine: String,
}

impl RoomAuditEntry {
    pub fn denied(&self) -> bool {
        self.code.is_some()
    }
}

/// 录制来源。ADR-25 只允许 `Client`。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum RecordingMode {
    Off,
    /// 客户端侧录制：旗标由服务端持有并广播给全员，媒体数据不进服务端。
    Client {
        /// 上报"我这边正在录制"的参与者（主持人权限校验在
        /// [`Room::set_recording`]）。
        reported_by: String,
    },
}

impl Default for RecordingMode {
    fn default() -> Self {
        Self::Off
    }
}

/// 服务端 Egress 的拒绝结论（ADR-25：录制只允许客户端侧）。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EgressError {
    #[error(
        "ADR-25: server-side egress recording is disabled; only client-side recording is allowed"
    )]
    Disabled,
}

/// 房间创建的两类失败：引擎闸拒绝，或参数本身不合法。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CreateError {
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(transparent)]
    Room(#[from] RoomError),
}

#[derive(Clone, Debug)]
pub struct Room {
    id: String,
    /// 房间归属（群/频道 id），空表示临时房间。
    owner_conv: String,
    host: String,
    locked: bool,
    recording: RecordingMode,
    capacity: usize,
    participants: BTreeMap<String, Participant>,
    /// 被踢名单：房间存活期间不可再进。
    banned: HashSet<String>,
    /// 每个观众一个出口码率分配器（参照 atm0s 的 EgressBitrateAllocator）。
    allocators: HashMap<String, EgressBitrateAllocator>,
    /// 通话信令队列（offer/answer/ICE），只在房间存活期间存在。
    signals: crate::signal::SignalBox,
    created_at: u64,
    /// ADR-25：这个房间的媒体事实由哪个引擎持有。一个房间只认一个引擎。
    engine: MediaEngine,
    /// 名册/管控状态的水印。每次生效的变更 +1；被拒的变更不动它。
    revision: u64,
    /// 成员代次。进出房/撤销即 +1，绑定它的凭证与媒体密钥整代失效。
    generation: u64,
    /// sidecar 侧房间名（LiveKit room name）。
    sidecar_room: String,
    /// 掉线宽限期（毫秒），可由 [`Room::with_reconnect_grace`] 调整（测试用）。
    reconnect_grace_ms: u64,
    pending_audit: std::collections::VecDeque<RoomAuditEntry>,
    audit_seq: u64,
    audit_dropped: u64,
    /// 上游 `Room.closed`（`pkg/rtc/room.go:136`）那根 channel 的值语义版本：
    /// `Some(reason)` 即房间已关闭且**不可逆**（`Close` 是幂等的，见
    /// [`Room::close`]）。`None` 是开放中。
    closed: Option<RoomCloseReason>,
    /// 上游 `protoRoom.CreationTime` 对应的"空房计时起点"（毫秒）。
    first_joined_at_ms: u64,
    /// 上游 `Room.LastLeftAt()`（秒）的毫秒版：最后一次"独立成员全部离席"的时刻。
    /// `0` 表示从未有人离开过——上游正是用 `FirstJoinedAt()>0 && LastLeftAt()>0`
    /// 来区分"空房超时"和"离席超时"两种计时起点。
    last_left_at_ms: u64,
    /// 上游 proto `Room.empty_timeout`（秒）。默认 300，取自服务端
    /// `config-sample.yaml:246` 的 `empty_timeout: 300`。
    empty_timeout_secs: u64,
    /// 上游 proto `Room.departure_timeout`（秒）。默认 20，取自同文件
    /// `departure_timeout: 20`。
    departure_timeout_secs: u64,
    /// 依附参与者（上游 `ParticipantImpl.IsDependent()`，
    /// `pkg/rtc/participant.go:615-622`）：agent/egress 这类连接**不决定房间
    /// 生死**。上游是从 grants 现算，我们的 grants 只在签发那一刻过手，
    /// 所以在 [`Room::mark_dependent`] 落一次，由调用方用
    /// `AccessClaims::is_dependent` 的结论喂进来。
    dependent: HashSet<String>,
}

/// 由会话 id 与房间 id 推出稳定且可反查的 sidecar 房间名。
/// 名字里只有 id，没有标题/用户昵称，避免房间名泄露群名。
fn sidecar_room_name(engine: MediaEngine, conv: &str, id: &str) -> String {
    let scope = match engine {
        MediaEngine::LiveKit => "call",
        MediaEngine::Str0m => "cold",
    };
    if conv.is_empty() {
        format!("wxlike-{scope}-temp-{id}")
    } else {
        format!("wxlike-{scope}-{conv}-{id}")
    }
}

impl Room {
    /// 兼容入口：既有路由（`/api/call/*`）创建的仍是 **str0m 冷备**房间——
    /// 它的媒体面靠 QUIC 信令里的 SDP/ICE 协商，正是 ADR-25 里要退役的那条路。
    /// 新链路请用 [`Room::livekit`]；把路由切过去的需求写在
    /// `integration-requests/G09/153-*.yaml` 里交给 G00。
    pub fn new(
        id: &str,
        owner_conv: &str,
        host: &str,
        capacity: usize,
        now_ms: u64,
    ) -> Result<Self> {
        Self::with_engine(id, owner_conv, host, capacity, now_ms, MediaEngine::Str0m)
    }

    /// ADR-25 线上房间：媒体面归 LiveKit sidecar，本对象只持名册与授权事实。
    pub fn livekit(
        id: &str,
        owner_conv: &str,
        host: &str,
        capacity: usize,
        now_ms: u64,
    ) -> Result<Self> {
        Self::with_engine(id, owner_conv, host, capacity, now_ms, MediaEngine::LiveKit)
    }

    /// 过单一在线引擎闸创建房间（推荐给服务启动时装配一个 [`EngineGate`]）。
    pub fn through_gate(
        gate: &EngineGate,
        id: &str,
        owner_conv: &str,
        host: &str,
        capacity: usize,
        now_ms: u64,
        want: MediaEngine,
    ) -> std::result::Result<Self, CreateError> {
        Ok(Self::with_engine(
            id,
            owner_conv,
            host,
            capacity,
            now_ms,
            gate.admit(want)?,
        )?)
    }

    pub fn with_engine(
        id: &str,
        owner_conv: &str,
        host: &str,
        capacity: usize,
        now_ms: u64,
        engine: MediaEngine,
    ) -> Result<Self> {
        if !(2..=1000).contains(&capacity) {
            return Err(RoomError::BadCapacity);
        }
        let mut participants = BTreeMap::new();
        participants.insert(
            host.to_string(),
            Participant::new(host, Role::Host, now_ms, MemberState::Active),
        );
        let mut allocators = HashMap::new();
        allocators.insert(
            host.to_string(),
            EgressBitrateAllocator::new(DEFAULT_MAX_EGRESS_BPS),
        );
        let sidecar_room = sidecar_room_name(engine, owner_conv, id);
        let mut room = Self {
            id: id.to_string(),
            owner_conv: owner_conv.to_string(),
            host: host.to_string(),
            locked: false,
            recording: RecordingMode::Off,
            capacity,
            participants,
            banned: HashSet::new(),
            allocators,
            signals: crate::signal::SignalBox::new(SIGNAL_QUEUE_CAP),
            created_at: now_ms,
            engine,
            // 房间创建本身即第一个状态版本。
            revision: 1,
            generation: 1,
            sidecar_room,
            reconnect_grace_ms: RECONNECT_GRACE_MS,
            pending_audit: std::collections::VecDeque::new(),
            audit_seq: 0,
            audit_dropped: 0,
            closed: None,
            // 建房人即第一个到场的人：`first_joined_at` 从这一刻算，
            // 与上游 `Room.FirstJoinedAt()` 的语义一致（0 = 还没人来过）。
            first_joined_at_ms: now_ms,
            last_left_at_ms: 0,
            empty_timeout_secs: DEFAULT_EMPTY_TIMEOUT_SECS,
            departure_timeout_secs: DEFAULT_DEPARTURE_TIMEOUT_SECS,
            dependent: HashSet::new(),
        };
        room.audit(RoomAuditAction::RoomCreated, None, host, None, None);
        room.audit(
            RoomAuditAction::EnginePinned,
            None,
            host,
            None,
            Some(engine.name()),
        );
        Ok(room)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn owner_conv(&self) -> &str {
        &self.owner_conv
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    pub fn is_recording(&self) -> bool {
        self.recording != RecordingMode::Off
    }

    /// 录制模式。ADR-25 下只可能是 `Off` 或 `Client`：服务端 Egress
    /// 不会被打开（见 [`Room::request_server_egress`]）。
    pub fn recording_mode(&self) -> &RecordingMode {
        &self.recording
    }

    /// 当前持有客户端录制旗标的参与者。
    pub fn recording_reported_by(&self) -> Option<&str> {
        match &self.recording {
            RecordingMode::Client { reported_by } => Some(reported_by),
            RecordingMode::Off => None,
        }
    }

    /// 这个房间的媒体事实由哪个引擎持有（ADR-25 单一在线闸的落点）。
    pub fn engine(&self) -> MediaEngine {
        self.engine
    }

    /// sidecar 房间名（发给 LiveKit 的那个）。
    pub fn sidecar_room(&self) -> &str {
        &self.sidecar_room
    }

    /// 名册/管控水印。客户端把它当作"我看到的房间快照版本"带回来做 CAS。
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// 成员代次：凭证与媒体密钥 epoch 绑定它，进出房即前滚。
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// 掉线宽限期（毫秒）。
    pub fn reconnect_grace_ms(&self) -> u64 {
        self.reconnect_grace_ms
    }

    /// 测试/部署调宽限期用（0 表示掉线即释放席位）。
    pub fn with_reconnect_grace(mut self, grace_ms: u64) -> Self {
        self.reconnect_grace_ms = grace_ms;
        self
    }

    /// 出口码率分配器只在本地 SFU（str0m）路径有意义：LiveKit 的带宽估计
    /// 由 sidecar 自己算，我们这边的 `allocate_egress` 结果只能当参考。
    pub fn egress_is_local(&self) -> bool {
        self.engine == MediaEngine::Str0m
    }

    pub fn created_at(&self) -> u64 {
        self.created_at
    }

    pub fn len(&self) -> usize {
        self.participants.len()
    }

    pub fn is_empty(&self) -> bool {
        self.participants.is_empty()
    }

    pub fn participant(&self, user: &str) -> Option<&Participant> {
        self.participants.get(user)
    }

    pub fn roster(&self) -> Vec<&Participant> {
        self.participants.values().collect()
    }

    /// 进房：满员拒绝、锁门拒绝（想跨过锁门请用
    /// [`MemberAction::JoinAsModerator`]）、被踢者拒绝。
    ///
    /// 这是既有的"一个用户一个席位"入口，仍走服务端裁决；带设备的入房
    /// 路径请看 [`Room::member_command`]。
    pub fn join(&mut self, user: &str, now_ms: u64) -> Result<&Participant> {
        self.join_as(user, Role::Member, now_ms)
    }

    /// 带角色的入房：`Moderator`/`Host` 可以跨过锁门（便于处置），
    /// `Member` 在锁门时被拒。角色只能由服务端指定，客户端自报无效。
    pub fn join_as(&mut self, user: &str, role: Role, now_ms: u64) -> Result<&Participant> {
        if self.participants.contains_key(user) {
            return Err(RoomError::AlreadyJoined);
        }
        if self.banned.contains(user) {
            return Err(RoomError::Banned);
        }
        if self.participants.len() >= self.capacity {
            return Err(RoomError::Full(self.capacity));
        }
        if self.locked && !role.can_moderate() {
            return Err(RoomError::Locked);
        }
        self.participants.insert(
            user.to_string(),
            Participant::new(user, role, now_ms, MemberState::Active),
        );
        // 进房即建出口分配器：订阅/结算都挂在它上面
        self.allocators.insert(
            user.to_string(),
            EgressBitrateAllocator::new(DEFAULT_MAX_EGRESS_BPS),
        );
        self.generation = self.generation.saturating_add(1);
        self.bump();
        self.audit(
            RoomAuditAction::MemberChanged,
            None,
            user,
            Some(user),
            Some("join"),
        );
        self.participants
            .get(user)
            .ok_or(RoomError::NoSuchParticipant)
    }

    /// 离房：主持人离开自动移交给最早加入的管理员/成员。
    /// 发给这个人的积压信令同时被丢弃——离席后的迟到信令不该再被投递。
    pub fn leave(&mut self, user: &str) -> Result<LeaveOutcome> {
        if self.participants.remove(user).is_none() {
            return Err(RoomError::NotMember);
        }
        self.allocators.remove(user);
        let dropped = self.signals.forget(user);
        self.generation = self.generation.saturating_add(1);
        self.bump();
        self.audit(
            RoomAuditAction::MemberChanged,
            None,
            user,
            Some(user),
            Some("leave"),
        );
        if dropped > 0 {
            self.audit(
                RoomAuditAction::SignalDropped,
                None,
                user,
                Some(user),
                Some("left_room"),
            );
        }
        if self.participants.is_empty() {
            self.audit(RoomAuditAction::RoomEnded, None, user, None, None);
            return Ok(LeaveOutcome {
                new_host: None,
                ended: true,
            });
        }
        if self.host == user {
            // 管理员优先，其次最早加入者（BTreeMap 按 id 排序，不能当加入序用，
            // 所以显式按 joined_at 再按 id 取最小）
            let successor = self
                .participants
                .values()
                .min_by_key(|p| (p.role != Role::Moderator, p.joined_at, p.user.clone()))
                .map(|p| p.user.clone());
            if let Some(next) = successor {
                self.host = next.clone();
                if let Some(p) = self.participants.get_mut(&next) {
                    p.role = Role::Host;
                }
                self.audit(
                    RoomAuditAction::HostTransferred,
                    None,
                    user,
                    Some(&next),
                    Some("host_left"),
                );
                return Ok(LeaveOutcome {
                    new_host: Some(next),
                    ended: false,
                });
            }
        }
        Ok(LeaveOutcome {
            new_host: None,
            ended: false,
        })
    }

    /// 静音：本人可静自己；管理员/主持人可静他人。返回静音后的状态。
    ///
    /// LiveKit 房间里这不只是一个 UI 图标：下一次凭证签发的 `canPublish`
    /// 会被降级（见 [`Room::prepare_access`]），所以客户端继续发包也不会
    /// 被 sidecar 转发。
    pub fn set_mute(
        &mut self,
        actor: &str,
        target: &str,
        track: Track,
        muted: bool,
    ) -> Result<bool> {
        let actor_role = self.role_of(actor)?;
        if actor != target && !actor_role.can_moderate() {
            self.audit(
                RoomAuditAction::Mute,
                Some(MemberErrorCode::Forbidden),
                actor,
                Some(target),
                Some("mute_other"),
            );
            return Err(RoomError::Forbidden);
        }
        let p = self
            .participants
            .get_mut(target)
            .ok_or(RoomError::NoSuchParticipant)?;
        match track {
            Track::Audio => p.audio_muted = muted,
            Track::Video => p.video_muted = muted,
        }
        self.bump();
        self.audit(
            RoomAuditAction::Mute,
            None,
            actor,
            Some(target),
            Some(if muted { "muted" } else { "unmuted" }),
        );
        Ok(muted)
    }

    /// 投递一条通话信令：只有房间成员能发，收件人也必须在房里。
    /// `from` 一律被改写成 `actor`——客户端自报的 from 不可信。
    ///
    /// ADR-25 的引擎一致性检查：LiveKit 房间的 ICE/DTLS 由 sidecar 与客户端
    /// 自己协商，str0m 那套 trickle candidate 进来意味着**两个引擎在同一个
    /// 房间里各写各的房间事实**，直接拒绝并留审计。
    pub fn post_signal(&mut self, actor: &str, mut signal: crate::signal::Signal) -> Result<usize> {
        if self.participant(actor).is_none() {
            self.audit(
                RoomAuditAction::SignalDropped,
                Some(MemberErrorCode::NotMember),
                actor,
                Some(&signal.to),
                Some("sender_not_member"),
            );
            return Err(RoomError::NotMember);
        }
        if self.participant(&signal.to).is_none() {
            self.audit(
                RoomAuditAction::SignalDropped,
                Some(MemberErrorCode::NoSuchDevice),
                actor,
                Some(&signal.to),
                Some("recipient_left"),
            );
            return Err(RoomError::NoSuchParticipant);
        }
        if self.engine == MediaEngine::LiveKit && signal.content.is_trickle_candidate() {
            self.audit(
                RoomAuditAction::EngineSignalMismatch,
                Some(MemberErrorCode::WrongEngine),
                actor,
                Some(&signal.to),
                Some("ice_candidate_in_livekit_room"),
            );
            return Err(RoomError::Forbidden);
        }
        signal.from = actor.to_string();
        let dropped = self.signals.push(signal);
        if dropped > 0 {
            self.audit(
                RoomAuditAction::SignalDropped,
                None,
                actor,
                None,
                Some("queue_overflow"),
            );
        }
        Ok(dropped)
    }

    /// 取走发给我的信令（读即清）。非成员拿不到任何东西。
    pub fn take_signals(&mut self, user: &str) -> Result<Vec<crate::signal::Signal>> {
        if self.participant(user).is_none() {
            return Err(RoomError::NotMember);
        }
        Ok(self.signals.drain(user))
    }

    /// 当前积压条数（诊断/测试用）。
    pub fn pending_signals(&self, user: &str) -> usize {
        self.signals.pending(user)
    }

    /// 全员静音/解除（**参照 Nextcloud Talk 的 force-mute**）：仅主持人/管理员。
    /// 返回受影响人数；主持人与管理员也会被静音（自己再解除即可，语义更简单）。
    pub fn mute_all(&mut self, actor: &str, track: Track, muted: bool) -> Result<usize> {
        if !self.role_of(actor)?.can_moderate() {
            self.audit(
                RoomAuditAction::MuteAll,
                Some(MemberErrorCode::Forbidden),
                actor,
                None,
                None,
            );
            return Err(RoomError::Forbidden);
        }
        for p in self.participants.values_mut() {
            match track {
                Track::Audio => p.audio_muted = muted,
                Track::Video => p.video_muted = muted,
            }
        }
        self.bump();
        self.audit(RoomAuditAction::MuteAll, None, actor, None, None);
        Ok(self.participants.len())
    }

    /// 踢人：主持人/管理员可踢；不能踢主持人；管理员不能踢管理员。
    pub fn kick(&mut self, actor: &str, target: &str) -> Result<()> {
        let actor_role = self.role_of(actor)?;
        if !actor_role.can_moderate() {
            self.audit(
                RoomAuditAction::Kicked,
                Some(MemberErrorCode::Forbidden),
                actor,
                Some(target),
                None,
            );
            return Err(RoomError::Forbidden);
        }
        let target_role = self.role_of(target)?;
        if target_role == Role::Host {
            self.audit(
                RoomAuditAction::Kicked,
                Some(MemberErrorCode::Forbidden),
                actor,
                Some(target),
                Some("cannot_kick_host"),
            );
            return Err(RoomError::CannotKickHost);
        }
        if actor_role == Role::Moderator && target_role == Role::Moderator {
            self.audit(
                RoomAuditAction::Kicked,
                Some(MemberErrorCode::Forbidden),
                actor,
                Some(target),
                Some("moderator_vs_moderator"),
            );
            return Err(RoomError::Forbidden);
        }
        self.participants.remove(target);
        self.allocators.remove(target);
        let dropped = self.signals.forget(target);
        self.banned.insert(target.to_string());
        self.generation = self.generation.saturating_add(1);
        self.bump();
        self.audit(RoomAuditAction::Kicked, None, actor, Some(target), None);
        if dropped > 0 {
            self.audit(
                RoomAuditAction::SignalDropped,
                None,
                actor,
                Some(target),
                Some("kicked"),
            );
        }
        Ok(())
    }

    /// 锁门：仅主持人。锁门后成员进不来；跨过锁门入房需要显式的
    /// [`MemberAction::JoinAsModerator`]（或 [`Room::join_as`]）角色入口。
    pub fn set_locked(&mut self, actor: &str, locked: bool) -> Result<()> {
        if let Err(err) = self.require_host(actor) {
            self.audit(
                RoomAuditAction::Lock,
                Some(MemberErrorCode::Forbidden),
                actor,
                None,
                Some("not_host"),
            );
            return Err(err);
        }
        self.locked = locked;
        self.bump();
        self.audit(
            RoomAuditAction::Lock,
            None,
            actor,
            None,
            Some(if locked { "locked" } else { "unlocked" }),
        );
        Ok(())
    }

    /// 录制**旗标**：仅主持人（合同把录制列为会议管控动作）。
    ///
    /// ADR-25 语义：这个开关只登记"有参与者正在客户端侧录制"，用于给全员
    /// 显示知情提示并驱动 [`crate::recording::RecordingSession`] 的同意流程。
    /// 它**不会**让服务端去起 LiveKit Egress——那条路是
    /// [`Room::request_server_egress`]，且恒被拒绝。
    pub fn set_recording(&mut self, actor: &str, recording: bool) -> Result<()> {
        self.require_host(actor)?;
        self.recording = if recording {
            RecordingMode::Client {
                reported_by: actor.to_string(),
            }
        } else {
            RecordingMode::Off
        };
        self.bump();
        self.audit(
            RoomAuditAction::RecordingFlag,
            None,
            actor,
            None,
            Some(if recording { "client_on" } else { "off" }),
        );
        Ok(())
    }

    /// 服务端 Egress 自动录制（ADR-25 关闭）。任何角色请求都是拒绝 + 留痕，
    /// 不存在可以把它打开的配置项。
    pub fn request_server_egress(
        &mut self,
        actor: &str,
        at_ms: u64,
    ) -> std::result::Result<(), EgressError> {
        let _ = at_ms;
        self.audit(
            RoomAuditAction::ServerEgress,
            Some(MemberErrorCode::Disabled),
            actor,
            None,
            Some("adr25_disabled"),
        );
        Err(EgressError::Disabled)
    }

    /// 转让主持。
    pub fn transfer_host(&mut self, actor: &str, target: &str) -> Result<()> {
        self.require_host(actor)?;
        if !self.participants.contains_key(target) {
            return Err(RoomError::NoSuchParticipant);
        }
        if let Some(old) = self.participants.get_mut(actor) {
            old.role = Role::Moderator;
        }
        if let Some(new) = self.participants.get_mut(target) {
            new.role = Role::Host;
        }
        self.host = target.to_string();
        self.bump();
        self.audit(
            RoomAuditAction::HostTransferred,
            None,
            actor,
            Some(target),
            None,
        );
        Ok(())
    }

    /// 提升/降级管理员（仅主持人）。
    pub fn set_role(&mut self, actor: &str, target: &str, role: Role) -> Result<()> {
        self.require_host(actor)?;
        if target == self.host {
            return Err(RoomError::Forbidden);
        }
        let p = self
            .participants
            .get_mut(target)
            .ok_or(RoomError::NoSuchParticipant)?;
        p.role = role;
        self.bump();
        self.audit(
            RoomAuditAction::RoleChanged,
            None,
            actor,
            Some(target),
            Some(role.as_str()),
        );
        Ok(())
    }

    /// 举手（主持人可看到申请发言队列）。
    pub fn raise_hand(&mut self, user: &str, raised: bool) -> Result<()> {
        let p = self
            .participants
            .get_mut(user)
            .ok_or(RoomError::NotMember)?;
        p.hand_raised = raised;
        self.bump();
        self.audit(
            RoomAuditAction::Hand,
            None,
            user,
            Some(user),
            Some(if raised { "raised" } else { "lowered" }),
        );
        Ok(())
    }

    /// 当前举手队列，按加入时间排序。
    pub fn hands_raised(&self) -> Vec<&Participant> {
        let mut raised: Vec<&Participant> = self
            .participants
            .values()
            .filter(|p| p.hand_raised)
            .collect();
        raised.sort_by_key(|p| (p.joined_at, p.user.clone()));
        raised
    }

    /// 观众订阅一路视频（`priority` 是权重：主讲 3、普通 1 之类）。
    pub fn subscribe_video(&mut self, viewer: &str, track: TrackId, priority: u64) -> Result<()> {
        let allocator = self
            .allocators
            .get_mut(viewer)
            .ok_or(RoomError::NotMember)?;
        allocator.set_video_track(track, priority);
        Ok(())
    }

    pub fn unsubscribe_video(&mut self, viewer: &str, track: TrackId) -> Result<()> {
        let allocator = self
            .allocators
            .get_mut(viewer)
            .ok_or(RoomError::NotMember)?;
        allocator.del_video_track(track);
        Ok(())
    }

    /// 按当前出口估算结算一次分配。`estimate_bps` 来自 BWE（或部署上限）。
    ///
    /// 没有订阅任何轨时返回空 `tracks` 与兜底 BWE 值——调用方据此把 pacer
    /// 降到探测档位，而不是继续按满码率发。
    pub fn allocate_egress(&mut self, viewer: &str, estimate_bps: u64) -> Result<EgressPlan> {
        let allocator = self
            .allocators
            .get_mut(viewer)
            .ok_or(RoomError::NotMember)?;
        allocator.set_egress_estimate(estimate_bps);
        allocator.on_tick();
        let mut tracks = Vec::new();
        let mut bwe = (0u64, 0u64);
        while let Some(out) = allocator.pop_output() {
            match out {
                crate::sfu::Output::Track(id, crate::sfu::Action::SetBitrate(bps)) => {
                    tracks.push((id, bps));
                }
                crate::sfu::Output::BweConfig(current, desired) => bwe = (current, desired),
            }
        }
        Ok(EgressPlan { tracks, bwe })
    }

    // ---------------- ADR-25：成员状态机、凭证绑定与审计 ----------------

    /// 状态变更即水印前滚。被拒的命令不动它（见 [`MemberCommand::expected_revision`]）。
    fn bump(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }

    fn audit(
        &mut self,
        action: RoomAuditAction,
        code: Option<MemberErrorCode>,
        actor: &str,
        target: Option<&str>,
        detail: Option<&str>,
    ) {
        self.audit_at(action, code, actor, target, None, None, detail);
    }

    fn audit_at(
        &mut self,
        action: RoomAuditAction,
        code: Option<MemberErrorCode>,
        actor: &str,
        target: Option<&str>,
        device: Option<&str>,
        at_ms: Option<u64>,
        detail: Option<&str>,
    ) {
        self.audit_seq = self.audit_seq.saturating_add(1);
        let entry = RoomAuditEntry {
            seq: self.audit_seq,
            at_ms: at_ms.unwrap_or_else(wxlike_core::types::now_ms),
            room: self.id.clone(),
            sidecar_room: self.sidecar_room.clone(),
            conv: self.owner_conv.clone(),
            actor: actor.to_string(),
            action,
            target: target.map(str::to_string),
            device: device.map(str::to_string),
            revision: self.revision,
            generation: self.generation,
            code: code.map(|c| c.as_str().to_string()),
            detail: detail.map(str::to_string),
            engine: self.engine.name().to_string(),
        };
        self.pending_audit.push_back(entry);
        while self.pending_audit.len() > AUDIT_QUEUE_CAP {
            // 房间是短生命周期对象；审计在落库前只排队。溢出说明有人在刷命令
            // ——丢最旧的一条并计数，绝不静默丢掉整个房间的记录。
            self.pending_audit.pop_front();
            self.audit_dropped = self.audit_dropped.saturating_add(1);
        }
    }

    /// 取走待落库的审计（交给 `crate::call_store::RoomAuditSink`）。
    /// 读即清；[`Room::audit_dropped`] 非零说明这段时间有记录被挤掉。
    pub fn take_audit(&mut self) -> Vec<RoomAuditEntry> {
        self.pending_audit.drain(..).collect()
    }

    /// 只看最近若干条（诊断用，不清空）。
    pub fn audit_tail(&self, n: usize) -> Vec<&RoomAuditEntry> {
        self.pending_audit.iter().rev().take(n).rev().collect()
    }

    /// 因队列上限被挤掉的条数（运维必须能看见丢失）。
    pub fn audit_dropped(&self) -> u64 {
        self.audit_dropped
    }

    /// 尚未落库的审计条数。
    pub fn pending_audit(&self) -> usize {
        self.pending_audit.len()
    }

    fn seat_state(&self, user: &str, device: &str) -> Option<MemberState> {
        self.participants
            .get(user)
            .and_then(|p| p.devices.get(device))
            .map(|seat| seat.state)
    }

    /// 这个席位现在能不能发媒体。`Left`/`Removed` 不行；服务端静音不体现在
    /// 这里，而是体现在下一次凭证的 `canPublish`（[`Room::prepare_access`]）。
    pub fn may_publish(&self, user: &str, device: &str) -> std::result::Result<bool, MemberError> {
        let p = self.participants.get(user).ok_or(MemberError::NotMember)?;
        let seat = p
            .devices
            .get(device)
            .ok_or_else(|| MemberError::NoSuchDevice(device.to_string()))?;
        if !seat.state.may_receive_token() {
            return Ok(false);
        }
        Ok(!p.audio_muted)
    }

    /// 成员状态机的**唯一**变更入口：先过 CAS 水印，再按角色/席位裁决，
    /// 允许与拒绝都留审计。
    ///
    /// 三类竞态都在这里裁决：
    ///
    /// * **同时加入**：席位与座位数在同一个 `&mut self` 下串行分配，容量不会
    ///   被超卖；同一设备的重复 `Join` 幂等。
    /// * **离席后的迟到信令**：离席即清空该用户的积压，再给它发信令得到
    ///   `NoSuchParticipant`，而不是"入队后永远没人取"。
    /// * **重放旧状态**：带着旧 `expected_revision` 的命令一律
    ///   [`MemberError::StaleRevision`]，且完全不改动状态。
    pub fn member_command(
        &mut self,
        cmd: &MemberCommand,
    ) -> std::result::Result<MemberOutcome, MemberError> {
        // 上游 `Room.IsClosed()`：关闭后的房间不接受任何命令，连 CAS 都不必再看。
        if let Some(reason) = self.closed {
            self.audit_at(
                RoomAuditAction::MemberChanged,
                Some(MemberErrorCode::RoomClosed),
                if cmd.actor.is_empty() {
                    "<anonymous>"
                } else {
                    &cmd.actor
                },
                None,
                cmd.action.device(),
                Some(cmd.at_ms),
                Some(reason.as_str_name()),
            );
            return Err(MemberError::RoomClosed(reason));
        }
        if cmd.actor.is_empty() {
            self.audit_at(
                RoomAuditAction::MemberChanged,
                Some(MemberErrorCode::BadInput),
                "<anonymous>",
                None,
                None,
                Some(cmd.at_ms),
                Some("empty_actor"),
            );
            return Err(MemberError::BadInput("actor is required"));
        }
        if let Some(seen) = cmd.expected_revision {
            if seen != self.revision {
                self.audit_at(
                    RoomAuditAction::MemberChanged,
                    Some(MemberErrorCode::StaleRevision),
                    &cmd.actor,
                    None,
                    cmd.action.device(),
                    Some(cmd.at_ms),
                    Some("cas"),
                );
                return Err(MemberError::StaleRevision {
                    seen,
                    current: self.revision,
                });
            }
        }
        let applied = self.apply_member_command(cmd);
        match applied {
            Ok(outcome) => {
                self.audit_at(
                    RoomAuditAction::MemberChanged,
                    None,
                    &cmd.actor,
                    Some(outcome.user.as_str()),
                    outcome.device.as_deref(),
                    Some(cmd.at_ms),
                    Some(cmd.action.name()),
                );
                Ok(outcome)
            }
            Err(err) => {
                let code = err.code();
                self.audit_at(
                    RoomAuditAction::MemberChanged,
                    Some(code),
                    &cmd.actor,
                    cmd.action.target_user().or(Some(cmd.actor.as_str())),
                    cmd.action.device(),
                    Some(cmd.at_ms),
                    Some(cmd.action.name()),
                );
                Err(err)
            }
        }
    }

    fn apply_member_command(
        &mut self,
        cmd: &MemberCommand,
    ) -> std::result::Result<MemberOutcome, MemberError> {
        let actor = cmd.actor.as_str();
        match &cmd.action {
            MemberAction::Join { device } | MemberAction::JoinAsModerator { device, .. } => {
                self.cmd_join(cmd, device, role_of_action(&cmd.action))
            }
            MemberAction::MarkActive { device } => {
                if !self.participants.contains_key(actor) {
                    return Err(MemberError::NotMember);
                }
                match self.seat_state(actor, device) {
                    None => return Err(MemberError::NoSuchDevice(device.clone())),
                    Some(MemberState::Active) => {
                        // 幂等：重复的 connected 不前滚代次。
                        return Ok(self.outcome(actor, Some(device), false, None, 0));
                    }
                    Some(MemberState::Joining | MemberState::Reconnecting) => {}
                    Some(other) => {
                        return Err(MemberError::IllegalState {
                            action: "mark_active",
                            state: other,
                        })
                    }
                }
                let p = self
                    .participants
                    .get_mut(actor)
                    .ok_or(MemberError::NotMember)?;
                if let Some(seat) = p.devices.get_mut(device) {
                    seat.state = MemberState::Active;
                    seat.grace_until_ms = 0;
                    // 与 sidecar 那张表对齐：这条路径生效时，proto 侧至少到
                    // ACTIVE（上游 JOINING→JOINED→ACTIVE 的最后一跳）。
                    seat.sidecar = SidecarParticipantState::Active;
                }
                p.refresh();
                self.bump();
                Ok(self.outcome(actor, Some(device), false, None, 0))
            }
            MemberAction::Disconnect { device } => {
                if !self.participants.contains_key(actor) {
                    return Err(MemberError::NotMember);
                }
                let current = match self.seat_state(actor, device) {
                    None => return Err(MemberError::NoSuchDevice(device.clone())),
                    Some(MemberState::Removed) => {
                        return Err(MemberError::IllegalState {
                            action: "disconnect",
                            state: MemberState::Removed,
                        })
                    }
                    Some(state) => state,
                };
                let grace = cmd.at_ms.saturating_add(self.reconnect_grace_ms);
                let need_bump = current != MemberState::Reconnecting;
                if let Some(p) = self.participants.get_mut(actor) {
                    if let Some(seat) = p.devices.get_mut(device) {
                        if need_bump {
                            seat.state = MemberState::Reconnecting;
                        }
                        // 掉线在 proto 侧就是 DISCONNECTED（同一张表，不另开一套）。
                        seat.sidecar = SidecarParticipantState::Disconnected;
                        // 宽限期内的重复上报只顺延截止，不再前滚状态。
                        seat.grace_until_ms = grace;
                    }
                    p.refresh();
                }
                if need_bump {
                    self.bump();
                }
                Ok(self.outcome(actor, Some(device), true, None, 0))
            }
            MemberAction::Reconnect { device } => {
                if self.banned.contains(actor) {
                    return Err(MemberError::Banned);
                }
                if !self.participants.contains_key(actor) {
                    return Err(MemberError::NotMember);
                }
                match self.seat_state(actor, device) {
                    None => return Err(MemberError::NoSuchDevice(device.clone())),
                    Some(MemberState::Left | MemberState::Removed) => {
                        return Err(MemberError::IllegalState {
                            action: "reconnect",
                            state: self.seat_state(actor, device).unwrap_or(MemberState::Left),
                        })
                    }
                    Some(_) => {}
                }
                // 重连必须换新凭证：旧 JWT 不再代表这一次连接。
                let p = self
                    .participants
                    .get_mut(actor)
                    .ok_or(MemberError::NotMember)?;
                if let Some(seat) = p.devices.get_mut(device) {
                    seat.state = MemberState::Joining;
                    seat.grace_until_ms = 0;
                    // 上游重连是**新建一个 ParticipantImpl**（状态表从 JOINING
                    // 重新开始），我们复用同一个席位，所以这里显式重置那台
                    // 单调时钟：否则 DISCONNECTED 之后新的 JOINING 会被
                    // `advance` 当成乱序丢掉，重连永远成功不了。
                    seat.sidecar = SidecarParticipantState::Joining;
                }
                p.refresh();
                self.generation = self.generation.saturating_add(1);
                self.bump();
                Ok(self.outcome(actor, Some(device), true, None, 0))
            }
            MemberAction::Leave { device } => {
                if !self.participants.contains_key(actor) {
                    return Err(MemberError::NotMember);
                }
                if self.seat_state(actor, device).is_none() {
                    return Err(MemberError::NoSuchDevice(device.clone()));
                }
                let dropped = self.remove_seat(actor, device, "leave", cmd.at_ms);
                let ended = self.participants.is_empty();
                if ended {
                    self.audit_at(
                        RoomAuditAction::RoomEnded,
                        None,
                        actor,
                        None,
                        Some(device),
                        Some(cmd.at_ms),
                        None,
                    );
                }
                let new_host = if ended {
                    None
                } else {
                    self.takeover_if_host(actor)
                };
                Ok(self.outcome(actor, Some(device), false, new_host, dropped))
            }
            MemberAction::MuteSelf { track, muted } => {
                if !self.participants.contains_key(actor) {
                    return Err(MemberError::NotMember);
                }
                self.set_mute(actor, actor, *track, *muted)
                    .map_err(room_to_member)?;
                Ok(self.outcome(actor, None, true, None, 0))
            }
            MemberAction::MuteOther {
                target,
                track,
                muted,
            } => {
                let allowed = self
                    .participants
                    .get(actor)
                    .is_some_and(|p| p.role.can_moderate());
                if !allowed {
                    return Err(MemberError::Forbidden);
                }
                if !self.participants.contains_key(target) {
                    return Err(MemberError::NotMember);
                }
                self.set_mute(actor, target, *track, *muted)
                    .map_err(room_to_member)?;
                Ok(self.outcome(target, None, true, None, 0))
            }
            MemberAction::Hand { raised } => {
                if !self.participants.contains_key(actor) {
                    return Err(MemberError::NotMember);
                }
                self.raise_hand(actor, *raised).map_err(room_to_member)?;
                Ok(self.outcome(actor, None, false, None, 0))
            }
        }
    }

    fn cmd_join(
        &mut self,
        cmd: &MemberCommand,
        device: &str,
        role: Role,
    ) -> std::result::Result<MemberOutcome, MemberError> {
        let actor = cmd.actor.as_str();
        if device.is_empty() {
            return Err(MemberError::BadInput("device is required"));
        }
        if self.banned.contains(actor) {
            return Err(MemberError::Banned);
        }
        let seat = DeviceSeat {
            state: MemberState::Joining,
            revision: self.revision.saturating_add(1),
            grace_until_ms: 0,
            // 上游 `ParticipantImpl` 的初始状态就是 JOINING（`participant.go:443`）。
            sidecar: SidecarParticipantState::Joining,
        };
        if let Some(p) = self.participants.get_mut(actor) {
            // 已经是成员：设备级幂等，同设备重复入房不改变任何东西。
            if let Some(existing) = p.devices.get_mut(device) {
                if existing.state.holds_media() {
                    return Ok(self.outcome(actor, Some(device), false, None, 0));
                }
                *existing = seat;
                p.refresh();
                self.generation = self.generation.saturating_add(1);
                self.bump();
                return Ok(self.outcome(actor, Some(device), true, None, 0));
            }
            if self.locked && !role.can_moderate() {
                return Err(MemberError::Locked);
            }
            p.devices.insert(device.to_string(), seat);
            p.refresh();
            self.generation = self.generation.saturating_add(1);
            self.bump();
            return Ok(self.outcome(actor, Some(device), true, None, 0));
        }
        if self.participants.len() >= self.capacity {
            return Err(MemberError::Full(self.capacity));
        }
        if self.locked && !role.can_moderate() {
            return Err(MemberError::Locked);
        }
        let mut p = Participant::new(actor, role, cmd.at_ms, MemberState::Joining);
        p.devices.insert(device.to_string(), seat);
        self.participants.insert(actor.to_string(), p);
        self.allocators.insert(
            actor.to_string(),
            EgressBitrateAllocator::new(DEFAULT_MAX_EGRESS_BPS),
        );
        self.generation = self.generation.saturating_add(1);
        self.bump();
        Ok(self.outcome(actor, Some(device), true, None, 0))
    }

    /// 席位回收的公共部分：清席位、清积压、必要时清名册并前滚代次。
    /// 返回被丢弃的积压信令条数。
    fn remove_seat(&mut self, user: &str, device: &str, reason: &str, at_ms: u64) -> usize {
        // 上游 `Room.LastLeftAt()`：离席时刻要记住，`close_if_empty` 用它决定
        // 走 `departure_timeout` 还是 `empty_timeout`。
        self.last_left_at_ms = at_ms;
        let mut became_empty = false;
        if let Some(p) = self.participants.get_mut(user) {
            p.devices.remove(device);
            if p.devices.is_empty() {
                p.state = MemberState::Left;
                became_empty = true;
            } else {
                p.refresh();
            }
        }
        self.generation = self.generation.saturating_add(1);
        self.bump();
        let dropped = if became_empty {
            self.participants.remove(user);
            self.allocators.remove(user);
            self.signals.forget(user)
        } else {
            0
        };
        self.audit_at(
            if reason == "expired" {
                RoomAuditAction::SeatExpired
            } else {
                RoomAuditAction::MemberChanged
            },
            None,
            user,
            Some(user),
            Some(device),
            Some(at_ms),
            Some(reason),
        );
        if dropped > 0 {
            self.audit_at(
                RoomAuditAction::SignalDropped,
                None,
                user,
                Some(user),
                Some(device),
                Some(at_ms),
                Some("seat_closed"),
            );
        }
        dropped
    }

    /// 主持人席位没了要自动移交（与 [`Room::leave`] 同一套规则）。
    fn takeover_if_host(&mut self, user: &str) -> Option<String> {
        if self.host != user || self.participants.is_empty() {
            return None;
        }
        let successor = self
            .participants
            .values()
            .min_by_key(|p| (p.role != Role::Moderator, p.joined_at, p.user.clone()))
            .map(|p| p.user.clone())?;
        self.host = successor.clone();
        if let Some(p) = self.participants.get_mut(&successor) {
            p.role = Role::Host;
        }
        self.audit(
            RoomAuditAction::HostTransferred,
            None,
            user,
            Some(&successor),
            Some("seat_reclaimed"),
        );
        Some(successor)
    }

    fn outcome(
        &self,
        user: &str,
        device: Option<&str>,
        needs_new_token: bool,
        new_host: Option<String>,
        dropped_signals: usize,
    ) -> MemberOutcome {
        let state = self
            .participants
            .get(user)
            .and_then(|p| match device {
                Some(d) => p.devices.get(d).map(|s| s.state),
                None => Some(p.state),
            })
            .unwrap_or(MemberState::Left);
        MemberOutcome {
            user: user.to_string(),
            device: device.map(str::to_string),
            state,
            revision: self.revision,
            generation: self.generation,
            room_ended: self.participants.is_empty(),
            new_host,
            dropped_signals,
            needs_new_token,
        }
    }

    /// 组装一枚入房凭证请求（真正签名的是 [`crate::token::LiveKitIssuer`]；
    /// 房间只决定"给不给、给什么能力、哪一代"）。
    ///
    /// 绑定关系：sidecar 房间名 + identity(=user) + device + call + 成员代次
    /// + 引擎。服务端静音在这里落地为 `canPublish=false`——这正是 ADR-25 下
    /// "服务端静音"唯一诚实的实现：我们不再自己转发媒体，只能通过凭证让
    /// sidecar 停止转发。
    pub fn prepare_access(
        &mut self,
        user: &str,
        device: &str,
        call: &str,
        at_ms: u64,
    ) -> std::result::Result<crate::token::AccessRequest, MemberError> {
        if let Some(reason) = self.closed {
            self.audit_at(
                RoomAuditAction::TokenIssued,
                Some(MemberErrorCode::RoomClosed),
                user,
                Some(user),
                Some(device),
                Some(at_ms),
                Some(reason.as_str_name()),
            );
            return Err(MemberError::RoomClosed(reason));
        }
        if self.engine != MediaEngine::LiveKit {
            self.audit_at(
                RoomAuditAction::TokenIssued,
                Some(MemberErrorCode::WrongEngine),
                user,
                Some(user),
                Some(device),
                Some(at_ms),
                Some("not_a_livekit_room"),
            );
            return Err(MemberError::WrongEngine(
                self.engine.name(),
                MediaEngine::LiveKit.name(),
            ));
        }
        if self.banned.contains(user) {
            self.audit_at(
                RoomAuditAction::TokenIssued,
                Some(MemberErrorCode::Banned),
                user,
                Some(user),
                Some(device),
                Some(at_ms),
                Some("banned"),
            );
            return Err(MemberError::Banned);
        }
        if !self.participants.contains_key(user) {
            // 未邀请者想进房：留痕，但不改任何状态。
            self.audit_at(
                RoomAuditAction::TokenIssued,
                Some(MemberErrorCode::NotMember),
                user,
                Some(user),
                Some(device),
                Some(at_ms),
                Some("not_invited"),
            );
            return Err(MemberError::NotMember);
        }
        let state = self.seat_state(user, device);
        let state = match state {
            Some(state) => state,
            None => {
                self.audit_at(
                    RoomAuditAction::TokenIssued,
                    Some(MemberErrorCode::NoSuchDevice),
                    user,
                    Some(user),
                    Some(device),
                    Some(at_ms),
                    Some("no_seat"),
                );
                return Err(MemberError::NoSuchDevice(device.to_string()));
            }
        };
        if !state.may_receive_token() {
            self.audit_at(
                RoomAuditAction::TokenIssued,
                Some(MemberErrorCode::IllegalState),
                user,
                Some(user),
                Some(device),
                Some(at_ms),
                Some(state.as_str()),
            );
            return Err(MemberError::IllegalState {
                action: "prepare_access",
                state,
            });
        }
        let p = self.participants.get(user).ok_or(MemberError::NotMember)?;
        let muted = p.audio_muted;
        self.audit_at(
            RoomAuditAction::TokenIssued,
            None,
            user,
            Some(user),
            Some(device),
            Some(at_ms),
            Some(if muted { "publish_revoked" } else { "allowed" }),
        );
        Ok(crate::token::AccessRequest {
            room: self.sidecar_room.clone(),
            identity: user.to_string(),
            device: device.to_string(),
            call: call.to_string(),
            scope: MediaScope::Call,
            generation: self.generation,
            engine: self.engine.name().to_string(),
            // sidecar 名册上的显示名由会话层给；房间只知道自己存了哪些 id，
            // 没有昵称事实，所以这里留空（空即不写进 claim，与上游同法）。
            name: String::new(),
            // ADR-25：我们只签普通客户端。`kind` 留空即 STANDARD；
            // egress/ingress/agent 会在 `LiveKitIssuer::issue` 里被拒签。
            kind: String::new(),
            metadata: String::new(),
            attributes: Default::default(),
            key_fingerprint: None,
            ttl_ms: Some(ACCESS_TTL_MS),
            muted,
        })
    }

    /// 用一枚**已验签**的令牌结论标记该参与者的依附性（上游是
    /// `Grants.IsDependent()` 现算，见 `pkg/rtc/participant.go:615-622`；
    /// 我们的 grants 只在签发/校验那一刻过手，所以在那里落一次）。
    ///
    /// 目的是让 [`Room::close_if_empty`] 不被录制器/agent 连接"撑活"——
    /// 否则一个自动录制的会话会让房间永远关不掉。
    pub fn mark_dependent_from_claims(
        &mut self,
        user: &str,
        claims: &crate::token::AccessClaims,
    ) -> bool {
        let dependent = claims.is_dependent();
        self.mark_dependent(user, dependent)
    }

    /// 退群/封禁：撤销该用户在本房间的全部席位与加入权限。比 [`Room::kick`]
    /// 更明确——由成员表触发，但同样进 ban 名单、同样前滚代次。
    pub fn revoke_membership(
        &mut self,
        user: &str,
        at_ms: u64,
    ) -> std::result::Result<(), MemberError> {
        if !self.participants.contains_key(user) {
            self.audit_at(
                RoomAuditAction::MembershipRevoked,
                Some(MemberErrorCode::NotMember),
                user,
                Some(user),
                None,
                Some(at_ms),
                None,
            );
            return Err(MemberError::NotMember);
        }
        let devices: Vec<String> = self
            .participants
            .get(user)
            .map(|p| p.devices.keys().cloned().collect())
            .unwrap_or_default();
        if devices.is_empty() {
            // 冷备路径（没有设备席位）也要能撤销：直接摘名册。
            self.participants.remove(user);
            self.allocators.remove(user);
            let dropped = self.signals.forget(user);
            self.generation = self.generation.saturating_add(1);
            self.bump();
            if dropped > 0 {
                self.audit_at(
                    RoomAuditAction::SignalDropped,
                    None,
                    user,
                    Some(user),
                    None,
                    Some(at_ms),
                    Some("revoked"),
                );
            }
        } else {
            for device in devices {
                self.remove_seat(user, &device, "revoke", at_ms);
            }
            if let Some(p) = self.participants.get_mut(user) {
                for seat in p.devices.values_mut() {
                    seat.state = MemberState::Removed;
                }
                p.state = MemberState::Removed;
                self.bump();
            }
            self.participants.remove(user);
            self.allocators.remove(user);
            let _ = self.signals.forget(user);
        }
        self.banned.insert(user.to_string());
        self.audit_at(
            RoomAuditAction::MembershipRevoked,
            None,
            user,
            Some(user),
            None,
            Some(at_ms),
            None,
        );
        let _ = self.takeover_if_host(user);
        if self.participants.is_empty() {
            self.audit_at(
                RoomAuditAction::RoomEnded,
                None,
                user,
                None,
                None,
                Some(at_ms),
                Some("revoked_last"),
            );
        }
        Ok(())
    }

    /// 定时器：回收超过宽限期的 `Reconnecting` 席位。返回被回收的用户。
    pub fn tick(&mut self, now_ms: u64) -> Vec<String> {
        let expired: Vec<(String, String)> = self
            .participants
            .iter()
            .flat_map(|(user, p)| {
                p.devices.iter().filter_map(move |(device, seat)| {
                    if seat.state == MemberState::Reconnecting
                        && seat.grace_until_ms != 0
                        && now_ms >= seat.grace_until_ms
                    {
                        Some((user.clone(), device.clone()))
                    } else {
                        None
                    }
                })
            })
            .collect();
        let mut removed: Vec<String> = Vec::new();
        let had_expiring = !expired.is_empty();
        for (user, device) in expired {
            self.remove_seat(&user, &device, "expired", now_ms);
            let gone = !self
                .participants
                .get(&user)
                .is_some_and(|p| p.devices.contains_key(&device));
            if gone && !removed.contains(&user) {
                removed.push(user.clone());
            }
            let _ = self.takeover_if_host(&user);
        }
        if self.participants.is_empty() && had_expiring {
            self.audit_at(
                RoomAuditAction::RoomEnded,
                None,
                "system",
                None,
                None,
                Some(now_ms),
                Some("last_seat_expired"),
            );
        }
        removed
    }

    // ---------------- 移植自 LiveKit 的房间关闭与状态上报 ----------------

    /// 房间是否已关闭（上游 `Room.IsClosed()`，`pkg/rtc/room.go:754-761`）。
    pub fn is_closed(&self) -> bool {
        self.closed.is_some()
    }

    /// 关闭原因；未关闭即 `None`。
    pub fn close_reason(&self) -> Option<RoomCloseReason> {
        self.closed
    }

    /// 关闭房间（上游 `Room.Close(reason)`，`pkg/rtc/room.go:800-822`）。
    ///
    /// 三条照抄的行为：
    ///
    /// * **幂等**：已经关了直接返回 `false`，不重复通知、不重复审计
    ///   （上游先 `select { case <-r.closed: return }`）；
    /// * 关闭时把每个在场参与者一起关掉，原因走
    ///   [`RoomCloseReason::to_disconnect_reason`] 那张表 —— 房间侧与参与者侧
    ///   的原因从此是**同一次决定的两个投影**；
    /// * 关闭即终态：之后再有任何命令都撞 [`MemberError::RoomClosed`]。
    ///
    /// 返回 `true` 表示这次调用真正完成了关闭。
    pub fn close(&mut self, reason: RoomCloseReason, at_ms: u64) -> bool {
        if self.is_closed() {
            return false;
        }
        self.closed = Some(reason);
        // 上游关闭房间时逐个 `p.Close(...)`；我们这里等价地清空所有设备席位，
        // 并把名册留在原地（房间对象还要出审计与最终快照）。
        let participant_reason = reason.to_disconnect_reason();
        let mut closed_users: Vec<String> = Vec::with_capacity(self.participants.len());
        for (user, p) in self.participants.iter_mut() {
            for seat in p.devices.values_mut() {
                if seat.state.holds_media() {
                    seat.state = MemberState::Left;
                    seat.sidecar = SidecarParticipantState::Disconnected;
                    seat.grace_until_ms = 0;
                }
            }
            p.refresh();
            closed_users.push(user.clone());
        }
        // 上游是先逐个 `p.Close(...)` 再打日志；这里同样把"每个参与者被一起
        // 关掉"留成独立审计行，原因用的是那张一致性表的**参与侧**投影。
        for user in closed_users {
            self.audit_at(
                RoomAuditAction::MemberChanged,
                None,
                "system",
                Some(&user),
                None,
                Some(at_ms),
                Some(participant_reason.as_str_name()),
            );
        }
        self.generation = self.generation.saturating_add(1);
        self.bump();
        self.audit_at(
            RoomAuditAction::RoomEnded,
            None,
            "system",
            None,
            None,
            Some(at_ms),
            Some(reason.as_str_name()),
        );
        true
    }

    /// 空房关闭（上游 `Room.CloseIfEmpty()`，`pkg/rtc/room.go:764-798`）。
    ///
    /// 判定顺序也照抄：
    ///
    /// 1. 已关闭 → 什么都不做；
    /// 2. **还有一个独立成员在场**（非依附参与者）→ 直接返回。依附参与者
    ///    （agent / egress 录制会话）不算数，见 [`Room::mark_dependent`]；
    /// 3. 计时起点二选一：有人来过且有人走过 → 从 `last_left_at` 起算、
    ///    用 `departure_timeout`；否则从建房时刻起算、用 `empty_timeout`；
    /// 4. `elapsed >= timeout` 才真的关，原因是 `IDLE_TIMEOUT`。
    ///
    /// 与上游一致，超时判定用**秒**（proto 里这两个字段就是秒）。
    pub fn close_if_empty(&mut self, now_ms: u64) -> Option<RoomCloseReason> {
        if self.is_closed() {
            return None;
        }
        for user in self.participants.keys() {
            if !self.is_dependent(user) {
                return None;
            }
        }
        let (elapsed_secs, timeout_secs, _reason_name) =
            if self.first_joined_at_ms != 0 && self.last_left_at_ms != 0 {
                (
                    (now_ms.saturating_sub(self.last_left_at_ms)) / 1_000,
                    self.departure_timeout_secs,
                    "departure timeout",
                )
            } else {
                (
                    (now_ms.saturating_sub(self.created_at)) / 1_000,
                    self.empty_timeout_secs,
                    "empty timeout",
                )
            };
        if elapsed_secs >= timeout_secs {
            let reason = RoomCloseReason::IdleTimeout;
            self.close(reason, now_ms);
            // 上游还会打一条 "closing idle room" 的 info 日志，把用的是哪个
            // 超时留在审计里，运维才分得清"没人来过"和"来过的都走了"。
            self.audit_at(
                RoomAuditAction::RoomEnded,
                None,
                "system",
                None,
                None,
                Some(now_ms),
                Some(if timeout_secs == self.departure_timeout_secs {
                    "departure_timeout"
                } else {
                    "empty_timeout"
                }),
            );
            return Some(reason);
        }
        None
    }

    /// 计时参数（部署可调，与上游 proto 字段同名同单位）。
    pub fn room_timeouts(&self) -> (u64, u64) {
        (self.empty_timeout_secs, self.departure_timeout_secs)
    }

    /// 测试/部署用：覆盖两个超时（秒）。`0` 表示"到点即关"。
    pub fn with_room_timeouts(mut self, empty_secs: u64, departure_secs: u64) -> Self {
        self.empty_timeout_secs = empty_secs;
        self.departure_timeout_secs = departure_secs;
        self
    }

    /// 标记一个参与者是"依附"的（agent / egress 那类）。
    ///
    /// 结论必须由调用方从令牌算出来 —— 上游是 `Grants.IsDependent()`
    /// （`pkg/rtc/participant.go:615-622`），我们这边对应
    /// [`crate::token::AccessClaims::is_dependent`]。ADR-25 下我们**不会**签出
    /// 这类令牌（见 `assert_participant_kind_allowed`），所以线上这里恒为空；
    /// 保留它是为了让回滚/演练路径也能表达"这一路不决定房间生死"。
    pub fn mark_dependent(&mut self, user: &str, dependent: bool) -> bool {
        if !self.participants.contains_key(user) {
            return false;
        }
        if dependent {
            self.dependent.insert(user.to_string());
        } else {
            self.dependent.remove(user);
        }
        true
    }

    /// 该参与者是否依附于房间（不存在的人按"独立"处理，宁可让房间活着）。
    pub fn is_dependent(&self, user: &str) -> bool {
        self.dependent.contains(user)
    }

    /// sidecar 回报某个设备席位的参与者状态。
    ///
    /// 这里是上游 `ParticipantImpl.updateState`（`pkg/rtc/participant.go:2333-2358`）
    /// 的行为移植：**乱序报告被忽略且不改任何状态**，只有严格前进才生效。
    /// 数值序即 proto 序（JOINING < JOINED < ACTIVE < DISCONNECTED），所以
    /// 已经 `ACTIVE` 的席位不会再被一条迟到的 `JOINED` 打回去，已经
    /// `DISCONNECTED` 的席位也不会被迟到的 `ACTIVE` 复活。
    ///
    /// 生效之后的房间副作用见 [`SidecarStateEffect`]，与上游
    /// `Room.onStateChange`（`pkg/rtc/room.go:1227-1281`）一一对应：
    /// `ACTIVE` → 订阅既有轨道 + 上报入房成功；`DISCONNECTED` → 上游立刻移除，
    /// 我们改为进入重连宽限期（刻意的偏离，理由见
    /// [`SidecarParticipantState::to_member_state`]）。
    pub fn report_sidecar_state(
        &mut self,
        user: &str,
        device: &str,
        next: SidecarParticipantState,
        at_ms: u64,
    ) -> std::result::Result<SidecarStateReport, MemberError> {
        if let Some(reason) = self.closed {
            return Err(MemberError::RoomClosed(reason));
        }
        let p = self.participants.get(user).ok_or(MemberError::NotMember)?;
        let seat = p
            .devices
            .get(device)
            .ok_or_else(|| MemberError::NoSuchDevice(device.to_string()))?;
        let current = seat.sidecar;
        let Some(old) = SidecarParticipantState::advance(current, next) else {
            // 上游对乱序只打 debug 日志、不动状态；我们把"被忽略"这件事留痕，
            // 因为它正是排障时想知道的"sidecar 报晚了还是报错了"。
            self.audit_at(
                RoomAuditAction::MemberChanged,
                Some(MemberErrorCode::OutOfOrderSidecarState),
                user,
                Some(user),
                Some(device),
                Some(at_ms),
                Some(current.as_str_name()),
            );
            return Ok(SidecarStateReport {
                applied: false,
                previous: current,
                current,
                effects: Vec::new(),
            });
        };
        let effects = match next {
            // 上游 `onStateChange` 的 ACTIVE 分支：先订阅既有轨道，再上报指标。
            SidecarParticipantState::Active => vec![
                SidecarStateEffect::SubscribeExistingTracks,
                SidecarStateEffect::ReportActive,
            ],
            SidecarParticipantState::Disconnected => vec![SidecarStateEffect::BeginReconnectGrace],
            SidecarParticipantState::Joining | SidecarParticipantState::Joined => Vec::new(),
        };
        let p = self
            .participants
            .get_mut(user)
            .ok_or(MemberError::NotMember)?;
        let seat = p
            .devices
            .get_mut(device)
            .ok_or_else(|| MemberError::NoSuchDevice(device.to_string()))?;
        seat.sidecar = next;
        seat.state = next.to_member_state();
        if next == SidecarParticipantState::Disconnected {
            seat.grace_until_ms = at_ms.saturating_add(self.reconnect_grace_ms);
        } else {
            seat.grace_until_ms = 0;
        }
        p.refresh();
        self.bump();
        // 上游只在 `ACTIVE -> DISCONNECTED` 时记一次会话时长
        // （`participant.go:2354-2357`）：从 JOINING 直接掉线的不算完整会话。
        let detail = if next.is_clean_disconnect_from(old) {
            "active_to_disconnected"
        } else {
            next.as_str_name()
        };
        self.audit_at(
            RoomAuditAction::MemberChanged,
            None,
            user,
            Some(user),
            Some(device),
            Some(at_ms),
            Some(detail),
        );
        Ok(SidecarStateReport {
            applied: true,
            previous: old,
            current: next,
            effects,
        })
    }

    /// 该席位当前对 sidecar 报的状态（诊断/给客户端的快照）。
    pub fn sidecar_state(&self, user: &str, device: &str) -> Option<SidecarParticipantState> {
        self.participants
            .get(user)
            .and_then(|p| p.devices.get(device))
            .map(|seat| seat.sidecar)
    }

    fn role_of(&self, user: &str) -> Result<Role> {
        self.participants
            .get(user)
            .map(|p| p.role)
            .ok_or(RoomError::NotMember)
    }

    fn require_host(&self, actor: &str) -> Result<()> {
        match self.role_of(actor)? {
            Role::Host => Ok(()),
            _ => Err(RoomError::Forbidden),
        }
    }
}

/// `RoomError` 到 `MemberError` 的收敛映射：成员状态机复用既有的管控原语
/// （[`Room::set_mute`] 等），但对外只暴露自己的错误类型。
fn room_to_member(err: RoomError) -> MemberError {
    match err {
        RoomError::Full(n) => MemberError::Full(n),
        RoomError::Locked => MemberError::Locked,
        RoomError::NotMember => MemberError::NotMember,
        RoomError::AlreadyJoined => MemberError::NotMember,
        RoomError::Banned => MemberError::Banned,
        RoomError::Forbidden => MemberError::Forbidden,
        RoomError::NoSuchParticipant => MemberError::NotMember,
        RoomError::CannotKickHost => MemberError::Forbidden,
        RoomError::BadCapacity => MemberError::BadInput("capacity out of range"),
    }
}

/// 入房命令期望落到的角色。`JoinAsModerator` 之外的入房永远是普通成员。
fn role_of_action(action: &MemberAction) -> Role {
    match action {
        MemberAction::JoinAsModerator { role, .. } => *role,
        _ => Role::Member,
    }
}
