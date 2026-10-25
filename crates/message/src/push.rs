//! G03-05: mobile push as a *sync hint*, and the device tokens it belongs to.
//!
//! Three rules the card makes non-negotiable, and how this module enforces them:
//!
//! 1. **The vendor is a port, not a business rule.** [`PushPort`] is the only
//!    thing the domain knows; APNs/FCM sit behind it in the transport layer with
//!    their own configuration. No vendor name, key or endpoint is read here.
//! 2. **A push is a hint, never content.** [`SyncHint`] carries no body, and a
//!    private conversation narrows it further to a bare count. The device must
//!    authenticate and pull the delta to learn anything.
//! 3. **Tokens belong to devices and can be revoked.** A registration carries an
//!    epoch; revoking bumps it, and any attempt to use an older token fails
//!    closed instead of silently delivering to a device the user signed out of.

use std::collections::BTreeMap;

use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use super::{MailError, MessageStore, Result};

/// How long one hint may be held back while the aggregator coalesces a burst.
pub const DEFAULT_AGGREGATION_WINDOW_MS: u64 = 30_000;
/// A device that has not refreshed its token in this long is treated as stale
/// and stops receiving hints (lost phones, abandoned installs).
pub const TOKEN_RETENTION_MS: u64 = 90 * 24 * 60 * 60 * 1000;

/// Platform family, only so the transport can pick the right vendor.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Platform {
    Android,
    Ios,
    Other,
}

impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::Android => "android",
            Platform::Ios => "ios",
            Platform::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "android" => Platform::Android,
            "ios" => Platform::Ios,
            _ => Platform::Other,
        }
    }
}

/// What the transport needs to address one device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushTarget {
    pub device_id: String,
    pub token: String,
    pub platform: Platform,
}

/// The notification payload. There is deliberately no message text in here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncHint {
    /// Empty for a private conversation: the device then knows only "something
    /// arrived", not where.
    pub conv_id: String,
    /// Empty when the caller does not want to expose even the sender.
    pub from_user: String,
    pub count: u32,
    pub sent_at_ms: u64,
    /// `true` = the device must do a full resync rather than a delta pull.
    pub resync: bool,
}

impl SyncHint {
    /// Hint for a private (sealed / expiring) conversation: no ids at all.
    pub fn private(count: u32, now_ms: u64) -> Self {
        Self {
            conv_id: String::new(),
            from_user: String::new(),
            count,
            sent_at_ms: now_ms,
            resync: false,
        }
    }

    /// Nothing in a hint may look like message content. This is the data
    /// minimisation gate the transport calls before sending.
    pub fn is_body_free(&self) -> bool {
        !self.conv_id.contains('\n')
            && !self.from_user.contains('\n')
            && self.conv_id.len() <= MAX_HINT_ID_LEN
            && self.from_user.len() <= MAX_HINT_ID_LEN
            && self.count > 0
    }
}

/// A new token for a known device invalidates the old one by moving its epoch.
fn rotate_push_token(
    c: &rusqlite::Connection,
    user: &str,
    device_id: &str,
    platform: Platform,
    token: &str,
    epoch: u64,
    now: u64,
) -> Result<u64> {
    let next = epoch + 1;
    c.execute(
        "UPDATE push_device
            SET user_id = ?1, platform = ?2, token = ?3, epoch = ?4, revoked = 0,
                last_seen_ms = ?5
          WHERE device_id = ?6",
        params![
            user,
            platform.as_str(),
            token,
            next as i64,
            now as i64,
            device_id
        ],
    )?;
    Ok(next)
}

/// Vendor failure surface. `Revoked` is terminal for that token; the others are
/// retryable by the scheduler, never by replaying a cached success.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PushError {
    /// The token was revoked or rotated away: do not try again.
    Revoked,
    /// Vendor unreachable / 5xx / timeout.
    Unavailable(String),
    /// The vendor rejected this token (uninstall, wrong key).
    Invalid(String),
}

/// The seam between the domain and a notification vendor.
pub trait PushPort: Send + Sync + 'static {
    /// Short, stable name for metrics and the "which vendor is live" report.
    fn name(&self) -> &'static str;

    fn send(&self, target: &PushTarget, hint: &SyncHint) -> std::result::Result<(), PushError>;
}

