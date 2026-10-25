//! WP-08 media layer: `str0m` ICE/DTLS/SRTP sessions, sans-I/O.
//!
//! str0m deliberately ships no sockets: it consumes `Input` and produces
//! `Output`, and the *host* moves bytes. That is what makes the acceptance
//! tests possible without TURN or even real network — two sessions in one
//! process negotiate and exchange **real DTLS/SRTP** through [`pump`] (a
//! virtual wire) or through real loopback UDP sockets ([`LoopbackPair`]). The
//! crypto path is the production one; only the wire differs.
//!
//! Negotiation is sample-level SDP: audio (+video for WP-08b) m-lines with
//! Opus/VP8 payload types. Payloads are opaque bytes — the `audio`/`camera`
//! features substitute real capture and codecs in [`crate::capture`].

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::format::Codec;
use str0m::media::{Direction, MediaKind, Mid};
use str0m::{Candidate, Event, Input, Output, Rtc, RtcError};

/// One datagram str0m wants on the wire (virtual or real).
#[derive(Debug, Clone)]
pub struct Datagram {
    pub data: Vec<u8>,
    pub source: SocketAddr,
    pub destination: SocketAddr,
}

/// One side's media engine: the str0m `Rtc`, its virtual clock, and the
/// negotiated m-lines.
pub struct MediaSession {
    pub rtc: Rtc,
    start: Instant,
    last: Instant,
    audio_mid: Option<Mid>,
    video_mid: Option<Mid>,
    connected: bool,
    pending: Option<SdpPendingOffer>,
    /// 发送轨开关（G09-03）：静音的是编码器出口，不是 UI 图标。
    audio_muted: bool,
    video_muted: bool,
    /// 当前摄像头/屏幕源 id；切换后旧源写出的帧被丢弃。
    video_source: Option<String>,
    /// Latest egress bitrate estimates in kbps (WP-08b adaptation input).
    pub bitrate_kbps_estimates: Vec<u64>,
}

/// str0m needs its DTLS/SRTP crypto backend installed once per process
/// (openssl on unix, wincrypto on Windows — chosen by feature flags).
static CRYPTO_INIT: std::sync::Once = std::sync::Once::new();

fn ensure_crypto_backend() {
    CRYPTO_INIT.call_once(|| {
        str0m::config::CryptoProvider::from_feature_flags().install_process_default();
    });
}

impl MediaSession {
    /// `controlling = true` for the caller (the ICE role tie-break).
    pub fn new(controlling: bool) -> Self {
        ensure_crypto_backend();
        let mut rtc = Rtc::new();
        rtc.direct_api().set_ice_controlling(controlling);
        let now = Instant::now();
        Self {
            rtc,
            start: now,
            last: now,
            audio_mid: None,
            video_mid: None,
            connected: false,
            pending: None,
            audio_muted: false,
            video_muted: false,
            video_source: Some("default".into()),
            bitrate_kbps_estimates: Vec::new(),
        }
    }

    /// Registers a local host candidate. Production hosts enumerate NICs
    /// (or a TURN allocation, `WXLIKE_TURN_URL`) and call this per address.
    pub fn add_host_candidate(&mut self, socket: SocketAddr) -> Option<Candidate> {
        Candidate::host(socket, "udp")
            .ok()
            .and_then(|c| self.rtc.add_local_candidate(c).cloned())
    }

    /// Adds an audio m-line (plus video for WP-08b) and returns the SDP
    /// offer as JSON for the signaling channel.
    pub fn offer(&mut self, video: bool) -> anyhow::Result<serde_json::Value> {
        let mut change = self.rtc.sdp_api();
        let audio = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        let mut video_mid = None;
        if video {
            video_mid =
                Some(change.add_media(MediaKind::Video, Direction::SendRecv, None, None, None));
        }
        let (offer, pending) = change
            .apply()
            .ok_or_else(|| anyhow::anyhow!("nothing to negotiate"))?;
        self.audio_mid = Some(audio);
        self.video_mid = video_mid;
        self.pending = Some(pending);
        Ok(serde_json::to_value(&offer)?)
    }

