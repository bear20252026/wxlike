# H-21：视频上传/作业源文件的流式落盘

状态：**已关闭**（2026-11-29，批次 84）
审计基线：`60895e18`，审计原文 H-21「视频上传/执行多份全量 buffer」
审计证据：`crates/video/src/lib.rs:66,386-410,854-888,1186-1201`、`crates/video/src/jobs.rs:645-674,1147-1159,1186-1204`
审计要求：流式落盘 + 有界 channel；峰值内存与文件大小解耦；near-limit 并发压测

## 原实现的内存路径

一次 500 MiB 内的上传，在旧代码里至少有三份近整份文件的缓冲：

1. `crates/video/src/lib.rs::upload` 的 multipart 读取用
   `field.bytes().await` 得到完整字段，再 `to_vec()` 复制一份；
2. `VideoStore::ingest(&[u8])` 把整份源字节交给 `spawn_blocking`，blocking
   闭包又持有该 `Vec`，哈希与转码期间无法释放；
3. 作业路径 `run_claimed_job` 先 `std::fs::read(staged)` 读成 `Vec`，再调用
   `ingest(&bytes)`；提交侧 `write_pending_source` 也把整份 `Vec` 写成
   `source.part`。

并发上传时，每条请求各持一份近 500 MiB 的 `Vec`，内存随并发线性增长。

## 新实现

### 1. 上传字段直接写临时文件

`crates/video/src/lib.rs` 新增：

- `UploadTemp`：在视频根目录用 `create_new` 创建唯一的
  `.upload-<16hex>.part`；路径完全由服务端生成，不接受客户端文件名；
  `Drop` 时尽力删除。正常成功路径由调用方移交/改名，异常路径不会留下
  半个 body。
- `stage_field(field, root)`：不再调用 `field.bytes()`。

`stage_field` 的读取链路是：

```text
multipart Field::chunk()
  -> 有界 mpsc channel（容量 4 个 body frame）
  -> 单独 writer task：tokio::fs::File::write_all + flush
```

channel 有界，慢盘会对网络读形成反压；writer 一次只持有一个 frame，
不会把整个字段拼成 `Vec`。累计大小用 `u64` 饱和加法，超过
`MAX_VIDEO_BYTES`（500 MiB）立即中止。空字段在 `stage_field` 返回前就被
拒绝，避免空临时文件进入后续流水线。

`read_field` 返回 `(UploadTemp, u64)`；`upload` 直接把临时文件路径交给
`VideoStore::ingest_file`，配额检查使用流式累计的 `size`。

### 2. ingest 只按路径读文件

`VideoStore` 新增/改造：

- `hash_file(path)`：用 64 KiB 固定缓冲区流式算 SHA-256，同时得到
  `source_size`；不把文件读进内存。
- `ingest_file(source)`：先 `hash_file`，再做内容寻址幂等检查
  （`contains(video_id)` 命中直接复用），未命中时进入
  `transcode_from_path(video_id, source, source_size)`。
- `transcode_from_path` / `run_pipeline`：ffprobe、ffmpeg、缩略图都以源文件
  路径为输入；`meta.source_size` 使用传入的流式累计值。旧实现里
  `run_pipeline` 的 `std::fs::write(source_bytes)` 已不存在。
- `ingest(bytes)` 作为兼容入口保留：先写 `UploadTemp`，再走 `ingest_file`；
  这里没有 `to_vec`，也没有第二次整份搬移。生产 HTTP 不再调用它。

### 3. 作业提交与执行

`crates/video/src/jobs.rs`：

- 新增 `submit_job_file(source, owner, callback_url)`：
  1. `hash_file(source)` 流式哈希并得到大小；
  2. `check_quota(owner, size)`；
  3. 同一视频根目录内 `rename` 到 `pending/<job_id>/source`，不复制字节。
- `submit_job(bytes, ...)` 兼容字节调用方：先写 `UploadTemp`，再委托
  `submit_job_file`；不复制调用方 `Vec`。
- `read_job_fields` 返回 `(UploadTemp, size, Option<callback_url>)`；`bytes` /
  `file` 字段走 `stage_field`，重复字段直接报错，不再有
  `read_bytes_field` 的 `field.bytes().await`。
- `run_claimed_job`：
  - 先 `metadata(staged)` 校验 `source_size`（缺失/不一致的错误文案分别改为
    `stat staged source` / 永久失败）；
  - 直接 `ingest_file(&staged)`，不 `std::fs::read` 整份源；
  - 校验 `ingested.video_id == job_id`；不一致按永久失败处理，防止
    pending 目录被替换后把错误内容记成该作业的结果。
