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
/// Backstop retention: the stream never keeps more than this many entries
/// (issue #20). `XADD ... MAXLEN ~` is approximate, so treat it as a ceiling,
/// not a precise cap - the pending-aware sweep below does the precise work.
pub const DEFAULT_MAX_LEN: u64 = 100_000;
/// Run the pending-aware trim sweep after this many acks (issue #20).
/// One sweep costs two extra round trips, so it is amortised.
pub const DEFAULT_SWEEP_EVERY: u64 = 256;

/// The id the stream should be trimmed **at** (exclusive: everything older goes).
///
/// - pending entries exist -> the oldest pending id. Everything older is
///   already acked, so trimming it is safe. **Never** use
///   "last delivered id" here: that would cut the in-flight entries a crashed
///   hub still needs to reclaim.
/// - no pending -> last delivered id: everything read has been acked.
/// - neither (empty stream) -> `"0"`: trim nothing.
fn trim_boundary(oldest_pending: Option<&str>, last_delivered: &str) -> String {
    if let Some(id) = oldest_pending {
        return id.to_string();
    }
    if last_delivered.is_empty() || last_delivered == "0" {
        return "0".to_string();
    }
    last_delivered.to_string()
}

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
    max_len: u64,
    sweep_every: u64,
    acks: std::sync::atomic::AtomicU64,
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
            max_len: DEFAULT_MAX_LEN,
            sweep_every: DEFAULT_SWEEP_EVERY,
            acks: std::sync::atomic::AtomicU64::new(0),
        })
    }

    pub fn with_timeouts(mut self, block_ms: u64, claim_idle_ms: u64) -> Self {
        self.block_ms = block_ms;
        self.claim_idle_ms = claim_idle_ms;
        self
    }

    /// Retention: at most `max_len` entries, and a precise trim sweep every
    /// `sweep_every` acks.
    pub fn with_retention(mut self, max_len: u64, sweep_every: u64) -> Self {
        self.max_len = max_len.max(1);
        self.sweep_every = sweep_every.max(1);
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
        // Backstop retention: even if the precise sweep never runs, the
        // stream cannot grow without bound. `~` is approximate trimming (no
        // full scan for an exact length), so this is a ceiling, not a value.
        let _: String = redis::cmd("XADD")
            .arg(&key)
            .arg("MAXLEN")
            .arg("~")
            .arg(self.max_len)
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
        drop(c);
        // Every `sweep_every` acks, do the precise trim (issue #20).
        // Failing is fine: the MAXLEN ceiling still holds and the next sweep
        // catches up.
        let n = self.acks.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_multiple_of(self.sweep_every) {
            if let Err(err) = self.sweep(queue).await {
                eprintln!("[mq] stream trim sweep failed for {queue}: {err:#}");
            }
        }
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

    /// Trim the part that is acked **and** has no pending entry in front of it.
    ///
    /// The boundary comes from [`trim_boundary`]: it never cuts into pending,
    /// which is exactly what lets a crashed hub's entries be reclaimed later.
    async fn sweep(&self, queue: &str) -> anyhow::Result<()> {
        let key = stream_key(queue);
        let mut c = self.conn().await?;
        // Oldest pending entry (IDLE 0 - + 1): one is enough to place the line.
        let pending: redis::Value = redis::cmd("XPENDING")
            .arg(&key)
            .arg(&self.group)
            .arg("IDLE")
            .arg(0)
            .arg("-")
            .arg("+")
            .arg(1)
            .query_async(&mut c)
            .await?;
        let oldest = pending_first_id(&pending);
        let last_delivered = if oldest.is_some() {
            String::new()
        } else {
            self.last_delivered_id(&mut c, &key).await?
        };
        let boundary = trim_boundary(oldest.as_deref(), &last_delivered);
        if boundary == "0" {
            return Ok(());
        }
        let _: i64 = redis::cmd("XTRIM")
            .arg(&key)
            .arg("MINID")
            .arg("~")
            .arg(&boundary)
            .query_async(&mut c)
            .await?;
        Ok(())
    }

    /// Stream length (issue #20: tests and ops need to see retention working).
    pub async fn stream_len(&self, queue: &str) -> anyhow::Result<u64> {
        let key = stream_key(queue);
        let mut c = self.conn().await?;
        Ok(redis::cmd("XLEN").arg(&key).query_async(&mut c).await?)
    }

    /// `last-delivered-id` of this group from `XINFO GROUPS` (empty when the
    /// group does not exist).
    async fn last_delivered_id(
        &self,
        c: &mut redis::aio::MultiplexedConnection,
        key: &str,
    ) -> anyhow::Result<String> {
        let groups: redis::Value = redis::cmd("XINFO")
            .arg("GROUPS")
            .arg(key)
            .query_async(c)
            .await?;
        Ok(xinfo_last_delivered(&groups, &self.group).unwrap_or_default())
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

/// The id of the first row in an `XPENDING ... IDLE 0 - + 1` reply.
///
/// Shape: `[[id, consumer, idle_ms, deliveries], ...]`; an empty pending set
/// comes back as `Nil` or an empty array.
fn pending_first_id(reply: &redis::Value) -> Option<String> {
    let redis::Value::Array(rows) = reply else {
        return None;
    };
    let first = rows.first()?;
    let redis::Value::Array(cols) = first else {
        return None;
    };
    match cols.first()? {
        redis::Value::BulkString(bytes) => Some(String::from_utf8_lossy(bytes).to_string()),
        _ => None,
    }
}

/// `last-delivered-id` of `group` from `XINFO GROUPS`.
///
/// Each group is a flat array `[name, consumers, pending, last-delivered-id,
/// ...]`, so: find the group name, then skip two fields.
fn xinfo_last_delivered(reply: &redis::Value, group: &str) -> Option<String> {
    let redis::Value::Array(groups) = reply else {
        return None;
    };
    for entry in groups {
        let redis::Value::Array(fields) = entry else {
            continue;
        };
        let mut iter = fields.iter();
        while let Some(field) = iter.next() {
            let redis::Value::BulkString(name) = field else {
                continue;
            };
            if String::from_utf8_lossy(name) != group {
                continue;
            }
            // name, consumers, pending, last-delivered-id
            iter.next();
            iter.next();
            return match iter.next()? {
                redis::Value::BulkString(bytes) => Some(String::from_utf8_lossy(bytes).to_string()),
                _ => None,
            };
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_never_cuts_into_pending() {
        // pending present: the boundary is the oldest pending id
        assert_eq!(trim_boundary(Some("111-1"), "999-9"), "111-1");
        // no pending: everything read is acked, so the last delivered id is safe
        assert_eq!(trim_boundary(None, "999-9"), "999-9");
        // empty stream: trim nothing
        assert_eq!(trim_boundary(None, ""), "0");
        assert_eq!(trim_boundary(None, "0"), "0");
    }

    #[test]
    fn pending_reply_without_rows_means_no_pending() {
        assert_eq!(pending_first_id(&redis::Value::Nil), None);
        assert_eq!(pending_first_id(&redis::Value::Array(vec![])), None);
    }

    #[test]
    fn pending_reply_reads_the_first_id() {
        let row = redis::Value::Array(vec![
            redis::Value::BulkString(b"111-1".to_vec()),
            redis::Value::BulkString(b"pid-1-0".to_vec()),
            redis::Value::Int(5_000),
            redis::Value::Int(1),
        ]);
        let reply = redis::Value::Array(vec![row]);
        assert_eq!(pending_first_id(&reply).as_deref(), Some("111-1"));
    }

    #[test]
    fn xinfo_reads_last_delivered_of_the_right_group() {
        let group = |name: &str, last: &str| {
            redis::Value::Array(vec![
                redis::Value::BulkString(name.as_bytes().to_vec()),
                redis::Value::Int(1),
                redis::Value::Int(0),
                redis::Value::BulkString(last.as_bytes().to_vec()),
                redis::Value::Int(0),
            ])
        };
        let reply = redis::Value::Array(vec![group("other", "1-1"), group("wxlike", "42-7")]);
        assert_eq!(
            xinfo_last_delivered(&reply, "wxlike").as_deref(),
            Some("42-7")
        );
        assert_eq!(xinfo_last_delivered(&reply, "missing"), None);
    }
}
