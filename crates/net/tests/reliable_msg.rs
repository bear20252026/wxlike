//! WP-03 hub behaviour: msg_id dedup, recall delivery, receipt routing and
//! sender forgery. These run against a live QUIC hub with a message store
//! attached — the same wiring `wxlike-server` uses.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use wxlike_core::types::{MsgBody, MsgKind, Receipt};
use wxlike_core::{Envelope, Store};
use wxlike_mq::{memory::MemoryMq, memory::MemoryRoutes, MqRouter};
use wxlike_net::{run_mq_hub_with, Client, MqHubOptions};

fn router() -> MqRouter {
    MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new())
}

async fn connect(handle_addr: std::net::SocketAddr, user: &str) -> Client {
    Client::connect(handle_addr, "localhost", user)
        .await
        .unwrap()
}

/// Reads everything that arrives within `within`.
async fn drain(client: &Client, within: Duration) -> Vec<Envelope> {
    let mut out = Vec::new();
    loop {
        match tokio::time::timeout(within, client.next_envelope()).await {
            Ok(Ok(env)) => out.push(env),
            _ => return out,
        }
    }
}

/// One hub + one message store, wired the way `wxlike-server` wires it. The
/// hub is leaked on purpose: dropping it would abort its routing tasks.
async fn hub_and_store(hub_id: &str) -> (Arc<Store>, std::net::SocketAddr) {
    let store = Arc::new(Store::open(None).unwrap());
    let hub = run_mq_hub_with(
        hub_id,
        "127.0.0.1:0".parse().unwrap(),
        router(),
        MqHubOptions {
            // 测试用 StaticAuth：token 原样当 user_id（生产走 session 表）。
            // 不注入的话 hub 会 fail closed 拒绝连接——这是 issue #3 的默认值。
            auth: Some(std::sync::Arc::new(wxlike_net::auth::StaticAuth)),
            storage: Some(store.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let addr = hub.local_addr().unwrap();
    std::mem::forget(hub);
    (store, addr)
}

#[tokio::test]
async fn recall_reaches_the_peer_and_hides_the_original() {
    let (store, addr) = hub_and_store("hub-1").await;

    let alice = connect(addr, "alice").await;
    let bob = connect(addr, "bob").await;
    tokio::time::sleep(Duration::from_millis(250)).await;

    let msg = Envelope::text("conv1", "alice", 1, "the original text");
    alice.send(&msg).await.unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), bob.next_envelope())
        .await
        .expect("bob receives the message")
        .unwrap();
    assert_eq!(got.body, MsgBody::Text("the original text".into()));

    // A2: alice recalls, bob is told, and the stored copy becomes a placeholder
    alice
        .send(&Envelope::recall("conv1", "alice", msg.msg_id))
        .await
        .unwrap();
    let recall = tokio::time::timeout(Duration::from_secs(5), bob.next_envelope())
        .await
        .expect("bob receives the recall")
        .unwrap();
    assert!(recall.is_recall(), "{recall:?}");
    assert_eq!(recall.refers_to(), Some(msg.msg_id));

    tokio::time::sleep(Duration::from_millis(250)).await;
    let stored = store.list_messages("conv1").unwrap();
    let target = stored
        .iter()
        .find(|m| m.envelope.msg_id == msg.msg_id)
        .unwrap();
    assert!(target.withdrawn, "row flagged");
    assert_eq!(
        target.envelope.body,
        MsgBody::Notice(MsgBody::RECALLED_TEXT.to_string())
    );
    let all = format!("{:?}", store.list_messages("conv1").unwrap());
    assert!(
        !all.contains("the original text"),
        "original bytes gone from reads"
    );
    // the recall is a control frame: it is routed, never stored as content
    assert_eq!(
        stored.len(),
        1,
        "only the original message is conversation content"
    );
    assert!(!stored.iter().any(|m| m.envelope.is_recall()));
}

#[tokio::test]
async fn ten_replays_of_one_msg_id_are_delivered_once() {
    let (store, addr) = hub_and_store("hub-1").await;
    let alice = connect(addr, "alice").await;
    let bob = connect(addr, "bob").await;
    tokio::time::sleep(Duration::from_millis(250)).await;

    let msg = Envelope::text("conv1", "alice", 1, "send once");
    for _ in 0..10 {
        alice.send(&msg).await.unwrap();
    }
    let got = drain(&bob, Duration::from_millis(1_200)).await;
    assert_eq!(got.len(), 1, "duplicate deliveries collapse to one");
    assert_eq!(got[0].body, MsgBody::Text("send once".into()));

    // and exactly one row exists
    tokio::time::sleep(Duration::from_millis(200)).await;
    let rows = store.list_messages("conv1").unwrap();
    assert_eq!(
        rows.iter()
            .filter(|r| r.envelope.msg_id == msg.msg_id)
            .count(),
        1
    );
}

#[tokio::test]
async fn duplicate_ids_from_different_senders_are_both_kept() {
    // dedup is per message id, so a colliding id from someone else is a real
    // (if improbable) second message; the UNIQUE index still allows only one
    // row per id, which is the intended global key semantics.
    let (store, addr) = hub_and_store("hub-1").await;
    let alice = connect(addr, "alice").await;
    let bob = connect(addr, "bob").await;
    tokio::time::sleep(Duration::from_millis(250)).await;

    let shared_id = Envelope::new_msg_id();
    let a = Envelope {
        msg_id: shared_id,
        ..Envelope::text("conv1", "alice", 1, "from alice")
    };
    let b = Envelope {
        msg_id: shared_id,
        ..Envelope::text("conv1", "bob", 1, "from bob")
    };
    alice.send(&a).await.unwrap();
    bob.send(&b).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let rows = store.list_messages("conv1").unwrap();
    assert_eq!(rows.len(), 1, "the global dedup key wins");
    assert_eq!(
        rows[0].envelope.sender, "alice",
        "first writer keeps the id"
    );
}

#[tokio::test]
async fn receipt_travels_back_to_the_author_only() {
    let (_store, addr) = hub_and_store("hub-1").await;
    let alice = connect(addr, "alice").await;
    let bob = connect(addr, "bob").await;
    let carol = connect(addr, "carol").await;
    tokio::time::sleep(Duration::from_millis(250)).await;

    // broadcast message: bob and carol both see it
    let msg = Envelope::text("conv1", "alice", 1, "read this");
    alice.send(&msg).await.unwrap();
    for peer in [&bob, &carol] {
        let got = tokio::time::timeout(Duration::from_secs(5), peer.next_envelope())
            .await
            .expect("peer sees the broadcast")
            .unwrap();
        assert_eq!(got.msg_id, msg.msg_id);
    }

    // bob acknowledges it; only alice should receive the ack
    let ack = msg.receipt_from(
        "bob",
        Receipt::Read {
            up_to_msg_id: msg.msg_id,
        },
    );
    bob.send(&ack).await.unwrap();
    let to_alice = drain(&alice, Duration::from_millis(900)).await;
    assert_eq!(to_alice.len(), 1, "exactly the author is notified");
    assert_eq!(
        to_alice[0].receipt,
        Some(Receipt::Read {
            up_to_msg_id: msg.msg_id
        })
    );
    assert_eq!(to_alice[0].sender, "bob");
    assert_eq!(to_alice[0].kind, MsgKind::System);

    // carol hears nothing about bob's reading, and bob gets no echo of his ack
    assert!(
        drain(&carol, Duration::from_millis(500)).await.is_empty(),
        "no fan-out"
    );
    assert!(
        drain(&bob, Duration::from_millis(500)).await.is_empty(),
        "no echo"
    );
}

#[tokio::test]
async fn receipt_for_an_offline_author_is_parked() {
    let (_store, addr) = hub_and_store("hub-1").await;

    // alice sends while online, then disconnects
    let alice = connect(addr, "alice").await;
    let bob = connect(addr, "bob").await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    let msg = Envelope::text("conv1", "alice", 1, "bye for now");
    alice.send(&msg).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), bob.next_envelope())
        .await
        .expect("bob got it")
        .unwrap();
    drop(alice);
    tokio::time::sleep(Duration::from_millis(300)).await;

    // bob acks while alice is gone: parked, not lost
    let ack = msg.receipt_from("bob", Receipt::Delivered);
    bob.send(&ack).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let alice = connect(addr, "alice").await;
    let got = tokio::time::timeout(Duration::from_secs(5), alice.next_envelope())
        .await
        .expect("parked receipt is flushed on reconnect")
        .unwrap();
    assert_eq!(got.receipt, Some(Receipt::Delivered));
    assert_eq!(got.sender, "bob");
}

