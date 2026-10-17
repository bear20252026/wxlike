//! WP-08a call manager: glues the state machine, signaling, and media into
//! the thing a UI actually drives.
//!
//! Design for testability: every method is **synchronous and pure with
//! respect to the clock** — signals that must go out are collected into the
//! return value, and time-based behavior (ring timeout, peer-silence
//! watchdog) is driven by [`CallManager::check_deadlines`](crate::call::CallManager::check_deadlines) with the caller's
//! `now`. The async shell (presence check, sending envelopes over QUIC) is
//! one thin layer at the edges.
//!
//! Glare ("双方同抢"): when two users invite each other simultaneously, both
//! receive the other's invite while their own is ringing. Both sides apply
//! the same deterministic rule ([`crate::signal::ours_wins_glare`]): the
//! invite whose `(from, call_id)` sorts smaller survives; its author stays
//! caller and the other yields its own call and answers the winner's.

use std::collections::HashMap;
use std::time::Instant;

use crate::media::MediaSession;
use crate::signal::{ours_wins_glare, Signal, SignalContent};
use crate::state::{CallState, End, Event, Phase, PEER_SILENCE_TIMEOUT_SECS, RING_TIMEOUT_SECS};

/// Something the host must do as a result of a manager call.
#[derive(Debug, Clone)]
pub enum Action {
    /// Send this signal envelope to the peer.
    Send(Signal),
    /// The call ended; reason included for the UI.
    Ended { call_id: String, end: End },
    /// Media datagrams are waiting (host pumps them; sans-I/O, so the host
    /// owns the socket). Never emitted by the sync core — see `media_mut`.
    MediaReady { call_id: String },
}

struct Call {
    peer: String,
    state: CallState,
    /// Whether the call asked for a video m-line (WP-08b); read via
    /// [`CallManager::wants_video`] by the UI and re-invite paths.
    #[allow(dead_code)]
    video: bool,
    /// The SDP answer to send once we accept (callee side holds the offer).
    remote_offer: Option<serde_json::Value>,
    media: Option<MediaSession>,
    entered_ringing_at: Option<Instant>,
    last_peer_activity: Option<Instant>,
}

impl Call {
    fn live(&self) -> bool {
        !matches!(self.state.phase(), Phase::Ended(_))
    }
}

/// Synchronous call manager core. `start`/`handle_*` return the actions the
/// host owes the world (send this, note that ended).
pub struct CallManager {
    me: String,
    calls: HashMap<String, Call>,
    seq: i64,
}

fn new_call_id(me: &str) -> String {
    let entropy = wxlike_core::types::now_ms() ^ (rand_for_id() as u64);
    format!("{me}-{entropy:x}")
}

fn rand_for_id() -> u32 {
    // one small random ingredient so two restarts in the same millisecond
    // still produce distinct call ids
    use rand::RngCore as _;
    rand::thread_rng().next_u32()
}

impl CallManager {
    pub fn new(me: impl Into<String>) -> Self {
        Self {
            me: me.into(),
            calls: HashMap::new(),
            seq: 0,
        }
    }

    pub fn me(&self) -> &str {
        &self.me
    }

    pub fn phase_of(&self, call_id: &str) -> Option<Phase> {
        self.calls.get(call_id).map(|c| c.state.phase().clone())
    }

    /// The live (non-ended) call with `peer`, if any.
    pub fn live_call_with(&self, peer: &str) -> Option<String> {
        self.calls
            .iter()
            .find(|(_, c)| c.peer == peer && c.live())
            .map(|(id, _)| id.clone())
    }

