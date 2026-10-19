//! P4/P5 通话房间的 HTTP 控制面（把 `wxlike-rtc` 的纯状态机接进平台）。
//!
//! `rtc::room` 只有状态机，没有存储、没有权限来源、没有路由——这个模块补那三层：
//!
//! * **注册表**：`conv_id -> Room`，进程内（通话是瞬时状态，重启即散，符合预期）；
//! * **成员资格**：直聊按 `d-a~b` 拆分、群按成员表、频道按社群成员表；
//! * **权限**：管控动作（静音他人/踢人/锁门/录制/移交）由 `Room` 内的角色裁决，
//!   这里只负责把 `RoomError` 翻译成 HTTP 语义。
//!
//! 房间不写消息序列号（ADR-7/不变式 3）：本模块只发系统事件，不碰信箱。

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use wxlike_rtc::room::{EgressPlan, Participant, Room, RoomError, Track};

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::validate;
use crate::{Platform, PlatformState};

/// 默认房间容量（合同只要求"房间制"，容量取产品常见值）。
pub const DEFAULT_CAPACITY: usize = 50;

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/call/{conv}", get(state))
        .route("/api/call/{conv}/start", post(start))
        .route("/api/call/{conv}/join", post(join))
        .route("/api/call/{conv}/leave", post(leave))
        .route("/api/call/{conv}/mute", post(mute))
        .route("/api/call/{conv}/mute-all", post(mute_all))
        .route("/api/call/{conv}/kick", post(kick))
        .route("/api/call/{conv}/lock", post(lock))
        .route("/api/call/{conv}/record", post(record))
        .route("/api/call/{conv}/hand", post(hand))
        .route("/api/call/{conv}/transfer", post(transfer))
        .route("/api/call/{conv}/egress", get(egress))
}

/// 进程内房间注册表。通话状态不落库：进程重启 = 所有通话结束。
#[derive(Default)]
pub struct CallRegistry {
    rooms: Mutex<HashMap<String, Room>>,
}

impl CallRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Room>> {
        self.rooms.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn remove(&self, conv: &str) -> bool {
        self.lock().remove(conv).is_some()
    }
}

#[derive(Serialize, Clone)]
pub struct ParticipantView {
    pub user: String,
    pub role: &'static str,
    pub audio_muted: bool,
    pub video_muted: bool,
    pub hand_raised: bool,
    pub joined_at: u64,
}

impl From<&Participant> for ParticipantView {
    fn from(p: &Participant) -> Self {
        Self {
            user: p.user.clone(),
            role: p.role.as_str(),
            audio_muted: p.audio_muted,
            video_muted: p.video_muted,
            hand_raised: p.hand_raised,
            joined_at: p.joined_at,
        }
    }
}

/// 对外视图（拥有数据：调用方拿到锁释放后的快照）。
#[derive(Serialize, Clone)]
pub struct CallView {
    pub conv_id: String,
    pub live: bool,
    pub host: Option<String>,
    pub locked: bool,
    pub recording: bool,
    pub participants: Vec<ParticipantView>,
}

fn map_room_error(e: RoomError) -> PlatformError {
    match e {
        RoomError::Full(_) => PlatformError::Conflict("call room is full".into()),
        RoomError::Locked => PlatformError::Forbidden,
        RoomError::NotMember => PlatformError::Forbidden,
        RoomError::AlreadyJoined => PlatformError::Conflict("already in the room".into()),
        RoomError::Banned => PlatformError::Forbidden,
        RoomError::Forbidden => PlatformError::Forbidden,
        RoomError::NoSuchParticipant => PlatformError::NotFound,
        RoomError::CannotKickHost => PlatformError::Forbidden,
        RoomError::BadCapacity => PlatformError::BadInput("capacity out of range"),
    }
}

impl Platform {
    /// 通话注册表（测试与运维用）。
    pub fn calls(&self) -> &CallRegistry {
        &self.calls
    }

    /// 会话成员判定：直聊按 `d-a~b`、群按成员表、频道按社群成员表。
    pub fn conv_member_allowed(&self, conv: &str, user: &str) -> Result<bool> {
        let conv = validate::conv(conv)?;
        if let Some(rest) = conv.strip_prefix("d-") {
            let (a, b) = rest
                .split_once('~')
                .ok_or(PlatformError::BadInput("malformed direct conversation"))?;
            return Ok(a == user || b == user);
        }
        if let Some(members) = self.cached_members(conv)? {
            return Ok(members.iter().any(|m| m == user));
        }
        // 频道：先看是不是频道，再按社群成员判定
        if let Some(community) = self.channel_community(conv)? {
            return Ok(self.require_member(&community, user).is_ok());
        }
        Err(PlatformError::NotFound)
    }

