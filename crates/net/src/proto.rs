//! G03-04: protocol v3 control plane — negotiation, capabilities and the
//! carrier-independent binary vectors.
//!
//! v2 is the frozen legacy shape (`Hello/Auth`-only, account-scoped routes,
//! socket-write acknowledgement). v3 adds the six named control frames the card
//! requires — `ClientHello`, `Auth`, `Subscribe`, `Event`, `Ack`, `Resume` — as
//! **append-only [`WireFrame`] variants**: bincode encodes an enum by variant
//! index, so appending keeps every v2 frame byte-identical. A v2 peer can still
//! talk; it simply never receives a v3 frame, because the hub only sends one
//! after the peer declared v3 in its `ClientHello`.
//!
//! The vectors in [`vectors`] are the shared fixture: the WS and QUIC tests feed
//! the same bytes through their real transport and must get the same decoded
//! frame and the same re-encoded bytes back.

use serde::{Deserialize, Serialize};

use crate::wire::WireFrame;

/// v3 = device-scoped control plane (ClientHello/Subscribe/Event/Ack/Resume).
pub const PROTOCOL_V3: u8 = 3;
/// Oldest protocol the hub still serves.
pub const MIN_SUPPORTED: u8 = 2;

/// What a peer claims to support. Unknown bits are ignored, so a newer client
/// cannot be rejected by an older hub for capabilities it does not use.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    /// The peer sends `Ack{stored_cursor}` after persisting; without it the hub
    /// keeps the v2 behaviour (ack on socket write) for that connection.
    StoredAck = 1,
    /// The peer sends `Resume{cursor}` after a reconnect instead of waiting for
    /// a parked-frame flush.
    Resume = 2,
    /// The peer honours `Event` range hints (server does not have to fan out
    /// full pages when a gap is small).
    EventHints = 4,
}

impl Capability {
    pub fn bit(self) -> u32 {
        self as u32
    }

    pub fn from_bit(v: u32) -> Option<Self> {
        match v {
            1 => Some(Self::StoredAck),
            2 => Some(Self::Resume),
            4 => Some(Self::EventHints),
            _ => None,
        }
    }
}

/// Encode a capability set as a bitmask (stable across carriers).
pub fn caps_bits(caps: &[Capability]) -> u32 {
    caps.iter().map(|c| c.bit()).sum()
}

/// Decode a bitmask, dropping bits this build does not know.
pub fn caps_of(bits: u32) -> Vec<Capability> {
    [
        Capability::StoredAck,
        Capability::Resume,
        Capability::EventHints,
    ]
    .into_iter()
    .filter(|c| bits & c.bit() != 0)
    .collect()
}

/// Outcome of the hello exchange — kept as data so the hub can *observe* which
/// protocol generation a connection is on (metrics, and the v2 kill switch).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Negotiated {
    pub version: u8,
    pub stored_ack: bool,
    pub resume: bool,
    pub event_hints: bool,
}

impl Negotiated {
    /// A v2 peer: account-scoped route, acknowledgement on socket write.
    pub const V2: Negotiated = Negotiated {
        version: 2,
        stored_ack: false,
        resume: false,
        event_hints: false,
    };

    /// Version negotiation: reject anything the build cannot serve, and never
    /// let a peer talk itself *up* by claiming a version it does not implement
    /// — features still come from the declared capability bits.
    pub fn negotiate(declared: u8, bits: u32) -> Result<Self, u8> {
        if !(MIN_SUPPORTED..=PROTOCOL_V3).contains(&declared) {
            return Err(declared);
        }
        let caps = caps_of(bits);
        Ok(Self {
            version: if declared >= PROTOCOL_V3 {
                PROTOCOL_V3
            } else {
                2
            },
            stored_ack: caps.contains(&Capability::StoredAck) && declared >= PROTOCOL_V3,
            resume: caps.contains(&Capability::Resume) && declared >= PROTOCOL_V3,
            event_hints: caps.contains(&Capability::EventHints) && declared >= PROTOCOL_V3,
        })
    }
}

/// Is this frame one of the v3 control frames? (A v2-only connection that sends
/// one is a protocol error, not a feature: the hub must close, not ignore.)
pub fn is_v3_control(frame: &WireFrame) -> bool {
    matches!(
        frame,
        WireFrame::ClientHello { .. }
            | WireFrame::Subscribe { .. }
            | WireFrame::Ack { .. }
            | WireFrame::Resume { .. }
    )
}

/// One shared-fixture entry: the bytes and the frame they must decode to.
#[derive(Clone, Debug)]
pub struct Vector {
    pub name: &'static str,
    pub frame: WireFrame,
}

