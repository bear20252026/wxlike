//! G03-02 / G03-04: the "a socket write is not an acknowledgement" ledger.
//!
//! A parked frame leaves the offline store only when the client says it
//! persisted it (`WireFrame::Ack{stored_cursor}`). Until then the hub keeps the
//! row id in this window, so
//!
//! * a connection that dies mid-flight puts every unacknowledged row back on the
//!   queue instead of losing it, and
//! * a client that never acks cannot grow the window past
//!   [`MAX_INFLIGHT_PER_CONNECTION`] — the overflow is released (re-parked), not
//!   dropped, so the backlog stays bounded but never silently shrinks.
//!
//! The ledger is per connection, not per user: two devices of one account ack
//! independently (that is the whole point of the device cursor).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

/// Connection id as the hub numbers it (`HubState::next_conn`).
pub type ConnId = u64;

/// Ceiling on unacknowledged frames handed to one connection. Beyond it the
/// oldest rows go back to the offline store.
pub const MAX_INFLIGHT_PER_CONNECTION: usize = 512;

/// One frame the hub wrote but the client has not persisted yet.
///
/// `seq` is `None` for a frame the hub could not parse into an envelope (a
/// legacy or malformed payload). Those rows can only be retired by the store
/// lease, never by a cursor ack — the ledger keeps them so the client's next
/// `Ack` does not accidentally credit them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub store_id: i64,
    pub conv_id: Option<String>,
    pub seq: Option<u64>,
}

impl Pending {
    pub fn sequenced(store_id: i64, conv_id: impl Into<String>, seq: u64) -> Self {
        Self {
            store_id,
            conv_id: Some(conv_id.into()),
            seq: Some(seq),
        }
    }

    pub fn opaque(store_id: i64) -> Self {
        Self {
            store_id,
            conv_id: None,
            seq: None,
        }
    }
}

/// Per-connection window of frames awaiting a stored ack.
#[derive(Default)]
pub struct InflightLedger {
    inner: Mutex<HashMap<ConnId, VecDeque<Pending>>>,
    cap: usize,
}

