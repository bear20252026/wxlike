//! P3 NATS backend (feature `nats`): JetStream durable pull consumers give
//! the same at-least-once semantics as the Redis Streams backend — entries
//! stay pending until acked, and the server's `ack_wait` redelivery recovers
//! what a dead hub left in flight (the [`ReliableQueue::claim_stale`] fetch).
//! Routes live in a JetStream KV bucket.
//!
//! Subjects map from the queue names (`wxlike:msg:hub:{hub}` →
//! `wxlike.msg.hub.{hub}`) under one `WXLIKE` stream; each queue gets its own
//! durable consumer so per-hub backlogs are independently observable.
//!
//! Real-backend test (same `--ignored` convention as Redis, WP-10):
//! ```bash
//! docker run -p 4222:4222 -d nats:2 -js
//! NATS_URL=nats://127.0.0.1:4222 cargo test -p wxlike-mq --features nats -- --ignored
//! ```

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt as _;
use tokio::sync::Mutex;

use crate::{MessageQueue, ReliableQueue, RouteTable};

const STREAM: &str = "WXLIKE";
const KV_BUCKET: &str = "wxlike";
/// How long the server holds an unacked entry before redelivering it.
const DEFAULT_ACK_WAIT_MS: u64 = 5_000;

pub struct NatsMq {
    client: async_nats::Client,
    jetstream: async_nats::jetstream::Context,
    consumers: Mutex<HashMap<String, async_nats::jetstream::consumer::PullConsumer>>,
    pub ack_wait: Duration,
}

fn subject_of(queue: &str) -> String {
    queue.replace(':', ".")
}

fn durable_of(queue: &str) -> String {
    subject_of(queue).replace('.', "_")
}

impl NatsMq {
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let client = async_nats::connect(url).await?;
        let jetstream = async_nats::jetstream::new(client.clone());
        jetstream
            .get_or_create_stream(async_nats::jetstream::stream::Config {
                name: STREAM.into(),
                subjects: vec!["wxlike.>".into()],
                ..Default::default()
            })
            .await?;
        let ack_wait = Duration::from_millis(
            std::env::var("NATS_ACK_WAIT_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_ACK_WAIT_MS),
        );
        Ok(Self {
            client,
            jetstream,
            consumers: Mutex::new(HashMap::new()),
            ack_wait,
        })
    }

    /// The JetStream context (route table shares this connection).
    pub fn context(&self) -> async_nats::jetstream::Context {
        self.jetstream.clone()
    }

    /// Durable pull consumer for one queue (created on first use, then cached).
    async fn consumer(
        &self,
        queue: &str,
    ) -> anyhow::Result<async_nats::jetstream::consumer::PullConsumer> {
        let mut cache = self.consumers.lock().await;
        if let Some(c) = cache.get(queue) {
            return Ok(c.clone());
        }
        let stream = self.jetstream.get_stream(STREAM).await?;
        let consumer = stream
            .get_or_create_consumer::<async_nats::jetstream::consumer::pull::Config>(
                &durable_of(queue),
                async_nats::jetstream::consumer::pull::Config {
                    durable_name: Some(durable_of(queue)),
                    filter_subject: subject_of(queue),
                    ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                    ack_wait: self.ack_wait,
                    ..Default::default()
                },
            )
            .await?;
        cache.insert(queue.to_string(), consumer.clone());
        Ok(consumer)
    }
}

#[async_trait]
impl MessageQueue for NatsMq {
    async fn publish(&self, queue: &str, payload: Vec<u8>) -> anyhow::Result<()> {
        self.jetstream
            .publish(subject_of(queue), payload.into())
            .await?
            .await?;
        Ok(())
    }

    async fn consume(&self, queue: &str) -> anyhow::Result<Vec<u8>> {
        let consumer = self.consumer(queue).await?;
        // one message per fetch, 5s server-side expiry → the hub's consume
        // loop retried like the Redis BRPOP path
        let mut messages = consumer
            .fetch()
            .max_messages(1)
            .expires(Duration::from_secs(5))
            .messages()
            .await?;
        let msg = messages
            .next()
            .await
            .transpose()
            .map_err(|e| anyhow::anyhow!("fetch error: {e}"))?
            .ok_or_else(|| anyhow::anyhow!("fetch expired without a message"))?;
        Ok(msg.message.payload.to_vec())
    }
}

