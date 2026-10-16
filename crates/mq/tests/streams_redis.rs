//! WP-10 A2: the Redis Streams backend, against a real Redis.
//!
//! These are `#[ignore]`d (so a normal `cargo test` never needs a server) and
//! additionally check `REDIS_URL` at run time rather than skipping unconditionally:
//!
//! ```text
//! REDIS_URL=redis://127.0.0.1:6399 cargo test -p wxlike-mq --features redis -- --ignored
//! ```
//!
//! The target itself only builds with `--features redis` (see Cargo.toml).

use std::time::Duration;

use wxlike_mq::{MessageQueue, ReliableQueue};

fn redis_url() -> Option<String> {
    std::env::var("REDIS_URL")
        .ok()
        .or_else(|| std::env::var("WXLIKE_REDIS_URL").ok())
}

macro_rules! require_redis {
    () => {
        match redis_url() {
            Some(url) => url,
            None => {
                eprintln!("skip: set REDIS_URL to exercise the streams backend");
                return;
            }
        }
    };
}

fn unique_queue(tag: &str) -> String {
    format!("wxlike:test:{tag}:{}", std::process::id())
}

#[tokio::test]
#[ignore = "requires REDIS_URL; run with: cargo test -p wxlike-mq --features redis -- --ignored"]
async fn streams_roundtrip_and_ack() {
    let url = require_redis!();
    let mq = wxlike_mq::streams::RedisStreamsMq::connect(&url, Some("test-consumer-1"))
        .unwrap()
        .with_timeouts(2_000, 1_000);
    let queue = unique_queue("roundtrip");

    mq.publish(&queue, b"across the wire".to_vec())
        .await
        .unwrap();
    let (id, payload) = mq.consume_entry(&queue).await.unwrap();
    assert_eq!(payload, b"across the wire");
    assert_eq!(
        mq.pending_count(&queue).await.unwrap(),
        1,
        "pending before ack"
    );
    mq.ack(&queue, &id).await.unwrap();
    assert_eq!(mq.pending_count(&queue).await.unwrap(), 0, "acked");
}

#[tokio::test]
#[ignore = "requires REDIS_URL"]
async fn streams_consumer_group_splits_entries() {
    let url = require_redis!();
    let queue = unique_queue("split");
    let a = wxlike_mq::streams::RedisStreamsMq::connect(&url, Some("split-a"))
        .unwrap()
        .with_timeouts(2_000, 1_000);
    let b = wxlike_mq::streams::RedisStreamsMq::connect(&url, Some("split-b"))
        .unwrap()
        .with_timeouts(2_000, 1_000);

    for i in 0..4u8 {
        a.publish(&queue, vec![b's', i]).await.unwrap();
    }
    let mut ids = std::collections::HashSet::new();
    for _ in 0..2 {
        ids.insert(a.consume_entry(&queue).await.unwrap().0);
        ids.insert(b.consume_entry(&queue).await.unwrap().0);
    }
    assert_eq!(ids.len(), 4, "one group, two consumers: no double reads");
    for id in ids {
        a.ack(&queue, &id).await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires REDIS_URL"]
async fn streams_reclaim_a_dead_consumers_entry() {
    let url = require_redis!();
    let queue = unique_queue("reclaim");
    // short idle window so the test does not wait the production 60s
    let dead = wxlike_mq::streams::RedisStreamsMq::connect(&url, Some("dead-consumer"))
        .unwrap()
        .with_timeouts(2_000, 300);
    let alive = wxlike_mq::streams::RedisStreamsMq::connect(&url, Some("alive-consumer"))
        .unwrap()
        .with_timeouts(2_000, 300);

    dead.publish(&queue, b"abandoned".to_vec()).await.unwrap();
    let (id, payload) = dead.consume_entry(&queue).await.unwrap();
    assert_eq!(payload, b"abandoned");
    // and it is never acknowledged, as if that process were killed

    tokio::time::sleep(Duration::from_millis(500)).await;
    let reclaimed = alive.claim_stale(&queue, 300).await.unwrap();
    assert_eq!(reclaimed.len(), 1, "XAUTOCLAIM hands it to the survivor");
    assert_eq!(reclaimed[0].0, id);
    assert_eq!(reclaimed[0].1, b"abandoned");
    alive.ack(&queue, &id).await.unwrap();
    assert!(alive.claim_stale(&queue, 300).await.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires REDIS_URL"]
async fn streams_default_claim_window_meets_the_contract() {
    let url = require_redis!();
    let mq = wxlike_mq::streams::RedisStreamsMq::connect(&url, None).unwrap();
    assert!(
        mq.claim_idle_ms() >= 60_000,
        "contract asks for >= 60s, got {}",
        mq.claim_idle_ms()
    );
    assert!(!mq.consumer().is_empty(), "generated consumer name");
}
