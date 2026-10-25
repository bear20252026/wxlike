//! G03-03 acceptance: conversation projection, unread and cross-device prefs.
//!
//! Pins the card's lines: unread derived from authorised seq + read cursor,
//! pin/mute/archive as account preferences, `clear local` and `delete for
//! everyone` as two different commands, keyset paging over 1000 conversations
//! without repeats or skips, and domain events for search/attachment on delete.

use wxlike_message::{
    BodyKind, ConversationFilter, ConversationRow, ConversationScope, MailError, MessageStore,
    OwnerType, PrefField, SubmitRecipient, SubmitRequest,
};

fn store() -> MessageStore {
    MessageStore::open(None).unwrap()
}

fn owner(cid: &str) -> (OwnerType, &str) {
    (OwnerType::Group, cid)
}

/// One message from `sender` into `cid` for every recipient in `users`.
fn send(s: &MessageStore, cid: &str, seq_no: u64, text: &str, users: &[&str]) {
    let rcpts: Vec<SubmitRecipient> = users.iter().map(|u| SubmitRecipient::member(u)).collect();
    s.submit(&SubmitRequest {
        owner: owner(cid),
        sender: "alice",
        sender_device: "dev-alice",
        idempotency_key: &format!("{cid}-{seq_no}"),
        msg_id: &format!("{cid}-m{seq_no}"),
        type_id: "text",
        payload: text,
        body_kind: BodyKind::Plain,
        recipients: &rcpts,
        forward_of: None,
    })
    .unwrap();
}

fn list(
    s: &MessageStore,
    user: &str,
    scope: ConversationScope<'_>,
    filter: ConversationFilter,
) -> Vec<ConversationRow> {
    s.conversations(user, scope, filter, None, 200)
        .unwrap()
        .items
}

#[test]
fn unread_is_derived_from_the_authorised_seq_and_the_read_cursor() {
    let s = store();
    for i in 1..=3 {
        send(&s, "g-1", i, &format!("t{i}"), &["bob"]);
    }
    let rows = list(&s, "bob", ConversationScope::All, Default::default());
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].last_seq, rows[0].unread), (3, 3));

    s.conversation_read("bob", "phone", "g-1", 1).unwrap();
    let rows = list(&s, "bob", ConversationScope::All, Default::default());
    assert_eq!(rows[0].unread, 2, "head minus cursor");

    // a second device of the same account sees the same number: the cursor is
    // an account fact, not a per-device counter
    let rows = list(&s, "bob", ConversationScope::All, Default::default());
    assert_eq!(rows[0].unread, 2);
    assert_eq!(s.unread(owner("g-1"), "bob").unwrap(), 2);
}

#[test]
fn two_devices_marking_read_at_once_never_rewinds_the_cursor() {
    let s = store();
    for i in 1..=5 {
        send(&s, "g-1", i, &format!("t{i}"), &["bob"]);
    }
    s.read_ack("bob", "phone", "g-1", 4).unwrap();
    s.read_ack("bob", "watch", "g-1", 2).unwrap();
    assert_eq!(s.device_cursor("watch", "g-1").unwrap().1, 2);
    assert_eq!(s.unread(owner("g-1"), "bob").unwrap(), 1);
    // the stale watch ack must not have dragged the account cursor back
    s.read_ack("bob", "watch", "g-1", 5).unwrap();
    assert_eq!(s.unread(owner("g-1"), "bob").unwrap(), 0);
}

#[test]
fn scope_limits_the_page_to_conversations_the_caller_may_see() {
    let s = store();
    send(&s, "g-mine", 1, "hello", &["bob"]);
    send(&s, "g-theirs", 1, "secret", &["carol"]);
    let all = list(&s, "bob", ConversationScope::All, ConversationFilter::all());
    assert_eq!(all.len(), 2, "the projection itself is global");

    let scoped = vec!["g-mine".to_string()];
    let visible = list(
        &s,
        "bob",
        ConversationScope::Only(&scoped),
        Default::default(),
    );
    assert_eq!(
        visible
            .iter()
            .map(|r| r.conv_id.as_str())
            .collect::<Vec<_>>(),
        ["g-mine"],
        "a client list cannot enumerate someone else's conversation"
    );

    let too_many: Vec<String> = (0..600).map(|i| format!("g-{i}")).collect();
    assert!(matches!(
        s.conversations("bob", ConversationScope::Only(&too_many), Default::default(), None, 10)
            .unwrap_err(),
        MailError::InvalidInput(m) if m.contains("scope")
    ));
}