#[async_trait]
impl ReliableQueue for NatsMq {
    async fn consume_entry(&self, queue: &str) -> anyhow::Result<(String, Vec<u8>)> {
        let consumer = self.consumer(queue).await?;
        let mut messages = consumer
            .fetch()
            .max_messages(1)
            .expires(Duration::from_secs(5))
            .messages()
            .await?;
        let msg = messages
            .next()
            .await
            .transpose()
            .map_err(|e| anyhow::anyhow!("fetch error: {e}"))?
            .ok_or_else(|| anyhow::anyhow!("fetch expired without a message"))?;
        // the reply subject IS the ack address; it is the entry id in this
        // backend (the Rust port of JetStream's terminal-ack model)
        let reply = msg
            .message
            .reply
            .as_ref()
            .map(|r| r.to_string())
            .ok_or_else(|| anyhow::anyhow!("jetstream message without reply subject"))?;
        Ok((reply, msg.message.payload.to_vec()))
    }

    async fn ack(&self, queue: &str, id: &str) -> anyhow::Result<()> {
        let _ = queue;
        // terminal ack = an empty payload to the reply subject, exactly what
        // Message::ack does but addressed by the stored id
        self.client
            .publish(id.to_string(), Vec::new().into())
            .await?;
        Ok(())
    }

    /// NATS redelivers unacked entries after `ack_wait` on its own; a fetch
    /// therefore IS the reclaim (what comes back is what a dead consumer
    /// left pending). Idle filtering happens server-side.
    async fn claim_stale(
        &self,
        queue: &str,
        _min_idle_ms: u64,
    ) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
        match self.consume_entry(queue).await {
            Ok(entry) => Ok(vec![entry]),
            // expiry with nothing pending is an empty reclaim, not an error
            Err(err) if err.to_string().contains("expired") => Ok(Vec::new()),
            Err(err) => Err(err),
        }
    }

    async fn pending_count(&self, queue: &str) -> anyhow::Result<u64> {
        let mut consumer = self.consumer(queue).await?;
        let info = consumer.info().await?;
        Ok(info.num_ack_pending as u64)
    }
}

/// Route table + hub registry in one KV bucket (`wxlike`): `route:{user}` →
/// hub id, `hub:{id}` → marker.
pub struct NatsRoutes {
    kv: async_nats::jetstream::kv::Store,
}

impl NatsRoutes {
    pub async fn connect(context: &async_nats::jetstream::Context) -> anyhow::Result<Self> {
        let kv = match context.get_key_value(KV_BUCKET).await {
            Ok(kv) => kv,
            Err(_) => {
                context
                    .create_key_value(async_nats::jetstream::kv::Config {
                        bucket: KV_BUCKET.into(),
                        ..Default::default()
                    })
                    .await?
            }
        };
        Ok(Self { kv })
    }
}

#[async_trait]
impl RouteTable for NatsRoutes {
    async fn bind_user(&self, user: &str, hub_id: &str) -> anyhow::Result<()> {
        self.kv
            .put(format!("route:{user}"), hub_id.as_bytes().to_vec().into())
            .await?;
        Ok(())
    }

    async fn unbind_user(&self, user: &str, hub_id: &str) -> anyhow::Result<()> {
        // only the hub currently owning the route may clear it (a second
        // device's hub must not delete a newer binding)
        if let Some(cur) = self.kv.get(format!("route:{user}")).await? {
            if cur.as_ref() == hub_id.as_bytes() {
                self.kv.delete(format!("route:{user}")).await?;
            }
        }
        Ok(())
    }

    async fn route_of(&self, user: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .kv
            .get(format!("route:{user}"))
            .await?
            .map(|b| String::from_utf8_lossy(&b).into_owned()))
    }

    async fn register_hub(&self, hub_id: &str) -> anyhow::Result<()> {
        self.kv
            .put(format!("hub:{hub_id}"), b"1".to_vec().into())
            .await?;
        Ok(())
    }

    async fn unregister_hub(&self, hub_id: &str) -> anyhow::Result<()> {
        self.kv.delete(format!("hub:{hub_id}")).await?;
        Ok(())
    }

    async fn list_hubs(&self) -> anyhow::Result<Vec<String>> {
        let mut keys = self.kv.keys().await?;
        let mut hubs = Vec::new();
        use futures_util::StreamExt as _;
        while let Some(key) = keys.next().await {
            let key = key?;
            if let Some(id) = key.strip_prefix("hub:") {
                hubs.push(id.to_string());
            }
        }
        hubs.sort();
        Ok(hubs)
    }
}
