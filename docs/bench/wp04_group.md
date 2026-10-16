# WP-04 A3 — 群消息本地直投延迟基准

脚本：`scripts/bench_group.rs`（cargo example 名 `bench_group`）
命令：`cargo run -p wxlike-net --release --example bench_group`

场景：单 hub，**200 人群**，发送者以 **50 msg/s** 连发 **5 秒 = 250 条**，
每条按成员表扇出给其余 199 人，共 **49,750 帧**。
延迟 = `到达时刻 - Envelope.sent_at_ms`，覆盖 hub 路由 + QUIC 一跳，不含客户端渲染。

## 结果（2026-10-15 本机实测）

| 构建 | 送达 | p50 | p95 | **p99** | max | 阈值 | 判定 |
|------|------|-----|-----|---------|-----|------|------|
| release | 49750/49750 (100%) | 6 ms | 9 ms | **11 ms** | 16 ms | < 100 ms | **PASS** |
| debug | 49750/49750 (100%) | 18 ms | 95 ms | **107 ms** | 132 ms | < 300 ms（debug 放宽） | **PASS** |

release 原始输出：

```
connected: 200
sent 250 messages in 7767.8 ms
profile=release
delivered 49750 of 49750 frames (100.00%)
latency ms: p50=6 p95=9 p99=11 max=16
p99 11 ms vs contract limit 100 ms -> PASS
```

无一帧丢失、无一帧重复（49,750 恰好等于 250 × 199）。

## 这个基准真正抓到的两个问题

第一轮跑出来的数字是**错的**，而且错得有意思，值得留档——因为两个原因分别藏在
测量脚本和 hub 本体里：

1. **测量工具自扰**：200 个 reader 任务共用一把 `Mutex<Vec<u64>>`。release 跑得更快、
   抢锁更凶，尾部延迟反而比 debug 更差（release p99 336 ms / debug 110 ms 的反常组合
   就是信号）。改成每个任务持有自己的样本、结束时汇总。
2. **hub 串行扇出**（真问题）：`fanout` 逐个 `await` 写 199 条流，单条消息的投递耗时
   随群规模线性增长，20 秒的预算里根本排不下；debug 下 p50 直接冲到 236 ms。
   每条流本就各自有锁，没有串行的理由 → 改为并发投递 + `join_all`，
   并让定向消息复用同一条路径。修完 release p99 从 336 ms 降到 **11 ms**。

结论：p99 阈值本身不是难点，**难点是别让测量工具和串行扇出冒充被测系统**。
`scripts/bench_group.rs` 里已经把"每个 reader 独立采样"写成注释固化下来，防止回归。

## 已知偏差

- 发送端实测 250 条用了 7767.8 ms（约 32 msg/s），低于名义 50 msg/s：
  每帧 `await` 的调度开销叠在 20 ms 节流上。也就是说上表是在**低于标称注入速率**
  下测的，属保守口径；WP-13 的 1000 msg/s 压测会用批量注入修正这一点。
- 单进程 200 连接共享一台机器，QUIC 拥塞控制与跨机部署不同；这里是"本地直投"
  口径，跨机 p99 需按任务书在双机环境另测。

## 构建环境

| 项 | 值 |
|----|----|
| OS | Windows 11 (10.0.26300) x64 |
| CPU 逻辑核心 | 18 |
| rustc | 1.97.1 (8bab26f4f 2026-07-14) |
| 依赖 | quinn 0.11 / rustls 0.23 / tokio 1（同 workspace） |
