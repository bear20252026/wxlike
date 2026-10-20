# Zone E 验收记录（`crates/video`）

> 卡：**E1** video 的三块欠账 · **E2** 直播 ingest 对接 · **E3** 转码流水线
> 分支 `feat/zone-e`，工作树 `/d/mimo/test/worktrees/zone-e`，2026-10-20
> 环境：Windows 11 x64 · cargo 1.97.1 · ffmpeg/ffprobe **9.0**-full（在 PATH）
> 本文只写**跑过的**；跑不了的都在 §7 待验清单里，没有打勾的绿。

---

## 1. 测试总量（卡面上"测试为 0"那条）

```bash
cargo test -p wxlike-video              # 不需要 ffmpeg
cargo test -p wxlike-video -- --ignored # 真 ffmpeg
```

| 文件 | 非忽略 | 忽略（ffmpeg） | 覆盖的规则 |
|---|---:|---:|---|
| `tests/media_range.rs` | 7 | — | `Range` 三种形式、垃圾/不可满足、200/206/416/404、`read_range` 短读要报错 |
| `tests/ladder_rules.rs` | 5 | — | 阶梯只降不升、master 清单格式与升序、按码率选流、档位目录/分片名校验 |
| `tests/playback_stats.rs` | 2 | — | 观看上报校验、聚合、完成度封顶、名单门、选流路由 |
| `tests/live_rules.rs` | 5 | — | 窗口裁剪与序号、滚动清单、重推拒绝、`live_url` 与平台同形、凭据签名/过期/篡改 |
| `tests/live_hls.rs` | 1 | — | HTTP 面：503（没配密钥）→ 授权 → 推片 → 清单 → 停播 → 409 → 配置错误不静默 |
| `tests/jobs_queue.rs` | 16 | — | 退避表、幂等入队、领取 CAS、卡死重领、失败/终态/人工重试、回调四类结论、HTTP 面 |
| `tests/video_ingest.rs`（原有） | 4 | 4 | 探针解析、id 校验、真转码、多档阶梯、HTTP 播放闭环、坏字节不留半成品 |
| `tests/jobs_pipeline.rs` | — | 3 | 提交→转码→可播→回调全链、坏字节终态可重试、新旧两条路结果一致 |
| `tests/live_ffmpeg.rs` | — | 1 | 真 TS 分片 → 滚动窗口 → 停播 → ffprobe 读落盘清单 |
| **合计** | **40 passed / 0 failed** | **8 passed / 0 failed** | |

`cargo clippy -p wxlike-video --all-targets -- -D warnings` 干净；
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p wxlike-video` 干净；
`cargo fmt --all -- --check` 干净。

---

## 2. E1 · `Range`（卡面验收命令）

对着真服务（`wxlike-server`，1080p 点播对象，`play.mp4` = 26 160 273 字节）：

```console
$ curl -D- -H 'Range: bytes=0-1023' http://127.0.0.1:18099/api/video/<id>/play.mp4
HTTP/1.1 206 Partial Content
content-type: video/mp4
accept-ranges: bytes
content-range: bytes 0-1023/26160273
content-length: 1024

$ curl -D- -H 'Range: bytes=8000000-8000999' .../play.mp4
HTTP/1.1 206 Partial Content
content-range: bytes 8000000-8000999/26160273
拿到字节数: 1000；与对象文件里 [8000000,8000999] 的字节 **逐字节相同** ✓

$ curl -D- -H 'Range: bytes=-4096' .../play.mp4            # 后缀形式
HTTP/1.1 206 Partial Content
content-range: bytes 26156177-26160272/26160273

$ curl -D- .../play.mp4                                    # 无 Range
HTTP/1.1 200 OK   accept-ranges: bytes   content-length: 26160273

