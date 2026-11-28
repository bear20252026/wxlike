//! wxlike-video — video-service（P2 最小版 → P5 完整版 / ADR-21）。
//!
//! 上传 → FFprobe 元数据 → FFmpeg **多分辨率阶梯** HLS（360/720/1080，只降不升）
//! → master playlist → 缩略图 → 本地对象存储。
//! 无状态任务式服务：`video_id` 是源字节的 SHA-256（内容寻址，与 media 一致，
//! 同一视频上传两次天然只转码一次）。消息侧只传引用（`msgmeta::VideoPayload`），
//! 视频二进制永不经过消息网关。
//!
//! 多分辨率约定：每个档位独立编码成 `play_{h}.mp4` 并从该文件 `-c copy` 切
//! HLS（**一次编码/档**，不重复转码）；`play.mp4` 取最高档，供 webview 原生
//! `<video>` 播放（倍速需要可控播放器，HLS 只做互操作面）。
//!
//! 播放闭环演示（需要 ffmpeg/ffprobe 在 PATH 或由 `WXLIKE_FFMPEG` 指定）：
//!
//! ```bash
//! ffmpeg -y -f lavfi -i testsrc=duration=3:size=640x360:rate=24 \
//!        -f lavfi -i sine=frequency=440:duration=3 -shortest /tmp/demo.mp4
//! curl --noproxy '*' -F bytes=@/tmp/demo.mp4 http://127.0.0.1:8099/api/video
//! curl --noproxy '*' http://127.0.0.1:8099/api/video/{video_id}/playlist.m3u8
//! ```

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use axum::extract::multipart::{Field, Multipart};
use axum::extract::{DefaultBodyLimit, Path as AxumPath, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use tokio::io::AsyncWriteExt as _;
use wxlike_api::{ApiError, ApiResult};

mod db;
mod jobs;
mod live;
pub mod policy;
mod range;
mod stats;
mod timing;

pub use jobs::{
    backoff_ms, deliver_callback, deliver_callback_signed, deliver_callback_signed_to,
    spawn_worker, CallbackBody, CallbackState, Delivery, Job, JobFailure, JobState, JobSubmit,
    RejectKind, Rejected, WorkerReport, BASE_DELAY_MS, CALLBACK_TIMEOUT_MS, CALLBACK_TOKEN_HEADER,
    MAX_DELAY_MS, MAX_ERROR_CHARS, MAX_JOB_ATTEMPTS, STALE_RUNNING_MS, WORKER_POLL,
};
pub use live::{
    check_live_segment_name, check_stream_key, ingest_secret_from_env, ingest_token, live_playlist,
    live_url, playlist_of, sign_ingest, verify_ingest_token, LiveSegment, LiveWindow,
    LIVE_DEFAULT_WINDOW, LIVE_MAX_WINDOW, LIVE_SECRET_ENV, LIVE_SEGMENT_MS_HEADER, LIVE_TARGET_MS,
    LIVE_TOKEN_HEADER, LIVE_WINDOW_ENV,
};
pub use policy::{CallbackPolicy, VideoAuth, VideoPolicy};
pub use range::{parse_range, read_range, serve_range, ByteRange, Served};
pub use stats::{
    ViewerWatch, WatchError, WatchEvent, WatchEventRecord, WatchStats, DEFAULT_VIEWER_PAGE,
    MAX_VIEWER_PAGE, MAX_WATCHED_MS, MAX_WATCH_ID_CHARS, STATS_TOKEN_ENV, STATS_TOKEN_HEADER,
};

/// Upload cap for the video endpoint: 500 MB, generous for a research VOD
/// service but bounded so a bad client cannot stream forever.
pub const MAX_VIDEO_BYTES: usize = 500 * 1024 * 1024;
/// 兼容常量：单档时代的高度上限（`width_of` 与老 meta 仍用它）。
pub const TARGET_HEIGHT: u32 = 720;
/// P5 多分辨率阶梯（高度，升序）。源比某档矮时该档不产出——**绝不上采样**。
pub const LADDER: [u32; 3] = [360, 720, 1080];
/// Segment target length.
pub const HLS_SECONDS: f64 = 4.0;
/// 外部工具单次执行上限。ffprobe 应该秒级返回；ffmpeg 走大视频时给足
/// 25 分钟，但仍低于 H-20 的 stale-running 阈值（30 分钟）。
pub const FFPROBE_TIMEOUT: Duration = Duration::from_secs(30);
pub const FFMPEG_TIMEOUT: Duration = Duration::from_secs(25 * 60);
const TOOL_VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const TOOL_POLL: Duration = Duration::from_millis(20);
/// 单次转码临时目录上限（源 + 各档 MP4 + HLS 分片 + 缩略图）。
pub const MAX_TRANSCODE_BYTES: u64 = 12 * 1024 * 1024 * 1024;
/// 单个外部工具进程的 CPU 硬上限（秒）；与命令 deadline 同量级。
const TOOL_CPU_LIMIT_SECS: u64 = 25 * 60;
/// 单个输出文件上限；HLS 分片/MP4 都不应该接近这个值。
const TOOL_FSIZE_LIMIT: u64 = 4 * 1024 * 1024 * 1024;
/// 外部工具地址空间上限（Linux）；防止病态媒体把 ffmpeg 吃成 OOM killer。
const TOOL_AS_LIMIT: u64 = 8 * 1024 * 1024 * 1024;

/// 上传字段的临时落盘文件。Drop 时尽量删除：成功路径由调用方移交到
/// pending/对象目录，失败路径不能把半个 500 MiB 的 body 留在磁盘上。
pub(crate) struct UploadTemp {
    path: PathBuf,
}

impl UploadTemp {
    /// 在视频根目录下创建一个唯一的新文件。`create_new` 让并发上传不会
    /// 互相覆盖；路径只由服务端生成，不接受客户端文件名。
    pub(crate) fn new_file(root: &Path, prefix: &str) -> anyhow::Result<(Self, std::fs::File)> {
        std::fs::create_dir_all(root)
            .with_context(|| format!("create upload temp root {}", root.display()))?;
        for _ in 0..16 {
            let path = root.join(format!(".{prefix}-{:016x}.part", rand::random::<u64>()));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => return Ok((Self { path }, file)),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => {
                    return Err(err)
                        .with_context(|| format!("create upload temp {}", path.display()))
                }
            }
        }
        anyhow::bail!("could not allocate a unique upload temp file")
    }

    pub(crate) fn new(root: &Path, prefix: &str) -> anyhow::Result<Self> {
        let (temp, file) = Self::new_file(root, prefix)?;
        drop(file);
        Ok(temp)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// 兼容旧 `VideoStore::ingest(&[u8])` 的落盘路径。生产 HTTP 面不再走
    /// 这里；保留给测试与调用方传字节的 API。
    pub(crate) fn write_bytes(&self, bytes: &[u8]) -> anyhow::Result<()> {
        std::fs::write(&self.path, bytes)
            .with_context(|| format!("write upload temp {}", self.path.display()))
    }
}

impl Drop for UploadTemp {
    fn drop(&mut self) {
        std::fs::remove_file(&self.path).ok();
    }
}

/// 把 multipart 的 `bytes`/`file` 字段流式写入临时文件。**不把整个字段
/// 读成 `Vec<u8>`**；长度边写边累计，超过 500 MiB 立即中止。
pub(crate) async fn stage_field(
    mut field: Field<'_>,
    root: &Path,
) -> Result<(UploadTemp, u64), String> {
    let (temp, std_file) =
        UploadTemp::new_file(root, "upload").map_err(|err| format!("create upload temp: {err}"))?;
    let mut file = tokio::fs::File::from_std(std_file);
    // 有界 channel：读 body 与写盘解耦，但最多积压 4 个 body frame；磁盘慢时
    // `send` 反压到网络读，峰值内存不随文件大小增长。
    let (tx, mut rx) = tokio::sync::mpsc::channel::<axum::body::Bytes>(4);
    let writer = tokio::spawn(async move {
        let mut size = 0u64;
        while let Some(chunk) = rx.recv().await {
            size = size.saturating_add(chunk.len() as u64);
            if size > MAX_VIDEO_BYTES as u64 {
                return Err(format!("upload exceeds {} bytes", MAX_VIDEO_BYTES));
            }
            file.write_all(&chunk)
                .await
                .map_err(|err| format!("write upload temp: {err}"))?;
        }
        file.flush()
            .await
            .map_err(|err| format!("flush upload temp: {err}"))?;
        Ok::<u64, String>(size)
    });

    let mut read_err: Option<String> = None;
    loop {
        match field.chunk().await {
            Ok(Some(chunk)) => {
                // writer 已经失败（超限/磁盘错）时接收端关闭，这里停止继续读 body。
                if tx.send(chunk).await.is_err() {
                    break;
                }
            }
            Ok(None) => break,
            Err(err) => {
                read_err = Some(format!("read upload chunk: {err}"));
                break;
            }
        }
    }
    drop(tx);

    let written = writer
        .await
        .map_err(|err| format!("upload writer task: {err}"))?;
    if let Some(err) = read_err {
        return Err(err);
    }
    let size = written?;
    if size == 0 {
        return Err("empty upload".into());
    }
    Ok((temp, size))
}

/// 实际产出的档位：阶梯中 ≤ 源高的项；源比最小档还矮时保留源高（封顶 720）。
///
/// 纯函数，便于在没有 ffmpeg 的环境里验收阶梯规则本身。
pub fn rendition_heights(src_h: u32) -> Vec<u32> {
    let mut out: Vec<u32> = LADDER.iter().copied().filter(|h| *h <= src_h).collect();
    if out.is_empty() {
        out.push(src_h.clamp(16, TARGET_HEIGHT));
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// 选流时最多允许用掉可用带宽的这几成。
///
/// 为什么留两成：`available_bps` 是客户端上一段的**实测**值，用它跑满档必然
/// 卡在下一段的抖动上。留余量这条规则写在 Rust 里（不是 TS/Dart 里），三端
/// 才会选到同一档。
pub const SELECTION_HEADROOM_PCT: u64 = 80;

/// 实测可用带宽 → 本轮可用额度。
pub fn usable_bandwidth(available_bps: u64) -> u64 {
    available_bps.saturating_mul(SELECTION_HEADROOM_PCT) / 100
}

/// 按码率选档：返回**放得下**的最高档；一档都放不下时给最低档
/// （宁可糊一点，也不要黑屏）；空阶梯返回 `None`。
///
/// `renditions` 不必预先排序（内部按高度取最大）。
pub fn select_rendition(renditions: &[Rendition], available_bps: u64) -> Option<&Rendition> {
    let usable = usable_bandwidth(available_bps);
    let fit = renditions
        .iter()
        .filter(|r| r.bandwidth_bps <= usable)
        .max_by_key(|r| r.height);
    fit.or_else(|| renditions.iter().min_by_key(|r| r.height))
}

/// master playlist（`#EXTM3U` + 每档一条 `#EXT-X-STREAM-INF`）。
///
/// 纯函数：格式、升序、BANDWIDTH/RESOLUTION/NAME 的取值都在这里，不需要 ffmpeg
/// 就能断言。入参顺序即输出顺序——调用方（`run_pipeline`）按阶梯升序传进来。
pub fn master_playlist(renditions: &[Rendition]) -> String {
    let mut out = String::from("#EXTM3U\n#EXT-X-VERSION:3\n");
    for r in renditions {
        out.push_str(&format!(
            "#EXT-X-STREAM-INF:BANDWIDTH={},RESOLUTION={}x{},NAME=\"{}p\"\n{}\n",
            r.bandwidth_bps, r.width, r.height, r.height, r.playlist
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// metadata
// ---------------------------------------------------------------------------

/// What ffprobe told us about the source, persisted as `meta.json`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct VideoMeta {
    pub video_id: String,
    /// Exact source size in bytes.
    pub source_size: u64,
    pub duration_ms: u32,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub video_codec: String,
    #[serde(default)]
    pub audio_codec: String,
    /// Transcode parameters actually used（最高档，兼容单档时代字段）。
    pub transcode: TranscodeInfo,
    /// P5 多分辨率档位。老 `meta.json` 没有这个字段 → 空（播放仍走 master）。
    #[serde(default)]
    pub renditions: Vec<Rendition>,
}

/// 一个分辨率档位。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Rendition {
    pub height: u32,
    pub width: u32,
    /// 实测码率 = mp4 字节数 × 8 / 时长（master playlist 的 BANDWIDTH）。
    pub bandwidth_bps: u64,
    /// 相对 `hls/` 的播放列表路径，如 `720/playlist.m3u8`。
    pub playlist: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TranscodeInfo {
    pub resolution: String,
    pub video_codec: String,
    pub audio_codec: String,
    pub created_at_ms: u64,
}

// ---------------------------------------------------------------------------
// tool discovery
// ---------------------------------------------------------------------------

fn tool(env_key: &str, default: &str) -> String {
    std::env::var(env_key).unwrap_or_else(|_| default.to_string())
}

pub fn ffmpeg_bin() -> String {
    tool("WXLIKE_FFMPEG", "ffmpeg")
}

pub fn ffprobe_bin() -> String {
    tool("WXLIKE_FFPROBE", "ffprobe")
}

/// Whether the external tools resolve — used by the `--ignored` tests to
/// bail with a clear message instead of a confusing spawn failure.
pub fn tools_available() -> bool {
    for bin in [ffmpeg_bin(), ffprobe_bin()] {
        let mut cmd = Command::new(&bin);
        cmd.arg("-version");
        let Ok(out) = run_limited(cmd, TOOL_VERSION_TIMEOUT, &bin, false) else {
            return false;
        };
        if !out.status.success() {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// store
// ---------------------------------------------------------------------------

/// On-disk layout, one directory per video (content-addressed):
/// `{root}/{video_id}/source` · `hls/playlist.m3u8` · `hls/seg_NNN.ts` ·
/// `thumb.png` · `meta.json`. A rename-into-place after a full temp
/// transcode keeps partial jobs invisible.
///
/// Zone E adds two siblings: `{root}/video.db`（播放统计 + 转码作业，见 `db` 模块）
/// 与 `{root}/live/{stream_key}/`（直播窗口，见 `live` 模块）。对象目录本身仍然
/// 只在转码完整成功后出现，所以老代码的"目录存在=可播"这条不变式没被改动。
#[derive(Clone)]
pub struct VideoStore {
    inner: Arc<Inner>,
}

struct Inner {
    root: PathBuf,
    /// 懒建：只有第一个统计/作业请求才会打开 `video.db`。
    /// 纯 VOD 部署（不碰 E1/E3 的接口）因此依旧一个文件都不多建。
    db: Mutex<Option<Arc<db::VideoDb>>>,
    /// 控制面准入（认证、配额、回调目标）。见 `policy` 模块与 issue #7/#8。
    policy: Arc<policy::VideoPolicy>,
}

impl VideoStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let inner = Inner {
            root: root.into(),
            db: Mutex::new(None),
            // 默认**关死**控制面：没有认证器就不接受上传/建作业/重试。
            // 装配层（server）必须显式装上认证器——忘了装是"不能用"，
            // 不是"谁都能用"。
            policy: Arc::new(policy::VideoPolicy::default()),
        };
        Self {
            inner: Arc::new(inner),
        }
    }

    /// 装上控制面策略（认证器、配额、回调目标）。
    pub fn with_policy(mut self, policy: policy::VideoPolicy) -> Self {
        self.inner = Arc::new(Inner {
            root: self.inner.root.clone(),
            db: Mutex::new(None),
            policy: Arc::new(policy),
        });
        self
    }

    pub fn policy(&self) -> &policy::VideoPolicy {
        &self.inner.policy
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// 播放统计与转码作业共用的库（`{root}/video.db`）。
    pub(crate) fn db(&self) -> anyhow::Result<Arc<db::VideoDb>> {
        let mut guard = self.inner.db.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(db) = guard.as_ref() {
            return Ok(Arc::clone(db));
        }
        std::fs::create_dir_all(self.root())
            .with_context(|| format!("create video root {}", self.root().display()))?;
        let opened = Arc::new(db::VideoDb::open(&self.root().join("video.db"))?);
        *guard = Some(Arc::clone(&opened));
        Ok(opened)
    }

    /// 内容寻址 id：源字节的 SHA-256。`video_id` 与 `job_id` 是同一个值——
    /// 幂等性就从这里来。
    pub fn content_id(&self, bytes: &[u8]) -> String {
        hex::encode(sha2::Sha256::digest(bytes))
    }

    /// 流式哈希一个源文件：只按 64 KiB 块读，不把文件读进内存。
    pub(crate) fn hash_file(&self, path: &Path) -> anyhow::Result<(String, u64)> {
        let mut file = std::fs::File::open(path)
            .with_context(|| format!("open ingest source {}", path.display()))?;
        let mut hasher = sha2::Sha256::new();
        let mut size = 0u64;
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = file
                .read(&mut buf)
                .with_context(|| format!("read ingest source {}", path.display()))?;
            if n == 0 {
                break;
            }
            size = size.saturating_add(n as u64);
            hasher.update(&buf[..n]);
        }
        Ok((hex::encode(hasher.finalize()), size))
    }

    fn dir(&self, video_id: &str) -> PathBuf {
        self.root().join(video_id)
    }

    pub fn contains(&self, video_id: &str) -> bool {
        self.dir(video_id).join("meta.json").is_file()
    }

    pub fn meta(&self, video_id: &str) -> anyhow::Result<VideoMeta> {
        let bytes = std::fs::read(self.dir(video_id).join("meta.json"))
            .with_context(|| format!("read meta for {video_id}"))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// master playlist（`hls/playlist.m3u8`，与 `hls/master.m3u8` 同内容）。
    /// 单档时代的老对象也满足这个约定：文件里直接是分片列表。
    pub fn playlist(&self, video_id: &str) -> anyhow::Result<Vec<u8>> {
        std::fs::read(self.dir(video_id).join("hls").join("playlist.m3u8"))
            .with_context(|| format!("read playlist for {video_id}"))
    }

    /// 某个档位的播放列表（`hls/{height}/playlist.m3u8`）。
    pub fn rendition_playlist(&self, video_id: &str, height: u32) -> anyhow::Result<Vec<u8>> {
        std::fs::read(
            self.dir(video_id)
                .join("hls")
                .join(height.to_string())
                .join("playlist.m3u8"),
        )
        .with_context(|| format!("read {height}p playlist for {video_id}"))
    }

    /// One HLS segment. `name` was validated by the caller; it may be
    /// `seg_000.ts`（单档老对象）或 `720/seg_000.ts`（多档）。
    pub fn segment(&self, video_id: &str, name: &str) -> anyhow::Result<Vec<u8>> {
        let rel = match name.split_once('/') {
            Some((dir, file)) => {
                anyhow::ensure!(
                    !dir.is_empty() && dir.chars().all(|c| c.is_ascii_digit()),
                    "rendition directory must be numeric"
                );
                PathBuf::from(dir).join(file)
            }
            None => PathBuf::from(name),
        };
        std::fs::read(self.dir(video_id).join("hls").join(rel))
            .with_context(|| format!("read segment {name} for {video_id}"))
    }

    pub fn thumb(&self, video_id: &str) -> anyhow::Result<Vec<u8>> {
        std::fs::read(self.dir(video_id).join("thumb.png"))
            .with_context(|| format!("read thumb for {video_id}"))
    }

    /// The video directory itself (used for artifacts that have no dedicated
    /// reader yet — `play.mp4`); the caller has already validated the id.
    pub fn dir_public(&self, video_id: &str) -> PathBuf {
        self.dir(video_id)
    }
}

/// The ingest result: what the message-side reference needs plus the
/// human-facing dimensions.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Ingested {
    pub video_id: String,
    pub playlist_url: String,
    /// Progressive MP4 remux of the same transcode — browsers' native
    /// `<video>` cannot read HLS, so in-app playback (and the 倍速 control)
    /// runs off this file; HLS stays the interop surface (players, P5 live).
    pub play_url: String,
    pub thumb_url: String,
    pub duration_ms: u32,
    pub width: u32,
    pub height: u32,
    /// 多分辨率档位（客户端可据此选初始档位，不必先读 master）。
    pub renditions: Vec<Rendition>,
}

impl Ingested {
    /// 由元数据造引用。地址是相对路径，与 `meta.json` 同源于内容哈希。
    pub fn from_meta(video_id: &str, meta: &VideoMeta) -> Self {
        Self {
            video_id: video_id.to_string(),
            playlist_url: playlist_url(video_id),
            play_url: play_url(video_id),
            thumb_url: thumb_url(video_id),
            duration_ms: meta.duration_ms,
            width: meta.width,
            height: meta.height,
            renditions: meta.renditions.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// ingest pipeline
// ---------------------------------------------------------------------------

impl VideoStore {
    /// 兼容字节调用方的入口：先把字节落临时文件，再走 [`Self::ingest_file`]。
    /// 不再 `to_vec`，也不会把 500 MiB 的副本搬进 blocking 任务；HTTP 上传
    /// 直接调用 `ingest_file`，不会经过这里。
    pub async fn ingest(&self, bytes: &[u8]) -> anyhow::Result<Ingested> {
        anyhow::ensure!(!bytes.is_empty(), "empty upload");
        let temp = UploadTemp::new(self.root(), "ingest")?;
        temp.write_bytes(bytes)?;
        self.ingest_file(temp.path()).await
    }

    /// 从已落盘文件 ingest。哈希、转码都只读文件，不在内存里复制源字节。
    /// 调用方拥有 `source` 的生命周期：成功/失败后自行删除。
    pub async fn ingest_file(&self, source: &Path) -> anyhow::Result<Ingested> {
        anyhow::ensure!(source.is_file(), "ingest source is missing");
        let source = source.to_path_buf();
        let store = self.clone();
        let hash_source = source.clone();
        let (video_id, source_size) =
            tokio::task::spawn_blocking(move || store.hash_file(&hash_source))
                .await
                .context("hash task panicked")??;
        anyhow::ensure!(source_size > 0, "empty upload");
        if self.contains(&video_id) {
            return self.ingested_from_meta(&video_id);
        }

        let store = self.clone();
        let task_id = video_id.clone();
        let meta = tokio::task::spawn_blocking(move || {
            store.transcode_from_path(&task_id, &source, source_size)
        })
        .await
        .context("transcode task panicked")??;

        Ok(Ingested::from_meta(&video_id, &meta))
    }

    /// 由已落盘的 `meta.json` 造出消息侧要的引用（不重跑 ffmpeg）。
    ///
    /// 同步上传、作业队列、重复上传三条路都收敛到这里，所以"对象存在"和
    /// "客户端拿到的地址"不可能对不上。
    pub fn ingested_from_meta(&self, video_id: &str) -> anyhow::Result<Ingested> {
        let meta = self.meta(video_id)?;
        Ok(Ingested::from_meta(video_id, &meta))
    }

    /// The blocking pipeline: probe → transcode → thumb → rename into place.
    /// `source` 是已落盘的源文件；本函数不再复制源字节。
    fn transcode_from_path(
        &self,
        video_id: &str,
        source: &Path,
        source_size: u64,
    ) -> anyhow::Result<VideoMeta> {
        let final_dir = self.dir(video_id);
        let temp_dir = self
            .root()
            .join(format!(".tmp-{video_id}-{}", wxlike_core::types::now_ms()));
        std::fs::create_dir_all(temp_dir.join("hls"))
            .with_context(|| "create temp transcode dir".to_string())?;

        match self.run_pipeline(video_id, source, source_size, &temp_dir) {
            Ok(meta) => self.finish_transcode(video_id, meta, &temp_dir, &final_dir),
            Err(err) => {
                cleanup(&temp_dir);
                Err(err)
            }
        }
    }

    /// 把临时目录换到最终位置；同内容并发转码时采用已存在的对象。
    fn finish_transcode(
        &self,
        video_id: &str,
        meta: VideoMeta,
        temp_dir: &std::path::Path,
        final_dir: &std::path::Path,
    ) -> anyhow::Result<VideoMeta> {
        match std::fs::rename(temp_dir, final_dir) {
            Ok(()) => Ok(meta),
            Err(err) => {
                // 同内容并发转码（同步上传撞上班里的同一个作业）时，赢家的
                // 目录已经在那儿了。此时"对象存在"是真话，不是降级——清掉
                // 自己的临时目录，返回同一份结果。
                if self.contains(video_id) {
                    cleanup(temp_dir);
                    tracing::debug!(video_id, error = %err, "并发转码：采用已存在的对象");
                    return Ok(meta);
                }
                cleanup(temp_dir);
                Err(err).with_context(|| "move transcoded artifacts into place".to_string())
            }
        }
    }

    fn run_pipeline(
        &self,
        video_id: &str,
        source: &Path,
        source_size: u64,
        temp_dir: &Path,
    ) -> anyhow::Result<VideoMeta> {
        let probe = ffprobe(source)?;
        let duration_ms = probe.duration_ms()?;
        let (_src_w, src_h) = probe.video_size();
        let heights = rendition_heights(src_h);

        let hls_root = temp_dir.join("hls");
        std::fs::create_dir_all(&hls_root)?;
        let (renditions, (best_h, best_mp4, best_w)) =
            encode_renditions(source, temp_dir, &hls_root, &probe, &heights, duration_ms)?;

        // 渐进播放源 = 最高档（webview 的 <video> 与倍速靠它）
        std::fs::copy(&best_mp4, temp_dir.join("play.mp4"))
            .with_context(|| "copy best rendition to play.mp4".to_string())?;
        publish_playlists(&hls_root, &renditions)?;
        make_thumbnail(source, temp_dir, duration_ms)?;
        ensure_dir_budget(temp_dir)?;

        let meta = VideoMeta {
            video_id: video_id.to_string(),
            source_size,
            duration_ms,
            width: best_w,
            height: best_h,
            video_codec: probe.video_codec(),
            audio_codec: probe.audio_codec(),
            transcode: TranscodeInfo {
                resolution: format!("-2x{best_h}"),
                video_codec: "h264".into(),
                audio_codec: "aac".into(),
                created_at_ms: wxlike_core::types::now_ms(),
            },
            renditions,
        };
        std::fs::write(
            temp_dir.join("meta.json"),
            serde_json::to_vec_pretty(&meta)?,
        )
        .with_context(|| "write meta.json".to_string())?;
        Ok(meta)
    }
}

/// 逐档编码：MP4 + 从 MP4 `-c copy` 切 HLS；返回升序 renditions 与最高档。
fn encode_renditions(
    source: &Path,
    temp_dir: &Path,
    hls_root: &Path,
    probe: &Probe,
    heights: &[u32],
    duration_ms: u32,
) -> anyhow::Result<(Vec<Rendition>, (u32, PathBuf, u32))> {
    let mut renditions = Vec::new();
    let mut best: Option<(u32, PathBuf, u32)> = None;
    for height in heights {
        let (rendition, mp4, width) =
            encode_one(source, temp_dir, hls_root, probe, *height, duration_ms)?;
        // 每档产出后都查一次预算：不等整个 3 档跑完才发现磁盘被写爆。
        ensure_dir_budget(temp_dir)?;
        renditions.push(rendition);
        best = Some((*height, mp4, width));
    }
    let best = best.context("no rendition was produced")?;
    Ok((renditions, best))
}

fn encode_one(
    source: &Path,
    temp_dir: &Path,
    hls_root: &Path,
    probe: &Probe,
    height: u32,
    duration_ms: u32,
) -> anyhow::Result<(Rendition, PathBuf, u32)> {
    let width = probe.width_at(height);
    let mp4 = encode_mp4(source, temp_dir, height)?;
    let playlist = encode_hls(&mp4, hls_root, height)?;
    let bandwidth_bps = bandwidth_of(&mp4, duration_ms)?;
    Ok((
        Rendition {
            height,
            width,
            bandwidth_bps,
            playlist,
        },
        mp4,
        width,
    ))
}

/// 一次编码/档：先出 progressive MP4，再从它 `-c copy` 切 HLS。
fn encode_mp4(source: &Path, temp_dir: &Path, height: u32) -> anyhow::Result<PathBuf> {
    // libx264 要求偶数宽高；`-2` 保持比例并取偶
    let scale = format!("scale=-2:{height}");
    let mp4 = temp_dir.join(format!("play_{height}.mp4"));
    run_tool(
        &ffmpeg_bin(),
        &[
            "-y",
            "-i",
            source.to_str().context("source path")?,
            "-vf",
            &scale,
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "26",
            "-c:a",
            "aac",
            "-b:a",
            "96k",
            "-movflags",
            "+faststart",
            mp4.to_str().context("mp4 path")?,
        ],
    )
    .with_context(|| format!("ffmpeg transcode {height}p to mp4"))?;
    anyhow::ensure!(mp4.is_file(), "ffmpeg produced no {height}p mp4");
    Ok(mp4)
}

/// 从 MP4 `-c copy` 切 HLS；返回相对 hls 根的 playlist 路径。
fn encode_hls(mp4: &Path, hls_root: &Path, height: u32) -> anyhow::Result<String> {
    let rendition_dir = hls_root.join(height.to_string());
    std::fs::create_dir_all(&rendition_dir)?;
    run_tool(
        &ffmpeg_bin(),
        &[
            "-y",
            "-i",
            mp4.to_str().context("mp4 path")?,
            "-c",
            "copy",
            "-hls_time",
            &format!("{HLS_SECONDS}"),
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
            rendition_dir
                .join("seg_%03d.ts")
                .to_str()
                .context("seg path")?,
            rendition_dir
                .join("playlist.m3u8")
                .to_str()
                .context("playlist path")?,
        ],
    )
    .with_context(|| format!("ffmpeg remux {height}p to hls"))?;
    anyhow::ensure!(
        rendition_dir.join("playlist.m3u8").is_file(),
        "ffmpeg produced no {height}p playlist"
    );
    Ok(format!("{height}/playlist.m3u8"))
}

/// 实测码率：比 CRF 猜一个常数靠谱，master 的 BANDWIDTH 直接可用。
fn bandwidth_of(mp4: &Path, duration_ms: u32) -> anyhow::Result<u64> {
    let size = std::fs::metadata(mp4)?.len();
    Ok(if duration_ms > 0 {
        size.saturating_mul(8).saturating_mul(1_000) / u64::from(duration_ms)
    } else {
        0
    })
}

/// master playlist 与老客户端兼容路径。
fn publish_playlists(hls_root: &Path, renditions: &[Rendition]) -> anyhow::Result<()> {
    let master = master_playlist(renditions);
    std::fs::write(hls_root.join("master.m3u8"), master.as_bytes())
        .with_context(|| "write master playlist".to_string())?;
    // 固定路径兼容：老客户端只认 `hls/playlist.m3u8`
    std::fs::write(hls_root.join("playlist.m3u8"), master.as_bytes())
        .with_context(|| "write legacy playlist path".to_string())?;
    Ok(())
}

/// 缩略图：第一条片段的 1s 处；短视频退回第 0 帧。
fn make_thumbnail(source: &Path, temp_dir: &Path, duration_ms: u32) -> anyhow::Result<()> {
    let seek = if duration_ms > 1_500 { "1" } else { "0" };
    let thumb = temp_dir.join("thumb.png");
    run_tool(
        &ffmpeg_bin(),
        &[
            "-y",
            "-ss",
            seek,
            "-i",
            source.to_str().context("source path")?,
            "-frames:v",
            "1",
            thumb.to_str().context("thumb path")?,
        ],
    )
    .context("ffmpeg thumbnail")?;
    anyhow::ensure!(thumb.is_file(), "ffmpeg produced no thumbnail");
    Ok(())
}

pub fn playlist_url(video_id: &str) -> String {
    format!("/api/video/{video_id}/playlist.m3u8")
}

pub fn thumb_url(video_id: &str) -> String {
    format!("/api/video/{video_id}/thumb.png")
}

pub fn play_url(video_id: &str) -> String {
    format!("/api/video/{video_id}/play.mp4")
}

// ---------------------------------------------------------------------------
// ffprobe output model (only the fields we consume)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct Probe {
    pub format: ProbeFormat,
    #[serde(default)]
    pub streams: Vec<ProbeStream>,
}

#[derive(Deserialize)]
pub struct ProbeFormat {
    pub duration: Option<String>,
}

#[derive(Deserialize)]
pub struct ProbeStream {
    #[serde(default, rename = "codec_type")]
    pub codec_type: Option<String>,
    #[serde(default, rename = "codec_name")]
    pub codec_name: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

impl Probe {
    pub fn duration_ms(&self) -> anyhow::Result<u32> {
        let secs: f64 = self
            .format
            .duration
            .as_deref()
            .and_then(|d| d.parse().ok())
            .context("ffprobe reported no duration")?;
        anyhow::ensure!(secs > 0.0, "non-positive duration");
        Ok((secs * 1000.0).round() as u32)
    }

    pub fn video(&self) -> Option<&ProbeStream> {
        self.streams
            .iter()
            .find(|s| s.codec_type.as_deref() == Some("video"))
    }

    pub fn video_size(&self) -> (u32, u32) {
        match self.video() {
            Some(s) => (s.width.unwrap_or(0), s.height.unwrap_or(0)),
            None => (0, 0),
        }
    }

    /// 目标高度下的输出宽度（`-2:{h}` 的等价计算：保比例、取偶）。
    pub fn width_at(&self, target_h: u32) -> u32 {
        let (w, h) = self.video_size();
        if w == 0 || h == 0 || target_h == 0 {
            return 0;
        }
        ((u64::from(w) * u64::from(target_h) + u64::from(h) / 2) / u64::from(h)) as u32 & !1
    }

    /// 兼容单档时代的签名：按 `TARGET_HEIGHT` 封顶后的宽度。
    pub fn width_of(&self, _scale: &str) -> u32 {
        let (_w, h) = self.video_size();
        self.width_at(h.clamp(16, TARGET_HEIGHT))
    }

    pub fn video_codec(&self) -> String {
        self.video()
            .and_then(|s| s.codec_name.clone())
            .unwrap_or_default()
    }

    pub fn audio_codec(&self) -> String {
        self.streams
            .iter()
            .find(|s| s.codec_type.as_deref() == Some("audio"))
            .and_then(|s| s.codec_name.clone())
            .unwrap_or_default()
    }
}

fn ffprobe(source: &Path) -> anyhow::Result<Probe> {
    let mut cmd = Command::new(ffprobe_bin());
    cmd.args([
        "-v",
        "quiet",
        // 上传的是本地文件；禁止 ffprobe 跟随远端协议（H-19 协议白名单）。
        "-protocol_whitelist",
        "file,pipe",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
    ])
    .arg(source);
    let out = run_limited(
        cmd,
        FFPROBE_TIMEOUT,
        "ffprobe (is ffmpeg installed? set WXLIKE_FFPROBE)",
        true,
    )
    .context("spawn ffprobe")?;
    anyhow::ensure!(out.status.success(), "ffprobe failed on the upload");
    serde_json::from_slice(&out.stdout).context("parse ffprobe json")
}

/// 递归统计目录字节数；只用于转码临时目录。
fn dir_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if meta.is_dir() {
            total = total.saturating_add(dir_bytes(&entry.path()));
        } else {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

fn ensure_dir_budget(path: &Path) -> anyhow::Result<()> {
    ensure_dir_budget_at(path, MAX_TRANSCODE_BYTES)
}

fn ensure_dir_budget_at(path: &Path, max: u64) -> anyhow::Result<()> {
    let used = dir_bytes(path);
    anyhow::ensure!(
        used <= max,
        "transcode temp dir uses {used} bytes, over the {max}-byte budget"
    );
    Ok(())
}

fn run_tool(bin: &str, args: &[&str]) -> anyhow::Result<()> {
    let mut cmd = Command::new(bin);
    // 源/目标都是本机路径；HLS 输出也只走 file。禁掉 http/rtmp 等协议，
    // 防止恶意媒体把 ffmpeg 变成出站客户端。
    cmd.args(["-protocol_whitelist", "file,pipe"]);
    cmd.args(args);
    let out =
        run_limited(cmd, FFMPEG_TIMEOUT, bin, false).with_context(|| format!("spawn {bin}"))?;
    anyhow::ensure!(out.status.success(), "{bin} exited non-zero");
    Ok(())
}

/// 限时执行外部工具，超时杀**整个进程组**。
///
/// ffmpeg 的转码进程会再起子进程；只 kill 直接子进程会留下孤儿占着 CPU/磁盘。
/// Unix 上让子进程自成一组（`process_group(0)`），超时对负 pid 发 SIGKILL。
/// 读取管道放在进程退出之后：ffprobe 的输出很小，不会先写满管道再卡死。
fn run_limited(
    mut cmd: Command,
    timeout: Duration,
    label: &str,
    capture_stdout: bool,
) -> anyhow::Result<Output> {
    cmd.stdout(if capture_stdout {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
        #[cfg(target_os = "linux")]
        {
            // pre_exec 在 fork 后 exec 前执行；只在这里设置子进程自己的
            // rlimit，不影响 hub 本体。
            unsafe {
                cmd.pre_exec(move || {
                    let cpu = libc::rlimit {
                        rlim_cur: TOOL_CPU_LIMIT_SECS,
                        rlim_max: TOOL_CPU_LIMIT_SECS,
                    };
                    let fsize = libc::rlimit {
                        rlim_cur: TOOL_FSIZE_LIMIT,
                        rlim_max: TOOL_FSIZE_LIMIT,
                    };
                    let address = libc::rlimit {
                        rlim_cur: TOOL_AS_LIMIT,
                        rlim_max: TOOL_AS_LIMIT,
                    };
                    if libc::setrlimit(libc::RLIMIT_CPU, &cpu) != 0
                        || libc::setrlimit(libc::RLIMIT_FSIZE, &fsize) != 0
                        || libc::setrlimit(libc::RLIMIT_AS, &address) != 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
    }
    let mut child = cmd.spawn().with_context(|| format!("spawn {label}"))?;
    let pid = child.id();
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= timeout {
            #[cfg(unix)]
            unsafe {
                // 负 pid 表示整个进程组；子进程已退出时 ESRCH 也无害。
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
            #[cfg(not(unix))]
            {
                let _ = child.kill();
            }
            let _ = child.wait();
            anyhow::bail!(
                "{label} timed out after {}s",
                timeout.as_secs_f64().round() as u64
            );
        }
        std::thread::sleep(TOOL_POLL);
    };
    let mut stdout = Vec::new();
    if capture_stdout {
        if let Some(mut pipe) = child.stdout.take() {
            pipe.read_to_end(&mut stdout)?;
        }
    }
    Ok(Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// HTTP surface
// ---------------------------------------------------------------------------

/// Shared service state.
#[derive(Clone)]
pub struct VideoState {
    pub store: VideoStore,
}

pub fn routes(state: VideoState) -> Router {
    // **播放面与控制面分开**（issue #7）。
    //
    // 播放面必须匿名可用：`<video src>` 与 HLS 播放器不会给图片/分片请求带
    // `Authorization`（与 #14 的媒体面同一个约束）。控制面（上传、建作业、
    // 查/重试作业）则必须在**解析请求体之前**就认出调用者——见 `policy::guard`。
    let playback = Router::new()
        .route("/api/video/{video_id}", get(meta))
        .route("/api/video/{video_id}/playlist.m3u8", get(playlist))
        .route("/api/video/{video_id}/seg/{name}", get(segment))
        .route(
            "/api/video/{video_id}/{rendition}/playlist.m3u8",
            get(rendition_playlist),
        )
        .route(
            "/api/video/{video_id}/{rendition}/seg/{name}",
            get(segment_in_rendition),
        )
        // 档位播放列表里的分片 URI 是**相对名**（`seg_000.ts`）。播放器拿它
        // 相对清单地址解析，落点就是 `/api/video/{id}/{rendition}/seg_000.ts`
        // ——没有 `/seg/` 这一段。以前只有带 `/seg/` 的路由，标准播放器解析
        // 出的地址全是 404；这一条让相对名直接命中（`playlist.m3u8` 的静态
        // 路由优先级更高，不会被它吞掉）。
        .route(
            "/api/video/{video_id}/{rendition}/{name}",
            get(segment_named),
        )
        .route("/api/video/{video_id}/play.mp4", get(play_mp4))
        .route("/api/video/{video_id}/thumb.png", get(thumb))
        .route("/api/video/{video_id}/select", get(select))
        .with_state(state.clone());

    let control = Router::new()
        .route("/api/video", post(upload))
        .with_state(state.clone())
        .merge(jobs::routes(state.clone()))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            policy::guard,
        ));

    playback
        .merge(control)
        // Zone E 的两面自带凭据（stats 用上报 token、live 用推流签名），
        // 不套控制面的闸门。
        .merge(stats::routes(state.clone()))
        .merge(live::routes(state))
        .layer(DefaultBodyLimit::max(MAX_VIDEO_BYTES))
}

async fn upload(
    State(state): State<VideoState>,
    headers: axum::http::HeaderMap,
    mut form: Multipart,
) -> ApiResult<Json<Ingested>> {
    // 同步上传也要登录（issue #7）：500 MiB 的匿名上传是"带宽 + 磁盘 + CPU
    // 三份资源任人取用"，不是"一个方便的调试入口"。
    let caller = policy::require_caller(state.store.policy(), &headers)?;
    let (source, size) = read_field(&mut form, state.store.root())
        .await
        .map_err(ApiError::unprocessable)?;
    if size == 0 {
        return Err(ApiError::unprocessable("empty upload"));
    }
    state
        .store
        .check_upload_quota(&caller, size)
        .map_err(|err| match err.downcast_ref::<jobs::Rejected>() {
            Some(rejected) => ApiError::new(
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                rejected.message.clone(),
            ),
            None => ApiError::internal(format!("quota: {err:#}")),
        })?;
    let ingested = state
        .store
        .ingest_file(source.path())
        .await
        .map_err(|e| ApiError::internal(format!("ingest: {e:#}")))?;
    state
        .store
        .set_owner(&ingested.video_id, &caller, size)
        .map_err(|e| ApiError::internal(format!("owner: {e:#}")))?;
    Ok(Json(ingested))
}

/// 读盘（含元数据 JSON 解析）一律进 blocking 池：异步线程上做同步 IO，
/// 一个慢盘就能拖住整条 worker（审计第 5 轮）。下面的 helper 收拢这一模式。
async fn read_meta_blocking(store: VideoStore, video_id: String) -> ApiResult<VideoMeta> {
    tokio::task::spawn_blocking(move || store.meta(&video_id))
        .await
        .map_err(panic_as_internal)?
        .map_err(not_found_if_missing)
}

async fn read_artifact_blocking(
    store: VideoStore,
    video_id: String,
    read: fn(&VideoStore, &str) -> anyhow::Result<Vec<u8>>,
) -> ApiResult<Vec<u8>> {
    tokio::task::spawn_blocking(move || read(&store, &video_id))
        .await
        .map_err(panic_as_internal)?
        .map_err(not_found_if_missing)
}

async fn read_segment_blocking(
    store: VideoStore,
    video_id: String,
    name: String,
) -> ApiResult<Vec<u8>> {
    tokio::task::spawn_blocking(move || store.segment(&video_id, &name))
        .await
        .map_err(panic_as_internal)?
        .map_err(not_found_if_missing)
}

fn panic_as_internal(err: tokio::task::JoinError) -> ApiError {
    tracing::error!(error = %err, "video read task panicked");
    ApiError::internal("video storage failure")
}

async fn meta(
    State(state): State<VideoState>,
    AxumPath(video_id): AxumPath<String>,
) -> ApiResult<Json<VideoMeta>> {
    check_id(&video_id)?;
    Ok(Json(read_meta_blocking(state.store, video_id).await?))
}

async fn playlist(
    State(state): State<VideoState>,
    AxumPath(video_id): AxumPath<String>,
) -> ApiResult<impl IntoResponse> {
    check_id(&video_id)?;
    let body = read_artifact_blocking(state.store, video_id, VideoStore::playlist).await?;
    Ok((
        [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
        body,
    ))
}

async fn segment(
    State(state): State<VideoState>,
    AxumPath((video_id, name)): AxumPath<(String, String)>,
) -> ApiResult<impl IntoResponse> {
    check_id(&video_id)?;
    check_segment_name(&name)?;
    let body = read_segment_blocking(state.store, video_id, name).await?;
    Ok(([(header::CONTENT_TYPE, "video/mp2t")], body))
}

/// 档位播放列表：`/api/video/{id}/720/playlist.m3u8`（master 里引用的就是它）。
async fn rendition_playlist(
    State(state): State<VideoState>,
    AxumPath((video_id, rendition)): AxumPath<(String, String)>,
) -> ApiResult<impl IntoResponse> {
    check_id(&video_id)?;
    check_rendition(&rendition)?;
    let height: u32 = rendition
        .parse()
        .map_err(|_| ApiError::not_found("malformed rendition"))?;
    let store = state.store;
    let body = tokio::task::spawn_blocking(move || store.rendition_playlist(&video_id, height))
        .await
        .map_err(panic_as_internal)?
        .map_err(not_found_if_missing)?;
    Ok((
        [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
        body,
    ))
}

/// 多档分片：`/api/video/{id}/720/seg/seg_000.ts`。档位目录与分片名都先校验
/// 再拼路径（`../` 变体一律 404）。
async fn segment_in_rendition(
    State(state): State<VideoState>,
    AxumPath((video_id, rendition, name)): AxumPath<(String, String, String)>,
) -> ApiResult<impl IntoResponse> {
    check_id(&video_id)?;
    check_rendition(&rendition)?;
    check_segment_name(&name)?;
    let body = read_segment_blocking(state.store, video_id, format!("{rendition}/{name}")).await?;
    Ok(([(header::CONTENT_TYPE, "video/mp2t")], body))
}

/// 档位内的**相对**分片名：`/api/video/{id}/720/seg_000.ts`。
///
/// 档位清单（ffmpeg 产物）里写的就是相对名，播放器相对清单 URL 解析出的
/// 就是这条路由（见 `routes()` 里挂载处的说明）。校验与 `segment_in_rendition`
/// 同一套：档位目录 2–4 位数字、分片名走 `check_segment_name`。
async fn segment_named(
    State(state): State<VideoState>,
    AxumPath((video_id, rendition, name)): AxumPath<(String, String, String)>,
) -> ApiResult<impl IntoResponse> {
    check_id(&video_id)?;
    check_rendition(&rendition)?;
    check_segment_name(&name)?;
    let body = read_segment_blocking(state.store, video_id, format!("{rendition}/{name}")).await?;
    Ok(([(header::CONTENT_TYPE, "video/mp2t")], body))
}

/// `play.mp4`：**支持 `Range`**（E1 的第一块欠账）。
///
/// `<video>` 拖进度条靠的就是这条：206 + `Content-Range` + `Accept-Ranges`。
/// 读盘跑在 blocking 池里，而且只读请求那一段，不再整文件进内存。
async fn play_mp4(
    State(state): State<VideoState>,
    AxumPath(video_id): AxumPath<String>,
    headers: axum::http::HeaderMap,
) -> ApiResult<axum::response::Response> {
    check_id(&video_id)?;
    let path = state.store.dir_public(&video_id).join("play.mp4");
    Ok(serve_file(path, "video/mp4", headers).await)
}

/// 缩略图同样走 Range 路径（长图渐进加载、以及断线重传）。
async fn thumb(
    State(state): State<VideoState>,
    AxumPath(video_id): AxumPath<String>,
    headers: axum::http::HeaderMap,
) -> ApiResult<axum::response::Response> {
    check_id(&video_id)?;
    let path = state.store.dir_public(&video_id).join("thumb.png");
    Ok(serve_file(path, "image/png", headers).await)
}

/// 阻塞文件响应统一走这里（206/416/404 的判定在 `range` 模块）。
///
/// 点播与直播的分片/清单共用这一条：两份 Range 实现必然漂移，而漂移的代价是
/// 某一类客户端拖不动进度条。
pub(crate) async fn serve_file(
    path: PathBuf,
    content_type: &'static str,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    tokio::task::spawn_blocking(move || range::respond_with_range(&path, content_type, &headers))
        .await
        .unwrap_or_else(|err| ApiError::internal(format!("file task: {err}")).into_response())
}

/// 选流（Track B 的"按码率选流"）：规则写在 Rust 里，三端才可能选到同一档。
///
/// `?kbps=1200` 是客户端上一段的**实测**可用带宽（千比特/秒）。服务端按
/// [`SELECTION_HEADROOM_PCT`] 留余量后挑最高档；一档都放不下时给最低档。
async fn select(
    State(state): State<VideoState>,
    AxumPath(video_id): AxumPath<String>,
    axum::extract::Query(q): axum::extract::Query<SelectQuery>,
) -> ApiResult<Json<SelectedRendition>> {
    check_id(&video_id)?;
    let kbps = q
        .kbps
        .ok_or_else(|| ApiError::unprocessable("need ?kbps=<measured available kbps>"))?;
    if kbps == 0 {
        return Err(ApiError::unprocessable("kbps must be positive"));
    }
    let meta = read_meta_blocking(state.store, video_id.clone()).await?;
    let available_bps = kbps.saturating_mul(1_000);
    let chosen = select_rendition(&meta.renditions, available_bps)
        .ok_or_else(|| ApiError::not_found("video has no renditions"))?;
    Ok(Json(SelectedRendition {
        video_id,
        available_bps,
        usable_bps: usable_bandwidth(available_bps),
        rendition: chosen.clone(),
        all_heights: meta.renditions.iter().map(|r| r.height).collect(),
    }))
}

/// `GET /api/video/{id}/select` 的查询参数。
#[derive(serde::Deserialize, Debug)]
pub struct SelectQuery {
    /// 实测可用带宽，千比特/秒
    pub kbps: Option<u64>,
}

/// 选流结论：客户端要的档位，外加"为什么是这档"的两个数。
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct SelectedRendition {
    pub video_id: String,
    /// 客户端报来的可用带宽
    pub available_bps: u64,
    /// 留完余量后本轮可用额度
    pub usable_bps: u64,
    pub rendition: Rendition,
    /// 这个对象一共有哪几档（便于客户端理解"为什么给了我 360p"）
    pub all_heights: Vec<u32>,
}

/// Ids are content hashes: anything else is a 404-shaped refusal, decided
/// before any path is joined (same rule as the media store).
pub fn check_id(video_id: &str) -> ApiResult<()> {
    if video_id.len() == wxlike_core::limits::SHA256_HEX_LEN
        && video_id.chars().all(|c| c.is_ascii_hexdigit())
    {
        Ok(())
    } else {
        Err(ApiError::not_found("malformed video id"))
    }
}

/// Segment names come from the playlist ffmpeg wrote: `seg_NNN.ts`（单档老对象）
/// or `720/seg_NNN.ts`（多档，由 `segment_in_rendition` 拼出）。其余一律拒绝，
/// 且在碰文件系统之前拒绝。
pub fn check_segment_name(name: &str) -> ApiResult<()> {
    let parts: Vec<&str> = name.split('/').collect();
    let file_ok = |f: &str| {
        !f.is_empty()
            && f.len() <= 64
            && f.ends_with(".ts")
            && !f.contains("..")
            && f.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    };
    let ok = match parts.as_slice() {
        [file] => file_ok(file),
        [dir, file] => {
            !dir.is_empty()
                && dir.len() <= 4
                && dir.chars().all(|c| c.is_ascii_digit())
                && file_ok(file)
        }
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(ApiError::not_found("malformed segment name"))
    }
}

/// 档位目录：2–4 位数字（360/720/1080）。
pub fn check_rendition(rendition: &str) -> ApiResult<()> {
    let ok = (2..=4).contains(&rendition.len()) && rendition.chars().all(|c| c.is_ascii_digit());
    if ok {
        Ok(())
    } else {
        Err(ApiError::not_found("malformed rendition"))
    }
}

/// Boolean forms of the validators, for tests and callers that only need a
/// gate (the router handlers use the Result forms for their 404 bodies).
pub fn check_id_ok(video_id: &str) -> bool {
    check_id(video_id).is_ok()
}

pub fn check_segment_ok(name: &str) -> bool {
    check_segment_name(name).is_ok()
}

/// 产物缺失 → 404，其余 → 500。
///
/// **不能靠错误文案判断**。原来的实现是
/// `text.contains("No such file") || text.contains("os error 2")`——那是 Unix 的
/// 说法；Windows 上同一条路径报的是「系统找不到指定的路径。(os error 3)」，
/// 于是"视频不存在"在 Windows 上返回 **500**，客户端看到的是"服务端炸了"
/// 而不是"这条还没转完"。这类 bug 只在另一个平台上现形，本地永远绿。
///
/// 改成看 [`std::io::ErrorKind`]：`anyhow` 的 `chain()` 会遍历 context 链，
/// 所以包了 `.context("read meta for {id}")` 也能找到底下那个 `io::Error`。
fn not_found_if_missing(err: anyhow::Error) -> ApiError {
    let missing = err
        .chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(|io| io.kind() == std::io::ErrorKind::NotFound);

    // 完整链只进日志：context 文案可能带磁盘布局信息，404/500 的 body
    // 都给固定一句（审计第 5 轮——此前 404 回显 `{err:#}`）。
    if missing {
        tracing::debug!(error = %format!("{err:#}"), "video artifact missing");
        return ApiError::not_found("no such video artifact");
    }

    tracing::error!(error = %format!("{err:#}"), "video artifact read failed");
    ApiError::internal("video storage failure")
}

/// 读上传的视频本体。字段名收 `bytes`（新客户端）与 `file`（老客户端/与
/// `/api/media` 的 `file` 同名）——与 media 面的收宽不同（那边还收 `data`），
/// 视频面只此两个别名，多收一个就多一个能绕过字段校验的口子。
///
/// 字段内容**流式落盘**，不经过 `Vec<u8>`。
async fn read_field(form: &mut Multipart, root: &Path) -> Result<(UploadTemp, u64), String> {
    while let Some(field) = form
        .next_field()
        .await
        .map_err(|e| format!("multipart: {e}"))?
    {
        if matches!(field.name(), Some("bytes") | Some("file")) {
            return stage_field(field, root).await;
        }
    }
    Err("missing `bytes` field".into())
}

/// 尽力删掉临时目录：失败也不影响主流程的错误。
fn cleanup(dir: &std::path::Path) {
    std::fs::remove_dir_all(dir).ok();
}

#[cfg(all(test, unix))]
mod tool_timeout_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn external_tool_timeout_kills_the_process_tree() {
        let dir = std::env::temp_dir().join(format!(
            "wxlike-tool-timeout-{}-{}",
            std::process::id(),
            wxlike_core::types::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("hang.sh");
        // 子进程 sleep 在同一个进程组；只 kill 直接子进程会留下它。
        std::fs::write(&script, "#!/bin/sh\nsleep 30 &\necho $! > \"$1\"\nwait\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let pidfile = dir.join("child.pid");

        let mut cmd = Command::new(&script);
        cmd.arg(&pidfile);
        let started = Instant::now();
        let err = run_limited(cmd, Duration::from_millis(120), "fake-ffmpeg", false).unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "超时必须及时返回: {err:#}"
        );
        assert!(format!("{err:#}").contains("timed out"), "{err:#}");

        // 等脚本把 sleep 的 pid 写出来，再确认整组都被 SIGKILL。
        for _ in 0..50 {
            if pidfile.is_file() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .expect("pid file")
            .trim()
            .parse()
            .expect("pid");
        std::thread::sleep(Duration::from_millis(100));
        let alive = unsafe { libc::kill(pid, 0) == 0 };
        assert!(!alive, "sleep 子进程仍活着（pid {pid}）");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn temp_dir_budget_rejects_oversize_output() {
        let dir = std::env::temp_dir().join(format!(
            "wxlike-budget-{}-{}",
            std::process::id(),
            wxlike_core::types::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("artifact.bin");
        let file = std::fs::File::create(&f).unwrap();
        file.set_len(2 * 1024 * 1024).unwrap();
        let err = ensure_dir_budget_at(&dir, 1024 * 1024).unwrap_err();
        assert!(format!("{err:#}").contains("budget"), "{err:#}");
        ensure_dir_budget_at(&dir, 4 * 1024 * 1024).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }
}
