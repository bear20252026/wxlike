//! P1 transient typing channel: online-only delivery, never persisted,
//! never parked. These three properties are the transitional form of the
//! P3 `high_priority` channel (ADR-17).

use std::time::Duration;

use wxlike_core::custom_kind;
use wxlike_core::{Envelope, MsgBody, Store};
use wxlike_mq::memory::{MemoryMq, MemoryRoutes};
use wxlike_mq::MqRouter;
use wxlike_net::{run_mq_hub_with, Client, MqHubOptions};

async fn stack() -> (
    wxlike_net::MqHub,
    std::net::SocketAddr,
    std::sync::Arc<Store>,
) {
    let router = MqRouter::new(std::sync::Arc::new(MemoryMq::new()), MemoryRoutes::new());
    let store = std::sync::Arc::new(Store::open(None).unwrap());
    let hub = run_mq_hub_with(
        "typing-test",
        "127.0.0.1:0".parse().unwrap(),
        router,
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
    (hub, addr, store)
}

fn typing(conv: &str, from: &str, to: Vec<String>) -> Envelope {
    Envelope::with_body(
        conv,
        from,
        to,
        0,
        MsgBody::Custom {
            kind: custom_kind::TYPING.into(),
            payload: serde_json::json!({ "conv_id": conv }),
        },
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typing_reaches_the_online_peer() {
    let (hub, addr, _store) = stack().await;
    let alice = Client::connect(addr, "localhost", "alice").await.unwrap();
    let bob = Client::connect(addr, "localhost", "bob").await.unwrap();

    // `Client::connect` returns after writing Hello, but the hub binds the
    // stream asynchronously. A typing frame sent before bob is bound is
    // *correctly* dropped (transient signals are never parked), so wait until
    // the hub sees both connections — the real-world equivalent is that every
    // keystroke re-sends the indicator, so a lost early ping is by design.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while hub.handle().online_users().await < 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "hub never bound both users"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    alice
        .send(&typing("d-alice~bob", "alice", vec!["bob".into()]))
        .await
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(15), bob.next_envelope())
        .await
        .expect("typing arrives while online")
        .unwrap();
    assert!(ev.is_typing());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typing_is_never_persisted() {
    let (_hub, addr, store) = stack().await;
    let alice = Client::connect(addr, "localhost", "alice").await.unwrap();

    // a content message plus a typing ping
    alice
        .send(&Envelope::direct(
            "d-alice~bob",
            "alice",
            vec!["bob".into()],
            1,
            "real",
        ))
        .await
        .unwrap();
    alice
        .send(&typing("d-alice~bob", "alice", vec!["bob".into()]))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let rows = store.list_messages("d-alice~bob").unwrap();
    assert_eq!(rows.len(), 1, "only the content message is stored");
    assert!(!rows[0].envelope.is_typing());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typing_is_not_parked_for_offline_recipients() {
    let (hub, addr, store) = stack().await;
    // alice connects, bob does not: a typing ping to bob must vanish
    let alice = Client::connect(addr, "localhost", "alice").await.unwrap();
    alice
        .send(&typing("d-alice~bob", "alice", vec!["bob".into()]))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // bob now connects: the offline drain must carry nothing
    let bob = Client::connect(addr, "localhost", "bob").await.unwrap();
    let parked = tokio::time::timeout(Duration::from_millis(500), bob.next_envelope()).await;
    assert!(
        parked.is_err(),
        "no typing frame was parked while bob was away"
    );

    // and the store agrees
    assert!(store.list_messages("d-alice~bob").unwrap().is_empty());
    drop(hub);
}
