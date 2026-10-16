//! WP-10 A1: at-least-once queue semantics on the in-memory backend — pending
//! until acked, stale entries reclaimable, ids never reused.
//!
//! The real Redis Streams backend lives in `tests/streams_redis.rs` (it only
//! builds with `--features redis`).

use std::sync::Arc;
use std::time::Duration;

use wxlike_mq::memory::{MemoryMq, MemoryReliableMq, MemoryRoutes};
use wxlike_mq::{MessageQueue, MqRouter, ReliableQueue};

#[tokio::test]
async fn entry_stays_pending_until_acked() {
    let mq = MemoryReliableMq::new("c1");
    mq.publish("q", b"hello".to_vec()).await.unwrap();

    let (id, payload) = mq.consume_entry("q").await.unwrap();
    assert_eq!(payload, b"hello");
    assert_eq!(
        mq.pending_count("q").await.unwrap(),
        1,
        "delivered but unacked is still pending"
    );

    mq.ack("q", &id).await.unwrap();
    assert_eq!(mq.pending_count("q").await.unwrap(), 0, "ack clears it");
}

#[tokio::test]
async fn two_consumers_split_the_queue() {
    let a = MemoryReliableMq::new("consumer-a");
    // one shared queue, two distinct consumer identities
    let b = a.clone();
    b.set_consumer("consumer-b");

    for i in 0..4u8 {
        a.publish("q", vec![i]).await.unwrap();
    }
    let mut seen = std::collections::HashSet::new();
    for _ in 0..2 {
        let (_, p) = a.consume_entry("q").await.unwrap();
        seen.insert(p[0]);
        let (_, p) = b.consume_entry("q").await.unwrap();
        seen.insert(p[0]);
    }
    assert_eq!(seen.len(), 4, "each entry goes to exactly one consumer");
    assert_eq!(a.pending_count("q").await.unwrap(), 4);
}

#[tokio::test]
async fn unacked_entry_is_reclaimed_after_the_idle_window() {
    let first = MemoryReliableMq::new("dead-hub");
    first.publish("q", b"orphan".to_vec()).await.unwrap();
    let (id, _) = first.consume_entry("q").await.unwrap();
    assert_eq!(first.pending_count("q").await.unwrap(), 1);

    // a 0ms window claims immediately; production uses >= 60s
    tokio::time::sleep(Duration::from_millis(5)).await;
    let second = first.clone();
    second.set_consumer("live-hub");
    let reclaimed = second.claim_stale("q", 0).await.unwrap();
    assert_eq!(reclaimed.len(), 1, "the abandoned entry comes back");
    assert_eq!(reclaimed[0].0, id, "same entry id");
    assert_eq!(reclaimed[0].1, b"orphan");

    // once acked it is no longer claimable
    second.ack("q", &id).await.unwrap();
    assert!(second.claim_stale("q", 0).await.unwrap().is_empty());
    assert_eq!(second.pending_count("q").await.unwrap(), 0);
}

#[tokio::test]
async fn fresh_entries_are_not_stolen() {
    let holder = MemoryReliableMq::new("holder");
    holder.publish("q", b"mine".to_vec()).await.unwrap();
    holder.consume_entry("q").await.unwrap();

    let other = holder.clone();
    other.set_consumer("other");
    assert!(
        other.claim_stale("q", 60_000).await.unwrap().is_empty(),
        "60s window not reached"
    );
    assert_eq!(
        other.pending_count("q").await.unwrap(),
        1,
        "still in flight"
    );
}

#[tokio::test]
async fn acking_an_unknown_id_is_not_an_error() {
    let mq = MemoryReliableMq::new("c");
    mq.publish("q", b"x".to_vec()).await.unwrap();
    mq.ack("q", "999-0").await.unwrap();
    assert_eq!(mq.pending_count("q").await.unwrap(), 0);
}

#[tokio::test]
async fn ids_are_never_reused_after_acks() {
    let mq = MemoryReliableMq::new("c");
    let mut ids = std::collections::HashSet::new();
    for i in 0..5u8 {
        mq.publish("q", vec![i]).await.unwrap();
        let (id, _) = mq.consume_entry("q").await.unwrap();
        assert!(ids.insert(id.clone()), "id {id} unique");
        mq.ack("q", &id).await.unwrap();
    }
    assert_eq!(ids.len(), 5);
    // the queue keeps draining in order despite the acks
    mq.publish("q", vec![9]).await.unwrap();
    let (id, payload) = mq.consume_entry("q").await.unwrap();
    assert_eq!(payload, vec![9]);
    assert!(ids.insert(id), "id after the drain is still new");
}

#[tokio::test]
async fn plain_message_queue_semantics_unchanged() {
    // MemoryMq still pops destructively: what WP-10 replaces, kept working
    let mq = MemoryMq::new();
    mq.publish("q", b"gone-once-read".to_vec()).await.unwrap();
    assert_eq!(mq.consume("q").await.unwrap(), b"gone-once-read");
}

#[tokio::test]
async fn router_carries_reliable_backend_optionally() {
    // plain router: no reliable handle, hub keeps at-most-once behaviour
    let plain = MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new());
    assert!(plain.reliable.is_none());

    let streams = MemoryReliableMq::new("c");
    let router = MqRouter::with_reliable(streams.clone(), MemoryRoutes::new(), streams);
    assert!(
        router.reliable.is_some(),
        "hub can ack through the same bundle"
    );
}
