# P5 进度报告：直播控制面 + 小程序包签名（第一增量）

> 合同第五条 P5 是 8 周量级（Flutter 三端 / 直播 / 小程序 M1–M5 / 点播完整版 /
> Web 工程 / 多语言 / 数据导出 / 国内厂商推送）。本文件记录**已交付的第一增量**
> 与**明确的环境受限项**，不做"已全部完成"的表述。

## 1. 本轮交付（可执行、可验证）

| 交付 | 代码 | 测试 | 状态 |
|---|---|---|---|
| video 多分辨率阶梯（360/720/1080 + master playlist） | `crates/video/src/lib.rs` | 1（真实转码） | ✅ |
| 直播会话控制面（开播授权/停播/状态） | `crates/platform/src/live.rs` | 9 | ✅ |
| RTMP 推流凭据（HMAC 自包含、过期/篡改/停播即拒） | 同上 | 同上 | ✅ |
| 观众计数与峰值 | 同上（`live_viewers`） | 同上 | ✅ |
| 弹幕路由（只发在房观众，不落库、不占序列号） | 同上 | 同上 | ✅ |
| 小程序包签名（ed25519）+ 防降级 + 权限白名单 | `crates/container/src/package.rs` | 7 | ✅ |
| 消息索引：**借用 tweb `SlicedArray`**（历史分页合并 + 实时插入） | `ui/src/vendor/tweb/slicedArray.ts` · `ui/src/msgIndex.ts` | 7（node 断言） | ✅ |
| **通话房间 HTTP 控制面**（开播/进出/静音/全员静音/踢人/锁门/录制/移交/举手/出口分配） | `crates/platform/src/call.rs` · `crates/rtc/src/{room,sfu}.rs` | 13 | ✅ |
| 同上 **HTTP 层契约**（204/200/403/404/409/422 的真实状态码） | `crates/platform/tests/call_http.rs` | 3 | ✅ |
| **通话页 UI**（参与者网格 + 底部控制条 + 参与者菜单 + 4 s 轮询） | `ui/src/callpage.ts` · `ui/src/styles.css` | tsc + esbuild | ✅ |
| **设备验证页**（12 组安全码 + 身份指纹 + 标记已验证） | `ui/src/callpage.ts` · `ui/src/api.ts` · `ui/src-tauri/src/lib.rs` | tsc + esbuild | ✅ |
| **权限矩阵**（Rocket.Chat 的 roles × permissions：9 个权限点 / 3 个角色；提权仅群主、同级不可互踢） | `crates/platform/src/perms.rs` | 10 | ✅ |
| **通话信令中继**（offer/answer/ICE；`from` 强制改写、收件人必须在房、队列 64 条有界丢最旧） | `crates/rtc/src/{signal,room}.rs` · `crates/platform/src/call.rs` | 6（平台 5 + HTTP 1） | ✅ |
| 信令桥（Tauri 命令 + TS 封装，前端只透传 SDP） | `ui/src-tauri/src/lib.rs` · `ui/src/api.ts` | tsc + esbuild | ✅ |
| **端到端媒体协商**：SDP 走中继 → 两端完成 ICE/DTLS/SRTP → 音频双向到达 | `crates/platform/tests/call_media.rs` | 2 | ✅ |
| **真 socket 冒烟**：axum 起在真实 TCP 端口，裸 HTTP/1.1 驱动（204 空 body、409 错误体、401） | `crates/platform/tests/call_tcp.rs` | 1 | ✅ |
| **客户端宿主层**：`CallHost` 把状态机 / str0m / 真 UDP socket 接在一起（候选注入早于 offer） | `crates/rtc/src/host.rs` | — | ✅ |
| **两通客户端真打通**：127.0.0.1 上真 UDP，ICE+DTLS 连通且音频帧到达 | `crates/rtc/tests/host_udp.rs` | 2 | ✅ |
| **桌面端接线**：引擎线程（10ms 节拍）+ 中继收发 + `call://rtc` 事件 + 通话页按钮 | `ui/src-tauri/src/callrtc.rs` · `ui/src/callpage.ts` | tsc + esbuild | ✅ |
| **真实声卡**：麦克风 → 48k 单声道 20ms 帧 → Opus → RTP；RTP → Opus → 扬声器（`--features audio`） | `crates/rtc/src/audio_io.rs` · `crates/rtc/src/capture.rs` | 需真机 | ✅ |
| **DSP 小件**：有状态线性重采样（跨缓冲相位连续）、20ms 分帧、声道平均 | `crates/rtc/src/dsp.rs` | 8 | ✅ |
| **TURN 客户端**：Allocate（401 挑战 + MESSAGE-INTEGRITY）、CreatePermission、ChannelBind、Refresh | `crates/rtc/src/turn.rs` | 6 | ✅ |
| **中继路径**：对着最小 TURN 服务器双向转发媒体 | `crates/rtc/tests/turn_alloc.rs` | 2 | ✅ |
| **多方通话（mesh）**：一人同时与两人通话，音频到达每一路 | `crates/rtc/tests/mesh_udp.rs` | 2 | ✅ |

