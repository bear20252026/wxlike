//! WP-03 A1: envelope v2 wire compatibility, msg_id dedup, recall, receipts,
//! quoting and per-device cursors.

use serde::{Deserialize, Serialize};
use wxlike_core::store::Store;
use wxlike_core::types::{
    custom_kind, Envelope, MsgBody, MsgId, MsgKind, Receipt, NO_MSG_ID, PROTOCOL_VERSION,
};

/// The exact version-1 layout, rebuilt here so the test proves the *old* wire
/// format is readable — not just that our own encoder round-trips.
#[derive(Serialize, Deserialize)]
struct EnvelopeV1 {
    version: u8,
    conv_id: String,
    sender: String,
    to: Vec<String>,
    seq: u64,
    kind: MsgKind,
    body: MsgBodyV1,
    sent_at_ms: u64,
}

#[derive(Serialize, Deserialize)]
enum MsgBodyV1 {
    Text(String),
    Bytes(Vec<u8>),
    Notice(String),
}

fn store_with(conv: &str) -> Store {
    let s = Store::open(None).unwrap();
    s.upsert_conversation(conv, "t").unwrap();
    s
}

#[test]
fn v2_roundtrip_carries_the_new_fields() {
    let e = Envelope::direct("c1", "alice", vec!["bob".into()], 3, "hello");
    let bytes = e.to_bytes().unwrap();
    let back = Envelope::from_bytes(&bytes).unwrap();
    assert_eq!(back, e);
    assert_eq!(back.version, PROTOCOL_VERSION);
    assert_ne!(back.msg_id, NO_MSG_ID, "every new message gets a dedup id");
}

#[test]
fn version_one_bytes_still_decode() {
    let legacy = EnvelopeV1 {
        version: 1,
        conv_id: "c1".into(),
        sender: "alice".into(),
        to: vec!["bob".into()],
        seq: 7,
        kind: MsgKind::Text,
        body: MsgBodyV1::Text("from the old build".into()),
        sent_at_ms: 1234,
    };
    let bytes = bincode::serialize(&legacy).unwrap();
    let up = Envelope::from_bytes(&bytes).unwrap();
    assert_eq!(up.version, 1, "the original version marker is kept");
    assert_eq!(up.msg_id, NO_MSG_ID, "legacy messages have no dedup key");
    assert_eq!(up.quote_of, None);
    assert_eq!(up.receipt, None);
    assert_eq!(up.body, MsgBody::Text("from the old build".into()));
    assert_eq!(up.to, vec!["bob".to_string()]);
    assert_eq!(up.seq, 7);
    assert_eq!(up.sent_at_ms, 1234);
}

#[test]
fn two_legacy_messages_can_both_be_stored() {
    // msg_id is NULL for v1 rows, and the partial unique index skips NULLs.
    let s = store_with("c1");
    let mk = |text: &str| Envelope {
        msg_id: NO_MSG_ID,
        ..Envelope::text("c1", "alice", 1, text)
    };
    assert!(s.append(&mk("one"), false).unwrap().is_some());
    assert!(s.append(&mk("two"), false).unwrap().is_some());
    assert_eq!(s.list_messages("c1").unwrap().len(), 2);
}

#[test]
fn duplicate_msg_id_is_dropped_without_error() {
    let s = store_with("c1");
    let e = Envelope::text("c1", "alice", 1, "only once");
    assert!(s.append(&e, false).unwrap().is_some(), "first insert");
    assert!(
        s.append(&e, false).unwrap().is_none(),
        "re-delivery is dropped silently"
    );
    assert_eq!(s.list_messages("c1").unwrap().len(), 1);
}

#[test]
fn ten_concurrent_duplicates_store_exactly_once() {
    // A3: the same msg_id racing in from several tasks lands once.
    let s = std::sync::Arc::new(store_with("c1"));
    let e = std::sync::Arc::new(Envelope::text("c1", "alice", 1, "racy"));
    let pool = (0..10)
        .map(|_| {
            let (s, e) = (s.clone(), e.clone());
            std::thread::spawn(move || s.append(&e, false))
        })
        .collect::<Vec<_>>();
    let stored = pool
        .into_iter()
        .map(|h| h.join().unwrap().unwrap().is_some())
        .filter(|first| *first)
        .count();
    assert_eq!(stored, 1, "exactly one insert wins");
    assert_eq!(s.list_messages("c1").unwrap().len(), 1);
}

#[test]
fn recall_marks_the_target_and_hides_the_original_body() {
    let s = store_with("c1");
    let e = Envelope::text("c1", "alice", 1, "the secret");
    let target = e.msg_id;
    s.append(&e, false).unwrap();

    assert!(s.withdraw(&target, "alice").unwrap(), "author may recall");
    let msgs = s.list_messages("c1").unwrap();
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].withdrawn);
    assert_eq!(
        msgs[0].envelope.body,
        MsgBody::Notice(MsgBody::RECALLED_TEXT.to_string())
    );
    let leaked = format!("{:?}", msgs[0].envelope);
    assert!(
        !leaked.contains("the secret"),
        "original text must not escape"
    );
    assert_eq!(msgs[0].envelope.msg_id, target, "id survives for the UI");
}

