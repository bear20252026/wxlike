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

/// Default route lease: 30s. A hub renews every third of it, so three missed
/// renewals (a dead hub) are enough for the route to disappear.
pub const DEFAULT_LEASE_MS: u64 = 30_000;

pub struct RedisRoutes {
    client: redis::Client,
    const_: &'static str,
    /// Route lease in ms; `0` = permanent entries (the old behaviour).
    lease_ms: u64,
}

impl RedisRoutes {
    pub fn connect(url: &str) -> anyhow::Result<Self> {
        Ok(Self {
            client: redis::Client::open(url)?,
            const_: "wxlike",
            lease_ms: DEFAULT_LEASE_MS,
        })
    }

    /// Route lease length. `0` keeps the old permanent-hash behaviour.
    ///
    /// With a lease, routes live in per-user keys with a TTL instead of one
    /// hash field (Redis hashes have no per-field expiry), so a hub that dies
    /// without unbinding stops receiving traffic once the lease lapses
    /// instead of forever.
    pub fn with_lease(mut self, lease_ms: u64) -> Self {
        self.lease_ms = lease_ms;
        self
    }

    /// 单用户路由键（只有带租约时才用；永久模式仍走 hash）。
    fn route_key(&self, user: &str) -> String {
        format!("{}:route:{}", self.const_, user)
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

impl RedisRoutes {
    /// 写一条路由：带租约用 `SET ... EX`，否则写回 hash。
    async fn write_route(
        &self,
        c: &mut redis::aio::MultiplexedConnection,
        user: &str,
        hub_id: &str,
    ) -> anyhow::Result<()> {
        if self.lease_ms == 0 {
            let _: () = c.hset(self.routes_key(), user, hub_id).await?;
            return Ok(());
        }
        let ttl_secs = (self.lease_ms / 1000).max(1);
        let _: () = c.set_ex(self.route_key(user), hub_id, ttl_secs).await?;
        Ok(())
    }
}

#[async_trait]
impl RouteTable for RedisRoutes {
    async fn bind_user(&self, user: &str, hub_id: &str) -> anyhow::Result<()> {
        let mut c = self.conn().await?;
        self.write_route(&mut c, user, hub_id).await
    }

    async fn unbind_user(&self, user: &str, hub_id: &str) -> anyhow::Result<()> {
        let mut c = self.conn().await?;
        if self.lease_ms == 0 {
            let cur: Option<String> = c.hget(self.routes_key(), user).await?;
            if cur.as_deref() == Some(hub_id) {
                let _: () = c.hdel(self.routes_key(), user).await?;
            }
        } else {
            let cur: Option<String> = c.get(self.route_key(user)).await?;
            if cur.as_deref() == Some(hub_id) {
                let _: () = c.del(self.route_key(user)).await?;
            }
        }
        Ok(())
    }

    async fn route_of(&self, user: &str) -> anyhow::Result<Option<String>> {
        let mut c = self.conn().await?;
        if self.lease_ms == 0 {
            Ok(c.hget(self.routes_key(), user).await?)
        } else {
            // 租约过期 = key 没了 = 没人持有。调用方据此重路由或落离线队列。
            Ok(c.get(self.route_key(user)).await?)
        }
    }

    async fn renew_user(&self, user: &str, hub_id: &str) -> anyhow::Result<()> {
        if self.lease_ms == 0 {
            return Ok(());
        }
        let mut c = self.conn().await?;
        let cur: Option<String> = c.get(self.route_key(user)).await?;
        if cur.as_deref() != Some(hub_id) {
            // 已经不是我的了（掉线过、被别人抢走）：不续，也不报错——
            // 调用方的下一次路由查询会看到真相。
            return Ok(());
        }
        self.write_route(&mut c, user, hub_id).await
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

    fn lease_ttl_ms(&self) -> u64 {
        self.lease_ms
    }
}
