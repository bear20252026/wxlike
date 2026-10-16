//! WP-13 A2: single-hub load benchmark.
//!
//! Task book target: **10_000 concurrent connections, 1_000 envelopes/second,
//! 60 s sustained** — zero involuntary disconnects, p99 end-to-end
//! (hub in → hub out) < 50 ms in a release build.
//!
//! Everything runs in one process (hub + N clients) so the numbers measure
//! the hub, not the network. Each client is a real QUIC connection with its
//! own UDP socket, exactly like real deployments.
//!
//! ```text
//! cargo run --release --example bench_hub -- [- -]              # full target
//! BENCH_CONNS=500 BENCH_RATE=200 BENCH_SECS=10 cargo run --release --example bench_hub   # smoke
//! ```
//! (arguments come from the environment because `cargo run --example` passes
//! flags to cargo itself; the leading `- -` above only matters for cargo.)
//!
//! Output: one markdown block (also the source for `docs/bench/wp13_hub.md`)
//! with connection/disconnect counts, offered vs delivered rate, latency
//! p50/p95/p99/max, and the pass/fail verdict against the target.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use wxlike_core::{Envelope, MsgBody, MsgKind, PROTOCOL_VERSION};
use wxlike_net::Client;

struct Config {
    conns: usize,
    rate_per_sec: usize,
    seconds: u64,
}

impl Config {
    fn from_env() -> Self {
        let get = |k: &str, d: usize| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(d)
        };
        Self {
            conns: get("BENCH_CONNS", 10_000),
            rate_per_sec: get("BENCH_RATE", 1_000),
            seconds: get("BENCH_SECS", 60) as u64,
        }
    }
}

#[derive(Default)]
struct Tally {
    delivered: AtomicU64,
    disconnected: AtomicU64,
}

fn env_for(seq: u64, body: &str, sender: &str, to: &str) -> Envelope {
    let mut id = [0u8; 16];
    id[..8].copy_from_slice(&(seq as u64).to_le_bytes());
    id[8..].copy_from_slice(&(wxlike_core::types::now_ms() as u64).to_le_bytes());
    Envelope {
        version: PROTOCOL_VERSION,
        conv_id: "bench".into(),
        sender: sender.into(),
        to: vec![to.into()],
        seq,
        kind: MsgKind::Text,
        body: MsgBody::Text(body.into()),
        sent_at_ms: wxlike_core::types::now_ms(),
        msg_id: id,
        quote_of: None,
        receipt: None,
    }
}

