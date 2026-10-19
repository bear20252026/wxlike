//! 客户端通话宿主：把三块拼起来的那一层。
//!
//! `CallManager` 是纯状态机（信号进出、无 IO），`MediaSession` 是 sans-I/O
//! 的 str0m 会话（进出都是 [`Datagram`]）。中间差的就是"谁去收发"——本模块
//! 就是这个缺口的唯一实现，宿主（Tauri / 测试）只负责三件事：
//!
//! 1. 调 [`CallHost::tick`]，把返回的 `Transmit` 真发出去；
//! 2. 收到 UDP 包就调 [`CallHost::handle_datagram`]；
//! 3. 收到信令就调 [`CallHost::handle_signal`]，并把 `Signal` 效应投递出去。
//!
//! 为什么单独成层而不是塞进 `CallManager`：状态机的可测性来自"同步且对时钟
//! 无副作用"，一旦让它在内部碰 socket 就再也测不动了。这一层**不碰 socket**，
//! 只把"该往哪发、发什么"作为返回值交出去——所以它能在 tokio 里跑，也能在
//! 纯同步循环里跑（本模块的测试就是后者）。

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::time::Instant;

use crate::call::{Action, CallManager};
use crate::media::Datagram;
use crate::signal::Signal;
use crate::state::End;

/// 宿主必须执行的副作用。
#[derive(Debug, Clone)]
pub enum Effect {
    /// 把这条信令投递出去（经 HTTP 中继或任何等价通道）。
    Signal(Signal),
    /// 把这个数据报原样发到 `to`（UDP）。
    Transmit { to: SocketAddr, bytes: Vec<u8> },
    /// 收到一帧对端音频（已解 RTP，payload 仍是 Opus 码流，未解码）。
    Audio { call_id: String, payload: Vec<u8> },
    /// ICE + DTLS 首次连通（每通电话只报一次）。
    Connected { call_id: String },
    /// 通话结束。
    Ended { call_id: String, end: End },
}

/// 一通电话的宿主。
pub struct CallHost {
    conv: String,
    mgr: CallManager,
    /// 本端媒体 socket 的地址（已作为 host candidate 注入 SDP）。
    local: SocketAddr,
    /// 已报过 `Connected` 的 call_id，避免每 tick 重复上报。
    connected: HashSet<String>,
    /// 对端媒体地址，仅用于诊断（发送地址来自 str0m 自己的 `destination`）。
    peers: HashMap<String, SocketAddr>,
}

impl CallHost {
    /// `local` 必须是宿主真实 socket 的 `local_addr()`，并且该地址对端可达
    /// （绑定 `0.0.0.0` 时要换成网卡地址，否则 SDP 里会出现不可达的候选）。
    pub fn new(me: impl Into<String>, conv: impl Into<String>, local: SocketAddr) -> Self {
        let mut mgr = CallManager::new(me);
        mgr.set_local_candidates(vec![local]);
        Self {
            conv: conv.into(),
            mgr,
            local,
            connected: HashSet::new(),
            peers: HashMap::new(),
        }
    }

