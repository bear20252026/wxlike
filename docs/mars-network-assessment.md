# Mars 评估：能借什么、不能借什么（2026-10-20）

> 上游：https://github.com/Tencent/mars
> 本地镜像：`refs/mars`（浅克隆 + sparse-checkout：`mars/comm`、`mars/stn`、`mars/sdt`、
> `mars/xlog`、`samples`，9 MB）
> 许可：**MIT**（`LICENSE` 首段写明源码与二进制均为 MIT；但捆绑 OpenSSL / Boost /
> Apache-2.0 / Zlib / BSD 第三方，需叠加遵守——见文末）

## 一、结论先行

**值得借的是"移动网络上的调度策略"，不是"网络库本身"。**

Mars 的核心 STN 是**客户端**信令层，服务端接入层不开源（sample 直连腾讯的
`marsopen.cn`）。所以照搬 STN 等于自建一套私有长连协议 + 服务端，而我们已经有
QUIC hub。真正可搬的是它踩了十几年移动网络坑之后固化的**参数与状态机**：

| 机制 | 值不值得搬 | 我们的现状 |
|---|---|---|
| 智能心跳（3.5–10 分钟自适应） | **值得**，且我们**更差** | QUIC 用 quinn 默认：**30s 空闲即断、无 keepalive** |
| 长短连一体（任务级兜底） | 值得（思想） | QUIC 单通道 + HTTP 控制面，无"任务级降级" |
| 防雪崩（频率 + 流量双漏斗） | 值得（思想） | 只有服务端风控，客户端无 |
| 动态超时（包大小 × 网络类型） | 值得（参数表） | 无 |
| WiFi / GPRS 双套参数 | 值得（参数表） | 无（一套参数走天下） |
| 僵尸任务（发送队列持久化） | 值得 | **无**：进程重启后未发出的消息没有队列 |
| Xlog（mmap + 压缩 + 加密 + 自动清理） | 移动端值得 | 服务端交 launchd/journal 够用；移动端不够 |
| SDT（DNS/HTTP/Ping/TCP 诊断） | 值得（做"网络诊断"页） | 无 |
| Comm 的协程 / 消息队列 / socket 封装 | **不需要** | tokio 已覆盖 |

## 二、核实的事实

| 项 | 值 |
|---|---|
| Star / Fork | 17,661 / 3,689 |
| 语言 / 许可 | C++ / **MIT**（GitHub 标 NOASSERTION，因为 LICENSE 里混了多份第三方许可） |
| 代码量（`mars/` 下 .cc/.h） | comm 224 文件 44,508 行；stn 98 文件 27,509 行；sdt 40 文件 5,599 行；xlog 33 文件 7,015 行 |
| 最近提交 | **2025-09-01**（再往前 2024-12-03）——近一年基本停更，514 个 open issue |
| 平台 | iOS / macOS / Android(NDK r20) / Windows(VS2015)；构建脚本要求 Python ≥ 3.10 |
| 模块 | `comm`（socket/线程/MQ/协程）、`xlog`（日志）、`sdt`（诊断）、`stn`（信令网络，主体） |

README 自述定位："Suitable for small amount data transmission"、"Mobile platform
friendly, low power and traffic consumption"——它优化的是**手机上的小包信令**，
不是大文件吞吐。这一点和我们的 IM 消息面吻合，和媒体面不吻合。

## 三、五个可以直接照抄的机制（含真实常量）

### 1. 智能心跳：按网络类型持久化的自适应区间

`mars/stn/config.h`：

```c
#define MinHeartInterval (3 * 60 * 1000 + 30 * 1000)  // 3.5 分钟
#define MaxHeartInterval (10 * 60 * 1000)             // 10 分钟
#define HeartStep        (60 * 1000)                  // 每次探测 +1 分钟
#define SuccessStep      (20 * 1000)                  // 收敛时退 20s 作为稳定值
#define BaseSuccCount    (5)                          // 连续 5 次成功才算稳
#define MaxHeartFailCount (2)
#define NetStableTestCount (3)                        // 用最短间隔测 3 次
```

算法（`stn/src/smart_heartbeat.cc`）：