    fn require_call_member(&self, conv: &str, user: &str) -> Result<()> {
        if self.conv_member_allowed(conv, user)? {
            Ok(())
        } else {
            Err(PlatformError::Forbidden)
        }
    }

    fn with_room<T>(&self, conv: &str, f: impl FnOnce(&mut Room) -> Result<T>) -> Result<T> {
        let mut guard = self.calls.lock();
        let room = guard.get_mut(conv).ok_or(PlatformError::NotFound)?;
        f(room)
    }

    pub fn call_state(&self, conv: &str, user: &str) -> Result<Option<CallView>> {
        self.require_call_member(conv, user)?;
        // 借用检查：先把需要的数据复制出来，再构造视图
        let mut guard = self.calls.lock();
        let Some(room) = guard.get_mut(conv) else {
            return Ok(None);
        };
        // 清理已空房间（所有人离开后房间自然结束）
        if room.is_empty() {
            guard.remove(conv);
            return Ok(None);
        }
        Ok(Some(CallView {
            conv_id: conv.to_string(),
            live: true,
            host: Some(room.host().to_string()),
            locked: room.is_locked(),
            recording: room.is_recording(),
            participants: room.roster().into_iter().map(Into::into).collect(),
        }))
    }

    pub fn start_call(&self, conv: &str, user: &str, capacity: usize) -> Result<()> {
        self.require_call_member(conv, user)?;
        let conv = validate::conv(conv)?.to_string();
        let mut guard = self.calls.lock();
        if guard.contains_key(&conv) {
            return Err(PlatformError::Conflict("call already in progress".into()));
        }
        let id = crate::auth::random_hex(8);
        let room = Room::new(&id, &conv, user, capacity, wxlike_core::types::now_ms())
            .map_err(map_room_error)?;
        guard.insert(conv.clone(), room);
        drop(guard);
        self.audit(user, "call.start", &conv, "");
        Ok(())
    }

    pub fn join_call(&self, conv: &str, user: &str) -> Result<()> {
        self.require_call_member(conv, user)?;
        let conv = validate::conv(conv)?.to_string();
        let mut guard = self.calls.lock();
        let room = guard.get_mut(&conv).ok_or(PlatformError::NotFound)?;
        // 房主本来就在房里：重复 join 视为幂等成功（客户端重连场景）
        if room.participant(user).is_some() {
            return Ok(());
        }
        room.join(user, wxlike_core::types::now_ms())
            .map_err(map_room_error)?;
        Ok(())
    }

    pub fn leave_call(&self, conv: &str, user: &str) -> Result<()> {
        let conv = validate::conv(conv)?.to_string();
        let mut guard = self.calls.lock();
        let Some(room) = guard.get_mut(&conv) else {
            return Err(PlatformError::NotFound);
        };
        let outcome = room.leave(user).map_err(map_room_error)?;
        if outcome.ended {
            guard.remove(&conv);
        }
        Ok(())
    }

    pub fn mute_in_call(
        &self,
        conv: &str,
        actor: &str,
        target: &str,
        track: Track,
        muted: bool,
    ) -> Result<()> {
        self.with_room(conv, |room| {
            room.set_mute(actor, target, track, muted)
                .map(|_| ())
                .map_err(map_room_error)
        })
    }

    /// 全员静音（参照 spreed 的 force-mute）：主持人/管理员。
    pub fn mute_all_in_call(
        &self,
        conv: &str,
        actor: &str,
        track: Track,
        muted: bool,
    ) -> Result<usize> {
        self.with_room(conv, |room| {
            room.mute_all(actor, track, muted).map_err(map_room_error)
        })
    }

    pub fn kick_from_call(&self, conv: &str, actor: &str, target: &str) -> Result<()> {
        self.with_room(conv, |room| {
            room.kick(actor, target).map_err(map_room_error)
        })
    }

    pub fn lock_call(&self, conv: &str, actor: &str, locked: bool) -> Result<()> {
        self.with_room(conv, |room| {
            room.set_locked(actor, locked).map_err(map_room_error)
        })
    }

    pub fn record_call(&self, conv: &str, actor: &str, recording: bool) -> Result<()> {
        self.with_room(conv, |room| {
            room.set_recording(actor, recording).map_err(map_room_error)
        })
    }

    pub fn hand_in_call(&self, conv: &str, user: &str, raised: bool) -> Result<()> {
        self.with_room(conv, |room| {
            room.raise_hand(user, raised).map_err(map_room_error)
        })
    }

    pub fn transfer_call(&self, conv: &str, actor: &str, target: &str) -> Result<()> {
        self.with_room(conv, |room| {
            room.transfer_host(actor, target).map_err(map_room_error)
        })
    }