#[test]
fn filters_actually_change_the_result_set() {
    let s = store();
    for cid in ["g-1", "g-2", "g-3"] {
        send(&s, cid, 1, "x", &["bob"]);
    }
    s.set_conversation_pref("bob", "g-1", PrefField::Pinned, true)
        .unwrap();
    s.set_conversation_pref("bob", "g-2", PrefField::Muted, true)
        .unwrap();
    s.set_conversation_pref("bob", "g-3", PrefField::Archived, true)
        .unwrap();

    let ids = |rows: Vec<ConversationRow>| -> Vec<String> {
        rows.into_iter().map(|r| r.conv_id).collect()
    };
    let base = list(&s, "bob", ConversationScope::All, Default::default());
    assert_eq!(
        ids(base),
        ["g-1", "g-2"],
        "archived leaves the main list, pinned leads it"
    );
    assert_eq!(
        ids(list(
            &s,
            "bob",
            ConversationScope::All,
            ConversationFilter::archived_only()
        )),
        ["g-3"]
    );
    assert_eq!(
        ids(list(
            &s,
            "bob",
            ConversationScope::All,
            ConversationFilter {
                pinned: Some(true),
                ..Default::default()
            }
        )),
        ["g-1"]
    );
    assert_eq!(
        ids(list(
            &s,
            "bob",
            ConversationScope::All,
            ConversationFilter {
                muted: Some(true),
                archived: None,
                ..Default::default()
            }
        )),
        ["g-2"]
    );
    assert_eq!(
        ids(list(
            &s,
            "bob",
            ConversationScope::All,
            ConversationFilter::all()
        ))
        .len(),
        3
    );
}

