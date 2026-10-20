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

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::Context as _;
use axum::extract::multipart::Multipart;
use axum::extract::{DefaultBodyLimit, Path as AxumPath, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

mod db;
mod range;
mod stats;
mod timing;

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
        let Ok(out) = std::process::Command::new(&bin).arg("-version").output() else {
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
/// Zone E adds a sibling: `{root}/video.db`（播放统计，见 `db` 模块）。对象目录
/// 本身仍然只在转码完整成功后出现，所以老代码的"目录存在=可播"这条不变式没被改动。
#[derive(Clone)]
pub struct VideoStore {
    inner: Arc<Inner>,
}

struct Inner {
    root: PathBuf,
    /// 懒建：只有第一个统计/作业请求才会打开 `video.db`。
    /// 纯 VOD 部署（不碰 E1/E3 的接口）因此依旧一个文件都不多建。
    db: Mutex<Option<Arc<db::VideoDb>>>,
}

impl VideoStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let inner = Inner {
            root: root.into(),
            db: Mutex::new(None),
        };
        Self {
            inner: Arc::new(inner),
        }
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
    /// Content-addressed ingest. Re-uploading identical bytes is a no-op
    /// that returns the existing object (idempotent, like the media store).
    ///
    /// External tools run to completion inside a blocking task context; the
    /// transcode happens in a sibling temp dir and is renamed into place
    /// only when every artifact exists.
    pub async fn ingest(&self, bytes: &[u8]) -> anyhow::Result<Ingested> {
        let video_id = self.content_id(bytes);
        if self.contains(&video_id) {
            return self.ingested_from_meta(&video_id);
        }

        let store = self.clone();
        let task_id = video_id.clone();
        let bytes = bytes.to_vec();
        let meta = tokio::task::spawn_blocking(move || store.transcode(&task_id, &bytes))
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
    fn transcode(&self, video_id: &str, bytes: &[u8]) -> anyhow::Result<VideoMeta> {
        let final_dir = self.dir(video_id);
        let temp_dir = self
            .root()
            .join(format!(".tmp-{video_id}-{}", wxlike_core::types::now_ms()));
        std::fs::create_dir_all(temp_dir.join("hls"))
            .with_context(|| "create temp transcode dir".to_string())?;

        let result = self.run_pipeline(video_id, bytes, &temp_dir);
        match result {
            Ok(meta) => match std::fs::rename(&temp_dir, &final_dir) {
                Ok(()) => Ok(meta),
                Err(err) => {
                    // 同内容并发转码（同步上传撞上班里的同一个作业）时，赢家的
                    // 目录已经在那儿了。此时"对象存在"是真话，不是降级——
                    // 清掉自己的临时目录，返回同一份结果。
                    if self.contains(video_id) {
                        std::fs::remove_dir_all(&temp_dir).ok();
                        tracing::debug!(video_id, error = %err, "并发转码：采用已存在的对象");
                        return Ok(meta);
                    }
                    std::fs::remove_dir_all(&temp_dir).ok();
                    Err(err).with_context(|| "move transcoded artifacts into place".to_string())
                }
            },
            Err(err) => {
                std::fs::remove_dir_all(&temp_dir).ok();
                Err(err)
            }
        }
    }

    fn run_pipeline(
        &self,
        video_id: &str,
        bytes: &[u8],
        temp_dir: &Path,
    ) -> anyhow::Result<VideoMeta> {
        let source = temp_dir.join("source");
        std::fs::write(&source, bytes).with_context(|| "stage source bytes".to_string())?;

        let probe = ffprobe(&source)?;
        let duration_ms = probe.duration_ms()?;
        let (_src_w, src_h) = probe.video_size();
        let heights = rendition_heights(src_h);

        let hls_root = temp_dir.join("hls");
        std::fs::create_dir_all(&hls_root)?;

        let mut renditions: Vec<Rendition> = Vec::new();
        // (height, mp4 path, width) —— 升序，最后一个即最高档
        let mut best: Option<(u32, PathBuf, u32)> = None;

        for height in &heights {
            let height = *height;
            let width = probe.width_at(height);
            // libx264 要求偶数宽高；`-2` 保持比例并取偶
            let scale = format!("scale=-2:{height}");
            let mp4 = temp_dir.join(format!("play_{height}.mp4"));

            // 一次编码/档：先出 progressive MP4，再从它 `-c copy` 切 HLS
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

            // 实测码率：比 CRF 猜一个常数靠谱，master 的 BANDWIDTH 直接可用
            let size = std::fs::metadata(&mp4)?.len();
            let bandwidth_bps = if duration_ms > 0 {
                size.saturating_mul(8).saturating_mul(1_000) / u64::from(duration_ms)
            } else {
                0
            };
            renditions.push(Rendition {
                height,
                width,
                bandwidth_bps,
                playlist: format!("{height}/playlist.m3u8"),
            });
            best = Some((height, mp4, width));
        }

        let (best_h, best_mp4, best_w) = best.context("no rendition was produced")?;
        // 渐进播放源 = 最高档（webview 的 <video> 与倍速靠它）
        std::fs::copy(&best_mp4, temp_dir.join("play.mp4"))
            .with_context(|| "copy best rendition to play.mp4".to_string())?;

        // master playlist：按升序声明各档，播放器自行选档
        let master = master_playlist(&renditions);
        std::fs::write(hls_root.join("master.m3u8"), master.as_bytes())
            .with_context(|| "write master playlist".to_string())?;
        // 固定路径兼容：老客户端只认 `hls/playlist.m3u8`
        std::fs::write(hls_root.join("playlist.m3u8"), master.as_bytes())
            .with_context(|| "write legacy playlist path".to_string())?;

        // thumbnail: first frame at 1s (or 0 for shorter clips)
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

        let meta = VideoMeta {
            video_id: video_id.to_string(),
            source_size: bytes.len() as u64,
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
    let out = std::process::Command::new(ffprobe_bin())
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(source)
        .stderr(Stdio::null())
        .output()
        .with_context(|| "spawn ffprobe (is ffmpeg installed? set WXLIKE_FFPROBE)".to_string())?;
    anyhow::ensure!(out.status.success(), "ffprobe failed on the upload");
    serde_json::from_slice(&out.stdout).context("parse ffprobe json")
}

fn run_tool(bin: &str, args: &[&str]) -> anyhow::Result<()> {
    let out = std::process::Command::new(bin)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .with_context(|| format!("spawn {bin}"))?;
    anyhow::ensure!(out.status.success(), "{bin} exited non-zero");
    Ok(())
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
    let vod = Router::new()
        .route("/api/video", post(upload))
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
        .route("/api/video/{video_id}/play.mp4", get(play_mp4))
        .route("/api/video/{video_id}/thumb.png", get(thumb))
        // Zone E 新增的面（各自带 handler，路径都在自己模块里）：
        //   stats → 播放时长统计（E1）
        .route("/api/video/{video_id}/select", get(select))
        .with_state(state.clone());
    vod.merge(stats::routes(state))
        .layer(DefaultBodyLimit::max(MAX_VIDEO_BYTES))
}

async fn upload(
    State(state): State<VideoState>,
    mut form: Multipart,
) -> std::result::Result<Json<Ingested>, (StatusCode, String)> {
    let bytes = read_field(&mut form)
        .await
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e))?;
    if bytes.is_empty() {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, "empty upload".into()));
    }
    let ingested = state
        .store
        .ingest(&bytes)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("ingest: {e:#}")))?;
    Ok(Json(ingested))
}

