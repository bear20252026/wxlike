use std::sync::Arc;
use std::time::Duration;
use wxlike_core::{Envelope, Store};
use wxlike_mq::{memory::MemoryMq, memory::MemoryRoutes, MqRouter};
use wxlike_net::offline::{MemoryOffline, OfflineStore};
use wxlike_net::{run_mq_hub_with, DeliveryPolicy, MqHubOptions, SendGate};

struct Rendezvous(tokio::sync::Barrier);

#[async_trait::async_trait]
impl DeliveryPolicy for Rendezvous {
    async fn blocked(&self, _: &str, _: &str) -> bool {
        false
    }
    async fn muted(&self, _: &str, _: &str) -> bool {
        false
    }
    async fn direct_allowed(&self, _: &str, _: &str) -> bool {
        true
    }
    async fn send_gate(&self, _: &str, _: &str, _: &str) -> SendGate {
        self.0.wait().await;
        SendGate::Allow
    }
}

fn quoted(id: u8, quote: u8) -> Envelope {
    let mut env = Envelope::direct("d-alice~bob", "alice", vec!["bob".into()], 0, "reply");
    env.msg_id = [id; 16];
    env.quoted([quote; 16])
}

async fn replay_is_rejected(warm: bool) {
    let store = Arc::new(Store::open(None).unwrap());
    let offline = MemoryOffline::new(100);
    let hub = run_mq_hub_with(
        "quote-replay",
        "127.0.0.1:0".parse().unwrap(),
        MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new()),
        MqHubOptions {
            storage: Some(store.clone()),
            offline: offline.clone(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let handle = hub.handle();
    let original = quoted(1, 2);
    if warm {
        handle.submit("alice", &original).await.unwrap();
    } else {
        store.append(&original, false).unwrap();
    }
    let before = offline.depth("bob").await.unwrap();
    let error = handle
        .submit("alice", &quoted(1, 3))
        .await
        .expect_err("different quote must fail");
    assert!(
        error.is::<wxlike_net::mq_hub::QuoteReplayConflict>(),
        "{error}"
    );
    assert_eq!(
        offline.depth("bob").await.unwrap(),
        before,
        "conflict must not be delivered"
    );
    let rows = store.list_messages("d-alice~bob").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].envelope.quote_of, original.quote_of);
    handle.submit("alice", &original).await.unwrap();
    assert_eq!(
        offline.depth("bob").await.unwrap(),
        1,
        "identical replay fills missing delivery only"
    );
}

#[tokio::test]
async fn persisted_duplicate_with_cold_cache_cannot_deliver_a_changed_quote() {
    replay_is_rejected(false).await;
}

#[tokio::test]
async fn seen_duplicate_cannot_deliver_a_changed_quote() {
    replay_is_rejected(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_quotes_choose_one_persisted_winner() {
    let store = Arc::new(Store::open(None).unwrap());
    let offline = MemoryOffline::new(100);
    let hub = run_mq_hub_with(
        "quote-race",
        "127.0.0.1:0".parse().unwrap(),
        MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new()),
        MqHubOptions {
            storage: Some(store.clone()),
            offline: offline.clone(),
            policy: Some(Arc::new(Rendezvous(tokio::sync::Barrier::new(2)))),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let handle = hub.handle();
    let left = quoted(4, 5);
    let right = quoted(4, 6);
    let (a, b) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            handle.submit("alice", &left),
            handle.submit("alice", &right)
        )
    })
    .await
    .expect("both requests must reach the persistence boundary");
    let winner = match (a, b) {
        (Ok(()), Err(error)) => {
            assert!(error.is::<wxlike_net::mq_hub::QuoteReplayConflict>());
            left
        }
        (Err(error), Ok(())) => {
            assert!(error.is::<wxlike_net::mq_hub::QuoteReplayConflict>());
            right
        }
        other => panic!("exactly one request must succeed: {other:?}"),
    };
    let rows = store.list_messages("d-alice~bob").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].envelope.quote_of, winner.quote_of);
    assert_eq!(offline.depth("bob").await.unwrap(), 1);
}
