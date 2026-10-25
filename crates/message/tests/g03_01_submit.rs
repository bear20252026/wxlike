//! G03-01 acceptance: the submit transaction and its idempotency.
//!
//! Real `MessageStore` on SQLite only — no mocks. Covers the card's acceptance
//! lines: one message per lost-response retry, no half message after an abort,
//! unique seqs under concurrency, and member visibility decided by event time
//! rather than wall clock.

use wxlike_message::{
    BodyKind, MailError, MessageStore, OwnerType, SubmitOutcome, SubmitRecipient, SubmitRequest,
    RECALL_WINDOW_MS,
};

const GROUP: (OwnerType, &str) = (OwnerType::Group, "g-100");

fn store() -> MessageStore {
    MessageStore::open(None).unwrap()
}

fn one(user: &str) -> Vec<SubmitRecipient> {
    vec![SubmitRecipient::member(user)]
}

fn req<'a>(
    sender: &'a str,
    device: &'a str,
    key: &'a str,
    msg_id: &'a str,
    payload: &'a str,
    recipients: &'a [SubmitRecipient],
) -> SubmitRequest<'a> {
    SubmitRequest {
        owner: (OwnerType::Group, "g-100"),
        sender,
        sender_device: device,
        idempotency_key: key,
        msg_id,
        type_id: "text",
        payload,
        body_kind: BodyKind::Plain,
        recipients,
        forward_of: None,
    }
}

#[test]
fn submit_writes_timeline_idempotency_and_outbox_in_one_transaction() {
    let s = store();
    let out = s
        .submit(&req("alice", "dev-a", "k1", "m1", "{\"t\":1}", &one("bob")))
        .unwrap();
    assert_eq!(
        out,
        SubmitOutcome::Created {
            msg_id: "m1".into(),
            seq: 1,
            recipients: 1
        }
    );
    let (rows, _) = s.fetch(GROUP, 0, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].sender_device, "dev-a");
    assert_eq!(rows[0].body_kind, BodyKind::Plain);
    assert_eq!(rows[0].edit_version, 0);
    assert_eq!(rows[0].forward_of, None);
    assert_eq!(
        s.unread(GROUP, "bob").unwrap(),
        1,
        "recipient owes one message"
    );
    assert_eq!(s.delivery_depth(Some("bob")).unwrap(), 1);
}

#[test]
fn lost_response_retry_returns_the_original_message_without_a_second_seq() {
    let s = store();
    let rcpts = one("bob");
    s.submit(&req("alice", "dev-a", "k1", "m1", "body", &rcpts))
        .unwrap();
    // client never saw the response and retries with a fresh msg_id
    let again = s.submit(&req("alice", "dev-a", "k1", "m1-rewritten", "body", &rcpts));
    match again {
        Ok(SubmitOutcome::Duplicate {
            msg_id,
            seq,
            recipients_added,
        }) => assert_eq!((msg_id.as_str(), seq, recipients_added), ("m1", 1, 0)),
        other => panic!("expected a duplicate ack, got {other:?}"),
    }
    assert_eq!(s.head_seq(GROUP).unwrap(), 1);
    let (rows, _) = s.fetch(GROUP, 0, 10).unwrap();
    assert_eq!(rows.len(), 1, "exactly one message for two submits");
    assert_eq!(rows[0].msg_id, "m1");
}

#[test]
fn idempotency_key_is_scoped_to_the_device_that_used_it() {
    let s = store();
    let rcpts = one("bob");
    for device in ["dev-a", "dev-b"] {
        let out = s
            .submit(&req("alice", device, "same-key", device, "body", &rcpts))
            .unwrap();
        assert!(matches!(out, SubmitOutcome::Created { .. }));
    }
    assert_eq!(s.head_seq(GROUP).unwrap(), 2, "two devices, two messages");
}

#[test]
fn key_reuse_for_different_content_is_refused_instead_of_resent() {
    let s = store();
    let rcpts = one("bob");
    s.submit(&req("alice", "dev-a", "k1", "m1", "first", &rcpts))
        .unwrap();
    let err = s
        .submit(&req("alice", "dev-a", "k1", "m2", "SECOND", &rcpts))
        .unwrap_err();
    assert!(matches!(err, MailError::IdempotencyConflict(d) if d == "dev-a"));
    assert_eq!(s.head_seq(GROUP).unwrap(), 1);
}

