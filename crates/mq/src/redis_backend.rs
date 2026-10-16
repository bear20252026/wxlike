//! Redis backend (feature `redis`): LPUSH/BRPOP queues + Hash/Set routes.
//! Same semantics as box-im's Redis List design.
//!
//! **Deprecated for cross-node message traffic.** BRPOP removes the payload
//! from the list the moment it is read, so a hub that dies between popping and
//! delivering loses the message. Use [`crate::streams::RedisStreamsMq`], which
//! keeps entries pending until they are acknowledged. Kept because `RedisRoutes`
//! is still the route table, and because old deployments may rely on it.

// This module implements the very type it deprecates.
#![allow(deprecated)]

use async_trait::async_trait;
use redis::AsyncCommands;

use crate::{MessageQueue, RouteTable};

/// At-most-once list queue. Deprecated: see the module note.
#[deprecated(
    since = "0.1.0",
    note = "BRPOP discards the payload on read; use streams::RedisStreamsMq for at-least-once"
)]
pub struct RedisMq {
    client: redis::Client,
    /// BRPOP block seconds
    pub block_seconds: u64,
}

#[allow(deprecated)]
impl RedisMq {
    pub fn connect(url: &str) -> anyhow::Result<Self> {
        Ok(Self {
            client: redis::Client::open(url)?,
            block_seconds: 5,
        })
    }

    async fn conn(&self) -> anyhow::Result<redis::aio::MultiplexedConnection> {
        Ok(self.client.get_multiplexed_tokio_connection().await?)
    }
}

#[async_trait]
impl MessageQueue for RedisMq {
    async fn publish(&self, queue: &str, payload: Vec<u8>) -> anyhow::Result<()> {
        let mut c = self.conn().await?;
        let _: i64 = c.lpush(queue, payload.as_slice()).await?;
        Ok(())
    }

    async fn consume(&self, queue: &str) -> anyhow::Result<Vec<u8>> {
        let mut c = self.conn().await?;
        // BRPOP already blocks server-side for `block_seconds`
        let (_, payload): (String, Vec<u8>) = redis::cmd("BRPOP")
            .arg(queue)
            .arg(self.block_seconds)
            .query_async(&mut c)
            .await?;
        Ok(payload)
    }
}

pub struct RedisRoutes {
    client: redis::Client,
    const_: &'static str,
}

impl RedisRoutes {
    pub fn connect(url: &str) -> anyhow::Result<Self> {
        Ok(Self {
            client: redis::Client::open(url)?,
            const_: "wxlike",
        })
    }

    async fn conn(&self) -> anyhow::Result<redis::aio::MultiplexedConnection> {
        Ok(self.client.get_multiplexed_tokio_connection().await?)
    }

    fn routes_key(&self) -> String {
        format!("{}:routes", self.const_)
    }

    fn hubs_key(&self) -> String {
        format!("{}:hubs", self.const_)
    }
}

#[async_trait]
impl RouteTable for RedisRoutes {
    async fn bind_user(&self, user: &str, hub_id: &str) -> anyhow::Result<()> {
        let mut c = self.conn().await?;
        let _: () = c.hset(self.routes_key(), user, hub_id).await?;
        Ok(())
    }

    async fn unbind_user(&self, user: &str, hub_id: &str) -> anyhow::Result<()> {
        let mut c = self.conn().await?;
        let cur: Option<String> = c.hget(self.routes_key(), user).await?;
        if cur.as_deref() == Some(hub_id) {
            let _: () = c.hdel(self.routes_key(), user).await?;
        }
        Ok(())
    }

    async fn route_of(&self, user: &str) -> anyhow::Result<Option<String>> {
        let mut c = self.conn().await?;
        Ok(c.hget(self.routes_key(), user).await?)
    }

    async fn register_hub(&self, hub_id: &str) -> anyhow::Result<()> {
        let mut c = self.conn().await?;
        let _: () = c.sadd(self.hubs_key(), hub_id).await?;
        Ok(())
    }

    async fn unregister_hub(&self, hub_id: &str) -> anyhow::Result<()> {
        let mut c = self.conn().await?;
        let _: () = c.srem(self.hubs_key(), hub_id).await?;
        Ok(())
    }

    async fn list_hubs(&self) -> anyhow::Result<Vec<String>> {
        let mut c = self.conn().await?;
        Ok(c.smembers(self.hubs_key()).await?)
    }
}