    /// Starts a call. `peer_online` comes from the presence oracle — an
    /// offline peer fails **immediately** (task book A3), no 30 s hang and
    /// nothing sent.
    pub fn start(&mut self, peer: &str, video: bool, peer_online: bool) -> (String, Vec<Action>) {
        let mut actions = Vec::new();
        let call_id = new_call_id(&self.me);

        if !peer_online {
            let mut state = CallState::caller();
            state.apply(Event::LocalInvite);
            // undeliverable is known before anything is sent
            state.apply(Event::InviteUndeliverable);
            let end = match state.phase() {
                Phase::Ended(end) => *end,
                _ => End::Failed,
            };
            self.calls.insert(
                call_id.clone(),
                Call {
                    peer: peer.to_string(),
                    state,
                    video,
                    remote_offer: None,
                    media: None,
                    entered_ringing_at: None,
                    last_peer_activity: None,
                },
            );
            actions.push(Action::Ended {
                call_id: call_id.clone(),
                end,
            });
            return (call_id, actions);
        }

        let mut media = MediaSession::new(true);
        let offer = media.offer(video).ok();
        let mut state = CallState::caller();
        state.apply(Event::LocalInvite);

        if let Some(offer) = &offer {
            let signal = Signal::new(
                call_id.clone(),
                self.me.clone(),
                peer.to_string(),
                SignalContent::Invite {
                    sdp: offer.clone(),
                    video,
                },
            );
            actions.push(Action::Send(signal));
        }

        let now = Instant::now();
        self.calls.insert(
            call_id.clone(),
            Call {
                peer: peer.to_string(),
                state,
                video,
                remote_offer: None,
                media: Some(media),
                entered_ringing_at: Some(now),
                last_peer_activity: Some(now),
            },
        );
        (call_id, actions)
    }

    /// Callee accepts a ringing call.
    pub fn accept(&mut self, call_id: &str) -> Vec<Action> {
        let Some(call) = self.calls.get_mut(call_id) else {
            return Vec::new();
        };
        if !call.state.apply(Event::LocalAccept) {
            return Vec::new();
        }
        call.entered_ringing_at = None;
        let now = Instant::now();
        call.last_peer_activity = Some(now);

        // answer the media offer if we have one
        if let (Some(media), Some(offer)) = (&mut call.media, call.remote_offer.clone()) {
            if let Ok(answer) = media.accept_offer(offer) {
                let signal = Signal::new(
                    call_id.to_string(),
                    self.me.clone(),
                    call.peer.clone(),
                    SignalContent::Accept { sdp: answer },
                );
                return vec![Action::Send(signal)];
            }
        }
        Vec::new()
    }

    /// Callee declines.
    pub fn reject(&mut self, call_id: &str) -> Vec<Action> {
        self.finish_with(call_id, Event::LocalReject, || Some(SignalContent::Reject))
    }

    /// Caller withdraws before an answer.
    pub fn cancel(&mut self, call_id: &str) -> Vec<Action> {
        self.finish_with(call_id, Event::LocalCancel, || Some(SignalContent::Cancel))
    }

    /// Either side ends an active call.
    pub fn hangup(&mut self, call_id: &str) -> Vec<Action> {
        self.finish_with(call_id, Event::LocalHangup, || Some(SignalContent::Hangup))
    }

    fn finish_with(
        &mut self,
        call_id: &str,
        event: Event,
        signal: impl FnOnce() -> Option<SignalContent>,
    ) -> Vec<Action> {
        let Some(call) = self.calls.get_mut(call_id) else {
            return Vec::new();
        };
        if !call.state.apply(event) {
            return Vec::new();
        }
        let mut actions = Vec::new();
        if let Some(content) = signal() {
            actions.push(Action::Send(Signal::new(
                call_id.to_string(),
                self.me.clone(),
                call.peer.clone(),
                content,
            )));
        }
        if let Phase::Ended(end) = call.state.phase() {
            actions.push(Action::Ended {
                call_id: call_id.to_string(),
                end: *end,
            });
        }
        actions
    }

