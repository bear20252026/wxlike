//! P3 万人群信箱写入压测（合同第五条 P3："万人群信箱写入压测报告"）。
//!
//! 场景：一个 10,000 人频道的信箱，多写者并发追加（模拟全员发言洪峰），
//! 度量纯写入吞吐与延迟分位；随后按 seq 拉取全量验证无洞。
//!
//! ```bash
//! cargo run --release -p wxlike-message --example bench_mailbox
//! MAILBOX_WRITERS=32 MAILBOX_MSGS=50000 cargo run --release -p wxlike-message --example bench_mailbox
//! ```

use std::sync::Arc;
use std::time::Instant;

use wxlike_message::{MessageStore, OwnerType};

fn main() {
    let writers: usize = std::env::var("MAILBOX_WRITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(16);
    let total: usize = std::env::var("MAILBOX_MSGS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);

    let store = Arc::new(MessageStore::open(None).expect("open mailbox"));
    let channel: (OwnerType, String) = (OwnerType::Channel, "c-10k".into());

    // 预热（连接/页分配与业务写入分离），其 seq 不计入压测拉取
    let warm_seq = store
        .append((channel.0, &channel.1), "warmup", "text", "warm", "{}")
        .unwrap()
        .expect("warmup");

    let per_writer = total / writers.max(1);
    let mut handles = Vec::new();
    let started = Instant::now();
    for w in 0..writers {
        let store = store.clone();
        let channel = channel.clone();
        handles.push(std::thread::spawn(move || {
            let mut latencies: Vec<u128> = Vec::with_capacity(per_writer);
            for i in 0..per_writer {
                let t0 = Instant::now();
                let msg_id = format!("w{w}-{i:0>8}");
                store
                    .append(
                        (channel.0, &channel.1),
                        &format!("u{w}"),
                        "text",
                        &msg_id,
                        "{}",
                    )
                    .expect("append");
                latencies.push(t0.elapsed().as_micros());
            }
            latencies
        }));
    }

    let mut all_lat = Vec::with_capacity(total);
    for h in handles {
        all_lat.extend(h.join().expect("writer thread"));
    }
    let wall = started.elapsed();

    // 无洞验证：从预热之后拉回全部，恰为 total 条
    let mut seen = 0u64;
    let mut after = warm_seq;
    loop {
        let (rows, next) = store.fetch((channel.0, &channel.1), after, 1000).unwrap();
        seen += rows.len() as u64;
        match next {
            Some(n) => after = n,
            None => break,
        }
    }
    assert_eq!(seen, total as u64, "no holes: every message readable");

    all_lat.sort_unstable();
    let p = |q: f64| -> u128 {
        let idx = ((all_lat.len() as f64) * q).min((all_lat.len() - 1) as f64) as usize;
        all_lat[idx]
    };
    let rate = total as f64 / wall.as_secs_f64();

    println!("# 万人群信箱写入压测（wxlike-message，SQLite 过渡存储）");
    println!();
    println!("| metric | value |");
    println!("|---|---|");
    println!("| writers | {writers} |");
    println!("| messages | {total} |");
    println!("| wall time | {:.2}s |", wall.as_secs_f64());
    println!("| write throughput | {rate:.0} msg/s |");
    println!(
        "| append latency p50 / p95 / p99 / max | {} / {} / {} / {} µs |",
        p(0.50),
        p(0.95),
        p(0.99),
        all_lat.last().copied().unwrap_or(0)
    );
    println!("| hole check | {seen}/{total} readable, 0 holes |");
    println!();
    println!("说明：以上是**信箱存储面**的写入吞吐（单 SQLite 文件、WAL、单连接互斥）——");
    println!(
        "网关投递吞吐另有 docs/bench/wp13_hub.md 的 hub 压测（10k conns / 1k msg·s / p99 9ms）。"
    );
    println!("P6 PG 分区迁移后同一基准重跑对比；契约是 seq 无洞无重。");
}
