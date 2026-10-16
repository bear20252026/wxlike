//! wxlike-mq: message-queue and routing abstractions for multi-hub deployment.
//!
//! Rust port of box-im's MIT-licensed cross-node design (see
//! THIRD-PARTY-NOTICES.md): each hub consumes its own queue, and a routing
//! table maps users to the hub they are connected to.
//!
//! Backends:
//! - [`memory::MemoryMq`] — tokio channels, zero deps, tests and single-node dev.
//! - [`memory::MemoryReliableMq`] — in-memory at-least-once queue (ack + reclaim).
//! - `redis_backend::RedisMq` (feature `redis`) — LPUSH/BRPOP queues + Hash
//!   routes. **Deprecated**: a popped payload is unrecoverable, so a hub that
//!   dies mid-delivery loses the message.
//! - `streams::RedisStreamsMq` (feature `redis`) — XADD/XREADGROUP/XACK with a
//!   shared consumer group, plus XAUTOCLAIM for stale in-flight entries.

pub mod memory;

#[cfg(feature = "redis")]
pub mod redis_backend;
#[cfg(feature = "redis")]
pub mod streams;

use async_trait::async_trait;

/// Queue name for a hub's inbound messages.
pub fn hub_queue(hub_id: &str) -> String {
    format!("wxlike:msg:hub:{hub_id}")
}

/// Work-queue semantics: publish appends; exactly one consumer receives each
/// payload (mpsc / BRPOP). At-least-once delivery and dead-letter queues are
/// future work (upgrade path: Redis Streams consumer groups).
#[async_trait]
pub trait MessageQueue: Send + Sync + 'static {
    async fn publish(&self, queue: &str, payload: Vec<u8>) -> anyhow::Result<()>;
    /// Await the next payload on `queue`. Blocking long-poll.
    async fn consume(&self, queue: &str) -> anyhow::Result<Vec<u8>>;
}

/// User→hub connection routing table, plus the live hub registry.
#[async_trait]
pub trait RouteTable: Send + Sync + 'static {
    async fn bind_user(&self, user: &str, hub_id: &str) -> anyhow::Result<()>;
    async fn unbind_user(&self, user: &str, hub_id: &str) -> anyhow::Result<()>;
    async fn route_of(&self, user: &str) -> anyhow::Result<Option<String>>;
    async fn register_hub(&self, hub_id: &str) -> anyhow::Result<()>;
    async fn unregister_hub(&self, hub_id: &str) -> anyhow::Result<()>;
    async fn list_hubs(&self) -> anyhow::Result<Vec<String>>;
}

/// At-least-once queue: entries carry an id and must be acknowledged.
///
/// A consumer that reads an entry and dies without acking it leaves it
/// pending, and [`ReliableQueue::claim_stale`] hands it to whoever asks after
/// the idle window — that is the difference from [`MessageQueue`], where a
/// consumed payload is gone whether or not the consumer finished its work.
#[async_trait]
pub trait ReliableQueue: Send + Sync + 'static {
    /// Read the next entry, blocking up to the backend's timeout.
    async fn consume_entry(&self, queue: &str) -> anyhow::Result<(String, Vec<u8>)>;

    /// Confirm processing; the entry leaves the pending set.
    async fn ack(&self, queue: &str, id: &str) -> anyhow::Result<()>;

    /// Reclaim entries idle for at least `min_idle_ms`, delivering them to this
    /// consumer. Returns the reclaimed `(id, payload)` pairs.
    async fn claim_stale(
        &self,
        queue: &str,
        min_idle_ms: u64,
    ) -> anyhow::Result<Vec<(String, Vec<u8>)>>;

    /// How many entries are in-flight but unacknowledged (backlog metric).
    async fn pending_count(&self, queue: &str) -> anyhow::Result<u64>;
}

/// Bundle passed to hubs.
#[derive(Clone)]
pub struct MqRouter {
    pub mq: std::sync::Arc<dyn MessageQueue>,
    pub routes: std::sync::Arc<dyn RouteTable>,
    /// Set when the backend can ack: the hub then consumes with at-least-once
    /// semantics instead of fire-and-forget.
    pub reliable: Option<std::sync::Arc<dyn ReliableQueue>>,
}

impl MqRouter {
    pub fn new(
        mq: std::sync::Arc<dyn MessageQueue>,
        routes: std::sync::Arc<dyn RouteTable>,
    ) -> Self {
        Self {
            mq,
            routes,
            reliable: None,
        }
    }

    /// Same, with an ack-capable backend behind the same connection.
    pub fn with_reliable(
        mq: std::sync::Arc<dyn MessageQueue>,
        routes: std::sync::Arc<dyn RouteTable>,
        reliable: std::sync::Arc<dyn ReliableQueue>,
    ) -> Self {
        Self {
            mq,
            routes,
            reliable: Some(reliable),
        }
    }
}