    /// Feed one inbound signal. Returns the actions owed.
    pub fn handle_signal(&mut self, signal: Signal) -> Vec<Action> {
        match signal.content.clone() {
            SignalContent::Invite { sdp, video } => self.on_invite(signal, sdp, video),
            SignalContent::Accept { sdp } => self.on_accept(signal, sdp),
            SignalContent::Reject => self.on_simple(signal, Event::RemoteReject),
            SignalContent::Cancel => self.on_simple(signal, Event::RemoteHangup),
            SignalContent::Hangup => self.on_simple(signal, Event::RemoteHangup),
            SignalContent::Candidate { .. } => {
                // candidates ride the media session; handshake happens at
                // accept/answer time in the in-process loopback paths, so
                // nothing to do here for now (host adds them via media_mut).
                Vec::new()
            }
        }
    }

    fn on_invite(&mut self, signal: Signal, sdp: serde_json::Value, video: bool) -> Vec<Action> {
        // glare: we are ringing *them* and they invite us too
        if let Some(ours) = self.ringing_call_to(&signal.from) {
            let (ours_from, ours_id) = (self.me.clone(), ours.0.clone());
            let win = ours_wins_glare((&ours_from, &ours_id), (&signal.from, &signal.call_id));
            if win {
                // our invite survives; drop theirs silently — they will see
                // ours and yield by the same rule
                return Vec::new();
            }
            // we lose: cancel our own call and become the callee of theirs
            let mut actions =
                self.finish_with(&ours.0, Event::LocalCancel, || Some(SignalContent::Cancel));
            actions.extend(self.become_callee(signal, sdp, video));
            return actions;
        }
        self.become_callee(signal, sdp, video)
    }

    fn become_callee(
        &mut self,
        signal: Signal,
        sdp: serde_json::Value,
        video: bool,
    ) -> Vec<Action> {
        let media = MediaSession::new(false);
        let call_id = signal.call_id.clone();
        let peer = signal.from.clone();
        let mut state = CallState::callee();
        if !state.apply(Event::RemoteInvite) {
            return Vec::new();
        }
        let now = Instant::now();
        self.calls.insert(
            call_id.clone(),
            Call {
                peer,
                state,
                video,
                remote_offer: Some(sdp),
                media: Some(media),
                entered_ringing_at: Some(now),
                last_peer_activity: Some(now),
            },
        );
        Vec::new()
    }

    fn ringing_call_to(&self, peer: &str) -> Option<(String, ())> {
        self.calls
            .iter()
            .find(|(_, c)| {
                c.peer == peer && c.state.is_caller() && matches!(c.state.phase(), Phase::Ringing)
            })
            .map(|(id, _)| (id.clone(), ()))
    }

    fn on_accept(&mut self, signal: Signal, sdp: serde_json::Value) -> Vec<Action> {
        let Some(call) = self.calls.get_mut(&signal.call_id) else {
            return Vec::new();
        };
        call.last_peer_activity = Some(Instant::now());
        if !call.state.apply(Event::RemoteAccept) {
            return Vec::new();
        }
        call.entered_ringing_at = None;
        if let Some(media) = &mut call.media {
            let _ = media.accept_answer(sdp);
        }
        Vec::new()
    }

    fn on_simple(&mut self, signal: Signal, event: Event) -> Vec<Action> {
        let Some(call) = self.calls.get_mut(&signal.call_id) else {
            return Vec::new();
        };
        call.last_peer_activity = Some(Instant::now());
        if !call.state.apply(event) {
            return Vec::new();
        }
        let mut actions = Vec::new();
        if let Phase::Ended(end) = call.state.phase() {
            actions.push(Action::Ended {
                call_id: signal.call_id.clone(),
                end: *end,
            });
        }
        actions
    }

