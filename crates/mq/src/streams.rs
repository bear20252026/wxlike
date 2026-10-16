//! Redis Streams backend (feature `redis`): at-least-once cross-node delivery.
//!
//! Replaces the BRPOP list backend. Entries live in a stream, hubs read them as
//! members of one consumer group (`wxlike`) with a per-process consumer name,
//! and each entry is removed from the pending set only after local delivery
//! succeeded. A hub that dies mid-delivery leaves the entry pending; the patrol
//! task reclaims it after the idle window, so nothing is lost.

use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;

use crate::{MessageQueue, ReliableQueue};

/// Consumer group shared by every hub process.
pub const GROUP: &str = "wxlike";
/// Contract floor: reclaim entries idle for at least this long.
pub const DEFAULT_CLAIM_IDLE_MS: u64 = 60_000;

static CONSUMER_SEQ: AtomicU64 = AtomicU64::new(0);

/// Streams are keyed separately from the legacy list backend so both can point
/// at one Redis during a migration without a WRONGTYPE collision.
fn stream_key(queue: &str) -> String {
    format!("{queue}:stream")
}

pub struct RedisStreamsMq {
    client: redis::Client,
    group: String,
    consumer: String,
    block_ms: u64,
    claim_idle_ms: u64,
}

impl RedisStreamsMq {
    /// `consumer` identifies this process; pass a stable per-node name (hub id
    /// + pid), or let it be generated.
    pub fn connect(url: &str, consumer: Option<&str>) -> anyhow::Result<Self> {
        let generated = format!(
            "pid-{}-{}",
            std::process::id(),
            CONSUMER_SEQ.fetch_add(1, Ordering::Relaxed)
        );
        Ok(Self {
            client: redis::Client::open(url)?,
            group: GROUP.to_string(),
            consumer: consumer.unwrap_or(&generated).to_string(),
            block_ms: 5_000,
            claim_idle_ms: DEFAULT_CLAIM_IDLE_MS,
        })
    }

    pub fn with_timeouts(mut self, block_ms: u64, claim_idle_ms: u64) -> Self {
        self.block_ms = block_ms;
        self.claim_idle_ms = claim_idle_ms;
        self
    }

    pub fn consumer(&self) -> &str {
        &self.consumer
    }

    pub fn stream_key(queue: &str) -> String {
        stream_key(queue)
    }

    async fn conn(&self) -> anyhow::Result<redis::aio::MultiplexedConnection> {
        Ok(self.client.get_multiplexed_tokio_connection().await?)
    }

