# H-19：视频外部进程无 timeout / 资源控制

关闭日期：2026-11-27。审计基线 `60895e18`，代码基线 `3616fc5`。

审计原文：`AUDIT-60895e18.md:262-267`。`ffprobe`/`FFmpeg` 用
`Command::output()`，无 deadline、无 CPU/内存/文件上限、无 process-group kill、
无协议隔离、无临时目录配额。

## 修复

`crates/video/src/lib.rs`：

- `run_limited(cmd, timeout, label, capture_stdout)` 取代 `Command::output()`：
  - 每 20 ms `try_wait`，超过 deadline 对**进程组**发 `SIGKILL`
    （`process_group(0)` + `libc::kill(-pid)`），再 `wait()` 回收；不会留下
    ffmpeg 子进程。
  - Linux 上 `pre_exec` 设置 `RLIMIT_CPU=1500s`、`RLIMIT_FSIZE=4 GiB`、
    `RLIMIT_AS=8 GiB`；上限在 fork 后 exec 前生效，不影响 hub 本体。
  - stdout 只在进程退出后读；ffprobe 输出很小，不会先写满管道卡死。
- 超时：`ffprobe` 30 秒；`ffmpeg` 25 分钟（低于 H-20 的 30 分钟
  stale-running 阈值）；`-version` 探测 10 秒。
- `ffprobe` 和所有 `run_tool` 调用加 `-protocol_whitelist file,pipe`：源/输出
  都是本机路径，禁掉 http/rtmp 等协议，恶意媒体不能把 ffmpeg 当出站客户端。
- `MAX_TRANSCODE_BYTES=12 GiB`：每档产出后和缩略图后递归统计临时目录，
  超预算立即失败并清理；不再等到整个 3 档跑完才看磁盘。
- `tools_available()` 的 `-version` 也走 `run_limited`。

## 证据

- `cargo test -p wxlike-video --lib`：7 passed，新增：
  - `external_tool_timeout_kills_the_process_tree`：脚本后台起 `sleep` 并写
    pid，120 ms deadline 后整组 SIGKILL；断言及时返回、错误含 `timed out`、
    `sleep` 子进程已不存在。
  - `temp_dir_budget_rejects_oversize_output`：2 MiB 文件在 1 MiB 预算下被
    拒绝，4 MiB 预算下通过。
- `cargo test -p wxlike-video --test video_ingest -- --ignored`：4 passed，真实
  ffmpeg 转码/HLS/缩略图/HTTP 播放闭环。
- `cargo test -p wxlike-video --test jobs_pipeline -- --ignored`：3 passed，
  作业队列真实转码与回调。
- `cargo test -p wxlike-video --test jobs_queue`：17 passed。

## 边界

没有用 cgroup：本机 hub 无特权创建 cgroup，rlimit 已覆盖 CPU/地址空间/单文件
大小。`RLIMIT_AS` 是进程级地址空间，不能精确限制 RSS；真正的内存隔离仍需
容器/cgroup。`MAX_TRANSCODE_BYTES` 是目录总量检查，不是内核级磁盘配额；并发
转码共享同一磁盘时仍可能整体写满，需要部署层磁盘配额或独立卷。ffmpeg 的
`-protocol_whitelist` 只限制协议，不解码器漏洞；解码器沙箱仍需容器。
