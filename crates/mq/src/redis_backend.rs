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
    /// Local generation source (G03-04). Uniqueness only has to hold per hub:
    /// the CAS compares `hub|generation` as a pair.
    next_generation: std::sync::atomic::AtomicU64,
}

/// Generation-aware route value: `<hub>|<generation>`. A value without the
/// separator was written by a build that has no generations (or through the
/// legacy `bind_user` path) and reads back as generation 0.
pub fn encode_route(hub: &str, generation: u64) -> String {
    format!("{hub}|{generation}")
}

pub fn decode_route(raw: &str) -> (&str, u64) {
    match raw.split_once('|') {
        Some((hub, gen)) => (hub, gen.parse().unwrap_or(0)),
        None => (raw, 0),
    }
}

impl RedisRoutes {
    pub fn connect(url: &str) -> anyhow::Result<Self> {
        Ok(Self {
            client: redis::Client::open(url)?,
            const_: "wxlike",
            lease_ms: DEFAULT_LEASE_MS,
            next_generation: std::sync::atomic::AtomicU64::new(0),
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
    fn generation(&self) -> u64 {
        self.next_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1
    }

    fn lease_secs(&self) -> u64 {
        (self.lease_ms / 1000).max(1)
    }

    /// 写一条路由：带租约用 `SET ... EX`，否则写回 hash。
    async fn write_route(
        &self,
        c: &mut redis::aio::MultiplexedConnection,
        user: &str,
        hub_id: &str,
    ) -> anyhow::Result<()> {
        self.write_route_at(c, user, &encode_route(hub_id, 0)).await
    }

    async fn write_route_at(
        &self,
        c: &mut redis::aio::MultiplexedConnection,
        user: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        if self.lease_ms == 0 {
            let _: () = c.hset(self.routes_key(), user, value).await?;
            return Ok(());
        }
        let _: () = c
            .set_ex(self.route_key(user), value, self.lease_secs())
            .await?;
        Ok(())
    }

    /// Compare-and-delete, atomically. A plain `GET` + `DEL` pair would let a
    /// route that was re-bound between the two calls still be removed.
    ///
    /// Written with raw `EVAL` rather than `redis::Script`: the script helper
    /// lives behind the crate's `script` feature, and pulling a new feature into
    /// the dependency tree is G00's call, not this card's.
    async fn cas_delete(
        &self,
        c: &mut redis::aio::MultiplexedConnection,
        user: &str,
        value: &str,
    ) -> anyhow::Result<bool> {
        let (script, key) = if self.lease_ms == 0 {
            (
                "if redis.call('HGET', KEYS[1], ARGV[1]) == ARGV[2] then                  return redis.call('HDEL', KEYS[1], ARGV[1]) else return 0 end",
                self.routes_key(),
            )
        } else {
            (
                "if redis.call('GET', KEYS[1]) == ARGV[1] then                  return redis.call('DEL', KEYS[1]) else return 0 end",
                self.route_key(user),
            )
        };
        let mut cmd = redis::cmd("EVAL");
        cmd.arg(script).arg(1usize).arg(key);
        if self.lease_ms == 0 {
            cmd.arg(user).arg(value);
        } else {
            cmd.arg(value);
        }
        let removed: i64 = cmd.query_async(c).await?;
        Ok(removed == 1)
    }

    /// Compare-and-extend: renew the lease only while this generation still owns
    /// the row, so a hub that fell behind cannot keep a dead route alive.
    async fn cas_renew(
        &self,
        c: &mut redis::aio::MultiplexedConnection,
        user: &str,
        value: &str,
    ) -> anyhow::Result<bool> {
        if self.lease_ms == 0 {
            let cur: Option<String> = c.hget(self.routes_key(), user).await?;
            return Ok(cur.as_deref() == Some(value));
        }
        let held: i64 = redis::cmd("EVAL")
            .arg(
                "if redis.call('GET', KEYS[1]) == ARGV[1] then                  return redis.call('PEXPIRE', KEYS[1], ARGV[2]) else return 0 end",
            )
            .arg(1usize)
            .arg(self.route_key(user))
            .arg(value)
            .arg(self.lease_ms)
            .query_async(c)
            .await?;
        Ok(held == 1)
    }
}

#[async_trait]
impl RouteTable for RedisRoutes {
    async fn bind_user(&self, user: &str, hub_id: &str) -> anyhow::Result<()> {
        let mut c = self.conn().await?;
        self.write_route(&mut c, user, hub_id).await
    }

    async fn unbind_user(&self, user: &str, hub_id: &str) -> anyhow::Result<()> {
        // Legacy path: matches on the hub alone and cannot tell two binds from
        // the same hub apart. Prefer `clear_route` (G03-04).
        let mut c = self.conn().await?;
        let cur: Option<String> = if self.lease_ms == 0 {
            c.hget(self.routes_key(), user).await?
        } else {
            c.get(self.route_key(user)).await?
        };
        let held = cur
            .as_deref()
            .map(|v| decode_route(v).0 == hub_id)
            .unwrap_or(false);
        if held {
            if self.lease_ms == 0 {
                let _: () = c.hdel(self.routes_key(), user).await?;
            } else {
                let _: () = c.del(self.route_key(user)).await?;
            }
        }
        Ok(())
    }

    async fn bind_route(&self, user: &str, hub_id: &str) -> anyhow::Result<u64> {
        let generation = self.generation();
        let mut c = self.conn().await?;
        self.write_route_at(&mut c, user, &encode_route(hub_id, generation))
            .await?;
        Ok(generation)
    }

    async fn renew_route(&self, user: &str, hub_id: &str, generation: u64) -> anyhow::Result<bool> {
        if generation == 0 {
            // a row this table did not install with a generation: legacy renew
            return self.renew_user(user, hub_id).await.map(|_| true);
        }
        let mut c = self.conn().await?;
        self.cas_renew(&mut c, user, &encode_route(hub_id, generation))
            .await
    }

    async fn clear_route(&self, user: &str, hub_id: &str, generation: u64) -> anyhow::Result<bool> {
        let mut c = self.conn().await?;
        self.cas_delete(&mut c, user, &encode_route(hub_id, generation))
            .await
    }

    fn generations(&self) -> bool {
        true
    }

    async fn route_of(&self, user: &str) -> anyhow::Result<Option<String>> {
        let mut c = self.conn().await?;
        let raw: Option<String> = if self.lease_ms == 0 {
            c.hget(self.routes_key(), user).await?
        } else {
            // 租约过期 = key 没了 = 没人持有。调用方据此重路由或落离线队列。
            c.get(self.route_key(user)).await?
        };
        // Values carry `<hub>|<generation>` since G03-04; callers only ever see
        // the hub, so an older reader of this key is unaffected.
        Ok(raw.map(|v| decode_route(&v).0.to_string()))
    }

    async fn renew_user(&self, user: &str, hub_id: &str) -> anyhow::Result<()> {
        if self.lease_ms == 0 {
            return Ok(());
        }
        let mut c = self.conn().await?;
        let cur: Option<String> = c.get(self.route_key(user)).await?;
        if cur.as_deref().map(|v| decode_route(v).0) != Some(hub_id) {
            // 已经不是我的了（掉线过、被别人抢走）：不续，也不报错——
            // 调用方的下一次路由查询会看到真相。
            return Ok(());
        }
        // Rewriting through the legacy path resets the row to generation 0, i.e.
        // "this hub does not track generations". A generation-aware caller must
        // use `renew_route` instead, which never downgrades.
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