#[test]
fn pinned_rows_lead_the_page_and_survive_the_cursor() {
    let s = store();
    for i in 1..=6 {
        send(&s, &format!("g-{i}"), 1, "x", &["bob"]);
    }
    s.set_conversation_pref("bob", "g-1", PrefField::Pinned, true)
        .unwrap();
    let page = list(&s, "bob", ConversationScope::All, Default::default());
    assert_eq!(page[0].conv_id, "g-1");
    assert!(page[0].pinned);
    assert!(!page[1].pinned, "the rest keep their time order");

    // paging past the pinned block does not lose or repeat it
    let mut seen: Vec<String> = Vec::new();
    let mut cursor = None;
    loop {
        let page = s
            .conversations(
                "bob",
                ConversationScope::All,
                ConversationFilter::all(),
                cursor.as_ref(),
                2,
            )
            .unwrap();
        seen.extend(page.items.iter().map(|r| r.conv_id.clone()));
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(seen.len(), 6);
    assert_eq!(unique.len(), 6, "no repeats across pages");
    assert_eq!(seen[0], "g-1");
}

#[test]
fn a_thousand_conversations_page_without_repeats_or_skips() {
    let s = store();
    for i in 1..=1000 {
        send(&s, &format!("g-{i:04}"), 1, "x", &["bob"]);
    }
    let mut seen: Vec<String> = Vec::new();
    let mut cursor = None;
    let mut pages = 0usize;
    loop {
        let page = s
            .conversations(
                "bob",
                ConversationScope::All,
                ConversationFilter::all(),
                cursor.as_ref(),
                200,
            )
            .unwrap();
        assert!(page.items.len() <= 200);
        seen.extend(page.items.iter().map(|r| r.conv_id.clone()));
        pages += 1;
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(pages, 5, "1000 / 200");
    assert_eq!(seen.len(), 1000);
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 1000, "no repeat, no skip");
}

#[test]
fn prefs_are_shared_across_devices_and_an_idempotent_write_does_not_bump_version() {
    let s = store();
    send(&s, "g-1", 1, "x", &["bob"]);
    let before = s.conversation_prefs("bob", "g-1").unwrap();
    assert_eq!((before.pinned, before.version), (false, 0));

    let after = s
        .set_conversation_pref("bob", "g-1", PrefField::Pinned, true)
        .unwrap();
    assert_eq!((after.pinned, after.version), (true, 1));
    // the same command again (double tap, retry) is not a new fact
    let again = s
        .set_conversation_pref("bob", "g-1", PrefField::Pinned, true)
        .unwrap();
    assert_eq!(again.version, 1, "idempotent write must not fan out a sync");
    // another device of the same account sees it without any extra push
    let other = s.conversation_prefs("bob", "g-1").unwrap();
    assert!(other.pinned);
    // another user is unaffected
    assert!(!s.conversation_prefs("carol", "g-1").unwrap().pinned);

    assert!(matches!(
        s.set_conversation_pref("bob", "g-nope", PrefField::Muted, true)
            .unwrap_err(),
        MailError::BadOwner(_)
    ));
}

#[test]
fn clear_local_history_moves_only_this_viewers_watermark() {
    let s = store();
    for i in 1..=3 {
        send(&s, "g-1", i, &format!("t{i}"), &["bob", "carol"]);
    }
    let cleared = s.clear_local_history("bob", "g-1", 2).unwrap();
    assert_eq!(cleared, 2);

    let rows = list(&s, "bob", ConversationScope::All, Default::default());
    assert_eq!(
        (rows[0].last_seq, rows[0].cleared_before_seq, rows[0].unread),
        (3, 2, 1)
    );
    // carol's view and the stored timeline are untouched
    let carol = list(&s, "carol", ConversationScope::All, Default::default());
    assert_eq!((carol[0].cleared_before_seq, carol[0].unread), (0, 3));
    let (rows, _) = s.fetch(owner("g-1"), 0, 10).unwrap();
    assert_eq!(rows.len(), 3, "clear is a view, not a delete");

    // monotonic: an older watermark cannot roll the view back
    assert_eq!(s.clear_local_history("bob", "g-1", 1).unwrap(), 2);
    // events tell the list that a local view changed, nothing else
    let events = s.conversation_events_since(0, 10).unwrap();
    assert!(events.iter().any(|e| e.kind == "conversation.cleared"));
    assert!(!events.iter().any(|e| e.kind == "conversation.deleted"));
}

#[test]
fn hide_and_delete_everywhere_are_different_commands_with_different_effects() {
    let s = store();
    send(&s, "g-1", 1, "x", &["bob", "carol"]);

    s.hide_conversation("bob", "g-1").unwrap();
    let bob = list(&s, "bob", ConversationScope::All, Default::default());
    assert!(bob.is_empty(), "gone from his list");
    let bob_all = list(&s, "bob", ConversationScope::All, ConversationFilter::all());
    assert_eq!(bob_all.len(), 1, "still there, just hidden");
    assert!(bob_all[0].hidden);
    let carol = list(&s, "carol", ConversationScope::All, Default::default());
    assert_eq!(carol.len(), 1, "carol never noticed");
    // a new message brings it back for the person who hid it
    send(&s, "g-1", 2, "y", &["bob", "carol"]);
    assert_eq!(
        list(&s, "bob", ConversationScope::All, Default::default()).len(),
        1
    );

    // now delete for everyone: a different command, a different effect
    s.delete_conversation("admin", "g-1").unwrap();
    assert!(list(
        &s,
        "carol",
        ConversationScope::All,
        ConversationFilter::all()
    )
    .is_empty());
    assert!(list(
        &s,
        "admin",
        ConversationScope::All,
        ConversationFilter::all()
    )
    .is_empty());
    // deleting twice is refused rather than quietly successful
    assert!(matches!(
        s.delete_conversation("admin", "g-1").unwrap_err(),
        MailError::BadOwner(_)
    ));
    assert!(matches!(
        s.hide_conversation("bob", "g-ghost").unwrap_err(),
        MailError::BadOwner(_)
    ));
}

#[test]
fn delete_publishes_the_domain_events_search_and_attachments_consume() {
    let s = store();
    send(&s, "g-1", 1, "x", &["bob"]);
    s.delete_conversation("admin", "g-1").unwrap();

    let events = s.conversation_events_since(0, 100).unwrap();
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    assert!(kinds.contains(&"conversation.deleted"), "{kinds:?}");
    assert!(kinds.contains(&"search.index.remove"), "{kinds:?}");
    assert!(kinds.contains(&"attachment.release"), "{kinds:?}");
    assert!(events.iter().all(|e| e.conv_id == "g-1"));
    assert!(events.iter().all(|e| e.actor == "admin"));

    // the feed is an append-only id cursor: two consumers, same rows, no gaps
    let late = s.conversation_events_since(1, 100).unwrap();
    assert_eq!(late.len(), events.len() - 1);
    assert_eq!(late[0].id, events[1].id);
    assert!(s.conversation_events_since(10_000, 10).unwrap().is_empty());
    // limit is honoured so a slow consumer cannot take the feed down
    assert_eq!(s.conversation_events_since(0, 1).unwrap().len(), 1);
}

#[test]
fn a_recalled_head_shows_a_placeholder_and_never_the_original_body() {
    let s = store();
    send(&s, "g-1", 1, "the original words", &["bob"]);
    assert_eq!(
        list(&s, "bob", ConversationScope::All, Default::default())[0].preview,
        "the original words"
    );

    s.recall_message(owner("g-1"), "g-1-m1", "alice").unwrap();
    let row = &list(&s, "bob", ConversationScope::All, Default::default())[0];
    assert_eq!(
        row.preview, "",
        "recalled body must not be served from the list"
    );
    assert!(row.last_withdrawn);
    assert_eq!(row.last_seq, 1, "the placeholder keeps its position");

    // the next message replaces the placeholder as the head
    send(&s, "g-1", 2, "after", &["bob"]);
    let row = &list(&s, "bob", ConversationScope::All, Default::default())[0];
    assert_eq!(
        (row.preview.as_str(), row.last_withdrawn, row.last_seq),
        ("after", false, 2)
    );
}

#[test]
fn sealed_bodies_never_reach_the_list_preview() {
    let s = store();
    let rcpts = vec![SubmitRecipient::member("bob")];
    let mut sealed = SubmitRequest {
        owner: owner("g-1"),
        sender: "alice",
        sender_device: "dev-a",
        idempotency_key: "k1",
        msg_id: "s1",
        type_id: "text",
        payload: "==opaque-sealed-envelope==",
        body_kind: BodyKind::Ciphertext,
        recipients: &rcpts,
        forward_of: None,
    };
    s.submit(&sealed).unwrap();
    let row = &list(&s, "bob", ConversationScope::All, Default::default())[0];
    assert_eq!(
        row.preview, "",
        "no plaintext and no ciphertext in the list"
    );
    assert!(!row.last_withdrawn, "sealed is not the same as recalled");

    sealed.body_kind = BodyKind::Plain;
    sealed.idempotency_key = "k2";
    sealed.msg_id = "p1";
    sealed.payload = "plain words";
    s.submit(&sealed).unwrap();
    assert_eq!(
        list(&s, "bob", ConversationScope::All, Default::default())[0].preview,
        "plain words"
    );
}

#[test]
fn group_change_notices_are_ordinary_timeline_rows_in_the_list() {
    let s = store();
    let rcpts = vec![SubmitRecipient::member("bob")];
    s.submit(&SubmitRequest {
        owner: owner("g-1"),
        sender: "system",
        sender_device: "server",
        idempotency_key: "notice-1",
        msg_id: "n1",
        type_id: "system",
        payload: "{\"text\":\"carol joined\"}",
        body_kind: BodyKind::Plain,
        recipients: &rcpts,
        forward_of: None,
    })
    .unwrap();
    let row = &list(&s, "bob", ConversationScope::All, Default::default())[0];
    assert_eq!((row.last_type.as_str(), row.unread), ("system", 1));
    assert_eq!(row.preview, "{\"text\":\"carol joined\"}");
}

#[test]
fn member_count_is_maintained_without_disturbing_the_message_head() {
    let s = store();
    send(&s, "g-1", 1, "x", &["bob"]);
    s.set_member_count(owner("g-1"), 7).unwrap();
    let row = &list(&s, "bob", ConversationScope::All, Default::default())[0];
    assert_eq!(row.member_count, 7);
    assert_eq!(row.last_seq, 1, "a head count is not a message");
    // a later message does not lose the count
    send(&s, "g-1", 2, "y", &["bob"]);
    let row = &list(&s, "bob", ConversationScope::All, Default::default())[0];
    assert_eq!((row.member_count, row.last_seq), (7, 2));
}

#[test]
fn read_and_limit_boundaries_are_enforced_not_silently_wrong() {
    let s = store();
    send(&s, "g-1", 1, "x", &["bob"]);
    // zero and absurd limits are clamped, never "give me everything"
    let page = s
        .conversations("bob", ConversationScope::All, Default::default(), None, 0)
        .unwrap();
    assert_eq!(page.items.len(), 1);
    let page = s
        .conversations(
            "bob",
            ConversationScope::All,
            Default::default(),
            None,
            100_000,
        )
        .unwrap();
    assert!(page.items.len() <= 200);
    // an unknown conversation has no prefs to read back
    assert_eq!(
        s.conversation_prefs("bob", "g-none").unwrap(),
        Default::default()
    );
    // reading past the head is refused by the cursor, not accepted as success
    s.conversation_read("bob", "phone", "g-none", 5).unwrap();
    assert_eq!(s.device_cursor("phone", "g-none").unwrap().0, 0);
}
