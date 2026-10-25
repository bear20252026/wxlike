//! G03-05: sync scheduling — what a device pulls when it wakes up, and the
//! projection refresh a retry is not allowed to skip.
//!
//! Two halves:
//!
//! * [`SyncScheduler`] is the *client-side* decision: hints arriving out of
//!   order, twice, or for a conversation already queued must not multiply work
//!   or lose a wake-up. It is pure state, bounded, and clock-injected.
//! * [`MessageStore::drive_queue`] is the *server-side* consequence of the device
//!   coming back: expired leases are re-armed, dead rows for that user are
//!   revived, and the conversation projection is rebuilt from the mailbox. The
//!   card is explicit that a phone retry may not merely clear `pending` — the
//!   list, the unread and the preview have to be true again too.

use std::collections::BTreeMap;

use rusqlite::{params, OptionalExtension as _};

use super::{
    apply_projection_row_tx, BodyKind, MailError, MailMessage, MessageStore, OwnerType, Result,
    ResumePlan, ResumePolicy,
};

/// Why the queue is being driven. All four must end in the same state; they only
/// differ in how urgent the reply is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncTrigger {
    /// App returned to the foreground.
    Foreground,
    /// Connectivity changed (wifi ↔ cellular, or back after an outage).
    NetworkRecovered,
    /// A push hint arrived while the socket was still down.
    HintArrived,
    /// Periodic sweep.
    Timer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteOutcome {
    /// First hint for this conversation: scheduled.
    Scheduled,
    /// Already queued; the count was folded in.
    Coalesced,
    /// Older than what we already queued: ignored (out-of-order delivery).
    Stale,
    /// The queue was full and the oldest conversation was evicted to make room.
    Overflowed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Queued {
    last_sent_at_ms: u64,
    count: u32,
    resync: bool,
}

/// Bounded, order-insensitive queue of "this conversation needs a sync".
///
/// Duplicate hints collapse, late hints are dropped, and a burst cannot grow the
/// map without limit. Nothing here touches message state: a hint is a *request*
/// to read, never an acknowledgement.
pub struct SyncScheduler {
    cap: usize,
    window_ms: u64,
    queued: BTreeMap<String, Queued>,
}

impl SyncScheduler {
    pub fn new(cap: usize, window_ms: u64) -> Self {
        Self {
            cap: cap.max(1),
            window_ms,
            queued: BTreeMap::new(),
        }
    }

    /// Record one hint. `conv_id` empty means "a private conversation arrived
    /// and the hint does not say which" — those are folded into one anonymous
    /// entry so the device still resyncs something.
    pub fn note(
        &mut self,
        conv_id: &str,
        count: u32,
        sent_at_ms: u64,
        resync: bool,
    ) -> NoteOutcome {
        // a private hint says nothing about *which* conversation, so all of them
        // share one sentinel entry; the sentinel is stripped again in `due`
        let key = if conv_id.is_empty() {
            "\u{0}".to_string()
        } else {
            conv_id.to_string()
        };
        match self.queued.get_mut(&key) {
            Some(existing) => {
                if sent_at_ms < existing.last_sent_at_ms {
                    return NoteOutcome::Stale;
                }
                existing.count = existing.count.saturating_add(count);
                existing.last_sent_at_ms = existing.last_sent_at_ms.max(sent_at_ms);
                existing.resync |= resync;
                NoteOutcome::Coalesced
            }
            None => {
                let evicted = self.evict_if_full();
                self.queued.insert(
                    key,
                    Queued {
                        last_sent_at_ms: sent_at_ms,
                        count,
                        resync,
                    },
                );
                if evicted {
                    NoteOutcome::Overflowed
                } else {
                    NoteOutcome::Scheduled
                }
            }
        }
    }

    fn evict_if_full(&mut self) -> bool {
        if self.queued.len() < self.cap {
            return false;
        }
        // drop the oldest-seen entry: it was requested first and is the one the
        // device has had the longest to fetch by itself
        let oldest = self
            .queued
            .iter()
            .min_by_key(|(_, q)| q.last_sent_at_ms)
            .map(|(k, _)| k.clone());
        match oldest {
            Some(k) => {
                self.queued.remove(&k);
                true
            }
            None => false,
        }
    }

    /// Conversations whose aggregation window has elapsed, in a stable order.
    /// Consumed entries leave the queue; the device owes the fetch.
    pub fn due(&mut self, now_ms: u64) -> Vec<(String, u32, bool)> {
        let ready: Vec<String> = self
            .queued
            .iter()
            .filter(|(_, q)| now_ms.saturating_sub(q.last_sent_at_ms) >= self.window_ms || q.resync)
            .map(|(k, _)| k.clone())
            .collect();
        let mut out = Vec::with_capacity(ready.len());
        for key in ready {
            if let Some(q) = self.queued.remove(&key) {
                let name = key.trim_start_matches('\u{0}').to_string();
                out.push((name, q.count, q.resync));
            }
        }
        out.sort();
        out
    }

    pub fn pending(&self) -> usize {
        self.queued.len()
    }
}

/// One conversation's worth of work handed back to a waking device.
#[derive(Clone, Debug, PartialEq)]
pub enum SyncStep {
    /// Pull this page; `next_cursor` is set when more rows remain.
    Delta {
        conv_id: String,
        items: Vec<MailMessage>,
        next_cursor: Option<u64>,
    },
    /// The device cursor is unusable: drop local state and refetch.
    Resync { conv_id: String, head: u64 },
}

/// Result of driving the queue for one device.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SyncSchedule {
    pub requeued: usize,
    pub rearmed: usize,
    pub projections_refreshed: usize,
    pub steps: Vec<SyncStep>,
}