#[test]
fn submit_rejects_incomplete_requests_before_touching_the_store() {
    let s = store();
    let rcpts = one("bob");
    let cases: Vec<(SubmitRequest, &str)> = vec![
        (req("alice", "", "k", "m", "b", &rcpts), "sender_device"),
        (req("alice", "d", "", "m", "b", &rcpts), "idempotency_key"),
        (req("alice", "d", "k", "", "b", &rcpts), "msg_id"),
        (req("alice", "d", "k", "m", "", &rcpts), "payload is empty"),
        (req("alice", "d", "k", "m", "b", &[]), "no recipients"),
    ];
    for (r, want) in cases {
        let err = s.submit(&r).unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains(want) || matches!(err, MailError::NoRecipients),
            "expected {want:?} in {text}"
        );
    }
    assert_eq!(s.head_seq(GROUP).unwrap(), 0);
    assert_eq!(s.delivery_depth(None).unwrap(), 0);
}

#[test]
fn submit_only_accepts_normal_channel_registered_types() {
    let s = store();
    let rcpts = one("bob");
    for (type_id, want) in [
        ("typing", "high_priority"),
        ("wallet.redpacket.claim", "ephemeral"),
    ] {
        let mut r = req("alice", "d", "k", "m", "b", &rcpts);
        r.type_id = type_id;
        let err = match s.submit(&r) {
            Err(e) => e,
            Ok(_) => panic!("{type_id} must not enter submit"),
        };
        assert!(
            matches!(&err, MailError::WrongChannel(t, class) if *class == want && t == type_id),
            "{err}"
        );
    }
    let mut r = req("alice", "d", "k", "m", "b", &rcpts);
    r.type_id = "not-registered";
    assert!(matches!(
        s.submit(&r).unwrap_err(),
        MailError::UnregisteredType(_)
    ));
}

#[test]
fn aborted_transaction_leaves_no_half_message_and_no_poisoned_key() {
    let s = store();
    let rcpts = one("bob");
    s.submit(&req("alice", "dev-a", "k1", "shared", "body", &rcpts))
        .unwrap();
    // same msg_id on a new key: seq is allocated, the insert fails, the
    // transaction is dropped — nothing may survive
    let err = s
        .submit(&req("alice", "dev-a", "k2", "shared", "body", &rcpts))
        .unwrap_err();
    assert!(matches!(err, MailError::InvalidInput(m) if m.contains("msg_id")));
    assert_eq!(s.head_seq(GROUP).unwrap(), 1, "seq was not consumed");
    let (rows, _) = s.fetch(GROUP, 0, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        s.delivery_depth(None).unwrap(),
        1,
        "only the first recipient row"
    );
    // the failed key is not recorded, so the client may reuse it after a fix
    assert!(s
        .submit(&req("alice", "dev-a", "k2", "k2-msg", "body", &rcpts))
        .is_ok());
}

#[test]
fn concurrent_submits_from_distinct_devices_never_collide_on_seq() {
    let s = std::sync::Arc::new(store());
    let mut handles = Vec::new();
    for t in 0..40 {
        let s = s.clone();
        handles.push(std::thread::spawn(move || {
            let rcpts = vec![SubmitRecipient::member("bob")];
            let out = s
                .submit(&req(
                    "alice",
                    &format!("dev-{t}"),
                    &format!("key-{t}"),
                    &format!("msg-{t}"),
                    "body",
                    &rcpts,
                ))
                .unwrap();
            match out {
                SubmitOutcome::Created { seq, .. } => seq,
                _ => panic!("distinct keys must not dedupe"),
            }
        }));
    }
    let mut seqs: Vec<u64> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    seqs.sort_unstable();
    seqs.dedup();
    assert_eq!(seqs.len(), 40, "40 distinct seqs");
    assert_eq!(s.head_seq(GROUP).unwrap(), 40);
    assert_eq!(s.delivery_depth(Some("bob")).unwrap(), 40);
}

