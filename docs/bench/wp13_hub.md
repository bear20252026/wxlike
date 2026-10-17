# WP-13 hub 压测报告

任务书目标：**单 hub 10_000 并发连接、每秒 1_000 条 Envelope、持续 60 s；
断连率 0；p99 端到端（hub 入→出）< 50 ms（release 构建）**。

## 结果（2026-10-16，全量目标运行）

| metric | value | target |
|---|---|---|
| connections | 10 000 | 10 000 |
| connect failures | 0 | 0 |
| offered | 60 000 msgs | 1 000/s × 60 s |
| routed by hub (`wxlike_hub_messages_total`) | 60 000 | = offered |
| delivered (receiver-side) | 60 000 | ~offered |
| involuntary disconnects | **0** | 0 |
| inject wall time | 60.0 s | 60 s |
| latency p50 / p95 / p99 / max | **3 / 4 / 5 / 9 ms** | p99 < 50 ms |
| verdict | **PASS** | |

时延 = 接收端读到的 `sent_at_ms` 与本地时钟差（同一进程内同源时钟，无跨机偏移）。
hub 侧 `wxlike_hub_messages_total` 与接收侧计数一致，说明无静默丢弃。

## 机器配置

| 项 | 值 |
|---|---|
| OS | Windows 11 家庭版（x64，10.0.26300） |
| CPU | Intel Core Ultra 5 135H（1 路，异构 12 核 14 线程档） |
| 内存 | 24 GB（23.4 GB 可见） |
| 构建 | `cargo run --release`（rustc stable，优化档，无 LTO 特调） |
| 网络 | 全部回环（hub 与 10 000 客户端同进程，测的是 hub 本身而非物理网络） |

## 复现

```bash
# 全量目标（同本报告）
cargo run --release -p wxlike-net --example bench_hub

# 冒烟（小规模快速验证）
BENCH_CONNS=300 BENCH_RATE=200 BENCH_SECS=8 cargo run --release -p wxlike-net --example bench_hub
```

环境变量：`BENCH_CONNS`（默认 10 000）、`BENCH_RATE`（默认 1 000/s）、
`BENCH_SECS`（默认 60）。脚本输出即本文件表格的来源（`docs/bench/wp13_hub_raw.txt`
为原始运行输出）。

## 说明与坑

- **注入步调用 100 ms 批量节拍**：按消息 `sleep(1/rate)` 在 Windows 上会被
  ~15.6 ms 的系统定时器粒度放大（实测速率掉到目标的 1/3），批量步调把聚合
  速率做准，且不影响 hub 的时延测量。
- **消息必须以客户端自己的身份发送**：脚本最初伪造 sender 被 hub 的 WP-03
  防伪造校验正确丢弃（delivered=0）——这本身是对该机制的意外验证。
- debug 构建的 p99 预算按任务书放宽到 300 ms；本报告数字为 release。
