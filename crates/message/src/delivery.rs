//! G03-02: device-stored acknowledgement, per-device cursors and the offline
//! delivery queue.
//!
//! The socket write is *not* the acknowledgement. A row leaves the queue only
//! after the client persists it and sends a stored ack, which also advances the
//! per-device cursor. Claims are leased so a crashed worker's tasks are
//! re-delivered after the lease expires instead of being lost.

use rusqlite::{params, OptionalExtension as _};

use super::{MailMessage, MessageStore, OwnerType, Result};

/// Lease for one delivery attempt. A worker that dies without acking loses the
/// lease and the task becomes claimable again.
pub const DEFAULT_LEASE_MS: u64 = 30_000;
/// Hard upper bound for one claim batch: offline queues stay bounded.
pub const MAX_CLAIM_BATCH: usize = 100;
/// Stored tasks are kept this long for replay/diagnostics, then pruned.
pub const TASK_RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;

const TASK_COLS: &str = "m.seq, m.msg_id, m.sender, m.type_id, m.payload, m.sent_at_ms, \
                         m.withdrawn, m.sender_device, m.body_kind, m.edit_version, \
                         m.recalled_at_ms, m.forward_of";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryTask {
    pub msg_id: String,
    pub owner_type: String,
    pub conv_id: String,
    pub seq: u64,
    pub target_user: String,
    pub target_device: String,
    pub attempts: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResumePolicy {
    /// A device cursor older than this many messages cannot be served
    /// incrementally; the caller must do a full resync.
    pub max_gap: u64,
    /// Bounded page size.
    pub batch: usize,
}

impl Default for ResumePolicy {
    fn default() -> Self {
        Self {
            max_gap: 2_000,
            batch: 100,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ResumePlan {
    /// Incremental page. `next_after` is `Some` when more rows remain.
    Messages {
        items: Vec<MailMessage>,
        next_after: Option<u64>,
        stored_cursor: u64,
    },
    /// Cursor too old (or ahead of the head): the client must drop local state
    /// and do a full conversation sync. This is the "过期提示" path.
    Resync { head: u64, cursor: u64 },
}

impl MessageStore {
    /// Claim up to `limit` ready tasks for `worker`. Expired leases are
    /// reclaimable, so a hub crash re-delivers instead of losing messages.
    pub fn claim_delivery(
        &self,
        worker: &str,
        now_ms: u64,
        limit: usize,
        lease_ms: u64,
    ) -> Result<Vec<DeliveryTask>> {
        let limit = limit.clamp(1, MAX_CLAIM_BATCH) as i64;
        let lease_ms = lease_ms.max(1_000);
        let mut conn = self.c();
        let tx = conn.transaction()?;
        let rows: Vec<DeliveryTask> = {
            let mut stmt = tx.prepare(
                "SELECT m.seq, d.msg_id, m.owner_type, m.owner_id, d.target_user,
                        d.target_device, d.attempts
                   FROM delivery_task d
                   JOIN mailbox m ON m.msg_id = d.msg_id
                  WHERE (d.state = 'pending' AND d.next_at <= ?1)
                     OR (d.state = 'inflight' AND d.lease_until <= ?1)
                  ORDER BY d.next_at, d.msg_id
                  LIMIT ?2",
            )?;
            let found = stmt
                .query_map(params![now_ms as i64, limit], |r| {
                    Ok(DeliveryTask {
                        seq: r.get::<_, i64>(0)? as u64,
                        msg_id: r.get(1)?,
                        owner_type: r.get(2)?,
                        conv_id: r.get(3)?,
                        target_user: r.get(4)?,
                        target_device: r.get(5)?,
                        attempts: r.get::<_, i64>(6)? as u32 + 1,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            found
        };
        for task in &rows {
            tx.execute(
                "UPDATE delivery_task
                    SET state = 'inflight', lease_owner = ?4, lease_until = ?5,
                        attempts = attempts + 1, updated_at = ?6
                  WHERE msg_id = ?1 AND target_user = ?2 AND target_device = ?3",
                params![
                    task.msg_id,
                    task.target_user,
                    task.target_device,
                    worker,
                    (now_ms + lease_ms) as i64,
                    now_ms as i64
                ],
            )?;
        }
        tx.commit()?;
        Ok(rows)
    }

    /// Socket write finished but the client has not persisted yet: put the task
    /// back (or mark it dead after the caller's retry budget is exhausted).
    pub fn release_delivery(
        &self,
        worker: &str,
        task: &DeliveryTask,
        retry_at_ms: u64,
        dead: bool,
    ) -> Result<bool> {
        let c = self.c();
        let changed = c.execute(
            "UPDATE delivery_task
                SET state = ?5, lease_owner = NULL, lease_until = 0, next_at = ?4,
                    updated_at = ?6
              WHERE msg_id = ?1 AND target_user = ?2 AND target_device = ?3
                AND lease_owner = ?7",
            params![
                task.msg_id,
                task.target_user,
                task.target_device,
                retry_at_ms as i64,
                if dead { "dead" } else { "pending" },
                wxlike_core::types::now_ms() as i64,
                worker
            ],
        )?;
        Ok(changed == 1)
    }

    /// The only acknowledgement that removes a message from the queue: the
    /// client has persisted through `seq` on `device_id`.
    ///
    /// Monotonic and idempotent — a repeated or out-of-order (lower) ack keeps
    /// the higher cursor and reports `false`.
    pub fn stored_ack(&self, user: &str, device_id: &str, conv_id: &str, seq: u64) -> Result<bool> {
        if seq == 0 {
            return Ok(false);
        }
        let now = wxlike_core::types::now_ms();
        let mut conn = self.c();
        let tx = conn.transaction()?;
        let advanced = advance_cursor(&tx, device_id, conv_id, seq, false, now)?;
        if advanced {
            tx.execute(
                "UPDATE delivery_task
                    SET state = 'stored', lease_owner = NULL, lease_until = 0, updated_at = ?4
                  WHERE target_user = ?1
                    AND (target_device = '*' OR target_device = ?2)
                    AND state != 'stored'
                    AND msg_id IN (SELECT msg_id FROM mailbox
                                    WHERE owner_id = ?3 AND seq <= ?5)",
                params![user, device_id, conv_id, now as i64, seq as i64],
            )?;
        }
        tx.commit()?;
        Ok(advanced)
    }

    /// Read acknowledgement. The account-level `read_cursor` (unread source)
    /// and the device-level cursor move together, both monotonic.
    pub fn read_ack(&self, user: &str, device_id: &str, conv_id: &str, seq: u64) -> Result<bool> {
        if seq == 0 {
            return Ok(false);
        }
        let now = wxlike_core::types::now_ms();
        let mut conn = self.c();
        let tx = conn.transaction()?;
        let advanced = advance_cursor(&tx, device_id, conv_id, seq, true, now)?;
        if let Some(owner_type) = owner_type_of(&tx, conv_id)? {
            let cursor_advanced = tx.execute(
                "UPDATE read_cursor SET last_ack_seq = ?4, updated_at = ?5
                  WHERE owner_type = ?1 AND owner_id = ?2 AND user_id = ?3
                    AND last_ack_seq < ?4",
                params![owner_type, conv_id, user, seq as i64, now as i64],
            )?;
            if cursor_advanced == 0 {
                tx.execute(
                    "INSERT OR IGNORE INTO read_cursor(
                         owner_type, owner_id, user_id, last_ack_seq, updated_at)
                     VALUES(?1, ?2, ?3, ?4, ?5)",
                    params![owner_type, conv_id, user, seq as i64, now as i64],
                )?;
            }
        }
        tx.commit()?;
        Ok(advanced)
    }

    pub fn device_cursor(&self, device_id: &str, conv_id: &str) -> Result<(u64, u64)> {
        let c = self.c();
        let row: Option<(i64, i64)> = c
            .query_row(
                "SELECT stored_cursor, read_cursor FROM device_cursor
                  WHERE device_id = ?1 AND conv_id = ?2",
                params![device_id, conv_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row.map(|(s, r)| (s as u64, r as u64)).unwrap_or((0, 0)))
    }

    /// resume(cursor): what this device still needs after reconnecting.
    ///
    /// Visibility is taken from the recipient's own delivery tasks, so a
    /// member who joined later does not fetch earlier conversation history and
    /// a member who left stops at their leave edge.
    pub fn resume(
        &self,
        owner: (OwnerType, &str),
        user: &str,
        device_id: &str,
        after_hint: u64,
        policy: ResumePolicy,
    ) -> Result<ResumePlan> {
        let (ot, oid) = (owner.0.as_str(), owner.1);
        let head = self.head_seq(owner)?;
        let (stored, _) = self.device_cursor(device_id, oid)?;
        let cursor = stored.max(after_hint);
        if cursor > head || head.saturating_sub(cursor) > policy.max_gap {
            return Ok(ResumePlan::Resync { head, cursor });
        }
        let batch = policy.batch.clamp(1, 500) as i64;
        let c = self.c();
        let mut stmt = c.prepare(&format!(
            "SELECT {TASK_COLS}
               FROM mailbox m
               JOIN (SELECT DISTINCT msg_id FROM delivery_task
                      WHERE target_user = ?5 AND (target_device = '*' OR target_device = ?6)
                    ) d ON d.msg_id = m.msg_id
              WHERE m.owner_type = ?1 AND m.owner_id = ?2 AND m.seq > ?3
              ORDER BY m.seq
              LIMIT ?4"
        ))?;
        let items = stmt
            .query_map(
                params![ot, oid, cursor as i64, batch, user, device_id],
                super::row_to_mail,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let next_after = if items.len() == batch as usize {
            items.last().map(|m| m.seq)
        } else {
            None
        };
        Ok(ResumePlan::Messages {
            items,
            next_after,
            stored_cursor: stored,
        })
    }

    /// Which owner kind the mailbox recorded for a conversation id.
    ///
    /// `None` = the mailbox has never seen this conversation, so the caller has
    /// nothing to sync against. The hub only holds the id string; the owner kind
    /// is a fact the mailbox already stores with every row.
    pub fn owner_kind_of(&self, conv_id: &str) -> Result<Option<OwnerType>> {
        let c = self.c();
        let row: Option<String> = c
            .query_row(
                "SELECT owner_type FROM mailbox WHERE owner_id = ?1 LIMIT 1",
                params![conv_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(row.as_deref().and_then(OwnerType::parse))
    }

    /// SyncHint for a reconnecting device: the range the hub is about to send,
    /// and whether the device is behind by more than one page (`gap`).
    ///
    /// `Ok(None)` means "nothing to say" — an unknown conversation, or a device
    /// already at the head. A `gap` hint is the client's cue to fetch history
    /// over HTTP instead of waiting for the push to trickle in.
    ///
    /// This is a **range hint, not an authorisation decision**: it is computed
    /// from the conversation head alone, because the hub's mirror path fills the
    /// timeline without per-device delivery rows. What the caller may actually
    /// read is still decided by the delivery tasks (`resume`) and by the HTTP
    /// surface's membership check.
    pub fn sync_hint(
        &self,
        conv_id: &str,
        _user: &str,
        _device_id: &str,
        cursor: u64,
    ) -> Result<Option<(u64, u64, bool)>> {
        let Some(owner_type) = self.owner_kind_of(conv_id)? else {
            return Ok(None);
        };
        let head = self.head_seq((owner_type, conv_id))?;
        if head <= cursor {
            return Ok(None);
        }
        let policy = ResumePolicy::default();
        Ok(Some((
            cursor + 1,
            head,
            head.saturating_sub(cursor) > policy.batch as u64,
        )))
    }

    /// Retention: stored tasks older than `cutoff_ms` disappear; an un-acked
    /// offline task is never pruned.
    pub fn prune_delivery(&self, cutoff_ms: u64) -> Result<usize> {
        let c = self.c();
        Ok(c.execute(
            "DELETE FROM delivery_task
              WHERE state = 'stored' AND updated_at < ?1",
            params![cutoff_ms as i64],
        )?)
    }

    /// Turn every expired in-flight lease back into a claimable task.
    pub fn requeue_expired(&self, now_ms: u64) -> Result<usize> {
        let c = self.c();
        Ok(c.execute(
            "UPDATE delivery_task
                SET state = 'pending', lease_owner = NULL, lease_until = 0, next_at = ?1,
                    updated_at = ?1
              WHERE state = 'inflight' AND lease_until <= ?1",
            params![now_ms as i64],
        )?)
    }
}

fn advance_cursor(
    tx: &rusqlite::Transaction<'_>,
    device_id: &str,
    conv_id: &str,
    seq: u64,
    read: bool,
    now: u64,
) -> Result<bool> {
    let current: Option<(i64, i64)> = tx
        .query_row(
            "SELECT stored_cursor, read_cursor FROM device_cursor
              WHERE device_id = ?1 AND conv_id = ?2",
            params![device_id, conv_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (stored, read_cursor) = current.unwrap_or((0, 0));
    let advanced = seq as i64 > if read { read_cursor } else { stored };
    if !advanced {
        return Ok(false);
    }
    let (next_stored, next_read) = if read {
        (stored, seq as i64)
    } else {
        (seq as i64, read_cursor)
    };
    tx.execute(
        "INSERT INTO device_cursor(device_id, conv_id, stored_cursor, read_cursor, version, updated_at)
         VALUES(?1, ?2, ?3, ?4, 1, ?5)
         ON CONFLICT(device_id, conv_id) DO UPDATE SET
             stored_cursor = MAX(device_cursor.stored_cursor, excluded.stored_cursor),
             read_cursor = MAX(device_cursor.read_cursor, excluded.read_cursor),
             version = device_cursor.version + 1,
             updated_at = excluded.updated_at",
        params![device_id, conv_id, next_stored, next_read, now as i64],
    )?;
    Ok(true)
}

fn owner_type_of(tx: &rusqlite::Transaction<'_>, conv_id: &str) -> Result<Option<String>> {
    let from_projection: Option<String> = tx
        .query_row(
            "SELECT owner_type FROM conversation_projection WHERE conv_id = ?1",
            params![conv_id],
            |r| r.get(0),
        )
        .optional()?;
    if from_projection.is_some() {
        return Ok(from_projection);
    }
    Ok(tx
        .query_row(
            "SELECT owner_type FROM mailbox WHERE owner_id = ?1 LIMIT 1",
            params![conv_id],
            |r| r.get(0),
        )
        .optional()?)
}
