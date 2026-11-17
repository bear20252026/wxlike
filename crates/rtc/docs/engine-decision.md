# G09-01 RTC 引擎决策与兼容矩阵（ADR-25 修订）

状态：**str0m 冷备路径已实现并通过本机自动化验收；LiveKit 线上接入未完成，
真实设备项见文末待验清单。**
决策日期：2026-10-24（str0m 采用）；**2026-10-28 ADR-25 改定 LiveKit，
2026-11-05 所有者确认执行**。适用范围：全部客户端（桌面浏览器、Android、
iOS、Flutter 移动/桌面、CI 探针）。

## 1. 结论（ADR-25 修订）

全平台仍然只保留**一套房间事实**，但线上引擎换为 LiveKit sidecar：

```text
signaling（QUIC envelope / rtc-signal）
        │
        ▼
LiveKit sidecar（线上：媒体转发 + 房间状态；独立进程）
        │
        └── 本仓库 Room 名册/授权事实 + LiveKit token（短时绑定 user/device/call）

冷备（代码保留，仅对照与回滚演练，不许拥有在线房间）：
str0m MediaSession（纯 Rust，sans-I/O，DTLS-SRTP）+ 本仓库 SFU
```

* 配置门禁 `compat::approved_engine` 白名单只接受 `livekit`（线上，缺省）
  与 `str0m`（冷备）；未知引擎直接失败报 `Unknown`、拒绝启动
  （`crates/rtc/src/compat.rs`）。
* **单一在线闸继续有效**：`room::EngineGate` 保证同一时刻只有一个引擎
  在线——线上 LiveKit 时冷备 str0m 不许拥有房间（`ColdStandby`）；回滚
  演练把冷备升为在线后 LiveKit 反过来不许开房间（`AlreadyOnline`）。
  任何第二个引擎同时在线都意味着两套房间事实（成员、静音、录制、计费
  无法对齐），再换引擎仍须新的全局 ADR。
* UI 组不得各自接入不同的 RTC 服务；跨端差异只能通过原生桥补能力，
  不能换协议栈。
* **接线现状（诚实标注）**：LiveKit token（`token/livekit.rs`）与房间/
  参与者状态机（`room.rs`，移植自上游并逐文件审查）已落地并有测试；
  sidecar 部署与四端 SDK 媒体接入**未完成**，现有 `/api/call/*` 路由仍走
  str0m 冷备路径（`Room::new`），切换需求登记在
  `integration-requests/G09/`。在切换完成前不得宣称线上通话已用 LiveKit。

## 2. 候选比较（2026-10-24 原评估，ADR-25 修订结论）

| 候选 | 语言/形态 | 与本仓库的契合度 | 结论 |
|---|---|---|---|
| str0m + 自建 SFU | 纯 Rust，无 C++ 绑定 | 已有 P2P/SFU/QUIC 信令与测试基座；符合任务书"禁 libwebrtc C++ 绑定" | 原采用；**ADR-25 转冷备** |
| **LiveKit** | Go 服务 + 多语言 SDK | 功能完整；房间/鉴权/Token 体系经逐文件审查移植为 Rust 状态机 + token（`UP-LIVEKIT-RUSTSDK`），媒体面以 sidecar 独立进程接入，不拼两套房间事实 | **ADR-25 采用（2026-11-05 确认执行）** |
| mediasoup | C++ worker | 违背纯 Rust 约束，构建与崩溃面扩大 | 排除 |
| Janus | C 插件 | 同上，且 SFU 转发策略需重写 | 排除 |
| Pion | Go | 客户端无法复用，服务端与现有 Rust 房间模型割裂 | 排除 |

ADR-25 的取舍：str0m 的媒体协议栈继续作为冷备与回滚演练保留；线上媒体
转发与房间状态交给 LiveKit sidecar，本仓库只持有名册/授权/计费事实与
短时 token，避免自维护 SFU 的长期运维面。移植边界与许可审查见
`third_party/reviews/registry.json`（`UP-LIVEKIT-RUSTSDK`）。

研究锚点（不复制代码）：LiveKit commit `9644b99…`、coturn commit
`596c48b…`、Nextcloud Talk commit `1fd0f21…`、Signal-Desktop commit
`17502b9…`。仓库为 AGPL-3.0-only，第三方代码逐文件审查后才可移植。

## 3. 兼容矩阵

