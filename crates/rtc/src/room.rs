//! 群通话房间制（ADR-7 / 合同 §3.7）：通话房间**独立于群消息管道**，
//! 不占用群信箱、不写序列号。房间是有状态的：谁在房里、谁能静音谁、
//! 锁门后谁能进——这些都不属于消息层。
//!
//! 与 1:1 通话（[`crate::call`]）的关系：房间复用同一套信令与媒体栈，
//! 只是把"两方状态机"换成"多方名册 + 管控动作"。所以本模块是纯状态机，
//! 不碰 str0m，也不碰网络——房间状态可以脱离媒体栈单独验收。
//!
//! 权限模型（合同 §3.7「静音/踢人/锁定/录制」）：
//!
//! | 动作 | 主持人 | 管理员 | 成员 | 本人 |
//! |---|---|---|---|---|
//! | 静音自己 | ✅ | ✅ | ✅ | — |
//! | 静音他人 | ✅ | ✅ | ❌ | — |
//! | 踢人 | ✅ | ✅（不能踢主持人/管理员） | ❌ | — |
//! | 锁定房间 | ✅ | ❌ | ❌ | — |
//! | 开始录制 | ✅ | ❌ | ❌ | — |
//! | 转让主持 | ✅ | ❌ | ❌ | — |
//!
//! 三条安全默认值：**锁门后成员进不来**（管理员仍可进，便于处置）、
//! **被踢者不可再进**（ban 名单随房间生命周期）、**主持人离开自动移交**
//! （移交给最早加入的管理员，没有则最早加入的成员；空房即结束）。

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::sfu::{EgressBitrateAllocator, TrackId, DEFAULT_MAX_EGRESS_BPS};

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

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Participant {
    pub user: String,
    pub role: Role,
    pub audio_muted: bool,
    pub video_muted: bool,
    pub hand_raised: bool,
    pub joined_at: u64,
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

/// 一次出口分配的结果（每个观众一份）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct EgressPlan {
    /// 各订阅轨的目标码率（顺序 = 订阅顺序）。
    pub tracks: Vec<(TrackId, u64)>,
    /// (current, desired) —— 交给 BWE/pacer 的目标区间。
    pub bwe: (u64, u64),
}

#[derive(Clone, Debug)]
pub struct Room {
    id: String,
    /// 房间归属（群/频道 id），空表示临时房间。
    owner_conv: String,
    host: String,
    locked: bool,
    recording: bool,
    capacity: usize,
    participants: BTreeMap<String, Participant>,
    /// 被踢名单：房间存活期间不可再进。
    banned: HashSet<String>,
    /// 每个观众一个出口码率分配器（参照 atm0s 的 EgressBitrateAllocator）。
    allocators: HashMap<String, EgressBitrateAllocator>,
    created_at: u64,
}

impl Room {
    pub fn new(
        id: &str,
        owner_conv: &str,
        host: &str,
        capacity: usize,
        now_ms: u64,
    ) -> Result<Self> {
        if !(2..=1000).contains(&capacity) {
            return Err(RoomError::BadCapacity);
        }
        let mut participants = BTreeMap::new();
        participants.insert(
            host.to_string(),
            Participant {
                user: host.to_string(),
                role: Role::Host,
                audio_muted: false,
                video_muted: false,
                hand_raised: false,
                joined_at: now_ms,
            },
        );
        let mut allocators = HashMap::new();
        allocators.insert(
            host.to_string(),
            EgressBitrateAllocator::new(DEFAULT_MAX_EGRESS_BPS),
        );
        Ok(Self {
            id: id.to_string(),
            owner_conv: owner_conv.to_string(),
            host: host.to_string(),
            locked: false,
            recording: false,
            capacity,
            participants,
            banned: HashSet::new(),
            allocators,
            created_at: now_ms,
        })
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
        self.recording
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

    /// 进房：满员拒绝、锁门拒绝（管理员/主持人例外，便于处置）、被踢者拒绝。
    pub fn join(&mut self, user: &str, now_ms: u64) -> Result<&Participant> {
        if self.participants.contains_key(user) {
            return Err(RoomError::AlreadyJoined);
        }
        if self.banned.contains(user) {
            return Err(RoomError::Banned);
        }
        if self.participants.len() >= self.capacity {
            return Err(RoomError::Full(self.capacity));
        }
        if self.locked {
            return Err(RoomError::Locked);
        }
        self.participants.insert(
            user.to_string(),
            Participant {
                user: user.to_string(),
                role: Role::Member,
                audio_muted: false,
                video_muted: false,
                hand_raised: false,
                joined_at: now_ms,
            },
        );
        // 进房即建出口分配器：订阅/结算都挂在它上面
        self.allocators.insert(
            user.to_string(),
            EgressBitrateAllocator::new(DEFAULT_MAX_EGRESS_BPS),
        );
        self.participants
            .get(user)
            .ok_or(RoomError::NoSuchParticipant)
    }

    /// 离房：主持人离开自动移交给最早加入的管理员/成员。
    pub fn leave(&mut self, user: &str) -> Result<LeaveOutcome> {
        if self.participants.remove(user).is_none() {
            return Err(RoomError::NotMember);
        }
        self.allocators.remove(user);
        if self.participants.is_empty() {
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
    pub fn set_mute(
        &mut self,
        actor: &str,
        target: &str,
        track: Track,
        muted: bool,
    ) -> Result<bool> {
        let actor_role = self.role_of(actor)?;
        if actor != target && !actor_role.can_moderate() {
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
        Ok(muted)
    }

    /// 全员静音/解除（**参照 Nextcloud Talk 的 force-mute**）：仅主持人/管理员。
    /// 返回受影响人数；主持人与管理员也会被静音（自己再解除即可，语义更简单）。
    pub fn mute_all(&mut self, actor: &str, track: Track, muted: bool) -> Result<usize> {
        if !self.role_of(actor)?.can_moderate() {
            return Err(RoomError::Forbidden);
        }
        for p in self.participants.values_mut() {
            match track {
                Track::Audio => p.audio_muted = muted,
                Track::Video => p.video_muted = muted,
            }
        }
        Ok(self.participants.len())
    }

    /// 踢人：主持人/管理员可踢；不能踢主持人；管理员不能踢管理员。
    pub fn kick(&mut self, actor: &str, target: &str) -> Result<()> {
        let actor_role = self.role_of(actor)?;
        if !actor_role.can_moderate() {
            return Err(RoomError::Forbidden);
        }
        let target_role = self.role_of(target)?;
        if target_role == Role::Host {
            return Err(RoomError::CannotKickHost);
        }
        if actor_role == Role::Moderator && target_role == Role::Moderator {
            return Err(RoomError::Forbidden);
        }
        self.participants.remove(target);
        self.allocators.remove(target);
        self.banned.insert(target.to_string());
        Ok(())
    }

    /// 锁门：仅主持人。锁门后成员进不来，管理员仍可进。
    pub fn set_locked(&mut self, actor: &str, locked: bool) -> Result<()> {
        self.require_host(actor)?;
        self.locked = locked;
        Ok(())
    }

    /// 录制开关：仅主持人（合同把录制列为会议管控动作）。
    pub fn set_recording(&mut self, actor: &str, recording: bool) -> Result<()> {
        self.require_host(actor)?;
        self.recording = recording;
        Ok(())
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
        Ok(())
    }

    /// 举手（主持人可看到申请发言队列）。
    pub fn raise_hand(&mut self, user: &str, raised: bool) -> Result<()> {
        let p = self
            .participants
            .get_mut(user)
            .ok_or(RoomError::NotMember)?;
        p.hand_raised = raised;
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
