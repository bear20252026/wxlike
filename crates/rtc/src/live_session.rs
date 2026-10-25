//! G09-05 直播/观看的服务端裁决。
//!
//! 直播与通话共享媒体栈与 SFU，但**权限面必须分开**：主播、观众、连麦者
//! 拿的是三种 `MediaScope` 的短时令牌（`LivePublish` / `LiveWatch` /
//! `LiveCohost`），call scope 的令牌进不了直播间，观众令牌也永远不含上行
//! 麦克风权限。
//!
//! 关键行为：
//!
//! * 观众计数按**用户**去重，多设备只增加连接数，不虚增人气；
//! * 主播断网有 `RECONNECT_GRACE_MS` 宽限期，重连后令牌代次前滚，
//!   超时则结束并释放全部观看连接；
//! * 停播是同步动作：先撤 G05 Feed 的源，再结束归档任务并踢掉观看者；
//! * 弱网恢复是显式状态机（Buffering / Playing / Stalled / AwaitingGesture），
//!   浏览器自动播放被拦截时宁可停在 `AwaitingGesture`，也不假装在播。

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::token::{MediaClaims, MediaScope, TokenError, TokenSigner};

/// 主播断网后保留直播间的宽限期。
pub const RECONNECT_GRACE_MS: u64 = 60_000;
/// 观看/推流令牌的默认寿命；长直播由客户端续签。
pub const LIVE_TOKEN_TTL_MS: u64 = 120_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveEndReason {
    /// 主播主动下播。
    HostStopped,
    /// 断网宽限期结束仍未回来。
    HostLost,
    /// 房间被运营/治理强制停播。
    Forced,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum LiveState {
    Live,
    Reconnecting { since_ms: u64, deadline_ms: u64 },
    Ended(LiveEndReason),
}

impl LiveState {
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Live)
    }

    pub fn is_ended(&self) -> bool {
        matches!(self, Self::Ended(_))
    }

    pub fn end_reason(&self) -> Option<LiveEndReason> {
        match self {
            Self::Ended(reason) => Some(*reason),
            _ => None,
        }
    }
}

/// 播放状态。只把 `Playing` 当"在播"，其余都是需要 UI 明说的恢复态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "playback")]
pub enum PlaybackState {
    Buffering,
    Playing,
    Stalled {
        since_ms: u64,
    },
    /// 浏览器要求用户手势后才能出声/播放。
    AwaitingGesture,
}

