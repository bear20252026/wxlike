//! G03-05 acceptance: push as a sync hint, device-owned tokens, and the
//! scheduling a waking phone drives.
//!
//! Vendor calls are injected through [`PushPort`]; the only "mock" here is the
//! port itself, which the card allows for vendor fault injection. All message,
//! cursor and projection state is the real SQLite store.

use wxlike_message::{
    BodyKind, MailError, MessageStore, NoteOutcome as SyncNote, OwnerType, Platform, PushError,
    RecordingPort, ResumePolicy, SubmitRecipient, SubmitRequest, SyncHint, SyncScheduler, SyncStep,
    SyncTrigger,
};

const GROUP: (OwnerType, &str) = (OwnerType::Group, "g-100");

fn store() -> MessageStore {
    MessageStore::open(None).unwrap()
}

fn t(offset: u64) -> u64 {
    wxlike_core::types::now_ms() + offset
}

fn send(s: &MessageStore, n: u64, text: &str, users: &[&str]) {
    let rcpts: Vec<SubmitRecipient> = users.iter().map(|u| SubmitRecipient::member(u)).collect();
    s.submit(&SubmitRequest {
        owner: GROUP,
        sender: "alice",
        sender_device: "dev-alice",
        idempotency_key: &format!("k-{n}"),
        msg_id: &format!("m{n}"),
        type_id: "text",
        payload: text,
        body_kind: BodyKind::Plain,
        recipients: &rcpts,
        forward_of: None,
    })
    .unwrap();
}

fn hint(conv: &str, count: u32) -> SyncHint {
    SyncHint {
        conv_id: conv.to_string(),
        from_user: "alice".to_string(),
        count,
        sent_at_ms: wxlike_core::types::now_ms(),
        resync: false,
    }
}

// --- tokens belong to devices, and revocation sticks -------------------------

#[test]
fn registration_is_per_device_idempotent_and_rotates_on_a_new_token() {
    let s = store();
    let e1 = s
        .register_push("bob", "dev-phone", Platform::Ios, "TOKEN-A")
        .unwrap();
    assert_eq!(e1, 1);
    // same token again (app relaunch) is not a new fact
    assert_eq!(
        s.register_push("bob", "dev-phone", Platform::Ios, "TOKEN-A")
            .unwrap(),
        1
    );
    // a second device of the same account has its own token
    assert_eq!(
        s.register_push("bob", "dev-pad", Platform::Android, "TOKEN-B")
            .unwrap(),
        1
    );
    assert_eq!(s.push_devices("bob").unwrap().len(), 2);

    // rotate: the epoch moves, so anything still holding TOKEN-A is refused
    let rotated = s
        .register_push("bob", "dev-phone", Platform::Ios, "TOKEN-A2")
        .unwrap();
    assert_eq!(rotated, 2);
    let target = s.push_token("dev-phone").unwrap().expect("token");
    assert_eq!(target.token, "TOKEN-A2");
    assert_eq!(target.platform, Platform::Ios);

    for (user, device, token) in [("", "d", "T"), ("bob", "", "T"), ("bob", "d", "")] {
        assert!(matches!(
            s.register_push(user, device, Platform::Other, token)
                .unwrap_err(),
            MailError::InvalidInput(_)
        ));
    }
}

#[test]
fn a_revoked_device_stops_receiving_even_private_hints() {
    let s = store();
    let port = RecordingPort::new();
    s.register_push("bob", "dev-old", Platform::Ios, "T-OLD")
        .unwrap();
    s.register_push("bob", "dev-new", Platform::Android, "T-NEW")
        .unwrap();
    let report = s
        .dispatch_hint(port.as_ref(), "bob", &hint("g-100", 1))
        .unwrap();
    assert_eq!((report.attempted, report.delivered), (2, 2));

    assert!(s.revoke_push("dev-old").unwrap());
    let report = s
        .dispatch_hint(port.as_ref(), "bob", &hint("g-100", 2))
        .unwrap();
    assert_eq!(
        (report.attempted, report.delivered),
        (1, 1),
        "only the live device"
    );
    let sent = port.sent();
    assert_eq!(sent.iter().filter(|(d, _)| d == "dev-old").count(), 1);
    assert_eq!(sent.iter().filter(|(d, _)| d == "dev-new").count(), 2);
    // revoking twice is not a second event
    assert!(!s.revoke_push("dev-old").unwrap());
    // and the revoked token is not even addressable any more
    assert!(s.push_token("dev-old").unwrap().is_none());
}