    pub fn conv(&self) -> &str {
        &self.conv
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    pub fn manager(&self) -> &CallManager {
        &self.mgr
    }

    pub fn manager_mut(&mut self) -> &mut CallManager {
        &mut self.mgr
    }

    /// 发起通话。`peer_online` 为 false 时会立刻返回 `Ended`（对方不在线）。
    pub fn start(&mut self, peer: &str, video: bool, peer_online: bool) -> (String, Vec<Effect>) {
        let (call_id, actions) = self.mgr.start(peer, video, peer_online);
        (call_id.clone(), self.drain(actions))
    }

    /// 接听。`peer` 只用于记录，媒体地址由 SDP 候选决定。
    pub fn accept(&mut self, call_id: &str, peer: Option<SocketAddr>) -> Vec<Effect> {
        if let Some(addr) = peer {
            self.peers.insert(call_id.to_string(), addr);
        }
        let actions = self.mgr.accept(call_id);
        self.drain(actions)
    }

    pub fn reject(&mut self, call_id: &str) -> Vec<Effect> {
        let actions = self.mgr.reject(call_id);
        self.drain(actions)
    }

    pub fn hangup(&mut self, call_id: &str) -> Vec<Effect> {
        let actions = self.mgr.hangup(call_id);
        self.drain(actions)
    }

    /// 收到一条信令（来自中继）。
    pub fn handle_signal(&mut self, signal: Signal) -> Vec<Effect> {
        let actions = self.mgr.handle_signal(signal);
        self.drain(actions)
    }

    /// 收到一个 UDP 包。`from`/`to` 必须如实填写——str0m 用它们匹配候选。
    pub fn handle_datagram(
        &mut self,
        from: SocketAddr,
        to: SocketAddr,
        bytes: &[u8],
    ) -> Vec<Effect> {
        let mut out = Vec::new();
        let d = Datagram {
            data: bytes.to_vec(),
            source: from,
            destination: to,
        };
        for call_id in self.live_call_ids() {
            // 1:1 通话只有一个媒体会话；包不属于它会自己丢弃。
            let accepted = match self.mgr.media_mut(&call_id) {
                Some(media) => media.handle_receive(&d).is_ok(),
                None => false,
            };
            if accepted {
                out.extend(self.pump_media(&call_id));
            }
        }
        out
    }

    /// 推进时钟：截止时间（振铃超时/静默看门狗）+ 媒体收发。
    pub fn tick(&mut self, now: Instant) -> Vec<Effect> {
        let actions = self.mgr.check_deadlines(now);
        let mut out = self.drain(actions);
        for call_id in self.live_call_ids() {
            out.extend(self.pump_media(&call_id));
        }
        out
    }

    /// 写入一帧音频（Opus 码流，由 `capture::OpusCodec` 编码）。
    pub fn write_audio(&mut self, call_id: &str, payload: &[u8]) -> anyhow::Result<()> {
        let media = self
            .mgr
            .media_mut(call_id)
            .ok_or_else(|| anyhow::anyhow!("no media session for {call_id}"))?;
        media.write_audio(payload)
    }

    pub fn is_connected(&self, call_id: &str) -> bool {
        self.mgr
            .media_ref(call_id)
            .map(|m| m.is_connected())
            .unwrap_or(false)
    }

    // -- 内部

    fn live_call_ids(&self) -> Vec<String> {
        self.mgr.live_calls()
    }

    /// 跑一轮媒体：把 str0m 的输出变成 `Transmit` / `Audio` / `Connected`。
    fn pump_media(&mut self, call_id: &str) -> Vec<Effect> {
        let mut out = Vec::new();
        let audio_mid = match self.mgr.media_mut(call_id) {
            Some(m) => m.audio_mid(),
            None => return out,
        };
        let step = match self.mgr.media_mut(call_id) {
            Some(m) => m.step(),
            None => return out,
        };
        let (events, datagrams) = match step {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(call_id, %err, "media step failed");
                return out;
            }
        };
        for d in datagrams {
            out.push(Effect::Transmit {
                to: d.destination,
                bytes: d.data,
            });
        }
        for ev in events {
            match ev {
                str0m::Event::MediaData(data) => {
                    if Some(data.mid) == audio_mid {
                        out.push(Effect::Audio {
                            call_id: call_id.to_string(),
                            payload: data.data,
                        });
                    }
                }
                str0m::Event::Connected if self.connected.insert(call_id.to_string()) => {
                    out.push(Effect::Connected {
                        call_id: call_id.to_string(),
                    });
                }
                _ => {}
            }
        }
        out
    }

    fn drain(&mut self, actions: Vec<Action>) -> Vec<Effect> {
        let mut out = Vec::new();
        for action in actions {
            match action {
                Action::Send(sig) => out.push(Effect::Signal(sig)),
                Action::Ended { call_id, end } => {
                    self.connected.remove(&call_id);
                    self.peers.remove(&call_id);
                    out.push(Effect::Ended { call_id, end });
                }
                // 同步核心从不发它；媒体由 `pump_media` 主动泵。
                Action::MediaReady { .. } => {}
            }
        }
        out
    }
}