1. 未稳定时一律用 `MinHeartInterval`（3.5 分钟）；
2. 连续 5 次成功后 `cur_heart += HeartStep`（+1 分钟），把 `is_stable_` 打回 false 重测；
3. 到达 `MaxHeartInterval - SuccessStep`（9 分 40 秒）就锁定为稳定值；
4. 稳定后**每 7 天**再往上探一次（`last_modify_time_` 判断），防止网络变好却一直用短心跳；
5. 失败 2 次 → 回落到最短；**心跳超时**才清零成功计数（普通失败不清零）；
6. 心跳结果按 `net_detail_`（网络指纹，如 `wifi-ssid` / `gprs-运营商`）**落 INI 持久化**——
   换网络重新探测，回到旧网络直接复用上次的稳定值。

最后一条是最容易被忽略也最值钱的：**心跳参数是"每个网络一份"的，不是全局一份**。

它还有一段专门处理 MIUI 对齐 5 分钟闹钟的 doze 逻辑（`__IsDozeStyle()`），
把心跳直接跳到 `MaxHeartInterval - SuccessStep`。

### 2. 长短连一体：任务在两条通道间自动降级

`stn/src/net_core.cc` 里有三个任务管理器：

```
LongLinkTaskManager   长连优先
ShortLinkTaskManager  长连不可用时的 HTTP 兜底
ZombieTaskManager     跨进程重启的任务持久化
```

`net_core.cc` 的 `StartTask` 先试长连；长连未就绪或任务被判定"长连不适合"就走短连。
调用方只提交 `Task`，不关心走哪条路。`kCallFromLong / kCallFromShort / kCallFromZombie`
标记结果来源，回调签名统一。

对我们的意义：现在 `msg.send` 走 QUIC、`conv.unread` 走 HTTP，是**两套调用路径**；
Mars 的做法是"一个任务队列 + 两条通道"，调用方不该知道通道。

### 3. 防雪崩：频率漏斗 + 流量漏斗

`stn/src/anti_avalanche.{h,cc}` + `frequency_limit` + `flow_limit`：

- `FrequencyLimit`：同一 CGI 单位时间内的请求数上限；
- `FlowLimit`：滑动窗口算"漏斗速度"，限制单位时间字节数；
- 两者都只在**非活跃态**生效（`Active()` 打开时全放行——用户主动操作不该被限）。

我们的服务端有 `wxlike_risk`（按用户/来源限流），但**客户端没有**。
弱网下重连风暴 + 重试风暴会把手机流量打光，服务端限流只会让它更慢。

### 4. 动态超时：按包大小 × 网络类型查表

`stn/config.h` 两套完整参数表（WiFi / GPRS）：

| 参数 | WiFi | GPRS |
|---|---|---|
| 首包超时 | 12s | 15s |
| 首包超时上限 | 22s | 30s |
| 最小速率 | 12 KB/s | 4 KB/s |
| 包间隔 | 8s | 12s |
| 任务延迟 | 1.5s | 3s |

动态超时（`stn/src/dynamic_timeout.{h,cc}`）按包大小分四档（3K / 10K / 30K / 更大），
每档给 WiFi 与 GPRS 各自的期望耗时（0.5s / 2s / 4s / 6s vs 1s / 3s / 5s / 7s），
再用 10 位 bitset 记最近 10 次失败、连续成功计数，在 `kEValuating / kExcellent / kBad`
三态间切换。**同一份代码，WiFi 和 4G 用两套数**——这是移动 IM 和桌面 IM 最大的差别。

### 5. 僵尸任务：进程被杀，任务不丢

`stn/src/zombie_task_manager.{h,cc}`：`SaveTask / StopTask / HasTask / ClearTasks`，
任务带 `taskcosttime` 落盘，进程重启后重新投递。

我们的现状：`crates/message` 的信箱是**服务端**的顺序权威；客户端本地
`core::Store` 存的是"已收到/已发出"的消息，**没有"待发出"队列**。
用户在地铁里发一条消息、进程被系统杀掉，那条消息就没了。

## 四、三处必须偏离（照抄会错）

**1. 服务端不开源，STN 只能搬策略不能搬协议。**
sample 里写死 `host = "marsopen.cn"`（`samples/android/marsSampleChat/.../TextMessageTask.java`）。
没有开源接入层，就没有可对接的 STN 服务端。我们已有 QUIC hub，正确做法是
**把 Mars 的调度策略套到我们自己的通道上**，而不是引入 STN。

**2. C++ 与 boost 生态，Rust 侧只重写不移植。**
`comm` 里有自研协程（`comm/coroutine`）、`MessageQueue`、`Alarm`、`socketselect`——
这些在 tokio 里都有等价物，逐行翻译只会得到一个更差的 tokio。
只借**参数与判定逻辑**，不借实现。