#[test]
fn signing_out_everywhere_keeps_the_device_in_use() {
    let s = store();
    for device in ["dev-1", "dev-2", "dev-3"] {
        s.register_push("bob", device, Platform::Ios, device)
            .unwrap();
    }
    assert_eq!(s.revoke_other_devices("bob", "dev-2").unwrap(), 2);
    let live: Vec<String> = s
        .push_devices("bob")
        .unwrap()
        .into_iter()
        .map(|d| d.device_id)
        .collect();
    assert_eq!(live, vec!["dev-2".to_string()]);
    // another user's devices are untouched by this call
    s.register_push("carol", "dev-c", Platform::Ios, "C")
        .unwrap();
    assert_eq!(s.push_devices("carol").unwrap().len(), 1);
}

// --- the hint is a hint, not the message ------------------------------------

#[test]
fn hints_never_carry_body_and_a_private_one_carries_no_ids() {
    let h = hint("g-100", 3);
    assert!(h.is_body_free());
    assert!(!h.conv_id.contains("payload"));

    let p = SyncHint::private(2, 1_000);
    assert!(p.conv_id.is_empty() && p.from_user.is_empty());
    assert!(p.is_body_free(), "a bare count is still a valid hint");

    // anything shaped like content is refused before it can reach a vendor
    let s = store();
    s.register_push("bob", "dev-1", Platform::Ios, "T").unwrap();
    let port = RecordingPort::new();
    let normal = SyncHint {
        conv_id: "g-100".into(),
        from_user: "alice".into(),
        count: 1,
        sent_at_ms: 0,
        resync: false,
    };
    assert!(s.dispatch_hint(port.as_ref(), "bob", &normal).is_ok());
    let oversized = SyncHint {
        conv_id: "x".repeat(200),
        from_user: String::new(),
        count: 1,
        sent_at_ms: 0,
        resync: false,
    };
    assert!(matches!(
        s.dispatch_hint(port.as_ref(), "bob", &oversized).unwrap_err(),
        MailError::InvalidInput(m) if m.contains("body-free")
    ));
    let multiline = SyncHint {
        conv_id: "g-1\nbody text".into(),
        from_user: String::new(),
        count: 1,
        sent_at_ms: 0,
        resync: false,
    };
    assert!(!multiline.is_body_free());
    assert!(s.dispatch_hint(port.as_ref(), "bob", &multiline).is_err());
    // zero count is not a hint
    let zero = SyncHint {
        conv_id: "g-100".into(),
        from_user: String::new(),
        count: 0,
        sent_at_ms: 0,
        resync: false,
    };
    assert!(!zero.is_body_free());
}

#[test]
fn vendor_failures_are_per_device_and_leave_message_state_untouched() {
    let s = store();
    send(&s, 1, "hello", &["bob"]);
    let before_depth = s.delivery_depth(Some("bob")).unwrap();
    let before_unread = s.unread(GROUP, "bob").unwrap();

    let port = RecordingPort::new();
    port.fail_with("dev-offline", PushError::Unavailable("apns 503".into()));
    port.fail_with("dev-gone", PushError::Invalid("Unregistered".into()));
    s.register_push("bob", "dev-ok", Platform::Ios, "T1")
        .unwrap();
    s.register_push("bob", "dev-offline", Platform::Android, "T2")
        .unwrap();
    s.register_push("bob", "dev-gone", Platform::Android, "T3")
        .unwrap();

    let report = s
        .dispatch_hint(port.as_ref(), "bob", &hint("g-100", 1))
        .unwrap();
    assert_eq!(
        (
            report.attempted,
            report.delivered,
            report.unavailable,
            report.invalid
        ),
        (3, 1, 1, 1)
    );
    assert_eq!(
        s.delivery_depth(Some("bob")).unwrap(),
        before_depth,
        "nothing retired"
    );
    assert_eq!(
        s.unread(GROUP, "bob").unwrap(),
        before_unread,
        "nothing read"
    );
    // the device that could not be reached still gets its hint later, and the one
    // that failed never becomes a "delivered" success in the report
    let later = s
        .dispatch_hint(port.as_ref(), "bob", &hint("g-100", 1))
        .unwrap();
    assert_eq!(later.delivered, 1);
}

