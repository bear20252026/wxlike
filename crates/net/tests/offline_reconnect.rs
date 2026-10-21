//! WP-10 A3 + durability: an offline recipient is parked, survives a hub
//! restart, and is flushed on the next Hello; and a cross-node hop through an
//! ack-capable queue leaves no double delivery.

use std::sync::Arc;
use std::time::Duration;

use wxlike_core::{Envelope, MsgBody};
use wxlike_mq::memory::{MemoryMq, MemoryReliableMq, MemoryRoutes};
use wxlike_mq::{MqRouter, ReliableQueue};
use wxlike_net::offline::{MemoryOffline, OfflineStore, SqliteOffline};
use wxlike_net::{run_mq_hub_with, Client, MqHub, MqHubOptions};

fn temp_db(tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!("wxlike-offline-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("inbox.db").to_str().unwrap().to_string()
}

async fn hub(offline: Arc<dyn wxlike_net::offline::OfflineStore>) -> (MqHub, std::net::SocketAddr) {
    let hub = run_mq_hub_with(
        "hub-1",
        "127.0.0.1:0".parse().unwrap(),
        MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new()),
        MqHubOptions {
            // 测试用 StaticAuth：token 原样当 user_id（生产走 session 表）。
            // 不注入的话 hub 会 fail closed 拒绝连接——这是 issue #3 的默认值。
            auth: Some(std::sync::Arc::new(wxlike_net::auth::StaticAuth)),
            offline,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let addr = hub.local_addr().unwrap();
    (hub, addr)
}

#[tokio::test]
async fn parked_message_is_flushed_on_hello() {
    let offline = MemoryOffline::new(500);
    let (hub, addr) = hub(offline.clone()).await;

    let alice = Client::connect(addr, "localhost", "alice").await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // bob has never connected anywhere
    alice
        .send(&Envelope::direct(
            "dm",
            "alice",
            vec!["bob".into()],
            1,
            "were you around?",
        ))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        offline.depth("bob").await.unwrap(),
        1,
        "parked, not dropped"
    );

    let bob = Client::connect(addr, "localhost", "bob").await.unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), bob.next_envelope())
        .await
        .expect("parked frame arrives right after Hello")
        .unwrap();
    assert_eq!(got.body, MsgBody::Text("were you around?".into()));
    assert_eq!(offline.depth("bob").await.unwrap(), 0, "drain empties");
    drop(hub);
}

#[tokio::test]
async fn parked_messages_survive_a_hub_restart() {
    // the durability claim, end to end: write through one hub process, read
    // through a *different* store instance on the same file
    let path = temp_db("restart");

    let first = SqliteOffline::open(Some(&path), 500).unwrap();
    let (hub_a, addr_a) = hub(first.clone()).await;
    let alice = Client::connect(addr_a, "localhost", "alice").await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    for i in 0..3u8 {
        alice
            .send(&Envelope::direct(
                "dm",
                "alice",
                vec!["bob".into()],
                i as u64,
                format!("m{i}"),
            ))
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(first.depth("bob").await.unwrap(), 3);
    drop(alice);
    drop(hub_a); // process gone

    // brand new store connection over the same file, brand new hub
    let reopened = SqliteOffline::open(Some(&path), 500).unwrap();
    assert_eq!(
        reopened.depth("bob").await.unwrap(),
        3,
        "parked frames are on disk"
    );
    let (hub_b, addr_b) = hub(reopened.clone()).await;
    let bob = Client::connect(addr_b, "localhost", "bob").await.unwrap();

    let mut got = Vec::new();
    for _ in 0..3 {
        got.push(
            tokio::time::timeout(Duration::from_secs(5), bob.next_envelope())
                .await
                .expect("all three parked frames replay")
                .unwrap(),
        );
    }
    let texts: Vec<String> = got
        .iter()
        .map(|e| match &e.body {
            MsgBody::Text(t) => t.clone(),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(texts, vec!["m0", "m1", "m2"], "FIFO order preserved");
    assert_eq!(reopened.depth("bob").await.unwrap(), 0);
    drop(hub_b);
    std::fs::remove_dir_all(std::path::Path::new(&path).parent().unwrap()).ok();
}

#[tokio::test]
async fn sqlite_cap_evicts_oldest_across_reopen() {
    let path = temp_db("cap");
    {
        let store = SqliteOffline::open(Some(&path), 2).unwrap();
        for i in 0..4u8 {
            store.enqueue("bob", vec![b'f', i]).await.unwrap();
        }
    }
    let store = SqliteOffline::open(Some(&path), 2).unwrap();
    let frames = store.drain("bob").await.unwrap();
    assert_eq!(
        frames,
        vec![vec![b'f', 2], vec![b'f', 3]],
        "newest two survive"
    );
    std::fs::remove_dir_all(std::path::Path::new(&path).parent().unwrap()).ok();
}

#[tokio::test]
async fn cross_hub_delivery_acks_the_queue() {
    // Two hubs on one reliable queue: the reading hub must ack after delivery,
    // otherwise the entry would be reclaimed and delivered twice.
    let queue = MemoryReliableMq::new("cluster");
    // one route table shared by both hubs, otherwise each hub would only see
    // its own bindings and park everything
    let routes = MemoryRoutes::new();
    let router_a = MqRouter::with_reliable(queue.clone(), routes.clone(), queue.clone());
    let router_b = MqRouter::with_reliable(queue.clone(), routes.clone(), queue.clone());

    let hub_a = run_mq_hub_with(
        "hub-a",
        "127.0.0.1:0".parse().unwrap(),
        router_a,
        MqHubOptions {
            // 测试用 StaticAuth：token 原样当 user_id（生产走 session 表）。
            // 不注入的话 hub 会 fail closed 拒绝连接——这是 issue #3 的默认值。
            auth: Some(std::sync::Arc::new(wxlike_net::auth::StaticAuth)),
            reclaim_idle_ms: Some(60_000),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let hub_b = run_mq_hub_with(
        "hub-b",
        "127.0.0.1:0".parse().unwrap(),
        router_b,
        MqHubOptions {
            // 测试用 StaticAuth：token 原样当 user_id（生产走 session 表）。
            // 不注入的话 hub 会 fail closed 拒绝连接——这是 issue #3 的默认值。
            auth: Some(std::sync::Arc::new(wxlike_net::auth::StaticAuth)),
            reclaim_idle_ms: Some(60_000),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let alice = Client::connect(hub_a.local_addr().unwrap(), "localhost", "alice")
        .await
        .unwrap();
    let bob = Client::connect(hub_b.local_addr().unwrap(), "localhost", "bob")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    alice
        .send(&Envelope::direct(
            "dm",
            "alice",
            vec!["bob".into()],
            1,
            "over the queue",
        ))
        .await
        .unwrap();

    let got = tokio::time::timeout(Duration::from_secs(5), bob.next_envelope())
        .await
        .expect("cross-node delivery")
        .unwrap();
    assert_eq!(got.body, MsgBody::Text("over the queue".into()));

    // give the consumer loop a moment to ack
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        queue.pending_count("wxlike:msg:hub:hub-b").await.unwrap(),
        0,
        "delivered entries are acknowledged, not left pending"
    );
    // and nothing further is redelivered
    assert!(
        tokio::time::timeout(Duration::from_millis(600), bob.next_envelope())
            .await
            .is_err(),
        "no duplicate delivery after the ack"
    );
}