    /// Consumes the peer's offer, returns the SDP answer as JSON.
    pub fn accept_offer(&mut self, offer: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let sdp: SdpOffer = serde_json::from_value(offer)?;
        let answer = self.rtc.sdp_api().accept_offer(sdp)?;
        Ok(serde_json::to_value(&answer)?)
    }

    /// Feeds the peer's answer into the caller's session (closes the
    /// negotiation opened by [`MediaSession::offer`]).
    pub fn accept_answer(&mut self, answer: serde_json::Value) -> anyhow::Result<()> {
        let sdp: SdpAnswer = serde_json::from_value(answer)?;
        let pending = self
            .pending
            .take()
            .ok_or_else(|| anyhow::anyhow!("no offer outstanding"))?;
        self.rtc.sdp_api().accept_answer(pending, sdp)?;
        Ok(())
    }

    pub fn audio_mid(&self) -> Option<Mid> {
        self.audio_mid
    }

    pub fn video_mid(&self) -> Option<Mid> {
        self.video_mid
    }

    /// Whether a video m-line is negotiated (WP-08b A1).
    pub fn has_video_mline(&self) -> bool {
        self.video_mid.is_some()
    }

    /// Writes one opaque encoded frame to the audio stream. A muted track
    /// produces no output at all: the far end genuinely receives nothing.
    pub fn write_audio(&mut self, sample: &[u8]) -> anyhow::Result<()> {
        if self.audio_muted {
            return Ok(());
        }
        self.write_mid(self.audio_mid, Codec::Opus, sample)
    }

    /// Writes one opaque encoded frame to the video stream (WP-08b).
    pub fn write_video(&mut self, frame: &[u8]) -> anyhow::Result<()> {
        if self.video_muted {
            return Ok(());
        }
        self.write_mid(self.video_mid, Codec::Vp8, frame)
    }

    pub fn set_audio_muted(&mut self, muted: bool) {
        self.audio_muted = muted;
    }

    pub fn set_video_muted(&mut self, muted: bool) {
        self.video_muted = muted;
    }

    pub fn audio_muted(&self) -> bool {
        self.audio_muted
    }

    pub fn video_muted(&self) -> bool {
        self.video_muted
    }

    /// 选择摄像头/屏幕源。切换后旧源的帧由 [`Self::write_video_from`] 丢弃。
    pub fn select_video_source(&mut self, source: &str) -> anyhow::Result<()> {
        if source.trim().is_empty() {
            anyhow::bail!("video source id is empty");
        }
        self.video_source = Some(source.to_string());
        Ok(())
    }

    pub fn video_source(&self) -> Option<&str> {
        self.video_source.as_deref()
    }

    /// 只发送当前选中源的帧。返回 `false` 表示帧来自已被切换掉的旧设备。
    pub fn write_video_from(&mut self, source: &str, frame: &[u8]) -> anyhow::Result<bool> {
        match self.video_source.as_deref() {
            Some(current) if current == source => {
                self.write_video(frame)?;
                Ok(true)
            }
            Some(_) => Ok(false),
            None => anyhow::bail!("no video source selected"),
        }
    }

    /// ICE 重启（G09-03 弱网恢复）：保留本地候选，返回新的 SDP offer。
    pub fn restart_ice(&mut self) -> anyhow::Result<serde_json::Value> {
        let mut change = self.rtc.sdp_api();
        change.ice_restart(true);
        let (offer, pending) = change
            .apply()
            .ok_or_else(|| anyhow::anyhow!("nothing to negotiate"))?;
        self.pending = Some(pending);
        self.connected = false;
        Ok(serde_json::to_value(&offer)?)
    }