#[test]
fn a_deployment_without_a_vendor_still_syncs() {
    let s = store();
    send(&s, 1, "hello", &["bob"]);
    s.register_push("bob", "dev-1", Platform::Ios, "T").unwrap();
    let report = s
        .dispatch_hint(&wxlike_message::NoopPort, "bob", &hint("g-100", 1))
        .unwrap();
    assert_eq!((report.attempted, report.delivered), (1, 1));
    // and the real catch-up does not depend on push at all
    let plan = s
        .resume(GROUP, "bob", "dev-1", 0, ResumePolicy::default())
        .unwrap();
    assert!(matches!(plan, wxlike_message::ResumePlan::Messages { items, .. } if items.len() == 1));
}

#[test]
fn stale_tokens_are_pruned_so_the_table_cannot_grow_forever() {
    let s = store();
    for i in 0..5 {
        s.register_push("bob", &format!("dev-{i}"), Platform::Ios, "T")
            .unwrap();
    }
    assert_eq!(
        s.prune_push_devices(t(1_000)).unwrap(),
        5,
        "all of them are older"
    );
    assert!(s.push_devices("bob").unwrap().is_empty());
    s.register_push("bob", "dev-live", Platform::Ios, "T")
        .unwrap();
    let just_now = wxlike_core::types::now_ms().saturating_sub(1_000);
    assert_eq!(
        s.prune_push_devices(just_now).unwrap(),
        0,
        "a live install stays"
    );
    assert_eq!(s.push_devices("bob").unwrap().len(), 1);
}

// --- the scheduler: out of order, duplicated, unreachable -------------------

#[test]
fn duplicate_hints_coalesce_and_a_late_one_is_dropped() {
    let mut sched = SyncScheduler::new(16, 30_000);
    assert_eq!(sched.note("g-1", 1, 1_000, false), SyncNote::Scheduled);
    assert_eq!(sched.note("g-1", 2, 1_000, false), SyncNote::Coalesced);
    assert_eq!(
        sched.note("g-1", 9, 500, false),
        SyncNote::Stale,
        "out of order"
    );
    assert_eq!(sched.pending(), 1, "one conversation, one fetch");

    // not due yet
    assert!(sched.due(20_000).is_empty());
    let due = sched.due(31_000);
    assert_eq!(due.len(), 1);
    assert_eq!(due[0], ("g-1".to_string(), 3, false), "counts folded");
    assert_eq!(sched.pending(), 0, "the entry left the queue");
    assert!(sched.due(99_000).is_empty());
}

#[test]
fn a_resync_hint_is_not_held_for_the_aggregation_window() {
    let mut sched = SyncScheduler::new(8, 30_000);
    sched.note("g-1", 1, 0, true);
    let due = sched.due(0);
    assert_eq!(due, vec![("g-1".to_string(), 1, true)]);
}

#[test]
fn a_hint_burst_cannot_grow_the_queue_past_its_cap() {
    let mut sched = SyncScheduler::new(3, 1_000);
    let mut outcomes = Vec::new();
    for i in 0..10 {
        outcomes.push(sched.note(&format!("g-{i}"), 1, 1_000 + i, false));
    }
    assert_eq!(sched.pending(), 3, "bounded");
    assert!(
        outcomes.contains(&SyncNote::Overflowed),
        "overflow is reported, not silent"
    );
    // the surviving entries are the newest ones
    let due = sched.due(10_000);
    let ids: Vec<String> = due.into_iter().map(|(c, _, _)| c).collect();
    assert_eq!(ids.len(), 3);
    assert!(ids.contains(&"g-9".to_string()));
}

#[test]
fn private_hints_fold_into_one_anonymous_entry() {
    let mut sched = SyncScheduler::new(4, 0);
    sched.note("", 1, 10, false);
    sched.note("", 1, 20, false);
    assert_eq!(sched.pending(), 1);
    let due = sched.due(20);
    assert_eq!(
        due,
        vec![("".to_string(), 2, false)],
        "no conversation id leaks"
    );
}

// --- driving the queue when the device comes back ---------------------------