**3. 它是 TCP 长连 + HTTP 短连，我们是 QUIC。**
QUIC 自带多路复用、0-RTT、连接迁移，所以 Mars 里"短连兜底""首包超时""包间隔"
这些为 TCP 队头阻塞设计的东西，在 QUIC 上大部分不需要。
真正仍然需要的是：**keepalive（QUIC 也不管 NAT 超时）**、**心跳区间**、
**发送队列持久化**、**限流**、**动态超时**。

## 五、我们的真实差距（grep 证据）

| 缺口 | 证据 | 后果 |
|---|---|---|
| **QUIC 无 keepalive、无 idle 配置** | `crates/net/src/transport.rs` 只 `ClientConfig::new(...)`，全仓无 `TransportConfig`；quinn 0.11 默认 `max_idle_timeout = 30s`、`keep_alive_interval = None`（`quinn-proto-0.11.15/src/config/transport.rs:365,381`） | 手机进后台 30 秒连接即断；NAT 映射 30–120 秒过期，回来必然重连。Mars 的心跳是 **3.5–10 分钟** |
| 重连退避上限 2s | `crates/net/src/client.rs`：`200ms → ×2 → 上限 2s` | 服务端维护期会变成 2 秒一次的重连风暴，没有"防雪崩" |
| 无发送队列持久化 | 全仓无 outbox / pending 表 | 进程被杀 = 未发出的消息丢失 |
| 无网络类型感知 | 无 `net_type` / `wifi` / `cellular` 判断 | 一套超时走天下，弱网下大量误判超时 |
| 无网络诊断 | 无 ping / DNS / TCP 探测 | 用户报"连不上"时只能看日志猜 |

## 六、建议的三步

### P0 ✅ 已落地（2026-10-20）：QUIC 保活与空闲策略

`crates/net/src/transport.rs` 新增 `transport_config()`，服务端与两个客户端
（TOFU / insecure）都用它：

```rust
cfg.keep_alive_interval(Some(Duration::from_secs(30)));   // 空闲时 PING，约 2 包/分钟
cfg.max_idle_timeout(Some(Duration::from_secs(90)));      // 3 个心跳周期的余量
```

为什么是 30s / 90s：Mars 的 3.5–10 分钟是 **TCP 长连**的应用层心跳；QUIC 没有
TCP keepalive 选项，只能靠 PING 帧维持 NAT 映射，所以取更保守的值。90s 而不是
30s，是为了让"单个 PING 丢了"不至于直接断连。

验收 `crates/net/tests/keepalive.rs`：连接后静置 **40 秒**（超过 quinn 默认的
30s idle），断言 `close_reason().is_none()` 且还能开流写数据。

> 这条测试在修复前跑过：`空闲 40s 后连接被关闭：Some(TimedOut)`。
> 也就是说它不是"写了就绿"的装饰——**先让它红过**。

### P1（中）：客户端发送队列 + 任务级重试

参照 `zombie_task_manager` 的形状（`SaveTask/StopTask/HasTask` + 成本计时），
在客户端本地库加一张 `outbox(task_id, conv, payload, attempts, next_retry_at)`，
启动时重放。重试间隔照抄 `DEF_TASK_RETRY_INTERNAL = 1s`、`DEF_TASK_RETRY_COUNT = 1`
（Mars 的任务级重试只重 1 次，剩下靠上层——这点很克制，值得学）。

### P2（中）：网络诊断页（SDT 的形状）

`dnschecker / httpchecker / pingchecker / tcpchecker` 四个 checker + 一份报告，
对应我们：DNS 解析 hub 域名 → TCP/UDP 可达性 → HTTP `/health` → QUIC 握手。
用户点"网络诊断"能看到"卡在哪一跳"，而不是"发送失败"。

心跳自适应（3.5–10 分钟、按网络指纹持久化）放在 P1 之后——它依赖"网络类型识别"，
而我们目前连这个都还没有。

## 七、许可结论

- Mars 自身代码 **MIT**，并入 AGPL-3.0 项目合法（MIT → AGPL 是允许方向），
  须保留版权声明与许可全文。
- 但它捆绑 OpenSSL / Boost / Apache-2.0 / Zlib / BSD，`LICENSE` 明确要求叠加遵守；
  OpenSSL 还有强制致谢与命名限制。
- **本评估只借设计，不 vendoring 任何 C++ 源码**，因此不触发上述第三方义务。
  若将来真要复制片段（例如某个退避公式的注释），按 `THIRD-PARTY-NOTICES.md`
  的文件级规则登记 MIT 来源即可。
