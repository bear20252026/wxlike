//! WP-08a call state machine: the single source of truth for what a call is
//! allowed to do next.
//!
//! ```text
//!  caller:  Idle ──LocalInvite──► Ringing ──RemoteAccept──► Active ──hangup──► Ended(HungUp)
//!                                     │  │                                         ▲
//!  callee:  Idle ──RemoteInvite─► Ringing ──LocalAccept──► Active ──hangup─────┘
//!                                     │  │
//!          reject/cancel/timeout ─────┘  └── peer gone / undeliverable / failed
//! ```
//!
//! Every terminal is a *why*: [`End::Rejected`] (callee said no),
//! [`End::Cancelled`] (caller withdrew, or ringing timed out at 30 s),
//! [`End::HungUp`] (an active call ended normally), [`End::Failed`]
//! (infrastructure: peer offline at invite, peer silent mid-call, transport
//! errors). Illegal transitions are refused and recorded — adversarial event
//! sequences dead-end instead of panicking, which is exactly what the
//! acceptance tests probe.

use std::fmt;

/// Why a call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum End {
    /// The callee said no.
    Rejected,
    /// The caller withdrew before an answer, or ringing timed out.
    Cancelled,
    /// An active call was ended normally by either side.
    HungUp,
    /// Signaling/media failure: peer offline at invite, peer vanished
    /// mid-call, negotiation impossible.
    Failed,
}

/// Task book WP-08a: unanswered invites self-cancel after 30 seconds.
pub const RING_TIMEOUT_SECS: u64 = 30;
/// Task book A3: a peer that goes silent mid-call (process killed) must move
/// us to `Failed` within 10 seconds.
pub const PEER_SILENCE_TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Idle,
    /// Caller: invite out, waiting. Callee: being rung.
    Ringing,
    /// Both sides answered; media is (being) established.
    Active,
    /// Terminal, with the reason.
    Ended(End),
}

/// Events from signaling, media, timers, or the user. `Remote*` arrive over
/// the wire; `Local*` are this user's actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Caller: we started a call.
    LocalInvite,
    /// Callee: an invite arrived.
    RemoteInvite,
    /// Caller: signaling accepted our invite for delivery.
    InviteDelivered,
    /// Caller: the callee answered.
    RemoteAccept,
    /// Callee: we answered.
    LocalAccept,
    /// Caller: the callee declined.
    RemoteReject,
    /// Callee: we decline.
    LocalReject,
    /// Caller: withdraw before an answer.
    LocalCancel,
    /// Either side ended an active call; during ringing it is a reject
    /// (caller side) / withdrawn ring (callee side).
    RemoteHangup,
    LocalHangup,
    /// The invite could not be delivered at all (peer offline) — A3: fail
    /// immediately instead of hanging.
    InviteUndeliverable,
    /// Media watchdog: nothing from the peer for [`PEER_SILENCE_TIMEOUT_SECS`].
    PeerSilent,
    /// Ring timeout expired without an answer.
    RingTimeout,
    /// Unexpected lower-layer failure.
    Failed,
}

impl Event {
    fn name(&self) -> &'static str {
        match self {
            Event::LocalInvite => "local_invite",
            Event::RemoteInvite => "remote_invite",
            Event::InviteDelivered => "invite_delivered",
            Event::RemoteAccept => "remote_accept",
            Event::LocalAccept => "local_accept",
            Event::RemoteReject => "remote_reject",
            Event::LocalReject => "local_reject",
            Event::LocalCancel => "local_cancel",
            Event::RemoteHangup => "remote_hangup",
            Event::LocalHangup => "local_hangup",
            Event::InviteUndeliverable => "invite_undeliverable",
            Event::PeerSilent => "peer_silent",
            Event::RingTimeout => "ring_timeout",
            Event::Failed => "failed",
        }
    }
}

/// One side's call state machine. Pure: timers arrive as events, so every
/// path is a unit test with no clock.
#[derive(Debug, Clone)]
pub struct CallState {
    phase: Phase,
    is_caller: bool,
    last_refused: Option<String>,
}

