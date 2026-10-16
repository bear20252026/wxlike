//! wxlike-mq: message-queue and routing abstractions for multi-hub deployment.
//!
//! Rust port of box-im's MIT-licensed cross-node design (see
//! THIRD-PARTY-NOTICES.md): each hub consumes its own queue, and a routing
//! table maps users to the hub they are connected to.
//!
//! Backends:
//! - [`MemoryMq`] — tokio channels, zero deps, tests and single-node dev.
//! - `RedisMq` (feature `redis`) — LPUSH/BRPOP queues + Hash routes,
//!   same semantics as box-im's Redis Lists.

pub mod memory;

#[cfg(feature = "redis")]
pub mod redis_backend;

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

/// Bundle passed to hubs.
#[derive(Clone)]
pub struct MqRouter {
    pub mq: std::sync::Arc<dyn MessageQueue>,
    pub routes: std::sync::Arc<dyn RouteTable>,
}

impl MqRouter {
    pub fn new(
        mq: std::sync::Arc<dyn MessageQueue>,
        routes: std::sync::Arc<dyn RouteTable>,
    ) -> Self {
        Self { mq, routes }
    }
}
