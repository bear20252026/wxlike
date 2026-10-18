//! P3 NATS backend (feature `nats`): JetStream durable pull consumers give
//! the same at-least-once semantics as the Redis Streams backend — entries
//! stay pending until acked, and an ack_wait redelivery recovers what a dead
//! hub left in flight. Needs a live `nats-server` (docker or binary).
//!
//! Contract test — same `--ignored` convention as the Redis real-backend
//! tests (WP-10):
//!
//! ```bash
//! docker run -p 4222:4222 -d nats:2 -js
//! NATS_URL=nats://127.0.0.1:4222 cargo test -p wxlike-mq --features nats -- --ignored
//! ```

use std::time::Duration;

use wxlike_mq::nats_backend::NatsMq;
use wxlike_mq::{MessageQueue, ReliableQueue};

fn url() -> String {
    std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".into())
}

async fn require_server() -> NatsMq {
    match NatsMq::connect(&url()).await {
        Ok(mq) => mq,
        Err(err) => panic!(
            "nats-server not reachable at {} ({err:#}) — start one with: docker run -p 4222:4222 -d nats:2 -js",
            url()
        ),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a live nats-server with JetStream (-js): cargo test -p wxlike-mq --features nats -- --ignored"]
async fn publish_consume_roundtrip() {
    let mq = require_server().await;
    let queue = format!("wxlike:msg:hub:test-{}", std::process::id());
    mq.publish(&queue, b"hello nats".to_vec()).await.unwrap();
    let payload = tokio::time::timeout(Duration::from_secs(10), mq.consume(&queue))
        .await
        .expect("consume within 10s")
        .unwrap();
    assert_eq!(payload, b"hello nats");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs nats-server: cargo test -p wxlike-mq --features nats -- --ignored"]
async fn unacked_entry_is_redelivered_after_ack_wait() {
    let mq = require_server().await;
    let queue = format!("wxlike:msg:hub:reliable-{}", std::process::id());
    mq.publish(&queue, b"must survive".to_vec()).await.unwrap();

    // read without acking: the entry stays pending
    let (id, payload) = tokio::time::timeout(Duration::from_secs(10), mq.consume_entry(&queue))
        .await
        .expect("first read")
        .unwrap();
    assert_eq!(payload, b"must survive");
    assert_eq!(mq.pending_count(&queue).await.unwrap(), 1);

    // ack_wait (NATS_ACK_WAIT_MS) later, a fetch hands the same entry back
    let wait = mq.ack_wait.as_millis() as u64;
    let (id2, payload2) = tokio::time::timeout(Duration::from_secs(wait + 15_000), async {
        loop {
            match mq.consume_entry(&queue).await {
                Ok((id2, p2)) => break (id2, p2),
                Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
    })
    .await
    .expect("redelivery within ack_wait + margin");
    assert_eq!(payload2, b"must survive", "at-least-once redelivery");
    let _ = id2;

    // ack both generations: backlog drains to zero
    mq.ack(&queue, &id).await.ok();
    mq.ack(&queue, &id2).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(mq.pending_count(&queue).await.unwrap(), 0);
}
