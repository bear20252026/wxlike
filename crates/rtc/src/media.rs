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

    /// Writes one opaque encoded frame to the audio stream.
    pub fn write_audio(&mut self, sample: &[u8]) -> anyhow::Result<()> {
        self.write_mid(self.audio_mid, Codec::Opus, sample)
    }

    /// Writes one opaque encoded frame to the video stream (WP-08b).
    pub fn write_video(&mut self, frame: &[u8]) -> anyhow::Result<()> {
        self.write_mid(self.video_mid, Codec::Vp8, frame)
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
                match &ev {
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
                events.push(ev);
            }
        }
        Ok((events, datagrams))
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
