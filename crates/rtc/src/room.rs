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
}

impl Default for DeviceSeat {
    fn default() -> Self {
        Self {
            state: MemberState::Joining,
            revision: 0,
            grace_until_ms: 0,
        }
    }
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
            key_fingerprint: None,
            ttl_ms: Some(ACCESS_TTL_MS),
            muted,
        })
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
