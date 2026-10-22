//! Cross-node routing: alice on hub-a, bob on hub-b, messages flow through
//! the shared MQ (memory backend stands in for Redis; same trait).

use std::sync::Arc;

use wxlike_core::Envelope;
use wxlike_mq::{memory::MemoryMq, memory::MemoryRoutes, MqRouter};
use wxlike_net::{run_mq_hub, Client};

#[tokio::test]
async fn direct_message_routes_across_hubs_via_mq() {
    let mq = Arc::new(MemoryMq::new());
    let routes = MemoryRoutes::new();
    let router = MqRouter::new(mq.clone(), routes.clone());

    let hub_a = run_mq_hub("hub-a", "127.0.0.1:0".parse().unwrap(), router.clone())
        .await
        .unwrap();
    let hub_b = run_mq_hub("hub-b", "127.0.0.1:0".parse().unwrap(), router)
        .await
        .unwrap();

    // alice connects to hub-a, bob to hub-b
    let alice = Client::connect(hub_a.local_addr().unwrap(), "localhost", "alice")
        .await
        .unwrap();
    let bob = Client::connect(hub_b.local_addr().unwrap(), "localhost", "bob")
        .await
        .unwrap();
    // let binds settle
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // directed message alice -> bob crosses hubs via the queue
    alice
        .send(&Envelope::direct(
            "dm-alice-bob",
            "alice",
            vec!["bob".into()],
            1,
            "hello across nodes",
        ))
        .await
        .unwrap();

    let got = tokio::time::timeout(std::time::Duration::from_secs(5), bob.next_envelope())
        .await
        .expect("cross-hub delivery within timeout")
        .unwrap();
    assert_eq!(got.sender, "alice");
    assert!(matches!(got.body, wxlike_core::MsgBody::Text(ref t) if t == "hello across nodes"));

    // reply crosses back
    bob.send(&Envelope::direct(
        "dm-alice-bob",
        "bob",
        vec!["alice".into()],
        1,
        "hi from node b",
    ))
    .await
    .unwrap();
    let reply = tokio::time::timeout(std::time::Duration::from_secs(5), alice.next_envelope())
        .await
        .expect("reply within timeout")
        .unwrap();
    assert_eq!(reply.sender, "bob");
}

#[tokio::test]
async fn broadcast_fans_out_to_all_hubs() {
    let mq = Arc::new(MemoryMq::new());
    let routes = MemoryRoutes::new();
    let router = MqRouter::new(mq.clone(), routes.clone());

    let hub_a = run_mq_hub("hub-a", "127.0.0.1:0".parse().unwrap(), router.clone())
        .await
        .unwrap();
    let hub_b = run_mq_hub("hub-b", "127.0.0.1:0".parse().unwrap(), router)
        .await
        .unwrap();

    let alice = Client::connect(hub_a.local_addr().unwrap(), "localhost", "alice")
        .await
        .unwrap();
    let bob = Client::connect(hub_b.local_addr().unwrap(), "localhost", "bob")
        .await
        .unwrap();
    let carol = Client::connect(hub_b.local_addr().unwrap(), "localhost", "carol")
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // **用户通道**的空收件人广播已经被拒（issue #13）：这条帧必须被丢掉，
    // 不能触达不相关的人。
    alice
        .send(&Envelope::text("room", "alice", 1, "should not fan out"))
        .await
        .unwrap();
    for (who, client) in [("bob", &bob), ("carol", &carol)] {
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(300),
                client.next_envelope()
            )
            .await
            .is_err(),
            "{who} 不该收到用户发起的广播"
        );
    }

    // 服务端内部广播走**独立可信入口**（`HubHandle::publish`），语义不变：
    // 本 hub 全体 + 其他 hub 各一份。
    hub_a
        .handle()
        .publish(&Envelope::text("room", "system", 2, "server broadcast"))
        .await
        .unwrap();

    for (who, client) in [("bob", &bob), ("carol", &carol)] {
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), client.next_envelope())
            .await
            .unwrap_or_else(|_| panic!("{who} should receive broadcast"))
            .unwrap();
        assert_eq!(got.sender, "system", "{who} 应当收到服务端广播");
    }
}

#[tokio::test]
async fn offline_recipient_is_reported_not_crashing() {
    let mq = Arc::new(MemoryMq::new());
    let router = MqRouter::new(mq, MemoryRoutes::new());
    let hub = run_mq_hub("hub-a", "127.0.0.1:0".parse().unwrap(), router)
        .await
        .unwrap();
    let alice = Client::connect(hub.local_addr().unwrap(), "localhost", "alice")
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // nobody named "ghost" anywhere: must not crash, just drop
    alice
        .send(&Envelope::direct(
            "dm",
            "alice",
            vec!["ghost".into()],
            1,
            "anyone?",
        ))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await; // let hub process
}