async fn meta(
    State(state): State<VideoState>,
    AxumPath(video_id): AxumPath<String>,
) -> std::result::Result<Json<VideoMeta>, (StatusCode, String)> {
    check_id(&video_id)?;
    state
        .store
        .meta(&video_id)
        .map(Json)
        .map_err(not_found_if_missing)
}

async fn playlist(
    State(state): State<VideoState>,
    AxumPath(video_id): AxumPath<String>,
) -> std::result::Result<impl IntoResponse, (StatusCode, String)> {
    check_id(&video_id)?;
    let body = state
        .store
        .playlist(&video_id)
        .map_err(not_found_if_missing)?;
    Ok((
        [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
        body,
    ))
}

async fn segment(
    State(state): State<VideoState>,
    AxumPath((video_id, name)): AxumPath<(String, String)>,
) -> std::result::Result<impl IntoResponse, (StatusCode, String)> {
    check_id(&video_id)?;
    check_segment_name(&name)?;
    let body = state
        .store
        .segment(&video_id, &name)
        .map_err(not_found_if_missing)?;
    Ok(([(header::CONTENT_TYPE, "video/mp2t")], body))
}

/// 档位播放列表：`/api/video/{id}/720/playlist.m3u8`（master 里引用的就是它）。
async fn rendition_playlist(
    State(state): State<VideoState>,
    AxumPath((video_id, rendition)): AxumPath<(String, String)>,
) -> std::result::Result<impl IntoResponse, (StatusCode, String)> {
    check_id(&video_id)?;
    check_rendition(&rendition)?;
    let height: u32 = rendition
        .parse()
        .map_err(|_| (StatusCode::NOT_FOUND, "malformed rendition".to_string()))?;
    let body = state
        .store
        .rendition_playlist(&video_id, height)
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
) -> std::result::Result<impl IntoResponse, (StatusCode, String)> {
    check_id(&video_id)?;
    check_rendition(&rendition)?;
    check_segment_name(&name)?;
    let body = state
        .store
        .segment(&video_id, &format!("{rendition}/{name}"))
        .map_err(not_found_if_missing)?;
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
) -> std::result::Result<axum::response::Response, (StatusCode, String)> {
    check_id(&video_id)?;
    let path = state.store.dir_public(&video_id).join("play.mp4");
    Ok(serve_file(path, "video/mp4", headers).await)
}

/// 缩略图同样走 Range 路径（长图渐进加载、以及断线重传）。
async fn thumb(
    State(state): State<VideoState>,
    AxumPath(video_id): AxumPath<String>,
    headers: axum::http::HeaderMap,
) -> std::result::Result<axum::response::Response, (StatusCode, String)> {
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
        .unwrap_or_else(|err| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("file task: {err}"),
            )
                .into_response()
        })
}

