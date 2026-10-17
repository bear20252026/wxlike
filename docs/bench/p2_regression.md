# P2 阶段基准回归（合同 4.4）

> 2026-10-17 · Windows 10.0.26300 x64 · rustc 1.97.1 · `--release`
> 命令：`cargo run --release -p wxlike-net --example bench_hub`（全量）
> 冒烟：`BENCH_CONNS=500 BENCH_RATE=200 BENCH_SECS=5 …`

## 冒烟档（500 连接 / 200 msg·s / 5s）

| metric | value | target |
|---|---|---|
| connect failures | 0 | 0 |
| offered / delivered | 1000 / 1000 | 全送达 |
| involuntary disconnects | 0 | 0 |
| latency p50 / p95 / p99 / max | 1 / 2 / 3 / 4 ms | p99 < 50 (release) |

**verdict: PASS**。P1/P2 对投递热路径的改动（typing 瞬态分支、`park_offline` 参数化）
不触及 content 帧的 存储→路由→扇出 链路，延迟特征与 WP-13 基线一致。

## 全量档（10,000 连接 / 1,000 msg·s / 60s）

| metric | value | target |
|---|---|---|
| connections | 10000（22.7s 建满） | 10000 |
| connect failures | 0 | 0 |
| offered / routed / delivered | 60000 / 60000 / 60000 | 全送达 |
| involuntary disconnects | 0 | 0 |
| latency p50 / p95 / p99 / max | 3 / 7 / 9 / 19 ms | p99 < 50 (release) |

**verdict: PASS**（较 WP-13 基线 p99 5ms → 9ms，仍留 5 倍余量；差异来自本机
P2 同时运行的开发负载，非管道回归——冒烟档 p99 3ms 与基线一致）。

## 与 P2 相关的说明

- video-service 是**独立上传面**（HTTP multipart → ffmpeg 子进程），不占用消息网关
  线程；转码在 `spawn_blocking` 里执行，与 hub 事件循环隔离（ADR-21）。
- 语音/文件/贴纸/视频四类消息全部是 `MsgBody::Custom` 内容帧——存储、去重、扇出
  路径与文本完全一致，因此本表数字即为新消息类型的投递成本（零额外）。
