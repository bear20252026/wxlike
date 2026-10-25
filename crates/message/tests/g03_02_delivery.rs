//! G03-02 acceptance: device-stored acknowledgement and the offline queue.
//!
//! The socket write is not the acknowledgement. These tests pin the card's
//! acceptance lines with a real SQLite store: a client that dies after the
//! socket write but before persisting still gets the message back, acks are
//! idempotent and monotonic per device, and the backlog stays bounded.

use wxlike_message::{
    BodyKind, DeliveryTask, MailMessage, MessageStore, OwnerType, ResumePlan, ResumePolicy,
    SubmitRecipient, SubmitRequest, DEFAULT_LEASE_MS, MAX_CLAIM_BATCH, TASK_RETENTION_MS,
};

const GROUP: (OwnerType, &str) = (OwnerType::Group, "g-100");
const CONV: &str = "g-100";

fn store() -> MessageStore {
    MessageStore::open(None).unwrap()
}

/// The queue is driven by wall clock, so tests speak in offsets from it.
fn t(offset: u64) -> u64 {
    wxlike_core::types::now_ms() + offset
}

/// `m{i}` occupies seq `i`; every listed recipient gets one task per message.
fn fill(s: &MessageStore, users: &[&str], n: u64, device: &str) {
    for i in 1..=n {
        let rcpts: Vec<SubmitRecipient> =
            users.iter().map(|u| SubmitRecipient::member(u)).collect();
        let payload = format!("body-{i}");
        s.submit(&SubmitRequest {
            owner: GROUP,
            sender: "alice",
            sender_device: device,
            idempotency_key: &format!("k-{device}-{i}"),
            msg_id: &format!("m{i}"),
            type_id: "text",
            payload: &payload,
            body_kind: BodyKind::Plain,
            recipients: &rcpts,
            forward_of: None,
        })
        .unwrap();
    }
}

fn ids(tasks: &[DeliveryTask]) -> Vec<String> {
    let mut v: Vec<String> = tasks.iter().map(|task| task.msg_id.clone()).collect();
    v.sort();
    v
}

fn mail_ids(items: &[MailMessage]) -> Vec<String> {
    let mut v: Vec<String> = items.iter().map(|m| m.msg_id.clone()).collect();
    v.sort();
    v
}

#[test]
fn socket_write_without_a_stored_ack_keeps_the_only_copy_claimable() {
    let s = store();
    fill(&s, &["bob"], 2, "dev-a");
    let claimed = s.claim_delivery("hub-1", t(0), 10, 2_000).unwrap();
    assert_eq!(ids(&claimed), ["m1", "m2"]);
    // the write "succeeded" and the client died before persisting
    assert_eq!(
        s.delivery_depth(Some("bob")).unwrap(),
        2,
        "write is not ack"
    );

    // lease still held: a second claim by another worker gets nothing
    assert!(s
        .claim_delivery("hub-2", t(500), 10, 2_000)
        .unwrap()
        .is_empty());

    // lease expiry puts it back on the queue; nothing was ever deleted
    assert_eq!(s.requeue_expired(t(3_000)).unwrap(), 2);
    let again = s.claim_delivery("hub-2", t(3_001), 10, 2_000).unwrap();
    assert_eq!(ids(&again), ["m1", "m2"]);
    assert!(
        again.iter().all(|task| task.attempts == 2),
        "the reclaimed row reports its attempt number"
    );
}

#[test]
fn stored_ack_is_the_only_path_that_retires_a_task() {
    let s = store();
    fill(&s, &["bob"], 3, "dev-a");
    s.claim_delivery("hub-1", t(0), 10, DEFAULT_LEASE_MS)
        .unwrap();
    assert_eq!(s.delivery_depth(Some("bob")).unwrap(), 3);

    assert!(s.stored_ack("bob", "dev-bob-1", CONV, 2).unwrap());
    assert_eq!(
        s.delivery_depth(Some("bob")).unwrap(),
        1,
        "cumulative: 1..=2 retired, 3 still owed"
    );
    // m3 is still under the lease hub-1 took a moment ago: owed, not lost
    assert!(s
        .claim_delivery("hub-2", t(1), 10, DEFAULT_LEASE_MS)
        .unwrap()
        .is_empty());
    assert_eq!(s.requeue_expired(t(31_000)).unwrap(), 1);
    let rest = s
        .claim_delivery("hub-2", t(31_001), 10, DEFAULT_LEASE_MS)
        .unwrap();
    assert_eq!(ids(&rest), ["m3"]);
    // the timeline copy is untouched by the whole ack dance
    let (rows, _) = s.fetch(GROUP, 0, 10).unwrap();
    assert_eq!(rows.len(), 3);
}

