# UP-ATM0S · `egress.rs` → `crates/rtc/src/sfu.rs`

- 上游：`8xFF/atm0s-media-server` @ `0ffc95a4537633a9916ac8879e953399a9f35bac`
  永久链接：<https://github.com/8xFF/atm0s-media-server/blob/0ffc95a4537633a9916ac8879e953399a9f35bac/packages/media_core/src/endpoint/internal/bitrate_allocator/egress.rs>
- 许可：MIT
- 本地 sha256：`d26e14fe1cc799b2ab187426c081c3dea4f52b74e0c5395227a9ff42d9aae535`
- 形态：**跨库重写**（不是逐行翻译，也不是 copy-paste 后改 import）

## 拿了什么

出口码率分配的算法形状：

- 按优先级权重比例分配，权重 0 视为不分；
- 可用带宽 = `min(估算带宽, 上限)`；
- 有轨道时 BWE 目标 = 当前 × 6/5（封顶），无轨道时用固定兜底值；
- 只在"有变化"时结算（dirty 标记 + tick）。

前 3 个单测是从上游等价移植的（构造同样的权重/带宽组合，断言同样的分配结果）。

## 改了什么

1. 类型换成我们的 `TrackId = u32` 与 `u64` 权重，去掉 `LocalTrackId` / `TrackPriority`；
2. 去掉 `log`、`indexmap`、`sans-io-runtime` 三个依赖：顺序用 `Vec` 保持插入序；
3. 去掉他们的异步运行时耦合，改成我们 `Room::on_tick` 驱动的同步结算。

## 没拿什么

上游的 endpoint/session 生命周期、优先级队列的持久化、以及 sans-io 的定时器抽象。
本项目 `Room` 自己管名册与管控，这套分配器只负责"这一轮给谁多少"。

## 复核方式

`python -m sourceaudit provenance check` 核对 sha256；文件头的来源声明不得删除。