`crates/rtc/src/compat.rs::support` 是唯一规则源，文档与测试都从这里取。
`native` = 本仓库直接实现；`bridge` = 存在但必须等原生桥；`blocked` =
该端不可行且必须上报。

| 能力 | 桌面浏览器 | Android | iOS | Flutter 移动 | Flutter 桌面 | CI 探针 |
|---|---|---|---|---|---|---|
| voice | native | native | native | native | native | bridge(G16 无声卡) |
| video | native | native | native | native | native | blocked(无摄像头) |
| screen_share | native | native | native | bridge(G12 插件) | bridge(G12 插件) | blocked |
| audio_route | native | bridge(G10 AudioManager) | bridge(G12 AVAudioSession) | bridge(G12 插件) | native | blocked |
| turn_relay | native | native | native | native | native | native |
| e2ee | native | native | native | native | native | native |
| background_incoming | native | bridge(G10 FGS+FCM) | bridge(G12 CallKit/PushKit) | bridge(G10/G12) | native | blocked |
| live_watch | native | native | native | native | native | native |
| live_publish | native | native | native | native | native | native |

验收四类端（桌面浏览器 / Android / iOS / Flutter 移动）两两组合共 6 对，
`tests/compat.rs` 在每对之间跑真实 ICE/DTLS/SRTP 的双向音频与单向视频。
这只证明协议与媒体栈跨端一致；**真机/浏览器 UI 仍待人工验收**。

## 4. 带宽与耗电预算

发送侧预算（kbps，不是画质承诺）：

| 档位 | 音频 | 视频 | 合计 |
|---|---|---|---|
| audio_only | 40 | 0 | 40 |
| video_360p | 40 | 600 | 640 |
| video_720p | 40 | 1500 | 1540 |
| screen_1080p | 40 | 3000 | 3040 |

耗电预算（mAh/小时，按 4000 mAh 手机估算，仅产品验收用）：

| 端 | audio_only | 360p | 720p | 1080p |
|---|---|---|---|---|
| 移动端（Android/iOS/Flutter） | 140 | 210 | 340 | 500 |
| 桌面端 | 80 | 150 | 280 | 440 |
| CI 探针 | 20 | 110 | 240 | 380 |

`compat.rs::budget` / `power_budget` 与测试保证档位单调、上限不超过 4 Mbps。

## 5. Spike 与实施结论

| 项目 | 结论 | 证据 |
|---|---|---|
| 编解码协商 | Opus + VP8，m-line 由 SDP 事件发现 | `tests/media_loopback.rs`、`tests/compat.rs` |
| 跨 NAT | host candidate 直连；STUN/TURN 由环境变量注入 | `tests/media_loopback.rs`（回环 UDP）、`turn_auth.rs` |
| TURN 凭据 | coturn REST 方案（HMAC-SHA1，短期用户名/密码） | `crates/rtc/src/turn_auth.rs`，单元测试覆盖过期/篡改/配额 |
| 多人转发 | 每参与者一个 UDP socket，服务端不解码 | `crates/sfu/tests/{three_party,media_gates}.rs` |
| iOS 音频路由 | Rust 不可直接控制 AVAudioSession | 阻塞：G12 原生桥 |
| 后台来电 | Android 需 FGS + FCM；iOS 需 CallKit/PushKit | 阻塞：G10/G12 原生桥 |
| 真机四端 | 本机无移动设备/浏览器自动化环境 | **待验**，见下 |

## 6. 待验清单（不得据此标 ACCEPTED）

0. **LiveKit 线上接入（ADR-25）**：sidecar 部署 + token/信令面接线 +
   四端 SDK 媒体接入与跨端解密验证；`/api/call/*` 路由从 str0m 冷备切到
   `Room::livekit` + `EngineGate::production()`。完成前线上通话仍走冷备
   路径，G09 不得标"LiveKit 已上线"。
1. 四类真实客户端两两语音/视频（真机 + 真实浏览器，含权限弹窗）。
2. iOS 听筒/扬声器/蓝牙切换、被其他 App 抢占音频后的恢复。
3. Android/iOS 后台来电响铃与接听（依赖 G10/G12 桥）。
4. 公网 NAT 下强制 TURN 的中继连通与配额（需要部署 coturn）。
5. 移动端 30 分钟通话的耗电与发热实测。

未完成上述项之前，G09-01 只能标"代码与自动化验收完成，集成待验"。