#[test]
fn repeated_and_out_of_order_stored_acks_are_idempotent() {
    let s = store();
    fill(&s, &["bob"], 5, "dev-a");
    assert!(s.stored_ack("bob", "dev-1", CONV, 4).unwrap());
    assert_eq!(s.device_cursor("dev-1", CONV).unwrap().0, 4);
    // same ack twice
    assert!(!s.stored_ack("bob", "dev-1", CONV, 4).unwrap());
    // a stale ack from a slow worker
    assert!(!s.stored_ack("bob", "dev-1", CONV, 2).unwrap());
    assert_eq!(s.device_cursor("dev-1", CONV).unwrap().0, 4, "monotonic");
    assert_eq!(s.delivery_depth(Some("bob")).unwrap(), 1);
    // zero is not a cursor
    assert!(!s.stored_ack("bob", "dev-1", CONV, 0).unwrap());
    assert_eq!(s.device_cursor("dev-1", CONV).unwrap().0, 4);
}

#[test]
fn cursors_are_per_device_and_the_account_read_cursor_stays_shared() {
    let s = store();
    fill(&s, &["bob"], 4, "dev-a");
    s.stored_ack("bob", "phone", CONV, 4).unwrap();
    // the laptop never came online: it still owes all four
    assert_eq!(s.device_cursor("phone", CONV).unwrap().0, 4);
    assert_eq!(s.device_cursor("laptop", CONV).unwrap().0, 0);
    let plan = s
        .resume(GROUP, "bob", "laptop", 0, ResumePolicy::default())
        .unwrap();
    match plan {
        ResumePlan::Messages { items, .. } => assert_eq!(items.len(), 4),
        other => panic!("laptop must still be caught up: {other:?}"),
    }

    // read is an account fact, and it moves the device read cursor too
    s.read_ack("bob", "laptop", CONV, 2).unwrap();
    assert_eq!(s.unread(GROUP, "bob").unwrap(), 2, "account-level unread");
    let (stored, read) = s.device_cursor("laptop", CONV).unwrap();
    assert_eq!(
        (stored, read),
        (0, 2),
        "stored cursor untouched by a read ack"
    );
    // a read ack cannot rewind the account cursor
    assert!(!s.read_ack("bob", "laptop", CONV, 1).unwrap());
    assert_eq!(s.unread(GROUP, "bob").unwrap(), 2);
}

