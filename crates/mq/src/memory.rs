//! In-memory backend: shared deque + Notify. Work-queue semantics —
//! multiple consumers on one queue split messages, mirroring BRPOP.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{Mutex, Notify, RwLock};

use crate::{MessageQueue, ReliableQueue, RouteTable};

struct MemQueue {
    buf: std::sync::Mutex<VecDeque<Vec<u8>>>,
    notify: Notify,
}

#[derive(Default)]
pub struct MemoryMq {
    queues: Mutex<HashMap<String, Arc<MemQueue>>>,
}

impl MemoryMq {
    pub fn new() -> Self {
        Self::default()
    }

    async fn queue(&self, name: &str) -> Arc<MemQueue> {
        self.queues
            .lock()
            .await
            .entry(name.to_string())
            .or_insert_with(|| {
                Arc::new(MemQueue {
                    buf: std::sync::Mutex::new(VecDeque::new()),
                    notify: Notify::new(),
                })
            })
            .clone()
    }
}

#[async_trait]
impl MessageQueue for MemoryMq {
    async fn publish(&self, queue: &str, payload: Vec<u8>) -> anyhow::Result<()> {
        let q = self.queue(queue).await;
        q.buf.lock().unwrap().push_back(payload);
        q.notify.notify_one();
        Ok(())
    }

    async fn consume(&self, queue: &str) -> anyhow::Result<Vec<u8>> {
        let q = self.queue(queue).await;
        loop {
            let notified = q.notify.notified();
            if let Some(payload) = q.buf.lock().unwrap().pop_front() {
                return Ok(payload);
            }
            notified.await;
        }
    }
}

/// In-memory at-least-once queue, so the ack/reclaim path is testable without
/// Redis. Same contract as `RedisStreamsMq`: a delivered entry stays pending
/// until acknowledged, and a pending entry idle past the claim window can be
/// handed to another consumer.
#[derive(Default)]
pub struct MemoryReliableMq {
    queues: Mutex<HashMap<String, Arc<RelQueue>>>,
    consumer: std::sync::Mutex<String>,
}

struct RelQueue {
    entries: std::sync::Mutex<VecDeque<Entry>>,
    /// Monotonic, never reused: ids stay unique even after entries are acked.
    counter: std::sync::Mutex<u64>,
    notify: Notify,
}

struct Entry {
    id: String,
    payload: Vec<u8>,
    /// Set once delivered: the consumer holding it and when.
    claimed: Option<(String, std::time::Instant)>,
}

impl RelQueue {
    fn new() -> Self {
        Self {
            entries: std::sync::Mutex::new(VecDeque::new()),
            counter: std::sync::Mutex::new(0),
            notify: Notify::new(),
        }
    }

    fn push(&self, payload: Vec<u8>) -> String {
        let mut counter = self.counter.lock().unwrap();
        *counter += 1;
        let id = format!("{}-0", *counter);
        self.entries.lock().unwrap().push_back(Entry {
            id: id.clone(),
            payload,
            claimed: None,
        });
        self.notify.notify_waiters();
        id
    }
}

impl MemoryReliableMq {
    pub fn new(consumer: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            queues: Mutex::new(HashMap::new()),
            consumer: std::sync::Mutex::new(consumer.into()),
        })
    }

    /// Rebind this instance's consumer name (a "second process" in tests).
    pub fn set_consumer(&self, consumer: &str) {
        *self.consumer.lock().unwrap() = consumer.to_string();
    }

    fn here(&self) -> String {
        self.consumer.lock().unwrap().clone()
    }

    async fn queue(&self, name: &str) -> Arc<RelQueue> {
        self.queues
            .lock()
            .await
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(RelQueue::new()))
            .clone()
    }
}

#[async_trait]
impl MessageQueue for MemoryReliableMq {
    async fn publish(&self, queue: &str, payload: Vec<u8>) -> anyhow::Result<()> {
        self.queue(queue).await.push(payload);
        Ok(())
    }

    /// Read and immediately acknowledge — for callers that do not need the
    /// ack round-trip.
    async fn consume(&self, queue: &str) -> anyhow::Result<Vec<u8>> {
        let (_, payload) = self.consume_entry(queue).await?;
        Ok(payload)
    }
}

#[async_trait]
impl ReliableQueue for MemoryReliableMq {
    async fn consume_entry(&self, queue: &str) -> anyhow::Result<(String, Vec<u8>)> {
        let q = self.queue(queue).await;
        let me = self.here();
        loop {
            let notified = q.notify.notified();
            {
                let mut entries = q.entries.lock().unwrap();
                if let Some(entry) = entries.iter_mut().find(|e| e.claimed.is_none()) {
                    let id = entry.id.clone();
                    let payload = entry.payload.clone();
                    entry.claimed = Some((me.clone(), std::time::Instant::now()));
                    return Ok((id, payload));
                }
            }
            notified.await;
        }
    }

    async fn ack(&self, queue: &str, id: &str) -> anyhow::Result<()> {
        let q = self.queue(queue).await;
        let mut entries = q.entries.lock().unwrap();
        if let Some(pos) = entries.iter().position(|e| e.id == id) {
            entries.remove(pos);
        }
        Ok(())
    }