**新增测试 17 项**（合同 P5 下限 15）。

```bash
cargo test -p wxlike-platform --test p5_live     # 9
cargo test -p wxlike-container --test package    # 7
cargo test -p wxlike-video -- --ignored          # 4（真实 ffmpeg 转码，含多档阶梯）
```

### 1.1 直播控制面

```bash
# 开播（频道必须 kind=live，调用者必须是社群 owner/admin）
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/live/<channel>/start \
  -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"recording":true}'
# {"session":{...,"stream_key":"...","state":"live"},
#  "ingest_token":"<exp>.<hmac>","ingest_url":"rtmp://127.0.0.1:1935/live/<key>"}

# RTMP on_publish 回调（ingest 节点持共享 secret，无用户 token）
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/live/ingest/authorize \
  -H 'content-type: application/json' \
  -d '{"stream_key":"<key>","token":"<exp>.<hmac>"}'      # 204 / 403

# 观众进出与弹幕
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/live/<channel>/join  -H "authorization: Bearer $TOKEN"
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/live/<channel>/danmaku \
  -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"text":"开场了"}'
# {"delivered":2}   ← 在房观众数（不含发送者）

# 停播（主播或社群管理员）；停播后凭据立即失效
curl -s --noproxy '*' -X POST http://127.0.0.1:8099/api/live/<channel>/stop -H "authorization: Bearer $TOKEN"
```

三条关键断言（测试覆盖）：

1. `stop` 后即使 `ingest_token` 未过期，`verify_ingest` 也拒绝——凭据有效性同时
   取决于签名与**会话状态**；
2. 弹幕走 `high_priority`（`live.danmaku` 已在类型注册表登记），测试直接断言
   频道时间线为空（不变式 3）；
3. 弹幕过内容管线：硬违规拒绝、超长/空串拒绝。

### 1.2 小程序包签名

```rust
// 发布方签名（CI 持有私钥）
let sig = sign_manifest(&manifest_bytes, &signing_key, "k1");
// 宿主验签 + 安装
let granted = registry.install(&manifest_bytes, &sig)?;
```

- 签名覆盖 manifest 原文，改任一字段即验签失败；
- 注册表拒绝**同版本重复**与**版本降级**（签名合法也不行——旧版漏洞包无法回滚覆盖）；
- `im_bridge` 授予面 = 清单声明 ∩ 宿主白名单（`im.send_message` / `im.profile.read` /
  `im.conversation.read` / `pay.request`），白名单外权限记录在 `denied_permissions`
  供审计，但**永不授予**（不变式 6）；
- 清单校验拒绝 `../` 入口路径与非法版本号。

## 2. 未完成项与阻塞原因

