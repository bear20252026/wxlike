//! Offline store-and-forward: messages for users who are not connected are
//! parked and delivered when they say Hello.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

/// Parked-message store. Scaffold semantics: per-user FIFO, capped, in-memory
/// (persisted variant lands with the platform service).
#[async_trait]
pub trait OfflineStore: Send + Sync + 'static {
    async fn enqueue(&self, user: &str, frame: Vec<u8>) -> anyhow::Result<()>;
    /// Remove and return all parked frames for `user`.
    async fn drain(&self, user: &str) -> anyhow::Result<Vec<Vec<u8>>>;
}

/// Per-user FIFO with a cap (oldest dropped beyond cap).
pub struct MemoryOffline {
    inner: Mutex<HashMap<String, VecDeque<Vec<u8>>>>,
    cap_per_user: usize,
}

impl MemoryOffline {
    pub fn new(cap_per_user: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(HashMap::new()),
            cap_per_user,
        })
    }
}

#[async_trait]
impl OfflineStore for MemoryOffline {
    async fn enqueue(&self, user: &str, frame: Vec<u8>) -> anyhow::Result<()> {
        let mut m = self.inner.lock().await;
        let dq = m.entry(user.to_string()).or_default();
        if dq.len() >= self.cap_per_user {
            dq.pop_front(); // drop oldest
        }
        dq.push_back(frame);
        Ok(())
    }

    async fn drain(&self, user: &str) -> anyhow::Result<Vec<Vec<u8>>> {
        let mut m = self.inner.lock().await;
        Ok(m.remove(user)
            .map(|dq| dq.into_iter().collect())
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn park_and_drain() {
        let store = MemoryOffline::new(3);
        store.enqueue("bob", b"m1".to_vec()).await.unwrap();
        store.enqueue("bob", b"m2".to_vec()).await.unwrap();
        assert_eq!(
            store.drain("bob").await.unwrap(),
            vec![b"m1".to_vec(), b"m2".to_vec()]
        );
        assert!(
            store.drain("bob").await.unwrap().is_empty(),
            "drain empties"
        );
    }

    #[tokio::test]
    async fn cap_drops_oldest() {
        let store = MemoryOffline::new(2);
        for m in ["a", "b", "c"] {
            store.enqueue("u", m.as_bytes().to_vec()).await.unwrap();
        }
        assert_eq!(
            store.drain("u").await.unwrap(),
            vec![b"b".to_vec(), b"c".to_vec()]
        );
    }
}
