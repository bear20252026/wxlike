//! WP-04 A3 benchmark: a 200-member group, 50 messages/s, measuring local
//! direct-delivery latency from the hub out to each member.
//!
//! ```text
//! cargo run -p wxlike-net --release --example bench_group
//! ```
//!
//! Latency is `arrival - Envelope.sent_at_ms`, so it covers hub routing plus
//! the QUIC hop and excludes client-side queueing effects.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use wxlike_core::{now_ms, Envelope};
use wxlike_mq::memory::{MemoryMq, MemoryRoutes};
use wxlike_mq::MqRouter;
use wxlike_net::{run_mq_hub_with, Client, Membership, MqHubOptions};

const MEMBERS: usize = 200;
const MSGS_PER_SEC: u64 = 50;
const SECONDS: u64 = 5;

struct AllMembers(Arc<RwLock<HashMap<String, Vec<String>>>>);

#[async_trait::async_trait]
impl Membership for AllMembers {
    async fn members_of(&self, conv_id: &str) -> anyhow::Result<Option<Vec<String>>> {
        Ok(self.0.read().await.get(conv_id).cloned())
    }
}

fn percentile(sorted: &[u64], pct: u64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as u64 * pct) / 100).min(sorted.len() as u64 - 1) as usize;
    sorted[idx]
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let membership = Arc::new(RwLock::new(HashMap::new()));
    let ids: Vec<String> = (0..MEMBERS).map(|i| format!("u{i:03}")).collect();
    membership
        .write()
        .await
        .insert("bench-group".into(), ids.clone());

    let router = MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new());
    let hub = run_mq_hub_with(
        "hub-bench",
        "127.0.0.1:0".parse()?,
        router,
        MqHubOptions {
            membership: Some(Arc::new(AllMembers(membership))),
            // 200 members x 5 s is a lot of parked frames if delivery lags
            offline: wxlike_net::offline::MemoryOffline::new(MEMBERS * 100),
            ..Default::default()
        },
    )
    .await?;
    let addr = hub.local_addr()?;
    println!("hub on {addr}; {MEMBERS} members, {MSGS_PER_SEC} msg/s for {SECONDS}s");

    let mut clients = Vec::with_capacity(MEMBERS);
    for id in &ids {
        clients.push(Client::connect(addr, "localhost", id.clone()).await?);
    }
    // let every Hello bind settle before timing anything
    tokio::time::sleep(Duration::from_secs(1)).await;
    println!("connected: {}", hub.handle().online_users().await);

    // Each reader owns its samples and hands them over when asked to stop.
    // Sharing one mutex across 200 tasks measures the harness instead of the
    // hub — an earlier version did that and made release look worse than debug.
    let stop = Arc::new(AtomicBool::new(false));
    let mut readers = Vec::with_capacity(MEMBERS - 1);
    for client in clients.iter().skip(1) {
        let client = client.clone();
        let stop = stop.clone();
        readers.push(tokio::spawn(async move {
            let mut samples: Vec<u64> = Vec::with_capacity(300);
            while !stop.load(Ordering::Relaxed) {
                match tokio::time::timeout(Duration::from_millis(200), client.next_envelope()).await
                {
                    Ok(Ok(env)) => samples.push(now_ms().saturating_sub(env.sent_at_ms)),
                    Ok(Err(_)) => break,
                    Err(_) => continue,
                }
            }
            samples
        }));
    }

    let total = MSGS_PER_SEC * SECONDS;
    let sender = &clients[0];
    let started = std::time::Instant::now();
    for i in 0..total {
        sender
            .send(&Envelope::text(
                "bench-group",
                ids[0].clone(),
                i,
                "bench payload",
            ))
            .await?;
        tokio::time::sleep(Duration::from_micros(1_000_000 / MSGS_PER_SEC)).await;
    }
    let sent_in = started.elapsed();
    println!(
        "sent {total} messages in {:.1} ms",
        sent_in.as_secs_f64() * 1000.0
    );

    // let in-flight frames land, then stop the readers and merge
    tokio::time::sleep(Duration::from_secs(3)).await;
    stop.store(true, Ordering::Relaxed);
    let mut sorted: Vec<u64> = Vec::new();
    for reader in readers {
        sorted.extend(reader.await?);
    }
    sorted.sort_unstable();

    let expected = total as usize * (MEMBERS - 1);
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let limit = if cfg!(debug_assertions) { 300 } else { 100 };
    let p99 = percentile(&sorted, 99);

    println!("profile={profile}");
    println!(
        "delivered {} of {expected} frames ({:.2}%)",
        sorted.len(),
        100.0 * sorted.len() as f64 / expected as f64
    );
    println!(
        "latency ms: p50={} p95={} p99={} max={}",
        percentile(&sorted, 50),
        percentile(&sorted, 95),
        p99,
        sorted.last().copied().unwrap_or(0)
    );
    println!(
        "p99 {p99} ms vs contract limit {limit} ms -> {}",
        if p99 <= limit { "PASS" } else { "FAIL" }
    );
    if sorted.len() < expected {
        println!(
            "note: {} frames not observed (reader timed out early or write failed)",
            expected - sorted.len()
        );
    }
    Ok(())
}