| P5 条目 | 状态 | 原因 / 下一步 |
|---|---|---|
| Flutter Android/iOS + 鸿蒙（UniFFI 绑定） | ⛔ 未开始 | 本机无 Android SDK / Flutter / 鸿蒙工具链，无法产出可验证安装包；Rust 侧 API 面已稳定，绑定层可在有工具链的机器上一次性生成 |
| Web 独立工程 | ⛔ 未开始 | 与桌面端共用 `api.ts` 契约，建议直接抽取共享包后建 Vite 工程 |
| 直播媒体面（RTMP ingest + HLS 分发 + 连麦） | 🟡 控制面完成 | ingest 走外部 Xiu 进程、HLS 走静态分发；本仓库只提供授权/凭据/状态，不在进程内跑媒体服务器 |
| video-service 多分辨率 | ✅ 已完成 | 360/720/1080 阶梯（只降不升）+ master playlist + 档位分片路由；真实 ffmpeg 用例 `ladder_produces_multiple_renditions` 验证 720p 源产出 360/720 两档 |
| 小程序 M1–M4（逻辑层/Bridge/多引擎/沙箱） | 🟡 M5 签名层完成 | 逻辑层已有 Boa 宿主与 DPD1 装载；Wasmtime 沙箱未接 |
| 多语言 / 数据导出 | ⛔ 未开始 | 数据导出建议随 P6 账号注销一起做（同一份 dump） |
| APNs / 国内厂商推送通道 | ⛔ 未开始 | 需要厂商证书；适配器可先按 FCM 客户端形状写，证书由部署方注入 |

## 2.5 Telegram 代码借用（首次实际复制）

按"能搬不重写"的要求，本轮首次实际复制第三方源码：

- **来源**：`refs/tweb`（morethanwords/tweb，GPL-3.0，与本项目同许可）
  `src/helpers/slicedArray.ts` → `ui/src/vendor/tweb/slicedArray.ts`
- **借的是什么**：降序分块数组（`SlicedArray`/`Slice` 的 ends 追踪 + `insertSlice`
  跨页合并）。这正是消息列表虚拟化的核心结构，自研等于重犯它的边界。
- **替换掉的自研代码**：`ui/src/api.ts` 里手写的 `mergeMessages`（`Map` + `sort`，
  每页 O(n log n)）。现在历史分页与实时消息都走 `MessageIndex`
  （`ui/src/msgIndex.ts`），同 seq 覆盖、跨页去重、乱序插入由 tweb 的切片完成。
- **适配**（三处，写在文件头）：去掉 `MOUNT_CLASS_TO` 调试挂载；内联
  `indexOfAndSplice` 与 number-only `compareValue`；按本项目 `strict` 补变量初始化。
- **回归**：`bash ui/scripts/check-msgindex.sh`（esbuild 打包 + node 断言 7 条：
  乱序页、历史页、实时插入、同 seq 覆盖、幂等、窗口取尾）。

第二轮借用（同一来源，`ui/src/vendor/tweb/listenerSetter.ts`）：

- **借的是什么**：`ListenerSetter`（监听器集中登记 + `removeAll` 一次性清理）。
- **修掉的问题**：消息菜单与贴纸面板各自手写 `document.addEventListener`
  + 命名函数自清；菜单被切视图/重渲染关掉时监听器会残留，越积越多。
  现在两处都登记到同一个 `menuListeners`，`hideMsgMenu()` 统一清理。
- **适配**：去掉 tweb 内部类型 `EventListenerBase`；`strict` 下补
  `let listener: Listener | undefined`。

## 3. 设计说明（为什么这样切）

- **弹幕不进时间线**：合同不变式 3 规定只有 normal 通道占用序列号。弹幕高频、
  无历史价值，落库只会把频道序列号打爆。用 `high_priority` + 在房观众收件集，
  离线者拉历史时看不到三小时前的弹幕——这正是产品预期。