impl InflightLedger {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::with_cap(MAX_INFLIGHT_PER_CONNECTION))
    }

    pub fn with_cap(cap: usize) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            cap: cap.max(1),
        }
    }

    fn c(&self) -> std::sync::MutexGuard<'_, HashMap<ConnId, VecDeque<Pending>>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record one written frame. Returns the rows that overflowed the window and
    /// must be released back to the queue (oldest first).
    pub fn record(&self, conn: ConnId, pending: Pending) -> Vec<Pending> {
        let mut m = self.c();
        let q = m.entry(conn).or_default();
        q.push_back(pending);
        let mut overflow = Vec::new();
        while q.len() > self.cap {
            match q.pop_front() {
                Some(p) => overflow.push(p),
                None => break,
            }
        }
        overflow
    }

    /// `Ack{conv_id, stored_cursor}`: every recorded frame of that conversation
    /// up to and including the cursor is now safely stored on the device.
    ///
    /// Out-of-order or repeated acks are harmless: an already-removed row is not
    /// in the window any more, and a lower cursor simply credits nothing new.
    pub fn confirm(&self, conn: ConnId, conv_id: &str, stored_cursor: u64) -> Vec<Pending> {
        let mut m = self.c();
        let Some(q) = m.get_mut(&conn) else {
            return Vec::new();
        };
        let mut confirmed = Vec::new();
        let mut retained = VecDeque::with_capacity(q.len());
        while let Some(p) = q.pop_front() {
            let covered = p.conv_id.as_deref().is_some_and(|c| c == conv_id)
                && p.seq.is_some_and(|s| s <= stored_cursor);
            if covered {
                confirmed.push(p);
            } else {
                retained.push_back(p);
            }
        }
        *q = retained;
        if q.is_empty() {
            m.remove(&conn);
        }
        confirmed
    }

    /// Everything still owed for one connection: called when the transport dies,
    /// so those rows go back to the queue rather than vanishing.
    pub fn discard(&self, conn: ConnId) -> Vec<Pending> {
        self.c().remove(&conn).map(Vec::from).unwrap_or_default()
    }

    pub fn depth(&self, conn: ConnId) -> usize {
        self.c().get(&conn).map(|q| q.len()).unwrap_or_default()
    }

    /// Total unacknowledged rows across all connections (bounded-memory metric).
    pub fn total(&self) -> usize {
        self.c().values().map(|q| q.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_never_retires_the_row_by_itself() {
        let ledger = InflightLedger::with_cap(8);
        ledger.record(1, Pending::sequenced(10, "g-1", 1));
        ledger.record(1, Pending::sequenced(11, "g-1", 2));
        assert_eq!(ledger.depth(1), 2, "both still owed after the socket write");
        assert!(
            !ledger.confirm(1, "g-1", 1).is_empty(),
            "cursor 1 credits one"
        );
        assert_eq!(ledger.depth(1), 1);
    }

    #[test]
    fn a_cursor_ack_credits_exactly_the_covered_prefix() {
        let ledger = InflightLedger::with_cap(8);
        for (id, seq) in [(1, 1u64), (2, 2), (3, 3)] {
            ledger.record(7, Pending::sequenced(id, "g-1", seq));
        }
        // one frame belongs to another conversation and must stay owed
        ledger.record(7, Pending::sequenced(4, "g-2", 9));
        let done = ledger.confirm(7, "g-1", 3);
        assert_eq!(
            done.iter().map(|p| p.store_id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(ledger.depth(7), 1, "g-2 is a separate cursor");
        // a repeated / lower ack credits nothing new
        assert!(ledger.confirm(7, "g-1", 1).is_empty());
        assert_eq!(ledger.depth(7), 1);
    }

    #[test]
    fn opaque_frames_are_never_credited_by_a_cursor() {
        let ledger = InflightLedger::with_cap(4);
        ledger.record(1, Pending::opaque(42));
        assert!(ledger.confirm(1, "g-1", 100).is_empty());
        assert_eq!(
            ledger.depth(1),
            1,
            "still owed: the store lease will retry it"
        );
    }

    #[test]
    fn an_overflow_releases_the_oldest_rows_instead_of_dropping_them() {
        let ledger = InflightLedger::with_cap(3);
        let mut overflowed = Vec::new();
        for id in 0..6 {
            overflowed.extend(ledger.record(1, Pending::sequenced(id, "g-1", id as u64)));
        }
        assert_eq!(
            overflowed.iter().map(|p| p.store_id).collect::<Vec<_>>(),
            vec![0, 1, 2],
            "the three oldest go back to the queue"
        );
        assert_eq!(ledger.depth(1), 3, "the window never grew past the cap");
        assert_eq!(ledger.total(), 3);
    }

    #[test]
    fn a_dead_connection_puts_everything_back_and_frees_the_window() {
        let ledger = InflightLedger::with_cap(8);
        ledger.record(1, Pending::sequenced(1, "g-1", 1));
        ledger.record(2, Pending::sequenced(2, "g-1", 2));
        let owed = ledger.discard(1);
        assert_eq!(owed.iter().map(|p| p.store_id).collect::<Vec<_>>(), vec![1]);
        assert_eq!(ledger.depth(1), 0);
        assert_eq!(ledger.total(), 1, "the other device is untouched");
        // discarding twice is not an error (late close + cleanup task)
        assert!(ledger.discard(1).is_empty());
        assert!(ledger.discard(99).is_empty());
    }

    #[test]
    fn devices_of_one_account_do_not_share_a_window() {
        let ledger = InflightLedger::new();
        for conn in [10u64, 11] {
            ledger.record(conn, Pending::sequenced(conn as i64, "g-1", 1));
        }
        assert_eq!(ledger.confirm(10, "g-1", 1).len(), 1);
        assert_eq!(ledger.depth(10), 0);
        assert_eq!(ledger.depth(11), 1, "the other device still owes it");
    }
}
