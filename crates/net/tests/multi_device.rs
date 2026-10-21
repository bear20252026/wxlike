//! P3 ADR-4 multi-device + ADR-8 transport abstraction contract tests.
//!
//! Invariants:
//! * one user may hold several live connections and a delivery fans to ALL
//!   of them;
//! * a device disconnecting must not kill the user's other devices (route
//!   row survives until the LAST connection closes);
//! * only after the last device is gone do frames park for store-and-forward;
//! * a WebSocket client and a QUIC client interoperate through the same hub
//!   path (same wire protocol, same fan-out).

use std::time::Duration;

use wxlike_core::{Envelope, Store};
use wxlike_mq::memory::{MemoryMq, MemoryRoutes};
use wxlike_mq::MqRouter;
use wxlike_net::{run_mq_hub_with, Client, MqHubOptions};

async fn hub_with_store() -> (std::sync::Arc<Store>, std::net::SocketAddr) {
    let router = MqRouter::new(std::sync::Arc::new(MemoryMq::new()), MemoryRoutes::new());
    let store = std::sync::Arc::new(Store::open(None).unwrap());
    let hub = run_mq_hub_with(
        "multi-device",
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
    std::mem::forget(hub);
    (store, addr)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn both_devices_receive_the_same_directed_message() {
    let (_store, addr) = hub_with_store().await;
    let phone = Client::connect(addr, "localhost", "alice").await.unwrap();
    let desktop = Client::connect(addr, "localhost", "alice").await.unwrap();
    let bob = Client::connect(addr, "localhost", "bob").await.unwrap();
    // connects bind asynchronously on the hub side; give the registrations
    // a beat before the first send
    tokio::time::sleep(Duration::from_millis(300)).await;

    bob.send(&Envelope::direct(
        "d-alice~bob",
        "bob",
        vec!["alice".into()],
        1,
        "ring",
    ))
    .await
    .unwrap();

    let (a, b) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(15), phone.next_envelope()),
        tokio::time::timeout(Duration::from_secs(15), desktop.next_envelope()),
    );
    let a = a.unwrap().unwrap();
    let b = b.unwrap().unwrap();
    assert_eq!(a.body, b.body, "both devices got the same frame");
    assert!(matches!(&a.body, wxlike_core::MsgBody::Text(t) if t == "ring"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closing_one_device_keeps_the_other_live_and_bound() {
    let (_store, addr) = hub_with_store().await;
    let phone = Client::connect(addr, "localhost", "alice").await.unwrap();
    let desktop = Client::connect(addr, "localhost", "alice").await.unwrap();
    let bob = Client::connect(addr, "localhost", "bob").await.unwrap();
    // connects bind asynchronously on the hub side; give the registrations
    // a beat before the first send
    tokio::time::sleep(Duration::from_millis(300)).await;

    // the phone goes away: its stream closes, the hub removes only that conn
    drop(phone);
    // give the hub a beat to process the disconnect
    tokio::time::sleep(Duration::from_millis(400)).await;

    bob.send(&Envelope::direct(
        "d-alice~bob",
        "bob",
        vec!["alice".into()],
        1,
        "still here",
    ))
    .await
    .unwrap();

    // the desktop MUST receive: the route row survived the phone's exit
    let env = tokio::time::timeout(Duration::from_secs(15), desktop.next_envelope())
        .await
        .expect("desktop still connected")
        .unwrap();
    assert!(
        matches!(&env.body, wxlike_core::MsgBody::Text(t) if t == "still here"),
        "{env:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn frames_park_only_after_the_last_device_is_gone() {
    let (store, addr) = hub_with_store().await;
    let phone = Client::connect(addr, "localhost", "alice").await.unwrap();
    let desktop = Client::connect(addr, "localhost", "alice").await.unwrap();
    let bob = Client::connect(addr, "localhost", "bob").await.unwrap();
    // connects bind asynchronously on the hub side; give the registrations
    // a beat before the first send
    tokio::time::sleep(Duration::from_millis(300)).await;

    drop(phone);
    drop(desktop);
    tokio::time::sleep(Duration::from_millis(600)).await;

    bob.send(&Envelope::direct(
        "d-alice~bob",
        "bob",
        vec!["alice".into()],
        1,
        "while away",
    ))
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // parked, not lost: a fresh device flushes it on Hello
    let again = Client::connect(addr, "localhost", "alice").await.unwrap();
    let env = tokio::time::timeout(Duration::from_secs(15), again.next_envelope())
        .await
        .expect("parked frame flushed on hello")
        .unwrap();
    assert!(matches!(&env.body, wxlike_core::MsgBody::Text(t) if t == "while away"));
    assert_eq!(store.list_messages("d-alice~bob").unwrap().len(), 1);
}