impl MessageStore {
    /// sync.delta — the device's paged catch-up for one conversation.
    ///
    /// Read-only with respect to state: nothing here marks anything stored. A
    /// cancelled or timed-out pull is therefore safe to repeat verbatim.
    pub fn sync_delta(
        &self,
        owner: (OwnerType, &str),
        user: &str,
        device_id: &str,
        cursor: u64,
        policy: ResumePolicy,
    ) -> Result<SyncStep> {
        match self.resume(owner, user, device_id, cursor, policy)? {
            ResumePlan::Messages {
                items, next_after, ..
            } => Ok(SyncStep::Delta {
                conv_id: owner.1.to_string(),
                items,
                next_cursor: next_after,
            }),
            ResumePlan::Resync { head, .. } => Ok(SyncStep::Resync {
                conv_id: owner.1.to_string(),
                head,
            }),
        }
    }

    /// Drive the queue for `user`'s `device` after a wake-up event.
    ///
    /// Order matters: expired leases first (so in-flight work is claimable
    /// again), then the rows a worker had given up on, then the projection
    /// rebuild — a device must never come back to a list that still shows a
    /// preview of a message that was recalled while it was away.
    ///
    /// The trigger is not decorative: [`SyncTrigger::Timer`] is a background
    /// sweep and deliberately does **not** resurrect rows a worker marked dead
    /// (nobody is waiting), while foreground / network / hint events mean a
    /// human is looking at the list now, so everything is re-armed.
    pub fn drive_queue(
        &self,
        user: &str,
        device_id: &str,
        trigger: SyncTrigger,
        now_ms: u64,
        policy: ResumePolicy,
    ) -> Result<SyncSchedule> {
        if user.is_empty() || device_id.is_empty() {
            return Err(MailError::InvalidInput("user and device_id are required"));
        }
        let schedule = {
            let c = self.c();
            c.execute(
                "UPDATE delivery_task
                    SET state = 'pending', lease_owner = NULL, lease_until = 0, next_at = ?1,
                        updated_at = ?1
                  WHERE state = 'inflight' AND lease_until <= ?1",
                params![now_ms as i64],
            )?
        };
        let requeued = schedule;
        let rearmed = if matches!(trigger, SyncTrigger::Timer) {
            0
        } else {
            let c = self.c();
            c.execute(
                "UPDATE delivery_task
                    SET state = 'pending', lease_owner = NULL, lease_until = 0,
                        next_at = ?3, updated_at = ?3
                  WHERE target_user = ?1 AND (target_device = '*' OR target_device = ?2)
                    AND state IN ('failed', 'dead')",
                params![user, device_id, now_ms as i64],
            )?
        };

        // Conversations this device still owes, newest traffic first.
        let owed = self.conversations_owed(user, device_id)?;
        let mut refreshed = 0usize;
        let mut steps = Vec::with_capacity(owed.len());
        for conv in owed {
            let Some(owner_type) = self.owner_kind_of(&conv)? else {
                continue;
            };
            self.refresh_projection(&conv, owner_type)?;
            refreshed += 1;
            // incremental from the device's own persisted cursor — a wake-up
            // must not re-send what the phone already has on disk
            let (cursor, _) = self.device_cursor(device_id, &conv)?;
            steps.push(self.sync_delta((owner_type, &conv), user, device_id, cursor, policy)?);
        }
        Ok(SyncSchedule {
            requeued,
            rearmed,
            projections_refreshed: refreshed,
            steps,
        })
    }

    fn conversations_owed(&self, user: &str, device_id: &str) -> Result<Vec<String>> {
        let c = self.c();
        let mut stmt = c.prepare(
            "SELECT DISTINCT m.owner_id
               FROM delivery_task d
               JOIN mailbox m ON m.msg_id = d.msg_id
              WHERE d.target_user = ?1
                AND (d.target_device = '*' OR d.target_device = ?2)
                AND d.state != 'stored'
              ORDER BY m.owner_id",
        )?;
        let rows = stmt
            .query_map(params![user, device_id], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Recompute the list projection row for one conversation from the mailbox.
    ///
    /// This is the part a "just clear the pending flag" retry would skip: the
    /// preview, the head seq and the recalled flag are rebuilt from the timeline,
    /// inside one transaction, so the list cannot drift from the messages.
    pub(crate) fn refresh_projection(&self, conv_id: &str, owner_type: OwnerType) -> Result<()> {
        let now = wxlike_core::types::now_ms();
        let mut conn = self.c();
        let tx = conn.transaction()?;
        let latest: Option<(i64, String, String, String, String, String, i64)> = tx
            .query_row(
                "SELECT seq, msg_id, sender, type_id, payload, body_kind, withdrawn
                   FROM mailbox WHERE owner_type = ?1 AND owner_id = ?2
                  ORDER BY seq DESC LIMIT 1",
                params![owner_type.as_str(), conv_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .optional()?;
        if let Some((seq, _msg_id, sender, type_id, payload, kind, withdrawn)) = latest {
            apply_projection_row_tx(
                &tx,
                owner_type.as_str(),
                conv_id,
                seq as u64,
                &sender,
                &type_id,
                &payload,
                BodyKind::parse(&kind).unwrap_or(BodyKind::Plain),
                withdrawn != 0,
                now,
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
