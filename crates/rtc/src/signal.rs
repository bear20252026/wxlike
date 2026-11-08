//! WP-08a signaling: call control rides the existing QUIC channel as
//! `MsgBody::Custom { kind: "rtc-signal" }` envelopes — no second transport,
//! so everything already true of envelopes (routing, offline parking, hub
//! anti-forgery) is true of call signaling for free.
//!
//! The JSON payload is versioned with a `v` field so a future change can be
//! rejected loudly instead of half-parsed.

use serde::{Deserialize, Serialize};
use wxlike_core::{custom_kind, Envelope, MsgBody, MsgKind};

/// The `MsgBody::Custom` kind all call signaling uses (task book WP-08a).
pub const SIGNAL_KIND: &str = custom_kind::RTC_SIGNAL;

/// Deterministic 1:1 conversation id (mirrors `wxlike_platform::direct_conv_id`,
/// kept local so this crate does not depend on the platform).
fn direct_conv_id(a: &str, b: &str) -> String {
    let (mut x, mut y) = (a.to_string(), b.to_string());
    if x > y {
        std::mem::swap(&mut x, &mut y);
    }
    format!("d-{x}~{y}")
}

/// 每个收件人一个有界队列：信令只在房间存活期间存在，不落库、不占消息序列号
/// （ADR-7 / 不变式 3——通话信令绝不消耗会话 seq）。
///
/// 有界是必须的：ICE trickle 在弱网下会短时间刷几十条候选，无界队列等于
/// 给对端塞一个 OOM。满了丢**最旧**的一条，新候选永远比旧候选有用。
#[derive(Debug, Default, Clone)]
pub struct SignalBox {
    per_user: std::collections::HashMap<String, std::collections::VecDeque<Signal>>,
    cap: usize,
}

impl SignalBox {
    pub fn new(cap: usize) -> Self {
        Self {
            per_user: std::collections::HashMap::new(),
            cap: cap.max(1),
        }
    }

    /// 入队；返回因超限被丢弃的条数（0 表示没丢）。
    pub fn push(&mut self, signal: Signal) -> usize {
        let cap = self.cap;
        let q = self.per_user.entry(signal.to.clone()).or_default();
        q.push_back(signal);
        let mut dropped = 0;
        while q.len() > cap {
            q.pop_front();
            dropped += 1;
        }
        dropped
    }

    /// 取走某人的全部信令（读即清）。
    pub fn drain(&mut self, user: &str) -> Vec<Signal> {
        self.per_user
            .remove(user)
            .map(|q| q.into_iter().collect())
            .unwrap_or_default()
    }

    pub fn pending(&self, user: &str) -> usize {
        self.per_user.get(user).map(|q| q.len()).unwrap_or(0)
    }

    /// 有人离房/被踢：把发给他的积压清掉，返回被丢弃的条数（审计要记数）。
    pub fn forget(&mut self, user: &str) -> usize {
        self.per_user.remove(user).map(|q| q.len()).unwrap_or(0)
    }

    pub fn total(&self) -> usize {
        self.per_user.values().map(|q| q.len()).sum()
    }
}

/// Everything both sides need to agree on a call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signal {
    pub v: u32,
    pub call_id: String,
    /// Sender user id — must equal `Envelope.sender` or the signal is forged.
    pub from: String,
    pub to: String,
    pub content: SignalContent,
}

/// SDP blobs are passed through as JSON values: this crate negotiates media
/// through `str0m`'s SDP types and never needs to parse the blobs itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignalContent {
    /// Caller → callee: ring, with the media offer.
    Invite { sdp: serde_json::Value, video: bool },
    /// Callee → caller: answered, with the media answer.
    Accept { sdp: serde_json::Value },
    /// Callee → caller: declined.
    Reject,
    /// Caller → callee: withdrawn before an answer (also the ring timeout).
    Cancel,
    /// Either side: an active call is over.
    Hangup,
    /// ICE candidate trickle (both directions).
    Candidate {
        sdp_mid: String,
        sdp_mline_index: u32,
        candidate: String,
    },
    /// ADR-25 / LiveKit path: the server hands out the sidecar room instead of
    /// negotiating media over this channel. The payload is a room name plus
    /// the engine that owns it — deliberately **not** an SDP blob, because a
    /// second engine writing into the same room means two room facts.
    RoomAssigned { room: String, engine: String },
}

impl SignalContent {
    /// True for the str0m cold-standby trickle path. A LiveKit room refuses
    /// these (see `crate::room::Room::post_signal`).
    pub fn is_trickle_candidate(&self) -> bool {
        matches!(self, Self::Candidate { .. })
    }

    /// True when the content carries an SDP body — only the cold-standby
    /// engine needs one; on the LiveKit path media negotiation happens
    /// between the client SDK and the sidecar.
    pub fn carries_sdp(&self) -> bool {
        matches!(self, Self::Invite { .. } | Self::Accept { .. })
    }
}

/// Client-side guard against replayed room snapshots.
///
/// Signaling is best-effort and can arrive out of order; a roster update whose
/// `revision` is behind what we already applied must be dropped instead of
/// resurrecting a member who left (or a call that ended). Equal revisions are
/// treated as duplicate delivery of the same snapshot and are ignored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RosterWatermark {
    applied: u64,
}