- **推流凭据自包含**：ingest 节点（Xiu）与平台可能不在同一进程/机器，回调验签
  不能依赖数据库。`<exp>.<hmac>` 让 ingest 侧只需共享 secret；会话状态检查留在
  平台侧，避免 ingest 被停播后的旧凭据复用。
- **签名与包体分离**：包体按内容寻址（sha256），签名旁挂 `(key_id, sig)`，
  密钥轮换不影响已发布包；注册表单独负责防降级，这是验签挡不住的一层。

## 媒体接线（P5 收尾）

链路上的每一层现在都有人负责，且边界清晰：

```
CallManager（纯状态机，无 IO）
      ↓  Action::Send / Ended
CallHost（host.rs：注入候选、泵媒体、把 Datagram 变成 Transmit）
      ↓  Effect::Signal / Transmit / Audio / Connected / Ended
callrtc.rs（唯一碰 socket 与 HTTP 的地方，10ms 节拍，专用线程）
      ↓  tauri 命令 + call://rtc 事件
callpage.ts（按钮 + "已连通 · N 帧" 徽标）
```

几个刻意的选择：

* **候选必须在 offer 之前注入**。`CallManager::set_local_candidates` 就是为了
  这一点存在的——漏了它 SDP 里没有 candidate 行，ICE 会永远停在 checking，
  而表面上"信令都通了"，极难查。`host_udp.rs` 里有一条断言专门守这个退化路径。
* **引擎用阻塞线程而不是 tokio 任务**：`ureq` 是同步的，媒体要 10ms 固定节拍，
  混进 async 运行时要不停 `spawn_blocking`，节拍反而容易拖歪。
* **绑 `0.0.0.0` 时换成本机网卡地址**再写进 SDP（`primary_ipv4`），否则对端
  拿到的是不可达候选。`WXLIKE_MEDIA_BIND` 可覆盖绑定地址。
* **群通话不接客户端点对点**：多方要 SFU，按钮只对 `d-` 直聊出现，不做假功能。

仍未接的：麦克风采集与播放（`rtc` 的 `audio` feature 需要声卡与 libopus，
CI 无设备），所以目前 `audio_frames` 是"媒体面在流动"的证据，不是"能听见声音"。

## 声音怎么过去的（`audio` feature）

```
麦克风（任意采样率/声道）
   → interleaved_to_mono（取平均，不是取左声道）
   → Resampler（有状态线性插值，跨缓冲保持相位）
   → FrameChunker（20ms = 960 样本）
   → OpusCodec::encode → host.write_audio → RTP
                                                   ↓
扬声器 ← mono_to_interleaved ← Resampler(48k→设备率) ← OpusCodec::decode ← Effect::Audio
```

三个容易踩的坑，都在 `dsp.rs` 里有测试钉住：

1. **重采样必须有状态**。每块输入独立插值会在每个缓冲边界产生相位跳变，
   听感是每 10–20ms 一声轻"咔"——单块测试完全测不出来，`chunk_boundaries_do_not_drop_samples`
   用"分两次喂 vs 一次喂总长度必须一致"来守。
2. **立体声要取平均**。取左声道会让"只用右耳麦"的人彻底没声音。
3. **播放队列必须有上限**（400ms，超了丢最旧）。时钟漂移会让队列单调增长，
   没有上限时延迟会从 20ms 一路涨到几秒。

构建注意：`audiopus_sys` 的 vendored libopus 写着 `cmake_minimum_required(VERSION 3.1)`，
CMake ≥ 4 会直接拒绝。`ui/src-tauri/.cargo/config.toml` 里固定了
`CMAKE_POLICY_VERSION_MINIMUM=3.5`，所以 `cargo build --features audio` 开箱即用。

没有声卡时引擎不报错：`AudioLeg::start` 逐项降级（无麦克风就只收不发），
通话照常建立，UI 上 ↑ 帧数停在 0——这个区分本身就是一个有用的诊断信号。