/// 选流（Track B 的"按码率选流"）：规则写在 Rust 里，三端才可能选到同一档。
///
/// `?kbps=1200` 是客户端上一段的**实测**可用带宽（千比特/秒）。服务端按
/// [`SELECTION_HEADROOM_PCT`] 留余量后挑最高档；一档都放不下时给最低档。
async fn select(
    State(state): State<VideoState>,
    AxumPath(video_id): AxumPath<String>,
    axum::extract::Query(q): axum::extract::Query<SelectQuery>,
) -> std::result::Result<Json<SelectedRendition>, (StatusCode, String)> {
    check_id(&video_id)?;
    let kbps = q.kbps.ok_or((
        StatusCode::UNPROCESSABLE_ENTITY,
        "need ?kbps=<measured available kbps>".to_string(),
    ))?;
    if kbps == 0 {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "kbps must be positive".to_string(),
        ));
    }
    let meta = state.store.meta(&video_id).map_err(not_found_if_missing)?;
    let available_bps = kbps.saturating_mul(1_000);
    let chosen = select_rendition(&meta.renditions, available_bps)
        .ok_or((StatusCode::NOT_FOUND, "video has no renditions".to_string()))?;
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
pub fn check_id(video_id: &str) -> Result<(), (StatusCode, String)> {
    if video_id.len() == 64 && video_id.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err((StatusCode::NOT_FOUND, "malformed video id".into()))
    }
}

/// Segment names come from the playlist ffmpeg wrote: `seg_NNN.ts`（单档老对象）
/// or `720/seg_NNN.ts`（多档，由 `segment_in_rendition` 拼出）。其余一律拒绝，
/// 且在碰文件系统之前拒绝。
pub fn check_segment_name(name: &str) -> Result<(), (StatusCode, String)> {
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
        Err((StatusCode::NOT_FOUND, "malformed segment name".into()))
    }
}

/// 档位目录：2–4 位数字（360/720/1080）。
pub fn check_rendition(rendition: &str) -> Result<(), (StatusCode, String)> {
    let ok = (2..=4).contains(&rendition.len()) && rendition.chars().all(|c| c.is_ascii_digit());
    if ok {
        Ok(())
    } else {
        Err((StatusCode::NOT_FOUND, "malformed rendition".into()))
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

fn not_found_if_missing(err: anyhow::Error) -> (StatusCode, String) {
    // missing artifacts surface as 404; everything else is a 500
    let text = format!("{err:#}");
    if text.contains("No such file") || text.contains("os error 2") {
        (StatusCode::NOT_FOUND, text)
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, text)
    }
}

async fn read_field(form: &mut Multipart) -> Result<Vec<u8>, String> {
    while let Some(field) = form
        .next_field()
        .await
        .map_err(|e| format!("multipart: {e}"))?
    {
        if field.name() == Some("bytes") {
            return field
                .bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| format!("read bytes: {e}"));
        }
    }
    Err("missing `bytes` field".into())
}