#[test]
fn a_waking_device_gets_a_delta_and_a_projection_that_tells_the_truth() {
    let s = store();
    s.register_push("bob", "dev-phone", Platform::Android, "T")
        .unwrap();
    send(&s, 1, "first", &["bob"]);
    send(&s, 2, "second", &["bob"]);

    // a hub took the tasks and then died: inflight with an expired lease
    let claimed = s.claim_delivery("hub-1", t(0), 10, 1_000).unwrap();
    assert_eq!(claimed.len(), 2);

    let schedule = s
        .drive_queue(
            "bob",
            "dev-phone",
            SyncTrigger::Foreground,
            t(5_000),
            ResumePolicy::default(),
        )
        .unwrap();
    assert_eq!(schedule.requeued, 2, "expired leases were put back");
    assert_eq!(schedule.projections_refreshed, 1, "the list was rebuilt");
    assert_eq!(schedule.steps.len(), 1);
    match &schedule.steps[0] {
        SyncStep::Delta {
            conv_id,
            items,
            next_cursor,
        } => {
            assert_eq!(conv_id, "g-100");
            assert_eq!(items.len(), 2, "both messages are owed");
            assert_eq!(items[0].payload, "first");
            assert_eq!(next_cursor, &None, "two rows are inside one page");
        }
        other => panic!("expected a delta, got {other:?}"),
    }
    // the projection is a fact about the mailbox, not a cached string
    let row = &s
        .conversations(
            "bob",
            wxlike_message::ConversationScope::All,
            Default::default(),
            None,
            10,
        )
        .unwrap()
        .items[0];
    assert_eq!(
        (row.preview.as_str(), row.last_seq, row.unread),
        ("second", 2, 2)
    );
}

#[test]
fn a_retry_while_the_device_was_away_still_rebuilds_the_projection() {
    let s = store();
    send(&s, 1, "top secret", &["bob"]);
    s.register_push("bob", "dev-phone", Platform::Ios, "T")
        .unwrap();
    // a moderator pulls the message while the phone is off
    s.withdraw_as(GROUP, "m1", "admin").unwrap();
    // a worker had already given up on this row
    let claimed = s.claim_delivery("hub-1", t(0), 10, 1_000).unwrap();
    s.release_delivery("hub-1", &claimed[0], t(9_000), true)
        .unwrap();
    assert_eq!(
        s.delivery_depth(Some("bob")).unwrap(),
        0,
        "dead is not owed"
    );

    let schedule = s
        .drive_queue(
            "bob",
            "dev-phone",
            SyncTrigger::NetworkRecovered,
            t(10_000),
            ResumePolicy::default(),
        )
        .unwrap();
    assert_eq!(
        schedule.rearmed, 1,
        "the dead row was re-armed for a human looking at the list"
    );
    assert_eq!(schedule.projections_refreshed, 1);
    let row = &s
        .conversations(
            "bob",
            wxlike_message::ConversationScope::All,
            Default::default(),
            None,
            10,
        )
        .unwrap()
        .items[0];
    assert_eq!(row.preview, "", "the recalled body is not offered again");
    assert!(row.last_withdrawn);
}

#[test]
fn a_timer_sweep_does_not_resurrect_work_nobody_asked_for() {
    let s = store();
    send(&s, 1, "x", &["bob"]);
    let claimed = s.claim_delivery("hub-1", t(0), 10, 1_000).unwrap();
    s.release_delivery("hub-1", &claimed[0], t(9_000), true)
        .unwrap();
    let schedule = s
        .drive_queue(
            "bob",
            "dev-phone",
            SyncTrigger::Timer,
            t(10_000),
            ResumePolicy::default(),
        )
        .unwrap();
    assert_eq!(
        schedule.rearmed, 0,
        "background sweeps must not revive dead rows"
    );
    // but an expired lease is still swept, because that one is genuinely owed
    send(&s, 2, "y", &["bob"]);
    let inflight = s.claim_delivery("hub-2", t(10_000), 10, 1_000).unwrap();
    assert_eq!(inflight.len(), 1);
    let swept = s
        .drive_queue(
            "bob",
            "dev-phone",
            SyncTrigger::Timer,
            t(30_000),
            ResumePolicy::default(),
        )
        .unwrap();
    assert_eq!(swept.requeued, 1);
}

