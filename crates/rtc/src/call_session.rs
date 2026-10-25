//! G09-02 服务端裁决的 1:1 呼叫状态机。
//!
//! 客户端状态机（[`crate::state`]）只描述"我这端看到什么"；真正决定
//! "谁接听、谁忙线、哪个设备的令牌有效"的必须是服务端。本模块是那层
//! 权威：
//!
//! ```text
//! Ringing ──accept(第一个设备)──► Accepted ──SDP 开始──► Connecting ──媒体连通──► Active
//!    │                                │                       │
//!    ├──reject────────────────────────┴───────────────────────┴──► Ended(Rejected/HungUp/...)
//!    ├──cancel / 30s 未接 ──────────────────────────────────────► Ended(Cancelled/Timeout)
//!    └──退群 / 封禁 ────────────────────────────────────────────► Ended(AuthorizationRevoked)
//! ```
//!
//! 四条硬规则：
//!
//! 1. **多设备竞争由服务端 CAS**：邀请同时发到被叫的全部设备，第一个
//!    `accept` 赢；其余设备得到 `AnsweredElsewhere`，不再签发令牌。
//! 2. **忙线先于信令**：任一参与方已在未结束的通话里，邀请直接 `Busy`，
//!    不产生第二个呼叫 ID。
//! 3. **令牌绑定 user/device/call/代次**（[`crate::token`]）；退群/封禁让
//!    代次失效，旧令牌连重新入房都做不到。
//! 4. **结束是终态**：乱序到达的 accept/reject 不会复活通话；重复挂断幂等，
//!    通话记录只落一条。
//!
//! 通话记录经 [`CallRecordSink`] 落库；本模块自带 SQLite 实现
//! （[`crate::call_store::SqliteCallRecords`]），真实服务接线由 G00 按
//! `integration-requests/G09/G09-02.yaml` 执行。

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::call_store::CallRecordSink;
use crate::token::{MediaClaims, MediaScope, TokenError, TokenSigner};

/// 振铃超时：30 秒无人接听即结束（与客户端状态机一致）。
pub const RING_TIMEOUT_MS: u64 = 30_000;
/// 接听后媒体建连超时：30 秒还没 Connected 视为失败。
pub const CONNECT_TIMEOUT_MS: u64 = 30_000;
/// 媒体令牌寿命：足够完成 ICE/DTLS，过期后必须重新入房。
pub const MEDIA_TOKEN_TTL_MS: u64 = 120_000;

/// 一次呼叫里的设备身份。`user` 来自认证会话，`device` 来自设备注册表。
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DeviceRef {
    pub user: String,
    pub device: String,
}

