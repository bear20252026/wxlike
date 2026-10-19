//! `wxlike-rtc` — WP-08 audio/video calling.
//!
//! Three layers, each independently testable:
//!
//! * [`state`] — the call state machine (who may do what, when);
//! * [`signal`] — call control over the existing QUIC envelope channel as
//!   `MsgBody::Custom { kind: "rtc-signal" }`, with glare arbitration;
//! * [`media`] — `str0m`-based ICE/DTLS/SRTP sessions (pure Rust WebRTC; the
//!   task book forbids libwebrtc C++ bindings). Media packets are opaque
//!   payloads: the `audio`/`camera` features wire real capture and codecs,
//!   and remain off by default because headless CI has neither a sound card
//!   nor a camera.
//!
//! NAT traversal: host candidates work on the same network segment with no
//! configuration; `WXLIKE_STUN_URL` adds a STUN server; `WXLIKE_TURN_URL`
#[cfg(feature = "audio")]
pub mod audio_io;
/// is the interface point for relayed setups (no bundled TURN server —
/// running one is an ops decision, see docs).
pub mod call;
pub mod capture;
pub mod dsp;
pub mod host;
pub mod ice;
pub mod media;
pub mod room;
pub mod sfu;
pub mod signal;
pub mod state;