$ curl -D- -H 'Range: bytes=999999999-' .../play.mp4
HTTP/1.1 416 Range Not Satisfiable
content-range: bytes */26160273
```

`thumb.png` 与直播分片走同一条 `serve_file` 路径（同一套 206/416/404 判定）。
HLS 清单与点播分片仍是整体响应（播放器对分片不发 Range），因此**不**给它们
`Accept-Ranges` —— 声明了就得兑现。

**语义与 `crates/platform/src/media.rs` 的 `parse_range` 对齐**：同一个 `bytes=a-b` /
`bytes=a-` / `bytes=-N` 语法、同样把越界的 end 夹到末尾、同样对多区间回 416。
两份实现在不同 crate 是因为 Zone E 所有权不许改 platform；合并到 `crates/core`
的事见 §8。

## 3. E1 · 播放统计（"谁看了多久"）

```console
$ POST /api/video/<id>/view  {"viewer":"alice","watched_ms":42000,"position_ms":42000}
201 {"event_id":4,"viewer":"alice","session_id":"s-243818e10a179ca3",...}   # 没给 session → 服务端补
$ POST /api/video/<id>/view  {"viewer":"alice","session_id":"s-2","watched_ms":18000,"position_ms":60000,"completed":true}
201 {...,"session_id":"s-2",...}                                            # 沿用同一个会话
$ POST /api/video/<id>/view  {"viewer":"bob","watched_ms":5000,"position_ms":5000}
201 {...}

$ GET /api/video/<id>/stats
{"events":6,"unique_viewers":2,"total_watched_ms":130000,
 "avg_watched_ms_per_viewer":65000,"avg_completion":0.5833333333333334,
 "first_seen_ms":...,"last_seen_ms":...}
# 片长 60s：alice 看了 120s → 封顶 1.0；bob 看了 10s → 0.1667；人均 0.5833 ✓

$ GET /api/video/<id>/stats/viewers                     → 403（配了 token 但请求没带）
$ GET /api/video/<id>/stats/viewers -H 'x-wxlike-video-stats-token: …'
[{"viewer":"alice","events":4,"sessions":3,"watched_ms":120000,"max_position_ms":60000,"completed":true,...},
 {"viewer":"bob","events":2,"sessions":2,"watched_ms":10000,"max_position_ms":5000,"completed":false,...}]