#[test]
fn retry_backfills_recipients_that_the_first_attempt_left_out() {
    let s = store();
    let first = one("bob");
    s.submit(&req("alice", "dev-a", "k1", "m1", "body", &first))
        .unwrap();
    // same key, widened recipient set (late-joining member / failed fan-out)
    let widened = vec![
        SubmitRecipient::member("bob"),
        SubmitRecipient::member("carol"),
    ];
    let out = s
        .submit(&req("alice", "dev-a", "k1", "m1", "body", &widened))
        .unwrap();
    assert_eq!(s.delivery_depth(Some("bob")).unwrap(), 1, "bob untouched");
    assert_eq!(s.delivery_depth(Some("carol")).unwrap(), 1, "carol added");
    match out {
        SubmitOutcome::Duplicate {
            msg_id,
            seq,
            recipients_added,
        } => {
            assert_eq!((msg_id.as_str(), seq), ("m1", 1));
            assert!(recipients_added >= 1);
        }
        _ => panic!("expected duplicate outcome"),
    }
    assert_eq!(s.head_seq(GROUP).unwrap(), 1, "no second message");
}

#[test]
fn membership_window_decides_delivery_by_event_time_not_wall_clock() {
    let s = store();
    // five messages already in the timeline, so the next submit lands at seq 6
    for i in 1..=5 {
        s.append(GROUP, "alice", "text", &format!("pre{i}"), "{}")
            .unwrap();
    }
    // bob joined at seq 5 (sees 6), carol left at seq 3 (sees nothing from 6),
    // dave is a plain member, erin joined after this message (sees nothing)
    let rcpts = vec![
        SubmitRecipient::joined("bob", 5),
        SubmitRecipient::left("carol", 0, 3),
        SubmitRecipient::member("dave"),
        SubmitRecipient::joined("erin", 7),
    ];
    let out = s
        .submit(&req("alice", "dev-a", "k1", "m1", "body", &rcpts))
        .unwrap();
    match out {
        SubmitOutcome::Created { seq, .. } => assert_eq!(seq, 6),
        _ => panic!("expected created"),
    }
    assert_eq!(s.delivery_depth(Some("dave")).unwrap(), 1);
    assert_eq!(
        s.delivery_depth(Some("bob")).unwrap(),
        1,
        "seq 6 >= joined 5"
    );
    assert_eq!(
        s.delivery_depth(Some("carol")).unwrap(),
        0,
        "left before this message"
    );
    assert_eq!(s.delivery_depth(Some("erin")).unwrap(), 0, "joined after");

    // a retry of the same key cannot widen the window: the rule is the message's
    // seq, so nobody is backfilled who was not a member when it was written
    let later = s
        .submit(&req("alice", "dev-a", "k1", "m1", "body", &rcpts))
        .unwrap();
    match later {
        SubmitOutcome::Duplicate {
            seq,
            recipients_added,
            ..
        } => {
            assert_eq!(seq, 6);
            assert_eq!(recipients_added, 0);
        }
        _ => panic!("expected duplicate"),
    }
    assert_eq!(s.delivery_depth(Some("carol")).unwrap(), 0);
    assert_eq!(s.delivery_depth(Some("erin")).unwrap(), 0);
    assert_eq!(s.head_seq(GROUP).unwrap(), 6, "no second message");
}

#[test]
fn submit_refuses_when_no_recipient_is_inside_the_window() {
    let s = store();
    let rcpts = vec![SubmitRecipient::joined("bob", 9)];
    let err = s
        .submit(&req("alice", "dev-a", "k1", "m1", "body", &rcpts))
        .unwrap_err();
    assert!(matches!(err, MailError::NoRecipients));
    assert_eq!(s.head_seq(GROUP).unwrap(), 0, "refusal must not burn a seq");
}

// --- recall / edit / forward -------------------------------------------------