#[test]
fn sync_delta_marks_nothing_so_a_cancelled_pull_can_be_repeated() {
    let s = store();
    send(&s, 1, "a", &["bob"]);
    send(&s, 2, "b", &["bob"]);
    let first = s
        .sync_delta(GROUP, "bob", "dev-1", 0, ResumePolicy::default())
        .unwrap();
    let again = s
        .sync_delta(GROUP, "bob", "dev-1", 0, ResumePolicy::default())
        .unwrap();
    assert_eq!(first, again, "the same request twice is the same page");
    assert_eq!(s.delivery_depth(Some("bob")).unwrap(), 2, "nothing retired");
    assert_eq!(s.device_cursor("dev-1", "g-100").unwrap(), (0, 0));
    // and a wake-up driven from a stale cursor asks for a resync, not a wrong page
    match s
        .sync_delta(GROUP, "bob", "dev-1", 900, ResumePolicy::default())
        .unwrap()
    {
        SyncStep::Resync { conv_id, head } => assert_eq!((conv_id.as_str(), head), ("g-100", 2)),
        other => panic!("stale cursor must resync, got {other:?}"),
    }
    assert!(matches!(
        s.drive_queue(
            "",
            "dev-1",
            SyncTrigger::Foreground,
            t(0),
            ResumePolicy::default()
        )
        .unwrap_err(),
        MailError::InvalidInput(_)
    ));
}

#[test]
fn killed_process_then_hint_then_open_then_incremental_sync_completes() {
    let s = store();
    let port = RecordingPort::new();
    s.register_push("bob", "dev-phone", Platform::Android, "T")
        .unwrap();

    // 1. traffic arrives while the app is dead: tasks parked, hint sent
    send(&s, 1, "one", &["bob"]);
    send(&s, 2, "two", &["bob"]);
    let report = s
        .dispatch_hint(port.as_ref(), "bob", &hint("g-100", 2))
        .unwrap();
    assert_eq!(report.delivered, 1);

    // 2. a worker claimed them and the process was killed (no ack ever came)
    let claimed = s.claim_delivery("hub-1", t(0), 10, 1_000).unwrap();
    assert_eq!(claimed.len(), 2);
    assert_eq!(
        s.delivery_depth(Some("bob")).unwrap(),
        2,
        "still owed after the crash"
    );

    // 3. the hint is noted, the app opens, the queue is driven
    let mut sched = SyncScheduler::new(8, 0);
    sched.note("g-100", 2, wxlike_core::types::now_ms(), false);
    assert_eq!(sched.due(wxlike_core::types::now_ms()).len(), 1);
    let schedule = s
        .drive_queue(
            "bob",
            "dev-phone",
            SyncTrigger::HintArrived,
            t(5_000),
            ResumePolicy::default(),
        )
        .unwrap();
    let SyncStep::Delta {
        items, next_cursor, ..
    } = &schedule.steps[0]
    else {
        panic!(
            "the device was behind, so a delta was expected: {:?}",
            schedule.steps
        )
    };
    assert_eq!(items.len(), 2, "the full missed range came back");
    assert_eq!(next_cursor, &None);

    // 4. the device persists and acknowledges: state converges, nothing owed
    s.stored_ack("bob", "dev-phone", "g-100", 2).unwrap();
    assert_eq!(s.delivery_depth(Some("bob")).unwrap(), 0);
    assert_eq!(s.device_cursor("dev-phone", "g-100").unwrap().0, 2);
    let tail = s
        .sync_delta(GROUP, "bob", "dev-phone", 2, ResumePolicy::default())
        .unwrap();
    match tail {
        SyncStep::Delta { items, .. } => assert!(items.is_empty(), "caught up"),
        other => panic!("no resync needed here: {other:?}"),
    }
}

#[test]
fn a_hint_that_never_arrived_cannot_make_the_state_wrong() {
    let s = store();
    s.register_push("bob", "dev-phone", Platform::Ios, "T")
        .unwrap();
    send(&s, 1, "x", &["bob"]);
    let port = RecordingPort::new();
    port.fail_with("dev-phone", PushError::Unavailable("network down".into()));
    let report = s
        .dispatch_hint(port.as_ref(), "bob", &hint("g-100", 1))
        .unwrap();
    assert_eq!(report.unavailable, 1);
    assert!(
        port.sent().is_empty(),
        "nothing was claimed to have been sent"
    );
    // the device opens anyway (user pulls to refresh): the sync is complete
    let schedule = s
        .drive_queue(
            "bob",
            "dev-phone",
            SyncTrigger::Foreground,
            t(1),
            ResumePolicy::default(),
        )
        .unwrap();
    match &schedule.steps[0] {
        SyncStep::Delta { items, .. } => assert_eq!(items.len(), 1),
        other => panic!("{other:?}"),
    }
}