impl PlaybackState {
    pub fn is_playing(self) -> bool {
        self == Self::Playing
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct LiveReport {
    pub reporter: String,
    pub reason: String,
    pub at_ms: u64,
}

#[derive(Clone, Debug)]
struct DeviceView {
    playback: PlaybackState,
    joined_at_ms: u64,
}

#[derive(Clone, Debug)]
struct Viewer {
    devices: BTreeMap<String, DeviceView>,
    joined_at_ms: u64,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LiveError {
    #[error("stream not found")]
    NotFound,
    #[error("not permitted for this live stream")]
    Forbidden,
    #[error("live stream already ended")]
    Ended,
    #[error("user is banned from this live stream")]
    Banned,
    #[error("live stream is not live")]
    NotLive,
    #[error("user is not watching this stream")]
    NotWatching,
    #[error("invalid playback transition from {0:?}")]
    BadPlayback(PlaybackState),
    #[error("token rejected: {0}")]
    Token(#[from] TokenError),
}

/// G05 Feed / G04 录播的端口。实现方负责真正写 Feed、起停归档任务；
/// 本模块只保证调用顺序与幂等。
pub trait LiveSideEffects {
    fn feed_live(&mut self, stream_id: &str, host: &str, now_ms: u64);
    fn feed_ended(&mut self, stream_id: &str, now_ms: u64);
    /// 停播/超时的第一动作：立刻撤掉媒体源，避免 Feed 还挂着死流。
    fn source_revoked(&mut self, stream_id: &str, now_ms: u64);
    fn archive_started(&mut self, _stream_id: &str, _now_ms: u64) {}
    fn archive_stopped(&mut self, _stream_id: &str, _now_ms: u64) {}
}

/// 没有接 Feed/G04 时的空实现（例如只做媒体转发的单元测试环境）。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopSideEffects;

impl LiveSideEffects for NoopSideEffects {
    fn feed_live(&mut self, _stream_id: &str, _host: &str, _now_ms: u64) {}
    fn feed_ended(&mut self, _stream_id: &str, _now_ms: u64) {}
    fn source_revoked(&mut self, _stream_id: &str, _now_ms: u64) {}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveStart {
    pub stream_id: String,
    pub publish_token: String,
    pub already_live: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchTicket {
    pub token: String,
    pub stream_id: String,
    /// 去重后的观众人数。
    pub viewer_count: usize,
    /// 实际观看连接数（同人多设备会 > viewer_count）。
    pub connections: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopOutcome {
    pub ended: bool,
    pub already_ended: bool,
    /// 被断开的观看用户（去重）。
    pub disconnected: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct LiveStream {
    id: String,
    conv_id: String,
    host: String,
    state: LiveState,
    generation: u64,
    started_at_ms: u64,
    ended_at_ms: Option<u64>,
    viewers: BTreeMap<String, Viewer>,
    cohosts: BTreeSet<String>,
    pending_cohosts: BTreeSet<String>,
    banned: BTreeSet<String>,
    reports: Vec<LiveReport>,
    reported_by: BTreeSet<String>,
    archive: bool,
}

impl LiveStream {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn conv_id(&self) -> &str {
        &self.conv_id
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn state(&self) -> &LiveState {
        &self.state
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn started_at_ms(&self) -> u64 {
        self.started_at_ms
    }

    pub fn ended_at_ms(&self) -> Option<u64> {
        self.ended_at_ms
    }

    /// 观众去重人数。
    pub fn viewer_count(&self) -> usize {
        self.viewers.len()
    }

    /// 观看连接数（同一用户多设备分别计数）。
    pub fn connections(&self) -> usize {
        self.viewers.values().map(|v| v.devices.len()).sum()
    }

    pub fn viewers(&self) -> Vec<&str> {
        self.viewers.keys().map(String::as_str).collect()
    }

    pub fn is_watching(&self, user: &str, device: &str) -> bool {
        self.viewers
            .get(user)
            .is_some_and(|v| v.devices.contains_key(device))
    }

    pub fn playback(&self, user: &str, device: &str) -> Option<PlaybackState> {
        self.viewers
            .get(user)
            .and_then(|v| v.devices.get(device))
            .map(|d| d.playback)
    }

    /// 该观众首次进入的时间（多设备时取最早）。
    pub fn viewer_since(&self, user: &str) -> Option<u64> {
        self.viewers.get(user).map(|v| v.joined_at_ms)
    }

    /// 某台设备开始观看的时间。
    pub fn device_since(&self, user: &str, device: &str) -> Option<u64> {
        self.viewers
            .get(user)
            .and_then(|v| v.devices.get(device))
            .map(|d| d.joined_at_ms)
    }

    pub fn cohosts(&self) -> Vec<&str> {
        self.cohosts.iter().map(String::as_str).collect()
    }

    pub fn is_cohost(&self, user: &str) -> bool {
        self.cohosts.contains(user)
    }

    pub fn pending_cohosts(&self) -> Vec<&str> {
        self.pending_cohosts.iter().map(String::as_str).collect()
    }

    pub fn is_banned(&self, user: &str) -> bool {
        self.banned.contains(user)
    }

    pub fn reports(&self) -> &[LiveReport] {
        &self.reports
    }

    pub fn is_archiving(&self) -> bool {
        self.archive
    }

    /// 宽限期内恢复推流：代次前滚，正在播的观看端回落到 Buffering。
    /// 宽限期已过或已结束返回 `None`。
    fn resume_generation(&mut self, now_ms: u64) -> Option<(u64, bool)> {
        match self.state {
            LiveState::Live => Some((self.generation, false)),
            LiveState::Reconnecting { deadline_ms, .. } if now_ms <= deadline_ms => {
                self.generation = self.generation.saturating_add(1);
                self.state = LiveState::Live;
                self.soften_playback();
                Some((self.generation, true))
            }
            _ => None,
        }
    }

    /// 主播回来意味着流可能已经换路：不能假装画面还在播。
    fn soften_playback(&mut self) {
        for viewer in self.viewers.values_mut() {
            for d in viewer.devices.values_mut() {
                if d.playback == PlaybackState::Playing {
                    d.playback = PlaybackState::Buffering;
                }
            }
        }
    }
}

/// 直播服务：一进程内可同时有多个直播间。
pub struct LiveServer<S: LiveSideEffects> {
    signer: TokenSigner,
    effects: S,
    streams: HashMap<String, LiveStream>,
}

impl<S: LiveSideEffects> LiveServer<S> {
    pub fn new(signer: TokenSigner, effects: S) -> Self {
        Self {
            signer,
            effects,
            streams: HashMap::new(),
        }
    }

    pub fn stream(&self, stream_id: &str) -> Option<&LiveStream> {
        self.streams.get(stream_id)
    }

    pub fn effects(&self) -> &S {
        &self.effects
    }

    pub fn effects_mut(&mut self) -> &mut S {
        &mut self.effects
    }

    fn stream_mut(&mut self, stream_id: &str) -> Result<&mut LiveStream, LiveError> {
        self.streams.get_mut(stream_id).ok_or(LiveError::NotFound)
    }

    fn live_mut(&mut self, stream_id: &str) -> Result<&mut LiveStream, LiveError> {
        let s = self.stream_mut(stream_id)?;
        if s.state.is_ended() {
            return Err(LiveError::Ended);
        }
        Ok(s)
    }

    /// 开播。同一主播重复调用是幂等的（不重复写 Feed），但会换发新令牌。
    pub fn start(
        &mut self,
        host: &str,
        stream_id: &str,
        conv_id: &str,
        now_ms: u64,
    ) -> Result<LiveStart, LiveError> {
        if self.streams.contains_key(stream_id) {
            return self.start_existing(host, stream_id, now_ms);
        }
        let stream = LiveStream {
            id: stream_id.to_string(),
            conv_id: conv_id.to_string(),
            host: host.to_string(),
            state: LiveState::Live,
            generation: 1,
            started_at_ms: now_ms,
            ended_at_ms: None,
            viewers: BTreeMap::new(),
            cohosts: BTreeSet::new(),
            pending_cohosts: BTreeSet::new(),
            banned: BTreeSet::new(),
            reports: Vec::new(),
            reported_by: BTreeSet::new(),
            archive: false,
        };
        self.effects.feed_live(stream_id, host, now_ms);
        self.streams.insert(stream_id.to_string(), stream);
        self.publish_ticket(stream_id, host, now_ms, false)
    }

    /// 已存在的直播间：别人开不了，主播自己重复开是幂等/续接。
    fn start_existing(
        &mut self,
        host: &str,
        stream_id: &str,
        now_ms: u64,
    ) -> Result<LiveStart, LiveError> {
        let existing = self.streams.get(stream_id).expect("checked caller");
        if existing.host != host {
            return Err(LiveError::Forbidden);
        }
        match existing.state {
            LiveState::Ended(_) => Err(LiveError::Ended),
            LiveState::Reconnecting { .. } => self.resume(host, stream_id, now_ms),
            LiveState::Live => self.publish_ticket(stream_id, host, now_ms, true),
        }
    }

    fn publish_ticket(
        &self,
        stream_id: &str,
        host: &str,
        now_ms: u64,
        already_live: bool,
    ) -> Result<LiveStart, LiveError> {
        let stream = self.streams.get(stream_id).expect("caller checked");
        let token = self.issue(
            stream_id,
            host,
            "host",
            MediaScope::LivePublish,
            stream.generation,
            now_ms,
        )?;
        Ok(LiveStart {
            stream_id: stream_id.to_string(),
            publish_token: token,
            already_live,
        })
    }

    /// 观看：观众加入（同人同设备重复调用幂等）。
    ///
    /// `require_gesture = true` 时初始状态是 `AwaitingGesture`——浏览器没有
    /// 用户手势就不能自动播放，服务端不会把它算成"正在播放"。
    pub fn watch(
        &mut self,
        stream_id: &str,
        user: &str,
        device: &str,
        require_gesture: bool,
        now_ms: u64,
    ) -> Result<WatchTicket, LiveError> {
        let (generation, count, connections) = {
            let stream = self.live_mut(stream_id)?;
            if user == stream.host {
                return Err(LiveError::Forbidden);
            }
            if stream.banned.contains(user) {
                return Err(LiveError::Banned);
            }
            let playback = if require_gesture {
                PlaybackState::AwaitingGesture
            } else {
                PlaybackState::Buffering
            };
            let entry = stream.viewers.entry(user.to_string()).or_insert(Viewer {
                devices: BTreeMap::new(),
                joined_at_ms: now_ms,
            });
            entry
                .devices
                .entry(device.to_string())
                .or_insert(DeviceView {
                    playback,
                    joined_at_ms: now_ms,
                });
            (
                stream.generation,
                stream.viewer_count(),
                stream.connections(),
            )
        };
        let token = self.issue(
            stream_id,
            user,
            device,
            MediaScope::LiveWatch,
            generation,
            now_ms,
        )?;
        Ok(WatchTicket {
            token,
            stream_id: stream_id.to_string(),
            viewer_count: count,
            connections,
        })
    }

    /// 观众退房（或多设备中的一台退出）。返回剩余连接数；未知连接返回 `None`。
    pub fn leave(&mut self, stream_id: &str, user: &str, device: &str) -> Option<usize> {
        let stream = self.streams.get_mut(stream_id)?;
        let viewer = stream.viewers.get_mut(user)?;
        viewer.devices.remove(device)?;
        if viewer.devices.is_empty() {
            stream.viewers.remove(user);
            stream.cohosts.remove(user);
            stream.pending_cohosts.remove(user);
        }
        Some(stream.connections())
    }

    /// 主播断网：进入宽限期，观看端不会立即被踢。
    pub fn host_offline(
        &mut self,
        host: &str,
        stream_id: &str,
        now_ms: u64,
    ) -> Result<(), LiveError> {
        let stream = self.live_mut(stream_id)?;
        if stream.host != host {
            return Err(LiveError::Forbidden);
        }
        if let LiveState::Live = stream.state {
            stream.state = LiveState::Reconnecting {
                since_ms: now_ms,
                deadline_ms: now_ms.saturating_add(RECONNECT_GRACE_MS),
            };
        }
        Ok(())
    }

    /// 主播在宽限期内回来：代次前滚，旧推流令牌全部失效，换发新令牌。
    pub fn host_reconnect(
        &mut self,
        host: &str,
        stream_id: &str,
        now_ms: u64,
    ) -> Result<LiveStart, LiveError> {
        self.resume(host, stream_id, now_ms)
    }

    fn resume(&mut self, host: &str, stream_id: &str, now_ms: u64) -> Result<LiveStart, LiveError> {
        let (generation, resumed) = {
            let stream = self.live_mut(stream_id)?;
            if stream.host != host {
                return Err(LiveError::Forbidden);
            }
            stream.resume_generation(now_ms).ok_or(LiveError::Ended)?
        };
        if resumed {
            self.effects.feed_live(stream_id, host, now_ms);
        }
        let token = self.issue(
            stream_id,
            host,
            "host",
            MediaScope::LivePublish,
            generation,
            now_ms,
        )?;
        Ok(LiveStart {
            stream_id: stream_id.to_string(),
            publish_token: token,
            already_live: false,
        })
    }

    /// 宽限期到点仍未回来：结束直播、撤源、断开所有观看者。
    pub fn tick(&mut self, now_ms: u64) -> Vec<String> {
        let expired: Vec<String> = self
            .streams
            .iter()
            .filter(|(_, s)| match s.state {
                LiveState::Reconnecting { deadline_ms, .. } => now_ms >= deadline_ms,
                _ => false,
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            self.end_stream(id, LiveEndReason::HostLost, now_ms);
        }
        expired
    }

    /// 主播下播。重复调用幂等；`forced` 供治理侧强制停播。
    pub fn stop(
        &mut self,
        host: &str,
        stream_id: &str,
        now_ms: u64,
    ) -> Result<StopOutcome, LiveError> {
        let stream = self.streams.get(stream_id).ok_or(LiveError::NotFound)?;
        if stream.host != host {
            return Err(LiveError::Forbidden);
        }
        if stream.state.is_ended() {
            return Ok(StopOutcome {
                ended: true,
                already_ended: true,
                disconnected: Vec::new(),
            });
        }
        Ok(self.end_stream(stream_id, LiveEndReason::HostStopped, now_ms))
    }

    /// 运营/治理强制停播：不要求主播本人。
    pub fn force_stop(&mut self, stream_id: &str, now_ms: u64) -> Result<StopOutcome, LiveError> {
        let stream = self.streams.get(stream_id).ok_or(LiveError::NotFound)?;
        if stream.state.is_ended() {
            return Ok(StopOutcome {
                ended: true,
                already_ended: true,
                disconnected: Vec::new(),
            });
        }
        Ok(self.end_stream(stream_id, LiveEndReason::Forced, now_ms))
    }

    fn end_stream(&mut self, stream_id: &str, reason: LiveEndReason, now_ms: u64) -> StopOutcome {
        let Some(stream) = self.streams.get_mut(stream_id) else {
            return StopOutcome {
                ended: false,
                already_ended: true,
                disconnected: Vec::new(),
            };
        };
        if stream.state.is_ended() {
            return StopOutcome {
                ended: true,
                already_ended: true,
                disconnected: Vec::new(),
            };
        }
        // 第一动作是撤源：Feed 不能继续挂着一个不会再出帧的房间。
        self.effects.source_revoked(stream_id, now_ms);
        self.effects.feed_ended(stream_id, now_ms);
        if stream.archive {
            self.effects.archive_stopped(stream_id, now_ms);
        }
        stream.state = LiveState::Ended(reason);
        stream.ended_at_ms = Some(now_ms);
        stream.generation = stream.generation.saturating_add(1);
        let disconnected: Vec<String> = stream.viewers.keys().cloned().collect();
        stream.viewers.clear();
        stream.cohosts.clear();
        stream.pending_cohosts.clear();
        StopOutcome {
            ended: true,
            already_ended: false,
            disconnected,
        }
    }

    /// 连麦申请（观众发起，主播审批）。
    pub fn request_cohost(
        &mut self,
        stream_id: &str,
        user: &str,
        _now_ms: u64,
    ) -> Result<bool, LiveError> {
        let stream = self.live_mut(stream_id)?;
        if stream.banned.contains(user) {
            return Err(LiveError::Banned);
        }
        if user == stream.host {
            return Err(LiveError::Forbidden);
        }
        if !stream.viewers.contains_key(user) {
            return Err(LiveError::NotWatching);
        }
        if stream.cohosts.contains(user) {
            return Ok(false);
        }
        Ok(stream.pending_cohosts.insert(user.to_string()))
    }

    /// 主播批准连麦：换发 `LiveCohost` 令牌（允许上行，但只能用于这路直播）。
    pub fn approve_cohost(
        &mut self,
        host: &str,
        stream_id: &str,
        user: &str,
        device: &str,
        now_ms: u64,
    ) -> Result<String, LiveError> {
        let generation = {
            let stream = self.live_mut(stream_id)?;
            if stream.host != host {
                return Err(LiveError::Forbidden);
            }
            if stream.banned.contains(user) {
                return Err(LiveError::Banned);
            }
            if !stream.pending_cohosts.remove(user) && !stream.cohosts.contains(user) {
                return Err(LiveError::Forbidden);
            }
            if !stream.is_watching(user, device) {
                return Err(LiveError::NotWatching);
            }
            stream.cohosts.insert(user.to_string());
            stream.generation
        };
        self.issue(
            stream_id,
            user,
            device,
            MediaScope::LiveCohost,
            generation,
            now_ms,
        )
        .map_err(LiveError::from)
    }

    /// 取消连麦。令牌不需要逐个吊销：校验时会查当前连麦名单。
    pub fn revoke_cohost(
        &mut self,
        host: &str,
        stream_id: &str,
        user: &str,
    ) -> Result<bool, LiveError> {
        let stream = self.live_mut(stream_id)?;
        if stream.host != host {
            return Err(LiveError::Forbidden);
        }
        stream.pending_cohosts.remove(user);
        Ok(stream.cohosts.remove(user))
    }

    /// 举报。同一举报人对同一路直播只计一次，避免刷量。
    pub fn report(
        &mut self,
        stream_id: &str,
        reporter: &str,
        reason: &str,
        now_ms: u64,
    ) -> Result<usize, LiveError> {
        let stream = self.live_mut(stream_id)?;
        if stream.banned.contains(reporter) || !stream.viewers.contains_key(reporter) {
            return Err(LiveError::NotWatching);
        }
        if stream.reported_by.insert(reporter.to_string()) {
            stream.reports.push(LiveReport {
                reporter: reporter.to_string(),
                reason: reason.to_string(),
                at_ms: now_ms,
            });
        }
        Ok(stream.reports.len())
    }

    /// 主播封禁观众：立即断开其全部设备并移出连麦名单。
    pub fn ban(
        &mut self,
        host: &str,
        stream_id: &str,
        user: &str,
        _now_ms: u64,
    ) -> Result<usize, LiveError> {
        let stream = self.live_mut(stream_id)?;
        if stream.host != host {
            return Err(LiveError::Forbidden);
        }
        if user == stream.host {
            return Err(LiveError::Forbidden);
        }
        stream.banned.insert(user.to_string());
        stream.cohosts.remove(user);
        stream.pending_cohosts.remove(user);
        Ok(stream.viewers.remove(user).map_or(0, |v| v.devices.len()))
    }

    /// 开场/停用归档（G04）。重复设置返回当前状态且不重复调用端口。
    pub fn set_archive(
        &mut self,
        host: &str,
        stream_id: &str,
        enabled: bool,
        now_ms: u64,
    ) -> Result<bool, LiveError> {
        let stream = self.live_mut(stream_id)?;
        if stream.host != host {
            return Err(LiveError::Forbidden);
        }
        if stream.archive == enabled {
            return Ok(enabled);
        }
        stream.archive = enabled;
        if enabled {
            self.effects.archive_started(stream_id, now_ms);
        } else {
            self.effects.archive_stopped(stream_id, now_ms);
        }
        Ok(enabled)
    }

    /// 播放状态机。只接受有意义的迁移，其余显式报错。
    pub fn set_playback(
        &mut self,
        stream_id: &str,
        user: &str,
        device: &str,
        next: PlaybackState,
        now_ms: u64,
    ) -> Result<PlaybackState, LiveError> {
        let stream = self.live_mut(stream_id)?;
        let view = stream
            .viewers
            .get_mut(user)
            .and_then(|v| v.devices.get_mut(device))
            .ok_or(LiveError::NotWatching)?;
        let current = view.playback;
        let allowed = match (current, next) {
            (a, b) if a == b => true,
            (_, PlaybackState::AwaitingGesture) => true,
            (PlaybackState::AwaitingGesture, PlaybackState::Buffering) => true,
            (PlaybackState::Buffering | PlaybackState::Stalled { .. }, PlaybackState::Playing) => {
                true
            }
            (PlaybackState::Playing, PlaybackState::Stalled { .. }) => true,
            (PlaybackState::Playing | PlaybackState::Stalled { .. }, PlaybackState::Buffering) => {
                true
            }
            _ => false,
        };
        if !allowed {
            return Err(LiveError::BadPlayback(current));
        }
        view.playback = match next {
            PlaybackState::Stalled { since_ms: 0 } => PlaybackState::Stalled { since_ms: now_ms },
            other => other,
        };
        Ok(view.playback)
    }

    pub fn playback(
        &self,
        stream_id: &str,
        user: &str,
        device: &str,
    ) -> Result<PlaybackState, LiveError> {
        self.stream(stream_id)
            .ok_or(LiveError::NotFound)?
            .playback(user, device)
            .ok_or(LiveError::NotWatching)
    }

    /// 校验直播令牌：签名/时效/绑定 + 当前房间事实（结束、封禁、连麦名单）。
    pub fn verify_token(
        &self,
        token: &str,
        now_ms: u64,
        stream_id: &str,
        scope: MediaScope,
    ) -> Result<MediaClaims, LiveError> {
        if !matches!(
            scope,
            MediaScope::LivePublish | MediaScope::LiveWatch | MediaScope::LiveCohost
        ) {
            return Err(LiveError::Forbidden);
        }
        // 先验签名与 call/scope 绑定，再读房间事实：否则"别的直播间的令牌"
        // 会被误报成 NotFound，调用方拿不到真实原因。
        let claims = self.signer.verify(token, now_ms)?;
        if claims.call != stream_id {
            return Err(LiveError::Token(TokenError::WrongCall));
        }
        if claims.scope != scope {
            return Err(LiveError::Token(TokenError::WrongScope));
        }
        let stream = self.stream(stream_id).ok_or(LiveError::NotFound)?;
        if stream.state.is_ended() {
            return Err(LiveError::Ended);
        }
        if claims.generation != stream.generation {
            return Err(LiveError::Token(TokenError::StaleGeneration));
        }
        if stream.banned.contains(&claims.user) {
            return Err(LiveError::Banned);
        }
        match scope {
            MediaScope::LivePublish => {
                if claims.user != stream.host {
                    return Err(LiveError::Forbidden);
                }
                if !stream.state.is_live() {
                    return Err(LiveError::NotLive);
                }
            }
            MediaScope::LiveCohost => {
                if !stream.cohosts.contains(&claims.user) {
                    return Err(LiveError::Forbidden);
                }
                if !stream.state.is_live() {
                    return Err(LiveError::NotLive);
                }
            }
            MediaScope::LiveWatch => {
                if !stream.is_watching(&claims.user, &claims.device) {
                    return Err(LiveError::NotWatching);
                }
            }
            MediaScope::Call => return Err(LiveError::Forbidden),
        }
        Ok(claims)
    }

    /// 换发主播令牌（长直播续期）。
    pub fn refresh_publish_token(
        &self,
        host: &str,
        stream_id: &str,
        now_ms: u64,
    ) -> Result<String, LiveError> {
        let stream = self.stream(stream_id).ok_or(LiveError::NotFound)?;
        if stream.host != host {
            return Err(LiveError::Forbidden);
        }
        if !stream.state.is_live() {
            return Err(LiveError::NotLive);
        }
        self.issue(
            stream_id,
            host,
            "host",
            MediaScope::LivePublish,
            stream.generation,
            now_ms,
        )
        .map_err(LiveError::from)
    }

    fn issue(
        &self,
        stream_id: &str,
        user: &str,
        device: &str,
        scope: MediaScope,
        generation: u64,
        now_ms: u64,
    ) -> Result<String, TokenError> {
        self.signer.issue_for(
            stream_id,
            user,
            device,
            scope,
            generation,
            now_ms,
            LIVE_TOKEN_TTL_MS,
        )
    }
}