## 跨 NAT：TURN

`WXLIKE_TURN_URL=turn:host:3478` + `WXLIKE_TURN_USER` / `WXLIKE_TURN_PASS`
一配，`callrtc::Transport` 就改走全程中继：

```
start() → TurnClient::allocate（401 挑战 → 带 MI 重发 → 拿到 relayed 地址）
        → SDP 里只放这一个 candidate
        → 每包 Effect::Transmit 都 ChannelData 包一层发给 TURN 服务器
        → 回流从服务器来，解出 (peer, data) 再喂给 str0m
```

**为什么不"直连优先、失败再中继"**：同时放 host 和 relay 两个候选，ICE 会先
试直连、等超时再回退，用户看到的是几秒钟的"正在连接"；而"有候选但打不通"
是最难解释的一类故障。配了 TURN 就是明确表态"我要中继"，行为可预期。

TURN 鉴权有两个极易写错的地方，测试专门守着：

1. **HMAC 覆盖的头部里 length 必须已经算上 MESSAGE-INTEGRITY 那 24 字节**
   （RFC 5389 §15.4）。写错的实现自签自验能过、真实服务器只会回 401——
   `integrity_is_verifiable_by_receiver` 同时验证"对的 key 通过 / 错的 key 失败 /
   改一字节失败"。
2. **属性必须 4 字节对齐**。错一位，后面所有属性解析全乱。

还有一个踩过的坑：**不能按首字节判断是不是 ChannelData**。裸数据的第一个字节
完全可能落在 `0x40–0x7F`（正好是通道号高位），`"world"` 的 `'w'` 就是 `0x77`。
真实 TURN 服务器按来源 socket 区分，测试里的服务器最初按首字节判断，结果
反向路径静默丢包——这类 bug 在真机上表现为"单向没声音"，极难定位。

未做：TCP/TLS 传输、Even-Port。不影响"能打通"，只影响极端网络下的成功率。
部署侧仍需一台 coturn（`turnserver --lt-cred-mech --realm=... --user=user:pass`），
这是运维决策，不进代码。

## 多方通话：mesh，以及踩到的一个真 bug

`CallHost::start_many` 对每个对端开一条独立 `MediaSession`，共用同一个 socket
（`CallManager` 本来就按 call_id 存多通电话）。

**上限不是拍脑袋定的**，照 Nextcloud Talk 的 `docs/scalability.md`：mesh 的带宽是
`单流码率 × (人数 − 1)`。视频 1 Mbps，音频约 50 kbps——所以音频-only 的 mesh
在 8 人时每人约 350 kbps 上行，完全可接受；Talk 自己的说法是音频-only 到
20 人"可能可行"，真正的瓶颈是浏览器解码与信令负载。我们是桌面端、且目前只有
音频，UI 定 8 人为提示线，超过就明说需要 SFU，而不是假装能用。

（Talk 超过这个量级用 HPB——一台 Janus 构成的 SFU。我们若要写 SFU，
`refs/atm0s-media-server` 是唯一同栈参考：Rust + str0m，分层为
`transport_webrtc` / `media_core` / `media_connector` / `media_runner`。）

**这个测试抓到一个一对一时永远看不见的 bug**：原来 `handle_datagram` 把每个
入向包喂给**所有**在进行的会话。一对一时只有一个会话，所以没问题；多人时
B 的 DTLS 包会被 C 的会话也吃一遍，str0m 按来源当成新的连通性检查处理，
结果一条通了、另一条卡在 checking——表现是"三方通话只有一个人连上"。

修法是按来源路由：`Effect::Transmit` 里的 `destination` 就是该会话的对端地址，
第一次发出去时记下 `远端地址 → call_id`，之后入向包只投给对应的那一条。
未知来源（还没发过包）才回退到广播。这条路由表在通话结束时清理。