impl DeviceRef {
    pub fn new(user: impl Into<String>, device: impl Into<String>) -> Self {
        Self {
            user: user.into(),
            device: device.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallState {
    Ringing,
    Accepted,
    Connecting,
    Active,
    Ended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    Rejected,
    Cancelled,
    HungUp,
    Busy,
    Timeout,
    Failed,
    AuthorizationRevoked,
}

/// 通话记录。落库后是审计与"最近通话"列表的唯一来源。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRecord {
    pub call_id: String,
    pub conv_id: String,
    pub caller: String,
    pub callee: String,
    pub caller_device: String,
    pub answer_device: Option<String>,
    pub video: bool,
    pub generation: u64,
    pub started_at_ms: u64,
    pub answered_at_ms: Option<u64>,
    pub ended_at_ms: u64,
    pub reason: EndReason,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CallEvent {
    Invited {
        call_id: String,
        generation: u64,
        devices: Vec<String>,
        expires_at_ms: u64,
    },
    Accepted {
        call_id: String,
        generation: u64,
        user: String,
        device: String,
    },
    AnsweredElsewhere {
        call_id: String,
        device: String,
        by_device: String,
    },
    Connecting {
        call_id: String,
    },
    Active {
        call_id: String,
    },
    Ended {
        call_id: String,
        reason: EndReason,
        ended_at_ms: u64,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CallError {
    #[error("call not found")]
    NotFound,
    #[error("user is not a participant of this call")]
    NotInvited,
    #[error("device was not invited")]
    DeviceNotInvited,
    #[error("participant is busy in call {0}")]
    Busy(String),
    #[error("call already ended")]
    AlreadyEnded,
    #[error("call was already answered on device {0}")]
    AnsweredElsewhere(String),
    #[error("cannot {action} while call is {state:?}")]
    Illegal {
        action: &'static str,
        state: CallState,
    },
    #[error("membership was revoked")]
    MembershipRevoked,
    #[error("media token rejected: {0}")]
    Token(#[from] TokenError),
    #[error("invalid call request: {0}")]
    BadInput(&'static str),
}

/// 结束类命令的结果。`already_ended` 为真表示这是重复提交（幂等成功）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndOutcome {
    pub ended: bool,
    pub already_ended: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptOutcome {
    pub event: CallEvent,
    pub token: String,
}

/// 一次呼叫的服务端事实。所有时间都由调用方传入，便于确定性测试。
#[derive(Clone, Debug)]
pub struct CallSession {
    call_id: String,
    conv_id: String,
    caller: DeviceRef,
    callee: String,
    invited_devices: BTreeSet<String>,
    video: bool,
    generation: u64,
    state: CallState,
    answered_by: Option<DeviceRef>,
    started_at_ms: u64,
    ring_deadline_ms: u64,
    answered_at_ms: Option<u64>,
    connect_deadline_ms: Option<u64>,
    ended_at_ms: Option<u64>,
    end_reason: Option<EndReason>,
    record_taken: bool,
}

impl CallSession {
    pub fn new(
        call_id: impl Into<String>,
        conv_id: impl Into<String>,
        caller: DeviceRef,
        callee: impl Into<String>,
        devices: Vec<String>,
        video: bool,
        now_ms: u64,
    ) -> Result<Self, CallError> {
        let callee = callee.into();
        let devices: BTreeSet<String> = devices.into_iter().filter(|d| !d.is_empty()).collect();
        if caller.user == callee {
            return Err(CallError::BadInput("cannot call yourself"));
        }
        if devices.is_empty() {
            return Err(CallError::BadInput("callee has no registered device"));
        }
        Ok(Self {
            call_id: call_id.into(),
            conv_id: conv_id.into(),
            caller,
            callee,
            invited_devices: devices,
            video,
            generation: 1,
            state: CallState::Ringing,
            answered_by: None,
            started_at_ms: now_ms,
            ring_deadline_ms: now_ms.saturating_add(RING_TIMEOUT_MS),
            answered_at_ms: None,
            connect_deadline_ms: None,
            ended_at_ms: None,
            end_reason: None,
            record_taken: false,
        })
    }

    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    pub fn conv_id(&self) -> &str {
        &self.conv_id
    }

    pub fn caller(&self) -> &DeviceRef {
        &self.caller
    }

    pub fn callee(&self) -> &str {
        &self.callee
    }

    pub fn state(&self) -> CallState {
        self.state
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn answered_by(&self) -> Option<&DeviceRef> {
        self.answered_by.as_ref()
    }

    pub fn video(&self) -> bool {
        self.video
    }

    pub fn ring_deadline_ms(&self) -> u64 {
        self.ring_deadline_ms
    }

    pub fn end_reason(&self) -> Option<EndReason> {
        self.end_reason
    }

    pub fn is_ended(&self) -> bool {
        self.state == CallState::Ended
    }

    pub fn is_participant(&self, user: &str) -> bool {
        self.caller.user == user || self.callee == user
    }

    /// 入房/换令牌阶段的设备校验：只认赢下接听竞争的那个设备。
    fn active_device_ok(&self, user: &str, device: &str) -> bool {
        if user == self.callee {
            self.answered_by
                .as_ref()
                .is_some_and(|d| d.user == user && d.device == device)
        } else {
            user == self.caller.user && device == self.caller.device
        }
    }

    /// 被叫接听。第一个合法设备获胜，其余设备收到 `AnsweredElsewhere`。
    pub fn accept(
        &mut self,
        user: &str,
        device: &str,
        now_ms: u64,
    ) -> Result<CallEvent, CallError> {
        self.expire_if_needed(now_ms)?;
        if user != self.callee {
            return Err(CallError::NotInvited);
        }
        if !matches!(self.state, CallState::Ringing) {
            return self.repeat_accept(device);
        }
        if !self.invited_devices.contains(device) {
            return Err(CallError::DeviceNotInvited);
        }
        self.state = CallState::Accepted;
        self.answered_by = Some(DeviceRef::new(user, device));
        self.answered_at_ms = Some(now_ms);
        self.connect_deadline_ms = Some(now_ms.saturating_add(CONNECT_TIMEOUT_MS));
        Ok(self.accepted_event(user, device))
    }

    /// 已被接听后的重入：同一设备幂等成功，其他设备得到 AnsweredElsewhere。
    fn repeat_accept(&self, device: &str) -> Result<CallEvent, CallError> {
        let winner = self.answered_by.as_ref().ok_or(CallError::AlreadyEnded)?;
        if winner.device == device {
            return Ok(self.accepted_event(&winner.user, device));
        }
        Err(CallError::AnsweredElsewhere(winner.device.clone()))
    }

    fn accepted_event(&self, user: &str, device: &str) -> CallEvent {
        CallEvent::Accepted {
            call_id: self.call_id.clone(),
            generation: self.generation,
            user: user.to_string(),
            device: device.to_string(),
        }
    }

    /// SDP 开始交换。幂等：重复调用不改变状态。
    pub fn begin_connecting(&mut self, user: &str) -> Result<CallEvent, CallError> {
        self.require_participant(user)?;
        match self.state {
            CallState::Accepted => self.state = CallState::Connecting,
            CallState::Connecting | CallState::Active => {}
            _ => return Err(self.illegal("begin connecting")),
        }
        Ok(CallEvent::Connecting {
            call_id: self.call_id.clone(),
        })
    }

    /// 媒体层报告 ICE/DTLS Connected。
    pub fn media_connected(&mut self, user: &str) -> Result<CallEvent, CallError> {
        self.require_participant(user)?;
        match self.state {
            CallState::Accepted | CallState::Connecting => self.state = CallState::Active,
            CallState::Active => {}
            _ => return Err(self.illegal("mark media connected")),
        }
        Ok(CallEvent::Active {
            call_id: self.call_id.clone(),
        })
    }

    pub fn reject(
        &mut self,
        user: &str,
        device: &str,
        now_ms: u64,
    ) -> Result<EndOutcome, CallError> {
        if self.is_ended() {
            return Ok(EndOutcome {
                ended: true,
                already_ended: true,
            });
        }
        if user != self.callee || !self.invited_devices.contains(device) {
            return Err(CallError::NotInvited);
        }
        if self.state != CallState::Ringing {
            return Err(self.illegal("reject"));
        }
        self.end(EndReason::Rejected, now_ms);
        Ok(EndOutcome {
            ended: true,
            already_ended: false,
        })
    }

    /// 主叫撤回振铃。已结束则幂等成功。
    pub fn cancel(&mut self, user: &str, now_ms: u64) -> Result<EndOutcome, CallError> {
        if self.is_ended() {
            return Ok(EndOutcome {
                ended: true,
                already_ended: true,
            });
        }
        if user != self.caller.user {
            return Err(CallError::NotInvited);
        }
        if self.state != CallState::Ringing {
            return Err(self.illegal("cancel"));
        }
        self.end(EndReason::Cancelled, now_ms);
        Ok(EndOutcome {
            ended: true,
            already_ended: false,
        })
    }

    /// 挂断。振铃阶段按角色映射为撤回/拒接；否则 HungUp。重复提交幂等。
    pub fn hangup(&mut self, user: &str, now_ms: u64) -> Result<EndOutcome, CallError> {
        if self.is_ended() {
            return Ok(EndOutcome {
                ended: true,
                already_ended: true,
            });
        }
        self.require_participant(user)?;
        let reason = match (self.state, user == self.caller.user) {
            (CallState::Ringing, true) => EndReason::Cancelled,
            (CallState::Ringing, false) => EndReason::Rejected,
            _ => EndReason::HungUp,
        };
        self.end(reason, now_ms);
        Ok(EndOutcome {
            ended: true,
            already_ended: false,
        })
    }

    /// 成员资格被撤销（退群/封禁）：结束通话并让所有旧令牌失效。
    pub fn revoke_membership(&mut self, user: &str, now_ms: u64) -> Option<CallEvent> {
        if self.is_ended() || !self.is_participant(user) {
            return None;
        }
        self.generation = self.generation.saturating_add(1);
        self.end(EndReason::AuthorizationRevoked, now_ms);
        self.end_reason.map(|reason| CallEvent::Ended {
            call_id: self.call_id.clone(),
            reason,
            ended_at_ms: now_ms,
        })
    }

    /// 成员变化触发密钥轮换：只推进代次，不结束通话。
    pub fn rotate_generation(&mut self) -> u64 {
        self.generation = self.generation.saturating_add(1);
        self.generation
    }

    /// 定时器：振铃/建连超时。
    pub fn tick(&mut self, now_ms: u64) -> Option<CallEvent> {
        if self.is_ended() {
            return None;
        }
        if self.state == CallState::Ringing && now_ms >= self.ring_deadline_ms {
            self.end(EndReason::Timeout, now_ms);
        } else if matches!(self.state, CallState::Accepted | CallState::Connecting)
            && self.connect_deadline_ms.is_some_and(|d| now_ms >= d)
        {
            self.end(EndReason::Failed, now_ms);
        } else {
            return None;
        }
        Some(CallEvent::Ended {
            call_id: self.call_id.clone(),
            reason: self.end_reason.unwrap_or(EndReason::Failed),
            ended_at_ms: now_ms,
        })
    }

    /// 取走待落库的记录（只返回一次，防止重复挂断写两条）。
    pub fn take_record(&mut self) -> Option<CallRecord> {
        if !self.is_ended() || self.record_taken {
            return None;
        }
        self.record_taken = true;
        Some(CallRecord {
            call_id: self.call_id.clone(),
            conv_id: self.conv_id.clone(),
            caller: self.caller.user.clone(),
            callee: self.callee.clone(),
            caller_device: self.caller.device.clone(),
            answer_device: self.answered_by.as_ref().map(|d| d.device.clone()),
            video: self.video,
            generation: self.generation,
            started_at_ms: self.started_at_ms,
            answered_at_ms: self.answered_at_ms,
            ended_at_ms: self.ended_at_ms.unwrap_or(self.started_at_ms),
            reason: self.end_reason.unwrap_or(EndReason::Failed),
        })
    }

    fn expire_if_needed(&mut self, now_ms: u64) -> Result<(), CallError> {
        if self.state == CallState::Ringing && now_ms >= self.ring_deadline_ms {
            self.end(EndReason::Timeout, now_ms);
            return Err(CallError::AlreadyEnded);
        }
        Ok(())
    }

    fn require_participant(&self, user: &str) -> Result<(), CallError> {
        if self.is_participant(user) {
            Ok(())
        } else {
            Err(CallError::NotInvited)
        }
    }

    fn illegal(&self, action: &'static str) -> CallError {
        CallError::Illegal {
            action,
            state: self.state,
        }
    }

    fn end(&mut self, reason: EndReason, now_ms: u64) {
        self.state = CallState::Ended;
        self.end_reason = Some(reason);
        self.ended_at_ms = Some(now_ms);
    }
}

/// 服务端需要的成员资格来源（G02 的会话/成员表）。测试与并行开发可以用
/// 真实集合实现；最终集成必须接平台成员表。
pub trait Membership {
    fn member_allowed(&self, conv_id: &str, user: &str) -> bool;
}

/// 一次邀请的输出：调用方据此把 ring 推给被叫的全部设备。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InviteOutcome {
    pub call_id: String,
    pub generation: u64,
    pub devices: Vec<String>,
    pub expires_at_ms: u64,
}

/// 服务端呼叫做者。所有命令都先鉴权/查忙，再改状态。
pub struct CallServer<M: Membership, R: CallRecordSink> {
    signer: TokenSigner,
    membership: M,
    records: R,
    calls: std::collections::HashMap<String, CallSession>,
    /// user -> call_id（忙线索引；结束即移除）。
    busy: std::collections::HashMap<String, String>,
    seq: u64,
}

impl<M: Membership, R: CallRecordSink> CallServer<M, R> {
    pub fn new(signer: TokenSigner, membership: M, records: R) -> Self {
        Self {
            signer,
            membership,
            records,
            calls: std::collections::HashMap::new(),
            busy: std::collections::HashMap::new(),
            seq: 0,
        }
    }

    pub fn signer(&self) -> &TokenSigner {
        &self.signer
    }

    pub fn membership(&self) -> &M {
        &self.membership
    }

    /// 成员表变更（退群/封禁）后，服务端把新状态交回状态机。
    pub fn membership_mut(&mut self) -> &mut M {
        &mut self.membership
    }

    pub fn records(&self) -> &R {
        &self.records
    }

    pub fn session(&self, call_id: &str) -> Option<&CallSession> {
        self.calls.get(call_id)
    }

    pub fn len(&self) -> usize {
        self.calls.len()
    }

    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    pub fn invite(
        &mut self,
        caller: DeviceRef,
        conv_id: &str,
        callee: &str,
        devices: Vec<String>,
        video: bool,
        now_ms: u64,
    ) -> Result<InviteOutcome, CallError> {
        if conv_id.is_empty() || callee.is_empty() {
            return Err(CallError::BadInput("conv and callee are required"));
        }
        if caller.user == callee {
            return Err(CallError::BadInput("cannot call yourself"));
        }
        if !self.membership.member_allowed(conv_id, &caller.user)
            || !self.membership.member_allowed(conv_id, callee)
        {
            return Err(CallError::MembershipRevoked);
        }
        if let Some(busy) = self.busy_call(&caller.user, callee) {
            return Err(CallError::Busy(busy));
        }
        let call_id = self.next_call_id(now_ms);
        let devices: Vec<String> = devices.into_iter().filter(|d| !d.is_empty()).collect();
        let session = CallSession::new(
            &call_id,
            conv_id,
            caller.clone(),
            callee,
            devices,
            video,
            now_ms,
        )?;
        let outcome = InviteOutcome {
            call_id: call_id.clone(),
            generation: session.generation(),
            devices: session.invited_devices.iter().cloned().collect(),
            expires_at_ms: session.ring_deadline_ms(),
        };
        self.busy.insert(caller.user.clone(), call_id.clone());
        self.busy.insert(callee.to_string(), call_id.clone());
        self.calls.insert(call_id, session);
        Ok(outcome)
    }

    /// 被叫设备接听；只有赢家拿到媒体令牌。
    pub fn accept(
        &mut self,
        call_id: &str,
        user: &str,
        device: &str,
        now_ms: u64,
    ) -> Result<AcceptOutcome, CallError> {
        let (event, generation) = {
            let session = self.calls.get_mut(call_id).ok_or(CallError::NotFound)?;
            let event = session.accept(user, device, now_ms)?;
            (event, session.generation())
        };
        let token = self.issue_token(call_id, user, device, generation, now_ms)?;
        Ok(AcceptOutcome { event, token })
    }

    pub fn begin_connecting(&mut self, call_id: &str, user: &str) -> Result<CallEvent, CallError> {
        let session = self.calls.get_mut(call_id).ok_or(CallError::NotFound)?;
        session.begin_connecting(user)
    }

    pub fn media_connected(&mut self, call_id: &str, user: &str) -> Result<CallEvent, CallError> {
        let session = self.calls.get_mut(call_id).ok_or(CallError::NotFound)?;
        session.media_connected(user)
    }

    pub fn reject(
        &mut self,
        call_id: &str,
        user: &str,
        device: &str,
        now_ms: u64,
    ) -> Result<EndOutcome, CallError> {
        let session = self.calls.get_mut(call_id).ok_or(CallError::NotFound)?;
        let outcome = session.reject(user, device, now_ms)?;
        self.finish(call_id);
        Ok(outcome)
    }

    pub fn cancel(
        &mut self,
        call_id: &str,
        user: &str,
        now_ms: u64,
    ) -> Result<EndOutcome, CallError> {
        let session = self.calls.get_mut(call_id).ok_or(CallError::NotFound)?;
        let outcome = session.cancel(user, now_ms)?;
        self.finish(call_id);
        Ok(outcome)
    }

    pub fn hangup(
        &mut self,
        call_id: &str,
        user: &str,
        now_ms: u64,
    ) -> Result<EndOutcome, CallError> {
        let session = self.calls.get_mut(call_id).ok_or(CallError::NotFound)?;
        let outcome = session.hangup(user, now_ms)?;
        self.finish(call_id);
        Ok(outcome)
    }

    /// 重新入房 / ICE 重启：再次校验成员资格并签发新令牌。
    pub fn rejoin(
        &mut self,
        call_id: &str,
        user: &str,
        device: &str,
        now_ms: u64,
    ) -> Result<String, CallError> {
        let generation = {
            let session = self.calls.get_mut(call_id).ok_or(CallError::NotFound)?;
            if session.is_ended() {
                return Err(CallError::AlreadyEnded);
            }
            if !session.is_participant(user) || !session.active_device_ok(user, device) {
                return Err(CallError::NotInvited);
            }
            if !self.membership.member_allowed(session.conv_id(), user) {
                session.revoke_membership(user, now_ms);
                self.finish(call_id);
                return Err(CallError::MembershipRevoked);
            }
            session.generation()
        };
        self.issue_token(call_id, user, device, generation, now_ms)
    }

    /// 校验媒体令牌：签名、时效、call、scope、代次，以及通话仍在进行。
    pub fn verify_media_token(&self, token: &str, now_ms: u64) -> Result<MediaClaims, CallError> {
        let claims = self.signer.verify(token, now_ms)?;
        let session = self.calls.get(&claims.call).ok_or(CallError::NotFound)?;
        if session.is_ended() {
            return Err(CallError::AlreadyEnded);
        }
        if claims.scope != MediaScope::Call {
            return Err(CallError::Token(TokenError::WrongScope));
        }
        if claims.generation != session.generation() {
            return Err(CallError::Token(TokenError::StaleGeneration));
        }
        if !session.active_device_ok(&claims.user, &claims.device) {
            return Err(CallError::Token(TokenError::WrongCall));
        }
        Ok(claims)
    }

    /// 退群/封禁：结束该用户在当前会话里的通话，并推进代次让旧令牌作废。
    pub fn revoke_member(&mut self, conv_id: &str, user: &str, now_ms: u64) -> Vec<CallEvent> {
        let affected: Vec<String> = self
            .calls
            .values()
            .filter(|s| s.conv_id() == conv_id && s.is_participant(user) && !s.is_ended())
            .map(|s| s.call_id().to_string())
            .collect();
        let mut events = Vec::new();
        for call_id in affected {
            if let Some(session) = self.calls.get_mut(&call_id) {
                if let Some(ev) = session.revoke_membership(user, now_ms) {
                    events.push(ev);
                }
            }
            self.finish(&call_id);
        }
        events
    }

    /// 成员变化（非移除）触发媒体密钥轮换。
    pub fn rotate_generation(&mut self, call_id: &str) -> Result<u64, CallError> {
        self.calls
            .get_mut(call_id)
            .ok_or(CallError::NotFound)
            .map(CallSession::rotate_generation)
    }

    /// 定时器：返回所有因超时结束的事件。
    pub fn tick(&mut self, now_ms: u64) -> Vec<CallEvent> {
        let ids: Vec<String> = self.calls.keys().cloned().collect();
        let mut events = Vec::new();
        for id in ids {
            let ended = self.calls.get_mut(&id).and_then(|s| s.tick(now_ms));
            if let Some(ev) = ended {
                events.push(ev);
                self.finish(&id);
            }
        }
        events
    }

    fn busy_call(&self, caller: &str, callee: &str) -> Option<String> {
        [caller, callee]
            .into_iter()
            .find_map(|u| self.busy.get(u).cloned())
    }

    fn next_call_id(&mut self, now_ms: u64) -> String {
        self.seq += 1;
        format!("c{now_ms:x}-{:x}", self.seq)
    }

    fn issue_token(
        &self,
        call_id: &str,
        user: &str,
        device: &str,
        generation: u64,
        now_ms: u64,
    ) -> Result<String, CallError> {
        Ok(self.signer.issue_for(
            call_id,
            user,
            device,
            MediaScope::Call,
            generation,
            now_ms,
            MEDIA_TOKEN_TTL_MS,
        )?)
    }

    /// 通话若已结束：落一条记录、清忙线索引。幂等。
    fn finish(&mut self, call_id: &str) {
        let Some(session) = self.calls.get_mut(call_id) else {
            return;
        };
        if !session.is_ended() {
            return;
        }
        self.busy.remove(&session.caller().user.clone());
        self.busy.remove(session.callee());
        if let Some(record) = session.take_record() {
            let _ = self.records.save(&record);
        }
    }
}