#[test]
fn resume_pages_from_the_cursor_without_skipping_or_repeating() {
    let s = store();
    fill(&s, &["bob"], 25, "dev-a");
    let policy = ResumePolicy {
        max_gap: 1_000,
        batch: 10,
    };
    let mut seen: Vec<u64> = Vec::new();
    let mut cursor = 0u64;
    for page in 1..=3u64 {
        let plan = s.resume(GROUP, "bob", "dev-1", cursor, policy).unwrap();
        match plan {
            ResumePlan::Messages {
                items, next_after, ..
            } => {
                let want = if page == 3 { 5 } else { 10 };
                assert_eq!(items.len(), want, "page {page}");
                for m in &items {
                    assert!(!seen.contains(&m.seq), "seq {} repeated", m.seq);
                    seen.push(m.seq);
                }
                match (page, next_after) {
                    (1..=2, Some(next)) => cursor = next,
                    (3, None) => break,
                    (other, got) => panic!("page {other} reported {got:?}"),
                }
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    seen.sort_unstable();
    assert_eq!(seen.len(), 25);
    assert_eq!(seen.first(), Some(&1));
    assert_eq!(seen.last(), Some(&25));
}

#[test]
fn resume_is_a_pure_read_so_a_cancelled_or_timed_out_pull_changes_nothing() {
    let s = store();
    fill(&s, &["bob"], 3, "dev-a");
    let before = s.delivery_depth(Some("bob")).unwrap();
    let first = s
        .resume(GROUP, "bob", "dev-1", 0, ResumePolicy::default())
        .unwrap();
    let second = s
        .resume(GROUP, "bob", "dev-1", 0, ResumePolicy::default())
        .unwrap();
    let page = |plan: &ResumePlan| match plan {
        ResumePlan::Messages { items, .. } => mail_ids(items),
        ResumePlan::Resync { .. } => vec!["resync".to_string()],
    };
    assert_eq!(page(&first), page(&second), "same page every attempt");
    assert_eq!(page(&first), ["m1", "m2", "m3"]);
    assert_eq!(s.delivery_depth(Some("bob")).unwrap(), before);
    assert_eq!(s.device_cursor("dev-1", CONV).unwrap().0, 0);
}

#[test]
fn a_stale_cursor_or_a_future_one_asks_for_a_full_resync() {
    let s = store();
    fill(&s, &["bob"], 50, "dev-a");
    let policy = ResumePolicy {
        max_gap: 20,
        batch: 10,
    };
    match s.resume(GROUP, "bob", "dev-1", 10, policy).unwrap() {
        ResumePlan::Resync { head, cursor } => assert_eq!((head, cursor), (50, 10)),
        other => panic!("gap of 40 must ask for resync, got {other:?}"),
    }
    // a cursor ahead of the head (device restore skew) is the same story
    match s.resume(GROUP, "bob", "dev-1", 900, policy).unwrap() {
        ResumePlan::Resync { head, cursor } => assert_eq!((head, cursor), (50, 900)),
        other => panic!("future cursor must ask for resync, got {other:?}"),
    }
    // just inside the window it stays incremental
    match s.resume(GROUP, "bob", "dev-1", 30, policy).unwrap() {
        ResumePlan::Messages {
            items,
            next_after,
            stored_cursor,
        } => {
            assert_eq!(items.len(), 10, "bounded batch");
            assert_eq!(next_after, Some(40));
            assert_eq!(stored_cursor, 0, "the device never acked");
        }
        other => panic!("gap of 20 is still incremental: {other:?}"),
    }
}

#[test]
fn claims_are_bounded_so_an_offline_backlog_cannot_run_away() {
    let s = store();
    fill(&s, &["bob"], 300, "dev-a");
    assert_eq!(s.delivery_depth(Some("bob")).unwrap(), 300);
    // asking for everything is refused politely: one bounded batch
    let got = s
        .claim_delivery("hub-1", t(0), 100_000, DEFAULT_LEASE_MS)
        .unwrap();
    assert_eq!(got.len(), MAX_CLAIM_BATCH);
    // asking for zero still makes progress, and a bogus lease is floored
    let one = s.claim_delivery("hub-1", t(0), 0, 1).unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(
        s.delivery_depth(Some("bob")).unwrap(),
        300,
        "still all owed"
    );
}

#[test]
fn a_stale_worker_cannot_release_a_lease_it_never_held() {
    let s = store();
    fill(&s, &["bob"], 1, "dev-a");
    let mine = s.claim_delivery("hub-1", t(0), 10, 5_000).unwrap();
    assert_eq!(mine.len(), 1);
    // hub-2 never claimed this row
    assert!(!s
        .release_delivery("hub-2", &mine[0], t(2_000), false)
        .unwrap());
    // hub-1 releases it: back to pending, claimable by anyone
    assert!(s
        .release_delivery("hub-1", &mine[0], t(2_000), false)
        .unwrap());
    let theirs = s.claim_delivery("hub-2", t(2_001), 10, 5_000).unwrap();
    assert_eq!(theirs.len(), 1, "released row is live again");
    // dead letters stop counting as owed until a submit retry re-arms them
    assert!(s
        .release_delivery("hub-2", &theirs[0], t(9_000), true)
        .unwrap());
    assert_eq!(
        s.delivery_depth(Some("bob")).unwrap(),
        0,
        "dead is not owed"
    );
    assert!(s
        .claim_delivery("hub-3", t(9_001), 10, 5_000)
        .unwrap()
        .is_empty());
}

#[test]
fn retention_prunes_stored_rows_and_never_touches_an_unacked_one() {
    let s = store();
    fill(&s, &["bob"], 2, "dev-a");
    s.stored_ack("bob", "dev-1", CONV, 1).unwrap();
    s.claim_delivery("hub-1", t(0), 10, DEFAULT_LEASE_MS)
        .unwrap();
    let cutoff = wxlike_core::types::now_ms().saturating_sub(TASK_RETENTION_MS / 2);
    assert_eq!(
        s.prune_delivery(cutoff).unwrap(),
        0,
        "nothing is that old yet"
    );
    let pruned = s.prune_delivery(wxlike_core::types::now_ms() + 1).unwrap();
    assert_eq!(pruned, 1, "the stored row goes, the owed row stays");
    assert_eq!(s.delivery_depth(Some("bob")).unwrap(), 1);
}

#[test]
fn resume_refuses_a_user_with_no_visible_messages_and_sees_only_their_own() {
    let s = store();
    fill(&s, &["bob"], 3, "dev-a");
    match s
        .resume(GROUP, "mallory", "dev-x", 0, ResumePolicy::default())
        .unwrap()
    {
        ResumePlan::Messages { items, .. } => assert!(items.is_empty(), "no leak"),
        other => panic!("mallory has no backlog: {other:?}"),
    }
    // depth is scoped per user
    assert_eq!(s.delivery_depth(Some("mallory")).unwrap(), 0);
    assert_eq!(s.delivery_depth(None).unwrap(), 3);
}

#[test]
fn member_who_left_stops_receiving_at_the_leave_edge() {
    let s = store();
    // carol leaves after seq 2
    let rcpts = vec![
        SubmitRecipient::left("carol", 0, 3),
        SubmitRecipient::member("dave"),
    ];
    for i in 1..=4 {
        s.submit(&SubmitRequest {
            owner: GROUP,
            sender: "alice",
            sender_device: "dev-a",
            idempotency_key: &format!("edge-{i}"),
            msg_id: &format!("e{i}"),
            type_id: "text",
            payload: "x",
            body_kind: BodyKind::Plain,
            recipients: &rcpts,
            forward_of: None,
        })
        .unwrap();
    }
    assert_eq!(
        s.delivery_depth(Some("carol")).unwrap(),
        2,
        "seq 1 and 2 only"
    );
    assert_eq!(
        s.delivery_depth(Some("dave")).unwrap(),
        4,
        "the rest keep receiving"
    );
    match s
        .resume(GROUP, "carol", "dev-carol", 0, ResumePolicy::default())
        .unwrap()
    {
        ResumePlan::Messages { items, .. } => {
            assert_eq!(mail_ids(&items), ["e1", "e2"]);
        }
        other => panic!("{other:?}"),
    }
}