impl CallState {
    pub fn caller() -> Self {
        Self {
            phase: Phase::Idle,
            is_caller: true,
            last_refused: None,
        }
    }

    pub fn callee() -> Self {
        Self {
            phase: Phase::Idle,
            is_caller: false,
            last_refused: None,
        }
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    pub fn is_caller(&self) -> bool {
        self.is_caller
    }

    /// The refusal recorded by the last rejected event, if any.
    pub fn last_refused(&self) -> Option<&str> {
        self.last_refused.as_deref()
    }

    /// Apply an event. `true` = accepted (possibly a pure acknowledgement),
    /// `false` = illegal in this phase (recorded in `last_refused`).
    pub fn apply(&mut self, event: Event) -> bool {
        let next: Option<Phase> = match (&self.phase, &event) {
            // ---- Idle ----
            (Phase::Idle, Event::LocalInvite) if self.is_caller => Some(Phase::Ringing),
            (Phase::Idle, Event::RemoteInvite) if !self.is_caller => Some(Phase::Ringing),

            // ---- Ringing ----
            (Phase::Ringing, Event::InviteDelivered) => Some(Phase::Ringing), // ack, no move
            (Phase::Ringing, Event::RemoteAccept) if self.is_caller => Some(Phase::Active),
            (Phase::Ringing, Event::LocalAccept) if !self.is_caller => Some(Phase::Active),
            (Phase::Ringing, Event::RemoteReject) if self.is_caller => {
                Some(Phase::Ended(End::Rejected))
            }
            (Phase::Ringing, Event::LocalReject) if !self.is_caller => {
                Some(Phase::Ended(End::Rejected))
            }
            (Phase::Ringing, Event::LocalCancel) if self.is_caller => {
                Some(Phase::Ended(End::Cancelled))
            }
            (Phase::Ringing, Event::RemoteHangup) => Some(if self.is_caller {
                // the callee we rang hung up / will not answer
                Phase::Ended(End::Rejected)
            } else {
                // the caller withdrew the ring before we answered
                Phase::Ended(End::Cancelled)
            }),
            (Phase::Ringing, Event::RingTimeout) => Some(Phase::Ended(End::Cancelled)),
            (Phase::Ringing, Event::InviteUndeliverable) => Some(Phase::Ended(End::Failed)),
            (Phase::Ringing, Event::Failed) => Some(Phase::Ended(End::Failed)),

            // ---- Active ----
            (Phase::Active, Event::RemoteHangup) | (Phase::Active, Event::LocalHangup) => {
                Some(Phase::Ended(End::HungUp))
            }
            // A3: peer process died; the watchdog reports it within 10 s
            (Phase::Active, Event::PeerSilent) => Some(Phase::Ended(End::Failed)),
            (Phase::Active, Event::Failed) => Some(Phase::Ended(End::Failed)),

            // everything else is illegal
            _ => None,
        };

        match next {
            Some(phase) => {
                self.phase = phase;
                self.last_refused = None;
                true
            }
            None => {
                self.last_refused = Some(format!(
                    "event {} illegal in {:?} (role: {})",
                    event.name(),
                    self.phase,
                    if self.is_caller { "caller" } else { "callee" }
                ));
                false
            }
        }
    }
}

impl fmt::Display for End {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            End::Rejected => "rejected",
            End::Cancelled => "cancelled",
            End::HungUp => "hung up",
            End::Failed => "failed",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_happy_path() {
        let mut c = CallState::caller();
        assert!(c.apply(Event::LocalInvite));
        assert_eq!(c.phase(), &Phase::Ringing);
        assert!(c.apply(Event::InviteDelivered), "delivery ack is accepted");
        assert_eq!(c.phase(), &Phase::Ringing, "ack does not advance");
        assert!(c.apply(Event::RemoteAccept));
        assert_eq!(c.phase(), &Phase::Active);
        assert!(c.apply(Event::LocalHangup));
        assert_eq!(c.phase(), &Phase::Ended(End::HungUp));
    }