- 成功时 `succeed_job` 删除 pending 目录；可重试/终态失败保留源文件，运营
  重试不需要客户端重新上传。

### 4. 内存上界

单条上传的常驻增量由以下部分组成，均与文件总大小无关：

- channel 最多 4 个 body frame；
- writer task 当前正在写的一个 frame；
- 哈希固定 64 KiB 缓冲；
- ffprobe/ffmpeg 子进程的地址空间由 H-19 的 rlimit 单独限制。

旧实现的峰值约为「并发数 × 文件大小」，新实现为「并发数 ×（4+1 个
body frame + 64 KiB）」。

## 验证

### Near-limit 并发压测

`crates/video/tests/video_ingest.rs` 新增
`near_limit_concurrent_uploads_do_not_buffer_the_bodies`：

- 4 路并发，每路 480 MiB（低于 500 MiB 上限但接近），multipart body 由
  自定义 `http_body::Body` 逐块产出；每块之后 `Pending` 一次，模拟真实
  socket 的读侧背压（`multer` 的 `poll_stream` 会 poll 到 `Pending` 才返回，
  “永远 Ready”的测试 body 会把它自己的缓冲区撑满，不能用来测服务端）。
- 请求走真实 `routes(state)` 的 `/api/video`，经过认证、body limit、
  multipart、`stage_field`、哈希和失败转码；测试进程每 20 ms 采样
  `/proc/self/status` 的 `VmRSS`。

实跑结果（Linux，`cargo test -p wxlike-video --test video_ingest -- --ignored near_limit --nocapture`）：

```text
near-limit stress: concurrency=4 file=503316480 bytes baseline=4736 KiB peak=50820 KiB growth=45 MiB
test result: ok. 1 passed; 0 failed; ... finished in 14.27s
```

4×480 MiB 的输入只带来 45 MiB 的 RSS 增长；旧实现的线性下界是
4×480 MiB ≈ 1.9 GiB。测试同时断言四路失败后视频根目录没有
`.upload-*` / `.ingest-*` 残留。

### 功能与回归

- `cargo test -p wxlike-video`：全部非 ignored 套件通过（71 passed，9 ignored）。
- `cargo test -p wxlike-video --test video_ingest -- --ignored`：5 passed
  （4 个真实 ffmpeg 用例 + near-limit 压测）。
- `cargo test -p wxlike-video --test jobs_pipeline -- --ignored`：3 passed
  （真实 ffmpeg 作业流水线）。
- `cargo test -p wxlike-video --test jobs_queue`：20 passed
  （源文件缺失/损坏、重试、终态、幂等、回调）。
- `cargo check -p wxlike-video --all-targets --locked`、`cargo clippy -p wxlike-video --all-targets --locked -- -D warnings`、`cargo fmt --all -- --check`：通过。
- `cargo check --workspace --locked`：通过。
- `python3 scripts/count_unwraps.py --fail-on-increase --details`：70 → 70。
- `python3 scripts/platform_surface_audit.py --max-pub 0 --check-facades`：通过。
- `PYTHONPATH=tools/source-audit python3 -m sourceaudit authz check --root .`：通过（249 routes / 249 matrix rows）。

## 影响面与兼容性

- 公共 HTTP 路由、请求/响应 JSON、错误分类未变；`/api/video` 仍接受
  `bytes` / `file`，作业面仍接受 `bytes` / `file` + 可选 `callback_url`。
- `VideoStore::ingest(&[u8])`、`submit_job(&[u8])` 作为嵌入/测试兼容入口保留；
  它们不做整份复制，但调用方本来已经持有输入 `Vec`。本仓生产 HTTP 只走
  `ingest_file` / `submit_job_file`。
- `Cargo.lock` 只新增 `wxlike-video` dev-dependency `http-body = "1"`
  （依赖树中已有 1.1.0，不引入新版本）。

## 残余（不阻塞 H-21 关闭）

- 进程在写临时文件途中被 SIGKILL，`.upload-*` 半个文件会留在视频根目录；
  单文件不超过 500 MiB，且正常错误路径由 `UploadTemp::Drop` 清理。启动时
  的陈旧 staging 清扫可作为后续磁盘卫生项。
- 作业提交会在提交时和 worker 执行时各哈希一次源文件；这是崩溃/重启后
  校验 pending 源与 job_id 一致的代价，属于磁盘 I/O，不是内存复制。