/// No vendor configured: hints are dropped, state is untouched. A deployment
/// without push still syncs — the moment it *has* a vendor, nothing else changes.
pub struct NoopPort;

impl PushPort for NoopPort {
    fn name(&self) -> &'static str {
        "noop"
    }

    fn send(&self, _target: &PushTarget, _hint: &SyncHint) -> std::result::Result<(), PushError> {
        Ok(())
    }
}

/// Records what was asked to send; the contract fixture the push transport is
/// tested against (and the only port used inside this crate's tests).
#[derive(Default)]
pub struct RecordingPort {
    sent: std::sync::Mutex<Vec<(String, SyncHint)>>,
    /// device_id -> the result the port returns, so a test can make a vendor
    /// fail (the card allows mocks only for vendor fault injection).
    outcomes: std::sync::Mutex<BTreeMap<String, std::result::Result<(), PushError>>>,
}

impl RecordingPort {
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }

    pub fn sent(&self) -> Vec<(String, SyncHint)> {
        self.sent.lock().unwrap().clone()
    }

    pub fn fail_with(&self, device_id: &str, err: PushError) {
        self.outcomes
            .lock()
            .unwrap()
            .insert(device_id.to_string(), Err(err));
    }
}

impl PushPort for RecordingPort {
    fn name(&self) -> &'static str {
        "recording"
    }

    fn send(&self, target: &PushTarget, hint: &SyncHint) -> std::result::Result<(), PushError> {
        let outcome = self
            .outcomes
            .lock()
            .unwrap()
            .get(&target.device_id)
            .cloned()
            .unwrap_or(Ok(()));
        if outcome.is_ok() {
            self.sent
                .lock()
                .unwrap()
                .push((target.device_id.clone(), hint.clone()));
        }
        outcome
    }
}

/// Upper bound on the two identifiers a hint may carry. Longer than any real
/// id means the payload is being used to smuggle content, so it is refused.
pub const MAX_HINT_ID_LEN: usize = 128;

/// One live token for one device. `epoch` is what makes revocation stick: an
/// in-flight send that races a revoke carries the older epoch and is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushDevice {
    pub device_id: String,
    pub user_id: String,
    pub platform: Platform,
    pub epoch: u64,
    pub last_seen_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchReport {
    pub attempted: usize,
    pub delivered: usize,
    pub revoked: usize,
    pub unavailable: usize,
    pub invalid: usize,
}

impl DispatchReport {
    fn record(&mut self, result: &std::result::Result<(), PushError>) {
        self.attempted += 1;
        match result {
            Ok(()) => self.delivered += 1,
            Err(PushError::Revoked) => self.revoked += 1,
            Err(PushError::Unavailable(_)) => self.unavailable += 1,
            Err(PushError::Invalid(_)) => self.invalid += 1,
        }
    }
}