#[test]
fn recall_is_author_only_and_server_gated_by_the_window() {
    assert_eq!(RECALL_WINDOW_MS, 2 * 60 * 1000);
    let s = store();
    let rcpts = one("bob");
    s.submit(&req("alice", "dev-a", "k1", "m1", "body", &rcpts))
        .unwrap();
    s.submit(&req("mallory", "dev-m", "k2", "m2", "body", &rcpts))
        .unwrap();

    assert!(matches!(
        s.recall_message(GROUP, "m1", "mallory").unwrap_err(),
        MailError::WithdrawRefused
    ));
    s.recall_message(GROUP, "m1", "alice").unwrap();
    let (rows, _) = s.fetch(GROUP, 0, 10).unwrap();
    assert!(rows[0].withdrawn);
    assert!(rows[0].recalled_at_ms.is_some());
    // a second recall of the same row is refused, not silently successful
    assert!(matches!(
        s.recall_message(GROUP, "m1", "alice").unwrap_err(),
        MailError::WithdrawRefused
    ));

    // an expired window is refused by the store, not by the client's clock
    std::thread::sleep(std::time::Duration::from_millis(5));
    let err = s
        .withdraw_within(GROUP, "m2", "mallory", Some(1))
        .unwrap_err();
    assert!(matches!(err, MailError::RecallWindowElapsed(age) if age >= 1));
    // the moderated path stays unbounded on purpose (audited actor)
    s.withdraw_as(GROUP, "m2", "admin").unwrap();
    let (rows, _) = s.fetch(GROUP, 1, 10).unwrap();
    assert!(rows[0].withdrawn);
}

#[test]
fn edit_is_author_only_versioned_and_reprojects_the_head() {
    let s = store();
    let rcpts = one("bob");
    s.submit(&req("alice", "dev-a", "k1", "m1", "{\"t\":\"v1\"}", &rcpts))
        .unwrap();

    assert!(matches!(
        s.edit_message(GROUP, "m1", "mallory", None, "x", BodyKind::Plain)
            .unwrap_err(),
        MailError::EditRefused("not the author")
    ));
    assert!(matches!(
        s.edit_message(GROUP, "m1", "alice", None, "", BodyKind::Plain)
            .unwrap_err(),
        MailError::InvalidInput(_)
    ));
    assert!(matches!(
        s.edit_message(GROUP, "nope", "alice", None, "x", BodyKind::Plain)
            .unwrap_err(),
        MailError::EditRefused("unknown message")
    ));
    let conflict = s.edit_message(GROUP, "m1", "alice", Some(7), "x", BodyKind::Plain);
    assert!(matches!(
        conflict,
        Err(MailError::EditRefused("edit version conflict"))
    ));

    let v = s
        .edit_message(
            GROUP,
            "m1",
            "alice",
            Some(0),
            "{\"t\":\"v2\"}",
            BodyKind::Plain,
        )
        .unwrap();
    assert_eq!(v, 1);
    let (rows, _) = s.fetch(GROUP, 0, 10).unwrap();
    assert_eq!(rows[0].payload, "{\"t\":\"v2\"}");
    assert_eq!(rows[0].edit_version, 1);
    // the edit is broadcast as a change event so other devices converge
    let (changes, _) = s.changes(GROUP, 0, 10).unwrap();
    assert!(changes.iter().any(|c| c.kind == "edit" && c.msg_id == "m1"));

    s.recall_message(GROUP, "m1", "alice").unwrap();
    assert!(matches!(
        s.edit_message(GROUP, "m1", "alice", None, "x", BodyKind::Plain)
            .unwrap_err(),
        MailError::EditRefused("message recalled")
    ));
}