/// The fixture both carriers are tested against. Every v3 control frame appears
/// here, plus one v2 frame of each legacy kind, so "the sequence numbering is
/// the same on every platform" is a checked fact rather than a claim.
/// A text envelope with every random/clock-dependent field pinned, so the
/// fixture blob is reproducible across runs and platforms.
fn fixed_env() -> wxlike_core::Envelope {
    let mut env = wxlike_core::Envelope::text("g-1", "u-alice", 7, "hello over both carriers");
    env.msg_id = [7u8; 16];
    env.sent_at_ms = 1_700_000_000_000;
    env
}

pub fn vectors() -> Vec<Vector> {
    let mut out = v2_vectors();
    out.extend(v3_vectors());
    out
}

/// Fixture sequence numbers. Arbitrary values, but pinned: both carriers compare
/// these bytes, so a change here is a protocol change.
const SEQ_FIRST: u64 = 41;
const SEQ_LAST: u64 = 44;
const SEQ_AHEAD: u64 = 900;
const SEQ_CURSOR: u64 = 40;

/// The frozen legacy vocabulary: every one of these must keep its exact bytes.
fn v2_vectors() -> Vec<Vector> {
    [
        (
            "v2.hello",
            WireFrame::Hello {
                token: "tok-legacy".into(),
            },
        ),
        (
            "v2.hello_ack",
            WireFrame::HelloAck {
                user_id: "u-alice".into(),
                session_id: "s-1".into(),
            },
        ),
        (
            "v2.hello_reject",
            WireFrame::HelloReject {
                reason: "invalid or expired credential".into(),
            },
        ),
        ("v2.ping", WireFrame::Ping),
        ("v2.msg", WireFrame::Msg(fixed_env())),
    ]
    .into_iter()
    .map(|(name, frame)| Vector { name, frame })
    .collect()
}

/// The v3 control frames named by the card.
fn v3_vectors() -> Vec<Vector> {
    let hello = WireFrame::ClientHello {
        device_id: "dev-phone-1".into(),
        protocol: PROTOCOL_V3,
        caps: caps_bits(&[Capability::StoredAck, Capability::Resume]),
    };
    let acked = v3_vectors_a();
    let mut out = vec![
        vector("v3.client_hello", hello),
        vector(
            "v3.auth",
            WireFrame::Auth {
                user_id: "u-alice".into(),
                device_id: "dev-phone-1".into(),
                session_id: "s-1".into(),
                version: PROTOCOL_V3,
            },
        ),
        vector(
            "v3.subscribe",
            WireFrame::Subscribe {
                scopes: vec!["conv:g-1".into(), "conv:g-2".into()],
            },
        ),
    ];
    out.extend(acked);
    out
}

fn v3_vectors_a() -> Vec<Vector> {
    let event = |from: u64, to: u64, gap: bool| WireFrame::Event {
        conv_id: "g-1".into(),
        from_seq: from,
        to_seq: to,
        gap,
    };
    vec![
        vector("v3.event", event(SEQ_FIRST, SEQ_LAST, false)),
        vector("v3.event_gap", event(SEQ_FIRST, SEQ_AHEAD, true)),
        vector(
            "v3.ack",
            WireFrame::Ack {
                conv_id: "g-1".into(),
                stored_cursor: SEQ_LAST,
            },
        ),
        vector(
            "v3.resume",
            WireFrame::Resume {
                conv_id: "g-1".into(),
                cursor: SEQ_CURSOR,
            },
        ),
    ]
}

fn vector(name: &'static str, frame: WireFrame) -> Vector {
    Vector { name, frame }
}

/// Deterministic bytes for the fixture: `name` + frame, one record per line,
/// hex-encoded. Both transports decode this same blob.
pub fn vectors_blob() -> String {
    let mut out = String::new();
    for v in vectors() {
        let bytes = v
            .frame
            .to_bytes()
            .unwrap_or_else(|e| panic!("{} must encode: {e}", v.name));
        out.push_str(&format!("{}\t{}\n", v.name, hex::encode(&bytes)));
    }
    out
}