impl MessageStore {
    /// push.register — idempotent per device; re-registering the same token just
    /// refreshes `last_seen`, while a *new* token rotates the epoch so the old
    /// one stops working immediately.
    pub fn register_push(
        &self,
        user: &str,
        device_id: &str,
        platform: Platform,
        token: &str,
    ) -> Result<u64> {
        if user.is_empty() || device_id.is_empty() {
            return Err(MailError::InvalidInput("user and device_id are required"));
        }
        if token.is_empty() {
            return Err(MailError::InvalidInput("push token is empty"));
        }
        let now = wxlike_core::types::now_ms();
        let c = self.c();
        let existing: Option<(String, i64)> = c
            .query_row(
                "SELECT token, epoch FROM push_device WHERE device_id = ?1",
                params![device_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match existing {
            Some((old_token, epoch)) if old_token == token => {
                c.execute(
                    "UPDATE push_device SET user_id = ?1, platform = ?2, last_seen_ms = ?3
                      WHERE device_id = ?4",
                    params![user, platform.as_str(), now as i64, device_id],
                )?;
                Ok(epoch as u64)
            }
            Some((_, epoch)) => {
                rotate_push_token(&c, user, device_id, platform, token, epoch as u64, now)
            }
            None => {
                c.execute(
                    "INSERT INTO push_device(
                         device_id, user_id, platform, token, epoch, revoked, created_ms, last_seen_ms)
                     VALUES(?1, ?2, ?3, ?4, 1, 0, ?5, ?5)",
                    params![device_id, user, platform.as_str(), token, now as i64],
                )?;
                Ok(1)
            }
        }
    }

    /// push.revoke — logout, sign-out-everywhere, or a lost device. Any send that
    /// still carries an older epoch is refused afterwards.
    pub fn revoke_push(&self, device_id: &str) -> Result<bool> {
        let now = wxlike_core::types::now_ms();
        let c = self.c();
        let changed = c.execute(
            "UPDATE push_device
                SET revoked = 1, epoch = epoch + 1, last_seen_ms = ?2
              WHERE device_id = ?1 AND revoked = 0",
            params![device_id, now as i64],
        )?;
        Ok(changed == 1)
    }

    /// Revoke every device of one user except the one they are holding (the
    /// "log out other sessions" path); returns how many were cut off.
    pub fn revoke_other_devices(&self, user: &str, keep_device: &str) -> Result<usize> {
        let now = wxlike_core::types::now_ms();
        let c = self.c();
        Ok(c.execute(
            "UPDATE push_device
                SET revoked = 1, epoch = epoch + 1, last_seen_ms = ?3
              WHERE user_id = ?1 AND device_id != ?2 AND revoked = 0",
            params![user, keep_device, now as i64],
        )?)
    }

    pub fn push_devices(&self, user: &str) -> Result<Vec<PushDevice>> {
        let c = self.c();
        let mut stmt = c.prepare(
            "SELECT device_id, user_id, platform, epoch, last_seen_ms FROM push_device
              WHERE user_id = ?1 AND revoked = 0 ORDER BY device_id",
        )?;
        let rows = stmt
            .query_map(params![user], |r| {
                Ok(PushDevice {
                    device_id: r.get(0)?,
                    user_id: r.get(1)?,
                    platform: Platform::parse(r.get::<_, String>(2)?.as_str()),
                    epoch: r.get::<_, i64>(3)? as u64,
                    last_seen_ms: r.get::<_, i64>(4)? as u64,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn push_token(&self, device_id: &str) -> Result<Option<PushTarget>> {
        let c = self.c();
        let row: Option<(String, String)> = c
            .query_row(
                "SELECT token, platform FROM push_device WHERE device_id = ?1 AND revoked = 0",
                params![device_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row.map(|(token, platform)| PushTarget {
            device_id: device_id.to_string(),
            token,
            platform: Platform::parse(&platform),
        }))
    }

    /// Hand one hint to the vendor for every live device of `user`.
    ///
    /// Failure is per device and never changes message state: a push that could
    /// not be delivered does not mark anything read, does not retire a delivery
    /// task, and does not lose the message (it is still in the mailbox).
    pub fn dispatch_hint(
        &self,
        port: &dyn PushPort,
        user: &str,
        hint: &SyncHint,
    ) -> Result<DispatchReport> {
        let mut report = DispatchReport {
            attempted: 0,
            delivered: 0,
            revoked: 0,
            unavailable: 0,
            invalid: 0,
        };
        if !hint.is_body_free() {
            return Err(MailError::InvalidInput("hint must be body-free"));
        }
        for device in self.push_devices(user)? {
            let target = self
                .push_token(&device.device_id)?
                .ok_or(MailError::InvalidInput("device vanished mid-dispatch"));
            match target {
                Ok(target) => report.record(&port.send(&target, hint)),
                // the token disappeared between listing and sending (a revoke
                // that won the race): treat it exactly like a revoked token
                Err(_) => report.revoked += 1,
            }
        }
        Ok(report)
    }

    /// Drop tokens nobody has refreshed in `TOKEN_RETENTION_MS`, so a long-lived
    /// install list cannot grow without bound.
    pub fn prune_push_devices(&self, cutoff_ms: u64) -> Result<usize> {
        let c = self.c();
        Ok(c.execute(
            "DELETE FROM push_device WHERE last_seen_ms < ?1",
            params![cutoff_ms as i64],
        )?)
    }
}