```

拒绝的形状（不是静默吞）：`watched_ms:0` → 422；视频不存在 → 404；`viewer` 空/超长 → 422；
`session_id` 超长 → 422。没配 `WXLIKE_VIDEO_STATS_TOKEN` 时名单是 **503**（"服务没配这个能力"），
配了但头不对是 **403**（"你没权限"）—— 两种原因不能挤成同一个码。
聚合（数，不是身份）不需要 token。

## 4. E1 · 按码率选流（Track B 清单里的第 2 项）

```console
$ GET /api/video/<id>/select?kbps=4000    → 720p；可用额度 3200000 bps；阶梯 [360, 720, 1080]
$ GET /api/video/<id>/select?kbps=900     → 360p；可用额度 720000 bps
$ GET /api/video/<id>/select?kbps=50000   → 1080p
$ GET /api/video/<id>/select              → 422（缺 kbps，不"随便选一个"）
$ GET /api/video/<id>/select?kbps=0       → 422
```

规则写在 Rust（`select_rendition`）：取**放得下**的最高档，一档都放不下时给最低档
（宁糊不黑），并留 20% 抖动余量（`SELECTION_HEADROOM_PCT`）。TS/Dart 端只传实测带宽。

## 5. E3 · 转码流水线（卡面门槛：50 MB → 3 分钟内可播）

源：`testsrc2` 合成的 1920×1080 / 30fps / 60s，**50 832 164 字节**。

```console
$ POST /api/video/jobs  (multipart: bytes=@big.mp4, callback_url=http://127.0.0.1:8123/cb)
HTTP 202
{"job":{"job_id":"990cd1c9…","state":"pending","attempts":0,"source_size":50832164,
        "callback_state":"pending","callback_url":"http://127.0.0.1:8123/cb",…},
 "newly_queued":true}

$ 轮询 GET /api/video/jobs/990cd1c9…
state=succeeded  elapsed=106s（提交 → 可播）        # 门槛 180s，实测 106s ✓
duration_ms 60000  档位 [360, 720, 1080]            # 真 1080p 源出满三档

$ 服务端日志
INFO wxlike_video::jobs: video worker 一轮 report=WorkerReport { succeeded: 1, … }
INFO wxlike_video::jobs: video worker 一轮 report=WorkerReport { callbacks_delivered: 1, … }

$ GET /api/video/990cd1c9…/play.mp4  → 26 160 273 字节的最高档；Range 见 §2
$ ls <root>/990cd1c9…/               → meta.json play.mp4 play_360.mp4 play_720.mp4
                                        play_1080.mp4 source thumb.png hls/{360,720,1080}
$ POST /api/video（老的同步上传）同一份字节 → HTTP 200，同一个 video_id ✓ 路径没被拆
$ POST /api/video/jobs 同一份字节          → HTTP 200（不是 202），newly_queued=false，
  用时 ~5s（全部花在传 50MB 上，没有第二次转码）
```

失败与重试（`jobs_queue.rs` / `jobs_pipeline.rs` 断言）：退避 1/2/4/8/16/30s、5 次上限、
`throttled` 不烧次数、不可重试错误一次就终态、`running` 卡 30 分钟可被重新领取、
终态保留源字节（人工重试不要客户端重传）、回调状态与作业状态**分开**。
坏字节作业：`state=failed` + `last_error` 指名 ffprobe/ffmpeg，对象目录里**不留半成品**。

诚实的一条边角：上面"轮询到可播"那段里，curl 有**一次**拿到 0 字节（脚本里表现为 JSON
解析失败）。时间点正落在三档连转把 CPU 打满的那一窗；紧接着的轮询与之后所有命令都正常，
`cargo test --workspace` 也全绿，所以按"负载下的瞬时连接失败"记账，不当成作业面的缺陷——
但集成方在弱机或 CI 上重跑这条时先别怀疑队列，多看一眼是不是同一个原因。

## 6. E2 · 直播接入

服务端口 `WXLIKE_LIVE_INGEST_SECRET` 与平台共用同一个 env（`server/src/lib.rs:103`）。
目录约定：`{video_root}/live/{stream_key}/`，播放地址 `/live/{stream_key}/index.m3u8`
= 平台 `live_state().hls_url` 的拼法（`live_rules.rs` 里有断言这两处一致）。

```console
# 凭据门
$ PUT /live/<key>/segment/seg_000.ts（没带凭据）        → 401
$ PUT /live/<key>/segment/seg_000.ts（错签名/过期）      → 401
$ 没配 WXLIKE_LIVE_INGEST_SECRET 时 PUT                  → 503（拒绝一切推流，无 dev 默认密钥）
$ GET /live/<key>/index.m3u8（没有会话）                 → 404 {"error":"no live session"}
$ GET /live/..%2f..%2fetc/index.m3u8                    → 404（先校验再拼路径）

# 接入节点（wxlike-live-ingest，12 秒真源，2 秒一片）
$ wxlike-live-ingest --source live_src.mp4 --stream-key <key> --token <exp>.<hmac> \
      --server http://127.0.0.1:18099 --segment-seconds 2
接入节点启动: …  切片=2s
已通知服务端停播（清单已写 #EXT-X-ENDLIST）
正常收尾，共推送 6 片                       退出码 0

$ GET /live/<key>/index.m3u8
#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:3
#EXT-X-MEDIA-SEQUENCE:0
#EXTINF:2.023,
seg_00000.ts
… 共 6 片，6 个不同分片名 …
#EXT-X-ENDLIST

$ PUT 重推最后一片                                        → 409，清单行数不变（6）
$ ffprobe <video_root>/live/<key>/index.m3u8             → format: hls  duration: 12.395  streams: 2
                                                           （样片 12s：清单与分片自洽，能播）
```

窗口裁剪/序号递增在真服务上不容易看见（文件源被 ffmpeg 以 50× 实时跑完），
所以由 `live_hls.rs` 用窗口=3、推 5 片确定性地断言：
`media_sequence` 走到 2、清单里只剩 3 片、序号连续。

**发现并修掉的缺陷**：第一次手工验收时误留了第二个接入节点，两个进程推同一路流，
清单里出现重复 `#EXTINF`，ffprobe 把 12 秒的片子读成 16.5 秒。打包器原先只保证
"顺序追加"，不认"同一片推两次"。现在 `LiveWindow` 记住窗口内的名字与紧邻的上一片，
重推一律 409（`live_rules.rs` + `live_hls.rs` + 上面 §6 的 409 各测一次）。
比窗口更早的旧名字仍会被当成新片——打包器只保留窗口大小的记忆，
真要防节点重放历史应当停播换流密钥（已写进代码注释，不是没想到的坑）。

## 7. 待验清单（我这台机器物理上跑不了的）

| 卡 | 条目 | 为什么这里跑不了 | 怎么验 |
|---|---|---|---|
| E2 | **OBS 推 RTMP → 浏览器 HLS 播放 30 秒不卡** | 需要 GUI 的 OBS 与一次真实推流；本仓库不含 RTMP 终止节点（ADR-21） | OBS 自定义服务 `rtmp://<ingest-host>:1935/live/<key>` → 外部 Xiu/ffmpeg 终止 → `wxlike-live-ingest --source rtmp://…/live/<key>` → 浏览器拉 `/live/<key>/index.m3u8`（hls.js）。30 秒内 `#EXT-X-MEDIA-SEQUENCE` 必须单调递增、无 404 分片 |
| E2 | 接入节点断流重连 / 中途观众加入 | 需要真实网络与持续推流 | 拔网线 10s 再接：`window.json` 序号不回退；新观众从清单第一片起播 |
| E2 | `live_ingest_secret` 与平台签发凭据**互认** | 需要跑平台库 + `POST /api/live/{channel}/start` 拿真 token | 开播拿 `ingest_token` → 直接喂给 `wxlike-live-ingest`。签名方案与 `crates/platform/src/live.rs::sign_ingest` 逐字一致，`live_rules.rs` 只证同形，不证跨 crate 互认（要改 platform 才能测，越界） |
| E3 | 生产并发下的转码排队（多路同时上传） | 单机 CPU 就是瓶颈 | `JOBS_PER_ROUND=1` 串行是有意的；要并发按 Q7 给 video-service 单独开进程 |
| E3 | 回调端点落在平台 | `crates/platform/src/lib.rs` 是热点文件，Zone E 不许改 | §8 第 2 条给了现成的接入方式 |
| 全部 | `cd ui && npx tsc --noEmit` | 这个 worktree 没有 `ui/node_modules`（不越界装依赖、不碰 ui） | 本卡零 UI 改动；集成方在 main 上跑即可 |

## 8. 需要集成方代合的四件事（Zone E 所有权之外）

1. **`THIRD-PARTY-NOTICES.md`**（§3.6）。本卡**没有复制任何外部源码**，
   "Vendored code" 表不需要新行；依赖表加一行、"Ported designs" 加四条：

   ```markdown
   | rusqlite / rand / hmac / ureq | 0.32 / 0.8 / 0.12 / 2.12 | MIT/Apache-2.0 | **已在 Cargo.lock 里**：`crates/video` 新增对这四个既有 crate 的依赖边（播放统计与作业队列的库、缺省 session_id、推流凭据 HMAC、出站回调与接入节点推片）。Cargo.lock 只 +4 行，无新包 |

   - **本仓库 `crates/core/src/outbox.rs`**：转码作业队列的状态机/退避/幂等入队/
     `throttled 不烧次数` → `crates/video/src/jobs.rs`（同仓库参照重写，两处偏离写在模块头）。
   - **本仓库 `crates/platform/src/media.rs`**：`Range` 解析与 206/416/Accept-Ranges 语义
     → `crates/video/src/range.rs`。日后应统一到 `crates/core`，删掉其中一份。
   - **本仓库 `crates/platform/src/live.rs`**：自包含推流凭据 `<exp>.<hmac(stream_key|exp)>`
     与 `constant_time_eq` → `crates/video/src/live.rs`（差别：打包器侧**不查库**）。
   - **本仓库 `crates/ledger`**：append-only 事件 + 重放聚合 → `crates/video/src/stats.rs`。
   ```

2. **E3 回调的平台侧接收点**。现在 `WXLIKE_VIDEO_CALLBACK_URL`（或提交时逐作业给
   `callback_url`）指哪儿就打哪儿，体是
   `{"job_id","state","result":{"video_id","playlist_url","play_url","thumb_url","duration_ms","width","height","renditions"},"attempts","updated_at"}`。
   平台要落一个"消息侧视频引用"需要一条 `POST /api/video/callback`（`platform` 的路由 +
   `crates/cmd` 的 catalog 一条）——两处都是热点，我没动。在此之前，回调地址可以先给
   部署侧的接收器；作业状态本身已经可查（`GET /api/video/jobs/{id}`）。

3. **worker 的装配位置**。`server/src/lib.rs` 是热点，所以 worker 由
   `POST /api/video/jobs` 首次提交时自起（`VideoStore::start_worker`，按存储根目录幂等）。
   Q7 装配分离开进程后，应在二进制里显式 `wxlike_video::spawn_worker(store, WORKER_POLL)`
   并去掉自起逻辑。

4. **命令面 catalog（§1.2 的设计规矩）没登记**。本卡的 11 条新路由只在 HTTP 面上，
   `crates/cmd` 的 catalog 一条都没加——那是别的区的所有权。按"新增功能 = catalog
   加一条 + 一个入口实现"的规矩，至少要登记这几条（Tauri/MCP/CLI 才能看到同一份能力）：
   `video.select`（读）、`video.stats`（读）、`video.job.create`（写）、
   `video.job.retry`（写）、`video.view.report`（写）。
   HTTP 面已经能用，登记只是把它们暴露给 agent 侧；需要谁做、什么时候做由集成方定。

另外两件是建议、不是必须：`GET /api/video/{id}/view` 目前是**自报身份**且公开可写
（点播面本来就无鉴权），要防伪造成交侧在 `crates/cmd` 加一条带鉴权的入口；
播放事件表暂无清理策略（`watch_events` 只增），需要定期聚合或按 `created_at` 裁剪时
告诉我，逻辑很薄。

## 9. 边界与没做的事

* **没有引入进程内 RTMP 服务器**（ADR-21 / `docs/p5-report.md`："不在进程内跑媒体服务器"）。
  `refs/xiu` 一行代码都没搬：它是外部进程，我们的接入节点用 ffmpeg 终止输入。
  弹幕、开播授权、`live_sessions`/`live_viewers` 仍全在 `crates/platform/src/live.rs`，没动。
* 直播**录制转点播**（`live_sessions.recording` 目前只有控制面含义）、DVR 回看范围可调、
  fMP4/LL-HLS 分片：都只留了命名空间（分片名允许 `.m4s`/`.mp4`），没实现。
* 没有把 `Range` 做成流式（`Body::from_stream`）：请求一段读一段，但无 Range 的整段仍是一次
  `read` 进内存。点播文件上限 500 MB，这条已经比改之前好（改之前所有请求都读满）。
* `crates/video` 的 8 条老路由与 `POST /api/video` 同步上传**行为不变**（原有 4 条 ffmpeg
  测试仍绿）。新增 11 条：3 条统计（含 1 条名单门）、1 条选流、3 条作业、4 条直播（清单/取片/推片/停播）。