    /// Drive time-based behavior. Returns actions (timeouts, watchdogs).
    pub fn check_deadlines(&mut self, now: Instant) -> Vec<Action> {
        let mut actions = Vec::new();
        let ids: Vec<String> = self.calls.keys().cloned().collect();
        for id in ids {
            let Some(call) = self.calls.get_mut(&id) else {
                continue;
            };
            match call.state.phase() {
                Phase::Ringing => {
                    if let Some(since) = call.entered_ringing_at {
                        if now.duration_since(since).as_secs() >= RING_TIMEOUT_SECS {
                            call.state.apply(Event::RingTimeout);
                            // tell the peer we stopped ringing (caller side)
                            if call.state.is_caller() {
                                actions.push(Action::Send(Signal::new(
                                    id.clone(),
                                    self.me.clone(),
                                    call.peer.clone(),
                                    SignalContent::Cancel,
                                )));
                            }
                            actions.push(Action::Ended {
                                call_id: id.clone(),
                                end: End::Cancelled,
                            });
                        }
                    }
                }
                Phase::Active => {
                    if let Some(last) = call.last_peer_activity {
                        if now.duration_since(last).as_secs() >= PEER_SILENCE_TIMEOUT_SECS {
                            call.state.apply(Event::PeerSilent);
                            actions.push(Action::Send(Signal::new(
                                id.clone(),
                                self.me.clone(),
                                call.peer.clone(),
                                SignalContent::Hangup,
                            )));
                            actions.push(Action::Ended {
                                call_id: id.clone(),
                                end: End::Failed,
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        actions
    }

    /// The media session of a call (host pumps datagrams / writes frames).
    pub fn media_mut(&mut self, call_id: &str) -> Option<&mut MediaSession> {
        self.calls.get_mut(call_id).and_then(|c| c.media.as_mut())
    }

    /// Notifies the manager that media/peer traffic happened (feeds the
    /// silence watchdog).
    pub fn note_peer_activity(&mut self, call_id: &str) {
        if let Some(call) = self.calls.get_mut(call_id) {
            call.last_peer_activity = Some(Instant::now());
        }
    }

    pub fn next_seq(&mut self) -> i64 {
        self.seq += 1;
        self.seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn offline_peer_fails_immediately() {
        let mut mgr = CallManager::new("alice");
        let (call_id, actions) = mgr.start("bob", false, false);
        assert_eq!(mgr.phase_of(&call_id), Some(Phase::Ended(End::Failed)));
        assert!(matches!(actions.as_slice(), [Action::Ended { .. }]));
        // and nothing was sent to anyone
        assert!(!actions.iter().any(|a| matches!(a, Action::Send(_))));
    }

    #[test]
    fn full_handshake_then_hangup() {
        let mut alice = CallManager::new("alice");
        let (call_id, actions) = alice.start("bob", false, true);
        assert_eq!(alice.phase_of(&call_id), Some(Phase::Ringing));
        assert!(matches!(
            actions[0],
            Action::Send(Signal {
                content: SignalContent::Invite { .. },
                ..
            })
        ));

        // bob receives the invite
        let mut bob = CallManager::new("bob");
        let invite_signal = match &actions[0] {
            Action::Send(s) => s.clone(),
            _ => unreachable!(),
        };
        bob.handle_signal(invite_signal.clone());
        let bob_id = invite_signal.call_id.clone();
        assert_eq!(bob.phase_of(&bob_id), Some(Phase::Ringing));

        // bob accepts -> accept signal -> alice becomes active
        let accept_actions = bob.accept(&bob_id);
        match &accept_actions[0] {
            Action::Send(Signal {
                content: SignalContent::Accept { .. },
                ..
            }) => {}
            other => panic!("expected accept signal, got {other:?}"),
        }
        alice.handle_signal(match &accept_actions[0] {
            Action::Send(s) => s.clone(),
            _ => unreachable!(),
        });
        assert_eq!(alice.phase_of(&call_id), Some(Phase::Active));
        assert_eq!(bob.phase_of(&bob_id), Some(Phase::Active));

        // bob hangs up -> alice ends HungUp
        let end_actions = bob.hangup(&bob_id);
        match &end_actions[0] {
            Action::Send(Signal {
                content: SignalContent::Hangup,
                ..
            }) => {}
            other => panic!("expected hangup signal, got {other:?}"),
        }
        let actions = alice.handle_signal(match &end_actions[0] {
            Action::Send(s) => s.clone(),
            _ => unreachable!(),
        });
        assert_eq!(alice.phase_of(&call_id), Some(Phase::Ended(End::HungUp)));
        assert!(matches!(actions[0], Action::Ended { .. }));
    }

    #[test]
    fn ring_timeout_after_30s() {
        let mut alice = CallManager::new("alice");
        let (call_id, mut actions) = alice.start("bob", false, true);
        actions.clear();
        // 29 s: nothing
        let t0 = Instant::now();
        assert!(alice
            .check_deadlines(t0 + Duration::from_secs(29))
            .is_empty());
        // 30 s: cancelled, and bob is told
        let actions = alice.check_deadlines(t0 + Duration::from_secs(31));
        assert!(actions.iter().any(|a| matches!(
            a,
            Action::Send(Signal {
                content: SignalContent::Cancel,
                ..
            })
        )));
        assert!(actions.iter().any(|a| matches!(
            a,
            Action::Ended {
                end: End::Cancelled,
                ..
            }
        )));
        assert_eq!(alice.phase_of(&call_id), Some(Phase::Ended(End::Cancelled)));
    }

    #[test]
    fn peer_silence_watchdog_fails_within_10s() {
        let mut alice = CallManager::new("alice");
        let (call_id, actions) = alice.start("bob", false, true);
        let mut bob = CallManager::new("bob");
        let invite_signal = match &actions[0] {
            Action::Send(s) => s.clone(),
            _ => unreachable!(),
        };
        bob.handle_signal(invite_signal.clone());
        let accept = bob.accept(&invite_signal.call_id);
        alice.handle_signal(match &accept[0] {
            Action::Send(s) => s.clone(),
            _ => unreachable!(),
        });
        assert_eq!(alice.phase_of(&call_id), Some(Phase::Active));

        let t0 = Instant::now();
        assert!(alice
            .check_deadlines(t0 + Duration::from_secs(9))
            .is_empty());
        let actions = alice.check_deadlines(t0 + Duration::from_secs(11));
        assert!(actions.iter().any(|a| matches!(
            a,
            Action::Ended {
                end: End::Failed,
                ..
            }
        )));
        assert_eq!(alice.phase_of(&call_id), Some(Phase::Ended(End::Failed)));
    }

    #[test]
    fn glare_converges_both_sides_same_decision() {
        // alice and bob invite each other in the same instant
        let mut alice = CallManager::new("alice");
        let mut bob = CallManager::new("bob");
        let (alice_call, alice_actions) = alice.start("bob", false, true);
        let (bob_call, bob_actions) = bob.start("alice", false, true);
        let alice_invite = match &alice_actions[0] {
            Action::Send(s) => s.clone(),
            _ => unreachable!(),
        };
        let bob_invite = match &bob_actions[0] {
            Action::Send(s) => s.clone(),
            _ => unreachable!(),
        };

        // each receives the other's invite while ringing
        let a_actions = alice.handle_signal(bob_invite.clone());
        let b_actions = bob.handle_signal(alice_invite.clone());

        // rule: (alice, alice_call) < (bob, bob_call) -> alice's call wins
        // (call ids are time-random, so determine the winner by the same rule)
        let alice_wins = ours_wins_glare(("alice", &alice_call), ("bob", &bob_invite.call_id));
        if alice_wins {
            // alice ignores bob's invite; bob yields and answers alice's
            assert!(
                a_actions.iter().all(|a| !matches!(
                    a,
                    Action::Send(Signal {
                        content: SignalContent::Accept { .. },
                        ..
                    })
                )),
                "alice does not become callee"
            );
            assert_eq!(bob.phase_of(&alice_invite.call_id), Some(Phase::Ringing));
            assert_eq!(alice.phase_of(&alice_call), Some(Phase::Ringing));
        } else {
            assert_eq!(alice.phase_of(&bob_invite.call_id), Some(Phase::Ringing));
            assert_eq!(bob.phase_of(&bob_call), Some(Phase::Ringing));
        }
        let _ = b_actions;
    }
}