    #[test]
    fn callee_happy_path() {
        let mut c = CallState::callee();
        assert!(c.apply(Event::RemoteInvite));
        assert_eq!(c.phase(), &Phase::Ringing);
        assert!(c.apply(Event::LocalAccept));
        assert_eq!(c.phase(), &Phase::Active);
        assert!(c.apply(Event::RemoteHangup));
        assert_eq!(c.phase(), &Phase::Ended(End::HungUp));
    }

    #[test]
    fn reject_cancel_timeout_paths() {
        let mut caller = CallState::caller();
        caller.apply(Event::LocalInvite);
        assert!(caller.apply(Event::RemoteReject));
        assert_eq!(caller.phase(), &Phase::Ended(End::Rejected));

        let mut c = CallState::caller();
        c.apply(Event::LocalInvite);
        assert!(c.apply(Event::LocalCancel));
        assert_eq!(c.phase(), &Phase::Ended(End::Cancelled));

        let mut t = CallState::caller();
        t.apply(Event::LocalInvite);
        assert!(t.apply(Event::RingTimeout));
        assert_eq!(t.phase(), &Phase::Ended(End::Cancelled));

        let mut callee = CallState::callee();
        callee.apply(Event::RemoteInvite);
        assert!(callee.apply(Event::LocalReject));
        assert_eq!(callee.phase(), &Phase::Ended(End::Rejected));
    }

    #[test]
    fn offline_invite_fails_immediately_not_30s() {
        // A3: 对离线用户发起呼叫 → 立即 Failed 而非挂起
        let mut caller = CallState::caller();
        caller.apply(Event::LocalInvite);
        let started = std::time::Instant::now();
        assert!(caller.apply(Event::InviteUndeliverable));
        assert_eq!(caller.phase(), &Phase::Ended(End::Failed));
        assert!(started.elapsed().as_secs() < RING_TIMEOUT_SECS);
    }

    #[test]
    fn peer_gone_mid_call_fails() {
        // A3: 通话中对方进程被杀 → 本端 Failed（由静默看门狗上报）
        let mut caller = CallState::caller();
        caller.apply(Event::LocalInvite);
        caller.apply(Event::RemoteAccept);
        assert!(caller.apply(Event::PeerSilent));
        assert_eq!(caller.phase(), &Phase::Ended(End::Failed));
    }

    #[test]
    fn illegal_transitions_refused_not_panicking() {
        let mut caller = CallState::caller();
        assert!(!caller.apply(Event::RemoteAccept), "accept with no invite");
        assert!(caller.last_refused().is_some());
        assert!(
            !caller.apply(Event::RemoteInvite),
            "caller cannot be rung from idle"
        );

        let mut callee = CallState::callee();
        assert!(!callee.apply(Event::LocalInvite), "callee never initiates");

        // ended is terminal: no zombie calls
        let mut done = CallState::caller();
        done.apply(Event::LocalInvite);
        done.apply(Event::RemoteReject);
        assert!(!done.apply(Event::RemoteAccept));
        assert!(!done.apply(Event::LocalHangup));
        assert_eq!(done.phase(), &Phase::Ended(End::Rejected));
    }

    #[test]
    fn hangup_during_ring_maps_by_role() {
        let mut caller = CallState::caller();
        caller.apply(Event::LocalInvite);
        assert!(caller.apply(Event::RemoteHangup));
        assert_eq!(caller.phase(), &Phase::Ended(End::Rejected));

        let mut callee = CallState::callee();
        callee.apply(Event::RemoteInvite);
        assert!(callee.apply(Event::RemoteHangup));
        assert_eq!(callee.phase(), &Phase::Ended(End::Cancelled));
    }

    #[test]
    fn ring_timeout_also_ends_the_callee() {
        let mut callee = CallState::callee();
        callee.apply(Event::RemoteInvite);
        assert!(callee.apply(Event::RingTimeout));
        assert_eq!(callee.phase(), &Phase::Ended(End::Cancelled));
    }
}