    /// 出口码率分配（参照 atm0s 的分配器）。`estimate_bps` 缺省用房间上限。
    pub fn call_egress(&self, conv: &str, user: &str, estimate_bps: u64) -> Result<EgressPlan> {
        self.with_room(conv, |room| {
            room.allocate_egress(user, estimate_bps)
                .map_err(map_room_error)
        })
    }

    /// 主持人离开后若房间移交，返回新主持人（供上层广播系统事件）。
    pub fn call_host(&self, conv: &str) -> Option<String> {
        let guard = self.calls.lock();
        guard.get(conv).map(|r| r.host().to_string())
    }
}

fn require_body_target(body: &TargetBody) -> Result<String> {
    validate::id(&body.target).map(|s| s.to_string())
}

#[derive(serde::Deserialize)]
pub struct StartBody {
    #[serde(default)]
    pub capacity: Option<usize>,
}

#[derive(serde::Deserialize)]
pub struct TargetBody {
    pub target: String,
}

#[derive(serde::Deserialize)]
pub struct MuteBody {
    /// `mute` 必填；`mute-all` 不需要（全员）。
    #[serde(default)]
    pub target: Option<String>,
    /// "audio" | "video"
    pub track: String,
    pub muted: bool,
}

#[derive(serde::Deserialize)]
pub struct ToggleBody {
    pub value: bool,
}

#[derive(serde::Deserialize, Default)]
pub struct EgressQuery {
    #[serde(default)]
    pub estimate_bps: Option<u64>,
}

async fn state(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    match platform.call_state(&conv, &authed.user_id)? {
        Some(view) => Ok(Json(serde_json::json!({
            "conv_id": view.conv_id,
            "live": view.live,
            "host": view.host,
            "locked": view.locked,
            "recording": view.recording,
            "participants": view.participants,
        }))),
        None => Ok(Json(serde_json::json!({
            "conv_id": conv, "live": false, "participants": []
        }))),
    }
}

async fn start(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    // 空 body 也合法（客户端只想用默认容量）。这里刻意不用 `Json<T>`：
    // 它会把空 body 直接判成 400，而"开房不带参数"是正常用法。
    body: axum::body::Bytes,
) -> std::result::Result<StatusCode, ApiError> {
    let capacity = if body.is_empty() {
        DEFAULT_CAPACITY
    } else {
        serde_json::from_slice::<StartBody>(&body)
            .ok()
            .and_then(|b| b.capacity)
            .unwrap_or(DEFAULT_CAPACITY)
    };
    platform.start_call(&conv, &authed.user_id, capacity)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn join(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.join_call(&conv, &authed.user_id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn leave(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.leave_call(&conv, &authed.user_id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn mute(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    Json(body): Json<MuteBody>,
) -> std::result::Result<StatusCode, ApiError> {
    let raw = body
        .target
        .as_deref()
        .ok_or_else(|| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "target"))?;
    let target = validate::id(raw)?.to_string();
    let track = match body.track.as_str() {
        "audio" => Track::Audio,
        "video" => Track::Video,
        _ => return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "track")),
    };
    platform.mute_in_call(&conv, &authed.user_id, &target, track, body.muted)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn mute_all(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    Json(body): Json<MuteBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let track = match body.track.as_str() {
        "audio" => Track::Audio,
        "video" => Track::Video,
        _ => return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "track")),
    };
    let affected = platform.mute_all_in_call(&conv, &authed.user_id, track, body.muted)?;
    Ok(Json(serde_json::json!({ "affected": affected })))
}

async fn kick(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    Json(body): Json<TargetBody>,
) -> std::result::Result<StatusCode, ApiError> {
    let target = require_body_target(&body)?;
    platform.kick_from_call(&conv, &authed.user_id, &target)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn lock(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    Json(body): Json<ToggleBody>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.lock_call(&conv, &authed.user_id, body.value)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn record(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    Json(body): Json<ToggleBody>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.record_call(&conv, &authed.user_id, body.value)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn hand(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    Json(body): Json<ToggleBody>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.hand_in_call(&conv, &authed.user_id, body.value)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn transfer(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    Json(body): Json<TargetBody>,
) -> std::result::Result<StatusCode, ApiError> {
    let target = require_body_target(&body)?;
    platform.transfer_call(&conv, &authed.user_id, &target)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn egress(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    axum::extract::Query(q): axum::extract::Query<EgressQuery>,
) -> std::result::Result<Json<EgressPlan>, ApiError> {
    let plan = platform.call_egress(
        &conv,
        &authed.user_id,
        q.estimate_bps
            .unwrap_or(wxlike_rtc::sfu::DEFAULT_MAX_EGRESS_BPS),
    )?;
    Ok(Json(plan))
}