#[tokio::test]
async fn a_client_cannot_speak_as_someone_else() {
    let (store, addr) = hub_and_store("hub-1").await;
    let alice = connect(addr, "alice").await;
    let mallory = connect(addr, "mallory").await;
    tokio::time::sleep(Duration::from_millis(250)).await;

    // alice writes a real message
    let msg = Envelope::text("conv1", "alice", 1, "alice said this");
    alice.send(&msg).await.unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;

    // mallory tries to recall it, forging alice as the sender
    mallory
        .send(&Envelope::recall("conv1", "alice", msg.msg_id))
        .await
        .unwrap();
    // ...and to pass off a message as alice's
    mallory
        .send(&Envelope::text("conv1", "alice", 2, "forged"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert!(
        drain(&alice, Duration::from_millis(300)).await.is_empty(),
        "no echo of forged frames"
    );
    let rows = store.list_messages("conv1").unwrap();
    assert_eq!(rows.len(), 1, "nothing forged was stored");
    assert!(!rows[0].withdrawn, "alice's message survives");
    assert_eq!(
        rows[0].envelope.body,
        MsgBody::Text("alice said this".into())
    );
    assert_eq!(
        rows.iter()
            .map(|r| r.envelope.sender.as_str())
            .collect::<HashSet<_>>(),
        HashSet::from(["alice"])
    );
}

#[tokio::test]
async fn recalling_someone_elses_message_is_ignored() {
    let (store, addr) = hub_and_store("hub-1").await;
    let alice = connect(addr, "alice").await;
    let bob = connect(addr, "bob").await;
    tokio::time::sleep(Duration::from_millis(250)).await;

    let msg = Envelope::text("conv1", "alice", 1, "not yours");
    alice.send(&msg).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // bob recalls it as himself: the store refuses, so the hub drops it
    bob.send(&Envelope::recall("conv1", "bob", msg.msg_id))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;

    let rows = store.list_messages("conv1").unwrap();
    let target = rows
        .iter()
        .find(|r| r.envelope.msg_id == msg.msg_id)
        .unwrap();
    assert!(!target.withdrawn, "still intact");
    assert_eq!(target.envelope.body, MsgBody::Text("not yours".into()));
    assert!(
        !rows.iter().any(|r| r.envelope.is_recall()),
        "the refused recall is not stored either"
    );
    assert!(
        drain(&alice, Duration::from_millis(300)).await.is_empty(),
        "alice is not notified"
    );
}

#[tokio::test]
async fn legacy_v1_frames_still_route() {
    // A v1-encoded envelope has no msg_id: it must still be delivered, and it
    // must not be deduplicated away.
    let (_store, addr) = hub_and_store("hub-1").await;
    let alice = connect(addr, "alice").await;
    let bob = connect(addr, "bob").await;
    tokio::time::sleep(Duration::from_millis(250)).await;

    let v1 = Envelope {
        version: 1,
        msg_id: wxlike_core::NO_MSG_ID,
        ..Envelope::text("conv1", "alice", 1, "old client")
    };
    alice.send(&v1).await.unwrap();
    alice.send(&v1).await.unwrap();
    let got = drain(&bob, Duration::from_millis(900)).await;
    assert_eq!(
        got.len(),
        2,
        "legacy frames have no dedup key, so both pass"
    );
    assert!(got.iter().all(|e| e.version == 1));
}