    async fn claim_stale(
        &self,
        queue: &str,
        min_idle_ms: u64,
    ) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
        let q = self.queue(queue).await;
        let me = self.here();
        let mut out = Vec::new();
        let mut entries = q.entries.lock().unwrap();
        for entry in entries.iter_mut() {
            let stale = match &entry.claimed {
                Some((who, at)) => {
                    *who != me && at.elapsed() >= std::time::Duration::from_millis(min_idle_ms)
                }
                None => false,
            };
            if stale {
                entry.claimed = Some((me.clone(), std::time::Instant::now()));
                out.push((entry.id.clone(), entry.payload.clone()));
            }
        }
        Ok(out)
    }

    async fn pending_count(&self, queue: &str) -> anyhow::Result<u64> {
        let q = self.queue(queue).await;
        // scoped so the guard is dropped before the future resolves
        let count = {
            let entries = q.entries.lock().unwrap();
            entries.iter().filter(|e| e.claimed.is_some()).count()
        };
        Ok(count as u64)
    }
}

/// User→hub routes + hub registry, all in memory.
#[derive(Default)]
pub struct MemoryRoutes {
    user_hub: RwLock<HashMap<String, String>>,
    hubs: RwLock<std::collections::BTreeSet<String>>,
}

impl MemoryRoutes {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[async_trait]
impl RouteTable for MemoryRoutes {
    async fn bind_user(&self, user: &str, hub_id: &str) -> anyhow::Result<()> {
        self.user_hub
            .write()
            .await
            .insert(user.to_string(), hub_id.to_string());
        Ok(())
    }

    async fn unbind_user(&self, user: &str, hub_id: &str) -> anyhow::Result<()> {
        let mut m = self.user_hub.write().await;
        if m.get(user).map(|h| h.as_str()) == Some(hub_id) {
            m.remove(user);
        }
        Ok(())
    }

    async fn route_of(&self, user: &str) -> anyhow::Result<Option<String>> {
        Ok(self.user_hub.read().await.get(user).cloned())
    }

    async fn register_hub(&self, hub_id: &str) -> anyhow::Result<()> {
        self.hubs.write().await.insert(hub_id.to_string());
        Ok(())
    }

    async fn unregister_hub(&self, hub_id: &str) -> anyhow::Result<()> {
        self.hubs.write().await.remove(hub_id);
        Ok(())
    }

    async fn list_hubs(&self) -> anyhow::Result<Vec<String>> {
        Ok(self.hubs.read().await.iter().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn publish_consume_roundtrip() {
        let mq = MemoryMq::new();
        mq.publish("q1", b"hello".to_vec()).await.unwrap();
        assert_eq!(mq.consume("q1").await.unwrap(), b"hello");
    }

    #[tokio::test]
    async fn fifo_order_preserved() {
        let mq = MemoryMq::new();
        for i in 0u8..5 {
            mq.publish("q", vec![i]).await.unwrap();
        }
        for i in 0u8..5 {
            assert_eq!(mq.consume("q").await.unwrap(), vec![i]);
        }
    }

    #[tokio::test]
    async fn work_queue_splits_between_consumers() {
        let mq = Arc::new(MemoryMq::new());
        for i in 0u8..4 {
            mq.publish("q", vec![i]).await.unwrap();
        }
        let (c1, c2) = (mq.clone(), mq.clone());
        let h1 = tokio::spawn(async move { c1.consume("q").await.unwrap() });
        let h2 = tokio::spawn(async move { c2.consume("q").await.unwrap() });
        let (a, b) = tokio::join!(h1, h2);
        assert_ne!(
            a.unwrap(),
            b.unwrap(),
            "each message to exactly one consumer"
        );
    }

    #[tokio::test]
    async fn consume_wakes_on_publish() {
        let mq = Arc::new(MemoryMq::new());
        let p = mq.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            p.publish("slow", b"late".to_vec()).await.unwrap();
        });
        let got = tokio::time::timeout(std::time::Duration::from_secs(2), mq.consume("slow"))
            .await
            .expect("woke within timeout")
            .unwrap();
        assert_eq!(got, b"late");
    }

    #[tokio::test]
    async fn routes_bind_and_lookup() {
        let routes = MemoryRoutes::new();
        routes.register_hub("hub-a").await.unwrap();
        routes.bind_user("alice", "hub-a").await.unwrap();
        assert_eq!(
            routes.route_of("alice").await.unwrap(),
            Some("hub-a".into())
        );
        routes.unbind_user("alice", "hub-b").await.unwrap(); // wrong hub: no-op
        assert_eq!(
            routes.route_of("alice").await.unwrap(),
            Some("hub-a".into())
        );
        routes.unbind_user("alice", "hub-a").await.unwrap();
        assert_eq!(routes.route_of("alice").await.unwrap(), None);
        assert_eq!(routes.list_hubs().await.unwrap(), vec!["hub-a".to_string()]);
    }
}