#[test]
fn recall_of_another_senders_message_is_ignored() {
    // A3: forged recall neither errors nor mutates the row.
    let s = store_with("c1");
    let e = Envelope::text("c1", "alice", 1, "mine");
    s.append(&e, false).unwrap();
    assert!(!s.withdraw(&e.msg_id, "mallory").unwrap());
    let msgs = s.list_messages("c1").unwrap();
    assert!(!msgs[0].withdrawn);
    assert_eq!(msgs[0].envelope.body, MsgBody::Text("mine".into()));
    // unknown id is also just "false"
    assert!(!s.withdraw(&[9u8; 16], "alice").unwrap());
    // and recalling twice is not a second change
    assert!(s.withdraw(&e.msg_id, "alice").unwrap());
    assert!(!s.withdraw(&e.msg_id, "alice").unwrap());
}

#[test]
fn sender_of_resolves_the_receipt_destination() {
    let s = store_with("c1");
    let a = Envelope::text("c1", "alice", 1, "hi");
    let b = Envelope::text("c1", "bob", 2, "yo");
    s.append(&a, false).unwrap();
    s.append(&b, false).unwrap();
    assert_eq!(s.sender_of(&a.msg_id).unwrap().as_deref(), Some("alice"));
    assert_eq!(s.sender_of(&b.msg_id).unwrap().as_deref(), Some("bob"));
    assert_eq!(s.sender_of(&[7u8; 16]).unwrap(), None);
}

#[test]
fn quote_reference_is_stored_and_returned() {
    let s = store_with("c1");
    let first = Envelope::text("c1", "alice", 1, "original");
    let reply = Envelope::text("c1", "bob", 2, "re: original").quoted(first.msg_id);
    s.append(&first, false).unwrap();
    s.append(&reply, false).unwrap();
    let msgs = s.list_messages("c1").unwrap();
    assert_eq!(msgs[1].envelope.quote_of, Some(first.msg_id));
    let wire = Envelope::from_bytes(&reply.to_bytes().unwrap()).unwrap();
    assert_eq!(wire.quote_of, Some(first.msg_id));
}

#[test]
fn recall_envelope_describes_its_target() {
    let e = Envelope::recall("c1", "alice", [42u8; 16]);
    assert!(e.is_recall());
    assert_eq!(e.refers_to(), Some([42u8; 16] as MsgId));
    let back = Envelope::from_bytes(&e.to_bytes().unwrap()).unwrap();
    assert_eq!(back.body, MsgBody::Recall { target: [42u8; 16] });
}

#[test]
fn receipt_builder_targets_the_original_author() {
    let msg = Envelope::direct("c1", "alice", vec!["bob".into()], 5, "read me");
    let ack = msg.receipt_from(
        "bob",
        Receipt::Read {
            up_to_msg_id: msg.msg_id,
        },
    );
    assert_eq!(ack.sender, "bob", "the ack is authored by the reader");
    assert_eq!(
        ack.to,
        vec!["alice".to_string()],
        "and addressed back to the author"
    );
    assert_eq!(
        ack.receipt,
        Some(Receipt::Read {
            up_to_msg_id: msg.msg_id
        })
    );
    assert_eq!(ack.refers_to(), Some(msg.msg_id));
    assert_eq!(ack.kind, MsgKind::System);

    let delivered = msg.receipt_from("bob", Receipt::Delivered);
    assert_eq!(delivered.receipt, Some(Receipt::Delivered));
    assert_eq!(
        delivered.refers_to(),
        Some(msg.msg_id),
        "falls back to quote_of"
    );
    let back = Envelope::from_bytes(&delivered.to_bytes().unwrap()).unwrap();
    assert_eq!(back.receipt, Some(Receipt::Delivered));
    assert_eq!(back.sender, "bob");
}

#[test]
fn custom_body_carries_structured_payloads() {
    let e = Envelope::with_body(
        "c1",
        "alice",
        vec!["bob".into()],
        1,
        MsgBody::Custom {
            kind: custom_kind::MENTION.to_string(),
            payload: serde_json::json!({"user_ids": ["bob"], "text": "ping"}),
        },
    );
    let back = Envelope::from_bytes(&e.to_bytes().unwrap()).unwrap();
    assert_eq!(back, e);
    match &back.body {
        MsgBody::Custom { kind, payload } => {
            assert_eq!(kind, "mention");
            assert_eq!(payload["user_ids"][0], "bob");
        }
        other => panic!("expected Custom, got {other:?}"),
    }
}

#[test]
fn custom_body_survives_storage() {
    let s = store_with("c1");
    let e = Envelope::with_body(
        "c1",
        "alice",
        vec!["bob".into()],
        1,
        MsgBody::Custom {
            kind: custom_kind::WALLET_REDPACKET.to_string(),
            payload: serde_json::json!({"redpacket_id": "rp1", "amount": 100}),
        },
    );
    s.append(&e, false).unwrap();
    let got = s.list_messages("c1").unwrap();
    match &got[0].envelope.body {
        MsgBody::Custom { kind, payload } => {
            assert_eq!(kind, "wallet.redpacket");
            assert_eq!(payload["redpacket_id"], "rp1");
            assert_eq!(payload["amount"], 100);
        }
        other => panic!("expected Custom, got {other:?}"),
    }
}

#[test]
fn msg_ids_are_distinct_across_many_messages() {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..5_000 {
        assert!(seen.insert(Envelope::new_msg_id()));
    }
}

#[test]
fn has_message_reports_membership() {
    let s = store_with("c1");
    let e = Envelope::text("c1", "alice", 1, "x");
    assert!(!s.has_message(&e.msg_id).unwrap());
    s.append(&e, false).unwrap();
    assert!(s.has_message(&e.msg_id).unwrap());
}
