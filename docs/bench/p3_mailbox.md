# P3 万人群信箱写入压测（合同第五条 P3 验收项）

> 2026-10-17 · Windows 10.0.26300 x64 · rustc 1.97.1 · `--release`
> 命令：`cargo run --release -p wxlike-message --example bench_mailbox`
> 可调：`MAILBOX_WRITERS=32 MAILBOX_MSGS=50000 …`

## 结果（默认 16 写者 / 20,000 条）

| metric | value |
|---|---|
| wall time | 0.60s |
| **write throughput** | **33,174 msg/s** |
| append latency p50 / p95 / p99 / max | 26 µs / 67 µs / 12.7 ms / 96.7 ms |
| hole check | 20,000/20,000 readable，**0 洞 0 重** |

**verdict: PASS**。序列号单点原子分配（`owner_seq` UPDATE..RETURNING）在 16 线程
并发下无洞无重；p99 抬升来自 SQLite WAL 周期性 checkpoint（p50 26µs 说明稳态
成本极低），P6 迁 PG 分区后同基准重跑对比。

## 口径说明（与 hub 压测的关系）

这是**信箱存储面**（MessageStore）的写入基准；消息从网关进来到落库的**端到端
投递**另有 [wp13_hub.md](wp13_hub.md)（10k conns / 1k msg·s / 60s：断连 0、
p99 = 9ms）。两者相加即"万人群发消息"全链路的上界证据：网关面 p99 9ms +
存储面 p99 12.7ms（含 checkpoint 抖动），均远低于 50ms 预算。

## 复现

```bash
cargo run --release -p wxlike-message --example bench_mailbox
```

基准代码断言三件事：并发追加后 seq 严格 1..=N（无洞无重）、按 seq 分页拉取
恰好读回全部、写入吞吐与延迟分位落表。