    /// Consumer groups must exist before the first read; idempotent.
    ///
    /// The group starts at `0`, not at the tail: with `$`, an entry published
    /// before the first hub created the group would never be delivered to it —
    /// silent message loss. Starting at `0` means the very first group creation
    /// replays whatever is already in the stream instead, which is the failure
    /// mode at-least-once can tolerate (and dedup by `msg_id` absorbs it).
    /// After creation, `">"` delivers only undelivered entries as usual.
    async fn ensure_group(&self, queue: &str) -> anyhow::Result<()> {
        let key = stream_key(queue);
        let mut c = self.conn().await?;
        let result: redis::RedisResult<redis::Value> = redis::cmd("XGROUP")
            .arg("CREATE")
            .arg(&key)
            .arg(&self.group)
            .arg("0")
            .arg("MKSTREAM")
            .query_async(&mut c)
            .await;
        // every hub calls this on its read loop, so "group already exists" is
        // the normal case, not a failure
        match result {
            Ok(_) => Ok(()),
            Err(err) if err.to_string().contains("BUSYGROUP") => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}

#[async_trait]
impl MessageQueue for RedisStreamsMq {
    async fn publish(&self, queue: &str, payload: Vec<u8>) -> anyhow::Result<()> {
        let key = stream_key(queue);
        let mut c = self.conn().await?;
        let _: String = redis::cmd("XADD")
            .arg(&key)
            .arg("*")
            .arg("p")
            .arg(payload.as_slice())
            .query_async(&mut c)
            .await?;
        Ok(())
    }

    /// Reads one entry and leaves it pending; prefer [`ReliableQueue`] so the
    /// caller can ack. Without an ack the entry will be reclaimed later and
    /// delivered a second time — at-least-once, which is the point.
    async fn consume(&self, queue: &str) -> anyhow::Result<Vec<u8>> {
        let (_, payload) = self.consume_entry(queue).await?;
        Ok(payload)
    }
}

#[async_trait]
impl ReliableQueue for RedisStreamsMq {
    async fn consume_entry(&self, queue: &str) -> anyhow::Result<(String, Vec<u8>)> {
        self.ensure_group(queue).await?;
        let key = stream_key(queue);
        let mut c = self.conn().await?;
        loop {
            let reply: redis::Value = redis::cmd("XREADGROUP")
                .arg("GROUP")
                .arg(&self.group)
                .arg(&self.consumer)
                .arg("COUNT")
                .arg(1)
                .arg("BLOCK")
                .arg(self.block_ms)
                .arg("STREAMS")
                .arg(&key)
                .arg(">")
                .query_async(&mut c)
                .await?;
            if let Some(entry) = first_entry(&reply) {
                return Ok(entry);
            }
            // BLOCK timed out: keep long-polling, same contract as BRPOP
        }
    }

    async fn ack(&self, queue: &str, id: &str) -> anyhow::Result<()> {
        let key = stream_key(queue);
        let mut c = self.conn().await?;
        let _: i64 = redis::cmd("XACK")
            .arg(&key)
            .arg(&self.group)
            .arg(id)
            .query_async(&mut c)
            .await?;
        Ok(())
    }

    async fn claim_stale(
        &self,
        queue: &str,
        min_idle_ms: u64,
    ) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
        self.ensure_group(queue).await?;
        let key = stream_key(queue);
        let mut c = self.conn().await?;
        // XAUTOCLAIM answers [next-cursor, [[id, [field, value], ...], ...] ].
        let reply: redis::Value = redis::cmd("XAUTOCLAIM")
            .arg(&key)
            .arg(&self.group)
            .arg(&self.consumer)
            .arg(min_idle_ms)
            .arg("0-0")
            .arg("COUNT")
            .arg(100)
            .query_async(&mut c)
            .await?;
        Ok(entries_of_claim(&reply))
    }

    async fn pending_count(&self, queue: &str) -> anyhow::Result<u64> {
        self.ensure_group(queue).await?;
        let key = stream_key(queue);
        let mut c = self.conn().await?;
        // XPENDING <key> <group> -> [pending, min-id, max-id, consumers]
        let reply: redis::Value = redis::cmd("XPENDING")
            .arg(&key)
            .arg(&self.group)
            .query_async(&mut c)
            .await?;
        Ok(match reply {
            redis::Value::Array(items) => match items.first() {
                Some(redis::Value::Int(n)) => *n as u64,
                // RESP2 can hand the count back as a bulk string
                Some(redis::Value::BulkString(bytes)) => String::from_utf8_lossy(bytes)
                    .trim()
                    .parse::<u64>()
                    .unwrap_or(0),
                _ => 0,
            },
            _ => 0,
        })
    }
}

/// Default idle window from the contract (>= 60s).
impl RedisStreamsMq {
    pub fn claim_idle_ms(&self) -> u64 {
        self.claim_idle_ms
    }
}

/// Pull `(id, payload)` pairs out of an **XREADGROUP** reply.
///
/// RESP2 shape: `[ [stream-key, [ [id, [field, value], ...], ... ]], ... ]`
fn entries_of(value: &redis::Value) -> Vec<(String, Vec<u8>)> {
    let redis::Value::Array(outer) = value else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in outer {
        // each item is a (stream key, entries) pair
        let redis::Value::Array(pair) = item else {
            continue;
        };
        let Some(entries) = pair.get(1) else { continue };
        out.extend(entry_list(entries));
    }
    out
}

/// Pull `(id, payload)` pairs out of an **XAUTOCLAIM** reply.
///
/// RESP2 shape: `[ next-cursor, [ [id, [field, value], ...], ... ], [deleted-ids] ]`
/// — a different nesting depth from XREADGROUP, so it needs its own reader.
fn entries_of_claim(value: &redis::Value) -> Vec<(String, Vec<u8>)> {
    let redis::Value::Array(outer) = value else {
        return Vec::new();
    };
    match outer.get(1) {
        Some(entries) => entry_list(entries),
        None => Vec::new(),
    }
}

/// A list of `[id, [field, value], ...]` entries.
fn entry_list(list: &redis::Value) -> Vec<(String, Vec<u8>)> {
    let redis::Value::Array(items) = list else {
        return Vec::new();
    };
    items.iter().filter_map(one_entry).collect()
}

fn one_entry(value: &redis::Value) -> Option<(String, Vec<u8>)> {
    let redis::Value::Array(parts) = value else {
        return None;
    };
    if parts.len() < 2 {
        return None;
    }
    let id = match &parts[0] {
        redis::Value::BulkString(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        _ => return None,
    };
    // field/value pairs arrive flattened: [p, <bytes>]
    let redis::Value::Array(fields) = &parts[1] else {
        return None;
    };
    let mut window = fields.iter();
    while let Some(key) = window.next() {
        let is_payload = matches!(key, redis::Value::BulkString(b) if b.as_slice() == b"p");
        if is_payload {
            return match window.next() {
                Some(redis::Value::BulkString(payload)) => Some((id, payload.clone())),
                _ => None,
            };
        }
    }
    None
}

fn first_entry(value: &redis::Value) -> Option<(String, Vec<u8>)> {
    entries_of(value).into_iter().next()
}