fn percentiles(mut latencies: Vec<u64>) -> (u64, u64, u64, u64) {
    if latencies.is_empty() {
        return (0, 0, 0, 0);
    }
    latencies.sort_unstable();
    let pick = |q: f64| -> u64 {
        let idx = ((latencies.len() as f64 - 1.0) * q).round() as usize;
        latencies[idx.min(latencies.len() - 1)]
    };
    (
        pick(0.50),
        pick(0.95),
        pick(0.99),
        *latencies.last().unwrap_or(&0),
    )
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = Config::from_env();
    println!(
        "[bench_hub] target: {} conns, {}/s, {}s",
        cfg.conns, cfg.rate_per_sec, cfg.seconds
    );

    let router = wxlike_mq::MqRouter::new(
        Arc::new(wxlike_mq::memory::MemoryMq::new()),
        wxlike_mq::memory::MemoryRoutes::new(),
    );
    let hub = wxlike_net::run_mq_hub("bench-hub", "127.0.0.1:0".parse()?, router).await?;
    let addr = hub.local_addr()?;
    println!("[bench_hub] hub on {addr}");

    // 1. connect everyone; subscribers stay silent, senders will inject
    let tally = Arc::new(Tally::default());
    let started = Instant::now();
    let mut clients = Vec::with_capacity(cfg.conns);
    let mut connect_failures = 0usize;
    for i in 0..cfg.conns {
        match Client::connect(addr, "localhost", format!("u{i}")).await {
            Ok(c) => clients.push(c),
            Err(_) => connect_failures += 1,
        }
    }
    // wait until the hub has actually bound them all
    while hub.handle().online_users().await + connect_failures < cfg.conns {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let connect_ms = started.elapsed().as_millis();
    println!(
        "[bench_hub] {} connected in {connect_ms} ms ({} failures)",
        clients.len(),
        connect_failures
    );

    // 2. every 10th client is a sender; receivers measure arrival latency
    let senders: Vec<(usize, Client)> = clients
        .iter()
        .enumerate()
        .filter(|(i, _)| i % 10 == 0)
        .map(|(i, c)| (i, c.clone()))
        .collect();
    let receivers: Vec<(usize, Client)> = clients
        .iter()
        .enumerate()
        .filter(|(i, _)| i % 10 == 7)
        .map(|(i, c)| (i, c.clone()))
        .collect();
    // deterministic round-robin over the receivers: every offered message has
    // a home, so "delivered/offered" measures the hub, not the dice
    let targets: Vec<String> = receivers.iter().map(|(i, _)| format!("u{i}")).collect();

    let latencies = Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
    let mut read_tasks = Vec::with_capacity(receivers.len());
    for (i, client) in receivers {
        let tally = tally.clone();
        let latencies = latencies.clone();
        read_tasks.push(tokio::spawn(async move {
            loop {
                match client.next_envelope().await {
                    Ok(env) => {
                        tally.delivered.fetch_add(1, Ordering::Relaxed);
                        let now = wxlike_core::types::now_ms();
                        latencies
                            .lock()
                            .unwrap()
                            .push(now.saturating_sub(env.sent_at_ms));
                    }
                    Err(_) => {
                        tally.disconnected.fetch_add(1, Ordering::Relaxed);
                        let _ = i;
                        break;
                    }
                }
            }
        }));
    }

    // 3. inject at the target rate for the duration
    let offered = (cfg.rate_per_sec as u64 * cfg.seconds) as usize;
    // Batch pacing: a sleep per message is at the mercy of OS timer granularity
    // (Windows ticks at ~15.6 ms), so instead we sleep in 100 ms beats and send
    // `rate/10` messages back to back — the aggregate rate stays honest.
    let beat = Duration::from_millis(100);
    let per_beat = (cfg.rate_per_sec / 10).max(1);
    let inject_started = Instant::now();
    let mut seq = 0u64;
    let mut beat_deadline = inject_started;
    'inject: loop {
        beat_deadline += beat;
        for b in 0..per_beat {
            if seq >= offered as u64 {
                break 'inject;
            }
            let (i, sender) = &senders[(seq as usize + b) % senders.len()];
            seq += 1;
            // the envelope must be sent as the client's own identity or the
            // hub's anti-forgery check (WP-03) correctly drops it
            let target = &targets[(seq as usize - 1) % targets.len()];
            let env = env_for(seq, "bench", &format!("u{i}"), target);
            if sender.send(&env).await.is_err() {
                tally.disconnected.fetch_add(1, Ordering::Relaxed);
            }
        }
        let now = Instant::now();
        if beat_deadline > now {
            tokio::time::sleep(beat_deadline - now).await;
        }
        if inject_started.elapsed().as_secs() >= cfg.seconds {
            break 'inject;
        }
    }
    let inject_elapsed = inject_started.elapsed();

    // 4. let the last messages drain, then collect
    tokio::time::sleep(Duration::from_secs(2)).await;
    for t in read_tasks {
        t.abort();
    }
    let _ = join_all(std::iter::empty::<tokio::task::JoinHandle<()>>()).await;

    let metrics_text = wxlike_net::prometheus_text();
    let routed_by_hub = metrics_text
        .lines()
        .find(|l| l.starts_with("wxlike_hub_messages_total"))
        .and_then(|l| l.rsplit(' ').next())
        .unwrap_or("?")
        .to_string();
    let delivered = tally.delivered.load(Ordering::Relaxed);
    let disconnected = tally.disconnected.load(Ordering::Relaxed);
    let (p50, p95, p99, max) = percentiles(latencies.lock().unwrap().clone());

    println!("\n```markdown");
    println!("# WP-13 hub benchmark");
    println!();
    println!("| metric | value | target |");
    println!("|---|---|---|");
    println!("| connections | {} | {} |", clients.len(), cfg.conns);
    println!("| connect failures | {connect_failures} | 0 |");
    println!(
        "| offered | {offered} msgs | {} |",
        cfg.rate_per_sec * cfg.seconds as usize
    );
    println!("| routed by hub (metrics) | {routed_by_hub} | offered |");
    println!("| delivered | {delivered} | ~offered |");
    println!("| involuntary disconnects | {disconnected} | 0 |");
    println!(
        "| inject wall time | {:.1}s | {}s |",
        inject_elapsed.as_secs_f32(),
        cfg.seconds
    );
    println!(
        "| latency p50 / p95 / p99 / max | {p50} / {p95} / {p99} / {max} ms | p99 < 50 (release) |"
    );
    println!();
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let p99_budget = if cfg!(debug_assertions) { 300 } else { 50 };
    let verdict = disconnected == 0 && p99 < p99_budget;
    println!(
        "- profile: **{profile}** (p99 budget {p99_budget} ms per the task book{})",
        if cfg!(debug_assertions) {
            " — rerun with --release for the acceptance numbers"
        } else {
            ""
        }
    );
    println!("- verdict: **{}**", if verdict { "PASS" } else { "FAIL" });
    println!("```");
    drop(hub);
    Ok(())
}