impl RosterWatermark {
    pub fn new(applied: u64) -> Self {
        Self { applied }
    }

    pub fn applied(&self) -> u64 {
        self.applied
    }

    pub fn is_stale(&self, revision: u64) -> bool {
        revision <= self.applied
    }

    /// True = apply this snapshot now. Advances the watermark.
    pub fn accept(&mut self, revision: u64) -> bool {
        if self.is_stale(revision) {
            return false;
        }
        self.applied = revision;
        true
    }
}

impl Signal {
    pub fn new(
        call_id: impl Into<String>,
        from: impl Into<String>,
        to: impl Into<String>,
        content: SignalContent,
    ) -> Self {
        Self {
            v: 1,
            call_id: call_id.into(),
            from: from.into(),
            to: to.into(),
            content,
        }
    }

    /// Wraps the signal into a hub-routable envelope.
    pub fn to_envelope(&self, seq: u64) -> anyhow::Result<Envelope> {
        Ok(Envelope {
            version: wxlike_core::PROTOCOL_VERSION,
            conv_id: direct_conv_id(&self.from, &self.to),
            sender: self.from.clone(),
            to: vec![self.to.clone()],
            seq,
            kind: MsgKind::Text,
            body: MsgBody::Custom {
                kind: SIGNAL_KIND.into(),
                payload: serde_json::to_value(self)?,
            },
            sent_at_ms: wxlike_core::types::now_ms(),
            msg_id: Envelope::new_msg_id(),
            quote_of: None,
            receipt: None,
        })
    }

    /// Extracts a signal, refusing envelopes whose claimed `from` disagrees
    /// with the envelope's authenticated sender (the hub already enforces
    /// sender identity; this is the callee-side belt to that braces).
    pub fn from_envelope(env: &Envelope) -> Option<Self> {
        let MsgBody::Custom { kind, payload } = &env.body else {
            return None;
        };
        if kind != SIGNAL_KIND {
            return None;
        }
        let signal: Self = serde_json::from_value(payload.clone()).ok()?;
        if signal.from != env.sender {
            return None; // forged sender
        }
        if signal.v != 1 {
            return None; // unknown signaling version
        }
        Some(signal)
    }
}

/// Glare arbitration: both users sent an invite to each other at once
/// ("双方同抢"). Both sides must reach the *same* decision without another
/// round trip, so the rule is a pure function of the two call identities:
/// the call whose `(from, call_id)` sorts smaller survives; its author stays
/// caller, the other side yields and becomes the callee.
///
/// Returns `true` when `ours` wins.
pub fn ours_wins_glare(ours: (&str, &str), theirs: (&str, &str)) -> bool {
    (ours.0, ours.1) < (theirs.0, theirs.1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxlike_core::{MsgBody, MsgKind};

    fn env_for(signal: &Signal) -> Envelope {
        let mut e = signal.to_envelope(1).unwrap();
        e.sender = signal.from.clone();
        e
    }

    #[test]
    fn envelope_round_trip_all_content_kinds() {
        let kinds = [
            SignalContent::Invite {
                sdp: serde_json::json!({"sdp": "offer"}),
                video: true,
            },
            SignalContent::Accept {
                sdp: serde_json::json!({"sdp": "answer"}),
            },
            SignalContent::Reject,
            SignalContent::Cancel,
            SignalContent::Hangup,
            SignalContent::Candidate {
                sdp_mid: "0".into(),
                sdp_mline_index: 0,
                candidate: "candidate:1 1 udp 2130706431 127.0.0.1 5000 typ host".into(),
            },
        ];
        for content in kinds {
            let s = Signal::new("call-1", "alice", "bob", content);
            let env = env_for(&s);
            let back = Signal::from_envelope(&env).expect("round trip");
            assert_eq!(back, s);
        }
    }

    #[test]
    fn forged_sender_is_rejected() {
        let s = Signal::new("call-1", "alice", "bob", SignalContent::Reject);
        let mut env = env_for(&s);
        env.sender = "mallory".into(); // envelope came from someone else
        assert!(Signal::from_envelope(&env).is_none());
    }

    #[test]
    fn non_signal_envelopes_are_ignored() {
        let env = Envelope {
            version: wxlike_core::PROTOCOL_VERSION,
            conv_id: "d".into(),
            sender: "alice".into(),
            to: vec!["bob".into()],
            seq: 1,
            kind: MsgKind::Text,
            body: MsgBody::Text("hi".into()),
            sent_at_ms: 0,
            msg_id: [1; 16],
            quote_of: None,
            receipt: None,
        };
        assert!(Signal::from_envelope(&env).is_none());
    }

    #[test]
    fn glare_rule_is_deterministic_and_symmetric() {
        let a = ("alice", "call-a");
        let b = ("bob", "call-b");
        // both orders of evaluation agree on the winner
        let ours = ours_wins_glare(a, b);
        let theirs = ours_wins_glare(b, a);
        assert!(ours != theirs, "exactly one side wins");
        assert!(ours, "(alice,call-a) sorts before (bob,call-b)");
    }

    #[test]
    fn glare_rule_breaks_ties_on_call_id_when_same_user() {
        // self-glare (two devices) still deterministic
        assert!(ours_wins_glare(("alice", "aaa"), ("alice", "bbb")));
        assert!(!ours_wins_glare(("alice", "bbb"), ("alice", "aaa")));
    }
}