#[test]
fn forward_records_the_source_and_refuses_recalled_or_sealed_bodies() {
    let s = store();
    let rcpts = one("bob");
    s.submit(&req("alice", "dev-a", "k1", "plain", "hello", &rcpts))
        .unwrap();
    let mut sealed = req("alice", "dev-a", "k2", "sealed", "==cipherbytes==", &rcpts);
    sealed.body_kind = BodyKind::Ciphertext;
    s.submit(&sealed).unwrap();
    s.submit(&req("alice", "dev-a", "k3", "gone", "x", &rcpts))
        .unwrap();
    s.recall_message(GROUP, "gone", "alice").unwrap();

    let out = s
        .forward_message(GROUP, "plain", "carol", "dev-c", "f1", "fwd-1", &rcpts)
        .unwrap();
    match out {
        SubmitOutcome::Created { seq, .. } => assert_eq!(seq, 4),
        _ => panic!("expected created"),
    }
    let (rows, _) = s.fetch(GROUP, 3, 10).unwrap();
    assert_eq!(rows[0].msg_id, "fwd-1");
    assert_eq!(rows[0].forward_of.as_deref(), Some("plain"));
    assert_eq!(rows[0].sender, "carol");

    for (source, why) in [
        ("sealed", "ciphertext"),
        ("gone", "recalled"),
        ("absent", "not found"),
    ] {
        let err = s
            .forward_message(
                GROUP,
                source,
                "carol",
                "dev-c",
                &format!("f-{source}"),
                "m-x",
                &rcpts,
            )
            .unwrap_err();
        assert!(
            matches!(&err, MailError::ForwardRefused(m) if m.contains(why)),
            "{source}: {err}"
        );
    }
    // a forward retried with the same key is the same message again
    let again = s
        .forward_message(GROUP, "plain", "carol", "dev-c", "f1", "fwd-2", &rcpts)
        .unwrap();
    assert!(matches!(again, SubmitOutcome::Duplicate { .. }));
}

#[test]
fn sealed_payload_is_stored_verbatim_and_never_rewritten_by_the_server() {
    let s = store();
    let rcpts = one("bob");
    let mut sealed = req(
        "alice",
        "dev-a",
        "k1",
        "m1",
        "==opaque-sealed-bytes==",
        &rcpts,
    );
    sealed.body_kind = BodyKind::Ciphertext;
    s.submit(&sealed).unwrap();
    let (rows, _) = s.fetch(GROUP, 0, 10).unwrap();
    assert_eq!(rows[0].payload, "==opaque-sealed-bytes==");
    assert_eq!(rows[0].body_kind, BodyKind::Ciphertext);
}

// --- migration / rollback compatibility -------------------------------------

#[test]
fn pre_g03_database_is_upgraded_and_old_rows_keep_default_semantics() {
    let dir = std::env::temp_dir().join(format!(
        "wxlike-g03-migrate-{}-{}",
        std::process::id(),
        wxlike_core::types::now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("msg.db");
    {
        let legacy = rusqlite::Connection::open(&path).unwrap();
        legacy
            .execute_batch(
                "CREATE TABLE owner_seq(owner_type TEXT NOT NULL, owner_id TEXT NOT NULL,
                         seq INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(owner_type, owner_id));
                 CREATE TABLE mailbox(owner_type TEXT NOT NULL, owner_id TEXT NOT NULL,
                         seq INTEGER NOT NULL, msg_id TEXT NOT NULL, sender TEXT NOT NULL,
                         type_id TEXT NOT NULL, payload TEXT NOT NULL DEFAULT '',
                         sent_at_ms INTEGER NOT NULL, withdrawn INTEGER NOT NULL DEFAULT 0,
                         PRIMARY KEY(owner_type, owner_id, seq));
                 CREATE UNIQUE INDEX idx_mailbox_msgid ON mailbox(msg_id);
                 INSERT INTO owner_seq VALUES('group','g-100',1);
                 INSERT INTO mailbox VALUES('group','g-100',1,'legacy','old','text','was here',10,0);",
            )
            .unwrap();
    }
    let s = MessageStore::open(Some(path.to_str().unwrap())).unwrap();
    let (rows, _) = s.fetch(GROUP, 0, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].msg_id, "legacy");
    assert_eq!(rows[0].sender_device, "");
    assert_eq!(rows[0].body_kind, BodyKind::Plain);
    assert_eq!(rows[0].edit_version, 0);
    assert_eq!(rows[0].recalled_at_ms, None);
    assert_eq!(rows[0].forward_of, None);
    // writes on the upgraded file behave like any new store
    s.submit(&req("alice", "dev-a", "k1", "m1", "body", &one("bob")))
        .unwrap();
    assert_eq!(s.head_seq(GROUP).unwrap(), 2);
    std::fs::remove_dir_all(&dir).ok();
}