/// Decode the fixture blob back into `(name, frame)` pairs, verifying the bytes
/// round-trip. Used by the WS and QUIC carrier tests.
pub fn parse_vectors_blob(blob: &str) -> anyhow::Result<Vec<(String, WireFrame)>> {
    let mut out = Vec::new();
    for line in blob.lines() {
        let (name, hex_bytes) = line
            .split_once('\t')
            .ok_or_else(|| anyhow::anyhow!("bad fixture line: {line}"))?;
        let bytes = hex::decode(hex_bytes)?;
        let frame = WireFrame::from_bytes(&bytes)?;
        let again = frame.to_bytes()?;
        anyhow::ensure!(again == bytes, "{name}: re-encode differs from the vector");
        out.push((name.to_string(), frame));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frozen v2 shape, declared independently. bincode encodes an enum by
    /// variant index, so if anyone reorders or inserts into [`WireFrame`] these
    /// mirrors stop matching and the test fails — that is the compatibility pin.
    #[derive(Serialize)]
    enum V2Mirror {
        Hello { token: String },
        HelloAck { user_id: String, session_id: String },
        HelloReject { reason: String },
        Msg(wxlike_core::Envelope),
        Ping,
    }

    #[test]
    fn legacy_frames_keep_their_byte_shape() {
        let cases: Vec<(WireFrame, V2Mirror)> = vec![
            (
                WireFrame::Hello {
                    token: "tok-legacy".into(),
                },
                V2Mirror::Hello {
                    token: "tok-legacy".into(),
                },
            ),
            (
                WireFrame::HelloAck {
                    user_id: "u-alice".into(),
                    session_id: "s-1".into(),
                },
                V2Mirror::HelloAck {
                    user_id: "u-alice".into(),
                    session_id: "s-1".into(),
                },
            ),
            (
                WireFrame::HelloReject {
                    reason: "no authenticator".into(),
                },
                V2Mirror::HelloReject {
                    reason: "no authenticator".into(),
                },
            ),
            (WireFrame::Msg(fixed_env()), V2Mirror::Msg(fixed_env())),
            (WireFrame::Ping, V2Mirror::Ping),
        ];
        for (now, legacy) in cases {
            let a = now.to_bytes().unwrap();
            let b = bincode::serialize(&legacy).unwrap();
            assert_eq!(a, b, "{} changed shape", name_of(&now));
            // and the legacy bytes still decode with the current enum
            let back = WireFrame::from_bytes(&b).unwrap();
            assert_eq!(format!("{back:?}"), format!("{now:?}"));
        }
    }

    fn name_of(frame: &WireFrame) -> &'static str {
        match frame {
            WireFrame::Hello { .. } => "Hello",
            WireFrame::HelloAck { .. } => "HelloAck",
            WireFrame::HelloReject { .. } => "HelloReject",
            WireFrame::Msg(_) => "Msg",
            WireFrame::Ping => "Ping",
            _ => "other",
        }
    }

    #[test]
    fn v3_variants_come_after_the_legacy_ones() {
        // first byte is the low byte of the variant index in bincode's fixint
        // encoding: legacy occupy 0..=4, v3 starts at 5
        let index = |f: &WireFrame| f.to_bytes().unwrap()[0];
        assert_eq!(index(&WireFrame::Ping), 4);
        assert_eq!(
            index(&WireFrame::ClientHello {
                device_id: String::new(),
                protocol: 3,
                caps: 0,
            }),
            5
        );
        assert_eq!(
            index(&WireFrame::Resume {
                conv_id: String::new(),
                cursor: 0,
            }),
            10
        );
    }

    #[test]
    fn negotiation_refuses_unknown_versions_and_talks_nothing_up() {
        assert_eq!(Negotiated::negotiate(1, 7), Err(1));
        assert_eq!(Negotiated::negotiate(9, 7), Err(9));
        assert_eq!(Negotiated::negotiate(2, 7).unwrap(), Negotiated::V2);
        let all = caps_of(caps_bits(&[
            Capability::StoredAck,
            Capability::Resume,
            Capability::EventHints,
        ]));
        assert_eq!(all.len(), 3);
        let v3 = Negotiated::negotiate(3, caps_bits(&all)).unwrap();
        assert!(v3.stored_ack && v3.resume && v3.event_hints);
        // unknown bits are ignored rather than rejected
        assert!(Negotiated::negotiate(3, 1 | 2 | 256).unwrap().stored_ack);
    }

    #[test]
    fn fixture_round_trips_and_is_deterministic() {
        let blob = vectors_blob();
        let parsed = parse_vectors_blob(&blob).unwrap();
        assert_eq!(parsed.len(), vectors().len());
        assert_eq!(blob, vectors_blob(), "the fixture is byte-stable");
        for (v, (name, frame)) in vectors().into_iter().zip(parsed) {
            assert_eq!(name, v.name);
            assert_eq!(format!("{frame:?}"), format!("{:?}", v.frame), "{}", v.name);
        }
    }
}
