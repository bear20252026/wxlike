use std::collections::HashMap;
use std::sync::Mutex;

use crate::clock::Clock;
use crate::error::{DiscoveryError, Result};
use crate::id::new_id;

const WINDOW_MS: i64 = 30_000;
const MIN_SHAKE_G: f64 = 2.5;
const MAX_SAMPLE_AGE_MS: i64 = 2_000;
const COOLDOWN_MS: i64 = 60_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionSample {
    pub magnitude_g: f64,
    pub sampled_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShakeTicket {
    pub id: String,
    pub user_id: String,
    pub expires_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchRecord {
    pub id: String,
    pub users: [String; 2],
    pub matched_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MatchOutcome {
    Pending { needs_host: bool, needs_guest: bool },
    Matched(MatchRecord),
}

struct Ticket {
    user_id: String,
    expires_at_ms: i64,
    proposal_id: Option<String>,
}

struct Proposal {
    ticket_id: String,
    host: String,
    guest: String,
    host_confirmed: bool,
    guest_confirmed: bool,
    expires_at_ms: i64,
}

pub struct ShakeMatcher<C: Clock> {
    clock: C,
    tickets: Mutex<HashMap<String, Ticket>>,
    proposals: Mutex<HashMap<String, Proposal>>,
    cooldown_until: Mutex<HashMap<String, i64>>,
}

impl<C: Clock> ShakeMatcher<C> {
    pub fn new(clock: C) -> Self {
        Self {
            clock,
            tickets: Mutex::new(HashMap::new()),
            proposals: Mutex::new(HashMap::new()),
            cooldown_until: Mutex::new(HashMap::new()),
        }
    }

    pub fn start(&self, actor: &str, sample: MotionSample) -> Result<ShakeTicket> {
        let now_ms = self.clock.now_ms();
        if !sample.magnitude_g.is_finite()
            || sample.magnitude_g < MIN_SHAKE_G
            || sample.sampled_at_ms > now_ms
            || now_ms - sample.sampled_at_ms > MAX_SAMPLE_AGE_MS
        {
            return Err(DiscoveryError::InvalidArgument("shake gesture"));
        }
        self.ensure_not_cooling(actor, now_ms)?;
        let mut tickets = self
            .tickets
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        let active = tickets
            .values()
            .find(|t| t.user_id == actor && t.expires_at_ms > now_ms);
        if active.is_some() {
            return Err(DiscoveryError::Conflict("active shake window"));
        }
        let id = new_id("shake");
        tickets.insert(
            id.clone(),
            Ticket {
                user_id: actor.to_owned(),
                expires_at_ms: now_ms + WINDOW_MS,
                proposal_id: None,
            },
        );
        Ok(ShakeTicket {
            id,
            user_id: actor.to_owned(),
            expires_at_ms: now_ms + WINDOW_MS,
        })
    }

    /// 加入一个仍在时限内的摇一摇窗口；同一窗口只接待一个对方，
    /// 防止广播式骚扰。
    pub fn join(&self, actor: &str, ticket_id: &str) -> Result<String> {
        let now_ms = self.clock.now_ms();
        self.ensure_not_cooling(actor, now_ms)?;
        let mut tickets = self
            .tickets
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        let ticket = tickets.get_mut(ticket_id).ok_or(DiscoveryError::NotFound)?;
        if ticket.expires_at_ms <= now_ms {
            return Err(DiscoveryError::Expired);
        }
        if ticket.user_id == actor {
            return Err(DiscoveryError::InvalidArgument("self join"));
        }
        if ticket.proposal_id.is_some() {
            return Err(DiscoveryError::Conflict("shake window busy"));
        }
        let proposal_id = new_id("match");
        let mut proposals = self
            .proposals
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        proposals.insert(
            proposal_id.clone(),
            Proposal {
                ticket_id: ticket_id.to_owned(),
                host: ticket.user_id.clone(),
                guest: actor.to_owned(),
                host_confirmed: false,
                guest_confirmed: false,
                expires_at_ms: ticket.expires_at_ms,
            },
        );
        ticket.proposal_id = Some(proposal_id.clone());
        Ok(proposal_id)
    }

    /// 双方各自确认后才产生匹配关系；单方确认只停在 Pending。
    pub fn confirm(&self, actor: &str, proposal_id: &str) -> Result<MatchOutcome> {
        let now_ms = self.clock.now_ms();
        let mut proposals = self
            .proposals
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        let proposal = proposals
            .get_mut(proposal_id)
            .ok_or(DiscoveryError::NotFound)?;
        if proposal.expires_at_ms <= now_ms {
            return Err(DiscoveryError::Expired);
        }
        if actor == proposal.host {
            proposal.host_confirmed = true;
        } else if actor == proposal.guest {
            proposal.guest_confirmed = true;
        } else {
            return Err(DiscoveryError::Forbidden);
        }
        if proposal.host_confirmed && proposal.guest_confirmed {
            let record = MatchRecord {
                id: proposal_id.to_owned(),
                users: [proposal.host.clone(), proposal.guest.clone()],
                matched_at_ms: now_ms,
            };
            let ticket_id = proposal.ticket_id.clone();
            let host = proposal.host.clone();
            let guest = proposal.guest.clone();
            proposals.remove(proposal_id);
            drop(proposals);
            self.close_ticket(&ticket_id, &host, &guest, now_ms)?;
            return Ok(MatchOutcome::Matched(record));
        }
        Ok(MatchOutcome::Pending {
            needs_host: !proposal.host_confirmed,
            needs_guest: !proposal.guest_confirmed,
        })
    }

    pub fn cancel(&self, actor: &str, ticket_id: &str) -> Result<()> {
        let now_ms = self.clock.now_ms();
        let mut tickets = self
            .tickets
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        let ticket = tickets.get(ticket_id).ok_or(DiscoveryError::NotFound)?;
        let mut participants = vec![ticket.user_id.clone()];
        if let Some(proposal_id) = &ticket.proposal_id {
            let mut proposals = self
                .proposals
                .lock()
                .map_err(|_| DiscoveryError::DependencyUnavailable)?;
            if let Some(proposal) = proposals.remove(proposal_id) {
                participants.push(proposal.guest);
            }
        }
        if !participants.iter().any(|user| user == actor) {
            return Err(DiscoveryError::Forbidden);
        }
        tickets.remove(ticket_id);
        self.set_cooldown(&participants, now_ms)?;
        Ok(())
    }

    pub fn ticket(&self, ticket_id: &str) -> Option<ShakeTicket> {
        let now_ms = self.clock.now_ms();
        let tickets = self.tickets.lock().ok()?;
        tickets
            .get(ticket_id)
            .filter(|t| t.expires_at_ms > now_ms)
            .map(|t| ShakeTicket {
                id: ticket_id.to_owned(),
                user_id: t.user_id.clone(),
                expires_at_ms: t.expires_at_ms,
            })
    }

    fn ensure_not_cooling(&self, actor: &str, now_ms: i64) -> Result<()> {
        let cooldown = self
            .cooldown_until
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        match cooldown.get(actor) {
            Some(until) if *until > now_ms => Err(DiscoveryError::RateLimited),
            _ => Ok(()),
        }
    }

    fn close_ticket(&self, ticket_id: &str, host: &str, guest: &str, now_ms: i64) -> Result<()> {
        self.tickets
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?
            .remove(ticket_id);
        self.set_cooldown(&[host.to_owned(), guest.to_owned()], now_ms)
    }

    fn set_cooldown(&self, users: &[String], now_ms: i64) -> Result<()> {
        let mut cooldown = self
            .cooldown_until
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        for user in users {
            cooldown.insert(user.clone(), now_ms + COOLDOWN_MS);
        }
        Ok(())
    }
}