    fn write_mid(&mut self, mid: Option<Mid>, codec: Codec, payload: &[u8]) -> anyhow::Result<()> {
        let mid = mid.ok_or_else(|| anyhow::anyhow!("m-line not negotiated"))?;
        let params = self
            .rtc
            .codec_config()
            .find(|p| p.spec().codec == codec)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("codec {codec:?} not negotiated"))?;
        let wallclock = self.start + self.elapsed();
        let time = self.elapsed().into();
        self.rtc
            .writer(mid)
            .ok_or_else(|| anyhow::anyhow!("writer unavailable for {mid}"))?
            .write(params.pt(), wallclock, time, payload.to_vec())?;
        Ok(())
    }

    pub fn elapsed(&self) -> Duration {
        self.last - self.start
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// One pump batch: advances the virtual clock to the next interesting
    /// instant and drains output — events out, datagrams out, clock forward.
    pub fn step(&mut self) -> Result<(Vec<Event>, Vec<Datagram>), RtcError> {
        let mut events = Vec::new();
        let mut datagrams = Vec::new();
        self.rtc.handle_input(Input::Timeout(self.last))?;
        match self.rtc.poll_output()? {
            Output::Timeout(next) => {
                let tick = self.last + Duration::from_millis(10);
                self.last = if next == self.last {
                    tick
                } else {
                    tick.min(next)
                };
            }
            Output::Transmit(v) => {
                datagrams.push(Datagram {
                    data: v.contents.to_vec(),
                    source: v.source,
                    destination: v.destination,
                });
            }
            Output::Event(ev) => {
                self.absorb_event(&ev);
                events.push(ev);
            }
        }
        Ok((events, datagrams))
    }

    /// 从事件流里提取会话事实：连通、出口估计、协商出的 m-line id。
    fn absorb_event(&mut self, ev: &Event) {
        match ev {
            Event::Connected => self.connected = true,
            Event::EgressBitrateEstimate(bwe) => {
                let bps = match bwe {
                    str0m::bwe::BweKind::Twcc(b) => b.as_u64(),
                    str0m::bwe::BweKind::Remb(_, b) => b.as_u64(),
                };
                self.bitrate_kbps_estimates.push(bps / 1000);
            }
            // mids are discovered through events (str0m has no
            // all_media iterator in this version)
            Event::MediaAdded(added) => match added.kind {
                MediaKind::Audio if self.audio_mid.is_none() => {
                    self.audio_mid = Some(added.mid);
                }
                MediaKind::Video if self.video_mid.is_none() => {
                    self.video_mid = Some(added.mid);
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// Feeds one peer datagram in.
    pub fn handle_receive(&mut self, d: &Datagram) -> Result<(), RtcError> {
        let recv = str0m::net::Receive {
            proto: str0m::net::Protocol::Udp,
            source: d.source,
            destination: d.destination,
            contents: d.data.as_slice().try_into()?,
        };
        self.rtc.handle_input(Input::Receive(self.last, recv))
    }
}

/// Virtual wire: pumps both sessions in lockstep for up to `max_virtual` of
/// virtual time, ferrying every datagram straight across. Both sessions'
/// events come back so tests can assert on real `MediaData` arrivals.
pub fn pump(
    a: &mut MediaSession,
    b: &mut MediaSession,
    max_virtual: Duration,
) -> Result<(Vec<Event>, Vec<Event>), RtcError> {
    let mut ev_a = Vec::new();
    let mut ev_b = Vec::new();
    let t0 = a.elapsed().max(b.elapsed());
    while a.elapsed() - t0 < max_virtual {
        let (evs, dgrams) = a.step()?;
        ev_a.extend(evs);
        for d in &dgrams {
            b.handle_receive(d)?;
        }
        let (evs, dgrams) = b.step()?;
        ev_b.extend(evs);
        for d in &dgrams {
            a.handle_receive(d)?;
        }
    }
    Ok((ev_a, ev_b))
}

/// Real loopback UDP wire (async): two OS sockets on 127.0.0.1 that carry
/// the sessions' datagrams through the kernel — the "无 TURN 时本地回环媒体
/// 连通" acceptance shape.
///
/// Order matters: str0m's SDP mode does not trickle candidates, so
/// [`LoopbackPair::register`] must run **before** offer/answer. Usage:
///
/// ```no_run
/// # use wxlike_rtc::media::{LoopbackPair, MediaSession};
/// # async fn demo() -> anyhow::Result<()> {
/// let mut a = MediaSession::new(true);
/// let mut b = MediaSession::new(false);
/// let pair = LoopbackPair::bind().await?;
/// pair.register(&mut a, &mut b);
/// let offer = a.offer(false)?;
/// let answer = b.accept_offer(offer)?;
/// a.accept_answer(answer)?;
/// let (a_to_b, b_to_a) = pair.run(&mut a, &mut b, std::time::Duration::from_secs(5)).await?;
/// # Ok(()) }
/// ```
pub struct LoopbackPair {
    sock_a: tokio::net::UdpSocket,
    sock_b: tokio::net::UdpSocket,
}

impl LoopbackPair {
    /// Binds both sockets and points each at the other.
    pub async fn bind() -> anyhow::Result<Self> {
        let sock_a = tokio::net::UdpSocket::bind("127.0.0.1:0").await?;
        let sock_b = tokio::net::UdpSocket::bind("127.0.0.1:0").await?;
        sock_a.connect(sock_b.local_addr()?).await?;
        sock_b.connect(sock_a.local_addr()?).await?;
        Ok(Self { sock_a, sock_b })
    }

    /// Registers each side's socket address as its host candidate. Call this
    /// before the SDP offer/answer (see the struct docs).
    pub fn register(&self, a: &mut MediaSession, b: &mut MediaSession) {
        a.add_host_candidate(self.sock_a.local_addr().expect("bound socket has addr"));
        b.add_host_candidate(self.sock_b.local_addr().expect("bound socket has addr"));
    }

    /// Ferries datagrams through the OS for `wall_budget`. Returns packets
    /// moved a→b and b→a.
    pub async fn run(
        &self,
        a: &mut MediaSession,
        b: &mut MediaSession,
        wall_budget: Duration,
    ) -> anyhow::Result<(usize, usize)> {
        let addr_a = self.sock_a.local_addr()?;
        let addr_b = self.sock_b.local_addr()?;
        let deadline = Instant::now() + wall_budget;
        let mut a_to_b = 0usize;
        let mut b_to_a = 0usize;
        let mut buf = vec![0u8; 4096];

        while Instant::now() < deadline {
            let (_evs, dgrams) = a.step()?;
            for d in dgrams {
                self.sock_a.send(&d.data).await?;
                a_to_b += 1;
            }
            let (_evs, dgrams) = b.step()?;
            for d in dgrams {
                self.sock_b.send(&d.data).await?;
                b_to_a += 1;
            }

            if let Ok(Ok(n)) =
                tokio::time::timeout(Duration::from_millis(5), self.sock_b.recv(&mut buf)).await
            {
                let d = Datagram {
                    data: buf[..n].to_vec(),
                    source: addr_a,
                    destination: addr_b,
                };
                b.handle_receive(&d)?;
            }
            if let Ok(Ok(n)) =
                tokio::time::timeout(Duration::from_millis(5), self.sock_a.recv(&mut buf)).await
            {
                let d = Datagram {
                    data: buf[..n].to_vec(),
                    source: addr_b,
                    destination: addr_a,
                };
                a.handle_receive(&d)?;
            }
        }
        Ok((a_to_b, b_to_a))
    }
}

// ---------------------------------------------------------------------------
// G09-03: ICE 健康度与重启
// ---------------------------------------------------------------------------

/// 弱网恢复的状态；`Buffering` 不是"还在播"，上层必须据此显示重连中。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IceHealth {
    Healthy,
    Degraded { silent_ms: u64 },
    RestartDue { silent_ms: u64 },
    Exhausted,
}

/// 以"最后收到包"为信号的 ICE 看门狗。纯逻辑，时间由调用方给。
#[derive(Debug, Clone)]
pub struct IceMonitor {
    degrade_after_ms: u64,
    restart_after_ms: u64,
    cooldown_ms: u64,
    max_restarts: u32,
    last_rx_ms: Option<u64>,
    last_restart_ms: Option<u64>,
    generation: u64,
    restarts: u32,
}

impl IceMonitor {
    /// 默认：2 秒无包降级，5 秒触发重启，重启冷却 10 秒，最多 3 次。
    pub fn new(degrade_after_ms: u64, restart_after_ms: u64) -> Self {
        Self {
            degrade_after_ms,
            restart_after_ms,
            cooldown_ms: 10_000,
            max_restarts: 3,
            last_rx_ms: None,
            last_restart_ms: None,
            generation: 0,
            restarts: 0,
        }
    }

    pub fn default_policy() -> Self {
        Self::new(2_000, 5_000)
    }

    pub fn observe(&mut self, now_ms: u64) {
        self.last_rx_ms = Some(now_ms);
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn restarts(&self) -> u32 {
        self.restarts
    }

    pub fn health(&self, now_ms: u64) -> IceHealth {
        let Some(last) = self.last_rx_ms else {
            return IceHealth::Healthy;
        };
        let silent_ms = now_ms.saturating_sub(last);
        if silent_ms < self.degrade_after_ms {
            return IceHealth::Healthy;
        }
        if self.restarts >= self.max_restarts {
            return IceHealth::Exhausted;
        }
        let cooling = self
            .last_restart_ms
            .is_some_and(|t| now_ms.saturating_sub(t) < self.cooldown_ms);
        if silent_ms >= self.restart_after_ms && !cooling {
            IceHealth::RestartDue { silent_ms }
        } else {
            IceHealth::Degraded { silent_ms }
        }
    }

    /// 只在 `RestartDue` 时推进代次；返回新的重启代次。
    pub fn begin_restart(&mut self, now_ms: u64) -> Option<u64> {
        if !matches!(self.health(now_ms), IceHealth::RestartDue { .. }) {
            return None;
        }
        self.generation = self.generation.saturating_add(1);
        self.restarts += 1;
        self.last_restart_ms = Some(now_ms);
        Some(self.generation)
    }
}

// ---------------------------------------------------------------------------
// WP-08b: bandwidth adaptation
// ---------------------------------------------------------------------------

/// Turns bandwidth estimates into a target send bitrate (基础档自适应,
/// REMB/TWCC 的估计值在 `Event::EgressBitrateEstimate` 里到达). Pure logic
/// so the degradation ladder is exactly testable: cut hard when the path
/// cannot carry the current target, climb cautiously when there is headroom.
#[derive(Debug, Clone)]
pub struct BitrateController {
    floor_kbps: u64,
    ceiling_kbps: u64,
    target_kbps: u64,
}

impl BitrateController {
    pub fn new(floor_kbps: u64, ceiling_kbps: u64) -> Self {
        Self {
            floor_kbps,
            ceiling_kbps,
            target_kbps: ceiling_kbps,
        }
    }

    pub fn target_kbps(&self) -> u64 {
        self.target_kbps
    }

    /// Feed one estimate (kbps); returns the new target when it changed.
    pub fn observe(&mut self, estimated_kbps: u64) -> Option<u64> {
        let new_target = if estimated_kbps < self.target_kbps {
            // degrade to just under what the path says it can carry
            (estimated_kbps.saturating_sub(50) / 50 * 50).max(self.floor_kbps)
        } else if estimated_kbps > self.target_kbps.saturating_mul(3) / 2 {
            // 50% headroom: climb by a quarter, never past the ceiling
            (self.target_kbps + self.target_kbps / 4).min(self.ceiling_kbps)
        } else {
            self.target_kbps
        };
        if new_target != self.target_kbps {
            self.target_kbps = new_target;
            Some(new_target)
        } else {
            None
        }
    }
}
