//! In-memory backend: shared deque + Notify. Work-queue semantics —
//! multiple consumers on one queue split messages, mirroring BRPOP.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{Mutex, Notify, RwLock};

use crate::{MessageQueue, RouteTable};

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
