//! wxlike-video — video-service 最小版 (contract P2 / ADR-21).
//!
//! 上传 → FFprobe 元数据 → FFmpeg 单分辨率 HLS → 缩略图 → 本地对象存储。
//! 无状态任务式服务：`video_id` 是源字节的 SHA-256（内容寻址，与 media 一致，
//! 同一视频上传两次天然只转码一次）。消息侧只传引用（`msgmeta::VideoPayload`），
//! 视频二进制永不经过消息网关。
//!
//! P6 完整版在此骨架上追加多分辨率 ladder 与任务队列；单分辨率 720p 封顶
//! 已覆盖 P2 验收的"上传→播放闭环"。
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

use anyhow::Context as _;
use axum::extract::multipart::Multipart;
use axum::extract::{DefaultBodyLimit, Path as AxumPath, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

/// Upload cap for the video endpoint: 500 MB, generous for a research VOD
/// service but bounded so a bad client cannot stream forever.
pub const MAX_VIDEO_BYTES: usize = 500 * 1024 * 1024;
/// HLS target: everything above this height is downscaled; smaller sources
/// keep their size (never upscaled).
pub const TARGET_HEIGHT: u32 = 720;
/// Segment target length.
pub const HLS_SECONDS: f64 = 4.0;

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
    /// Transcode parameters actually used (single-res ladder step).
    pub transcode: TranscodeInfo,
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
#[derive(Clone)]
pub struct VideoStore {
    root: PathBuf,
}

impl VideoStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn dir(&self, video_id: &str) -> PathBuf {
        self.root.join(video_id)
    }

    pub fn contains(&self, video_id: &str) -> bool {
        self.dir(video_id).join("meta.json").is_file()
    }

    pub fn meta(&self, video_id: &str) -> anyhow::Result<VideoMeta> {
        let bytes = std::fs::read(self.dir(video_id).join("meta.json"))
            .with_context(|| format!("read meta for {video_id}"))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn playlist(&self, video_id: &str) -> anyhow::Result<Vec<u8>> {
        std::fs::read(self.dir(video_id).join("hls").join("playlist.m3u8"))
            .with_context(|| format!("read playlist for {video_id}"))
    }

    /// One HLS segment. `name` was validated by the caller.
    pub fn segment(&self, video_id: &str, name: &str) -> anyhow::Result<Vec<u8>> {
        std::fs::read(self.dir(video_id).join("hls").join(name))
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
#[derive(Serialize, Clone, Debug)]
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
        let video_id = hex::encode(sha2::Sha256::digest(bytes));
        if self.contains(&video_id) {
            let meta = self.meta(&video_id)?;
            return Ok(Ingested {
                video_id: video_id.clone(),
                playlist_url: playlist_url(&video_id),
                play_url: play_url(&video_id),
                thumb_url: thumb_url(&video_id),
                duration_ms: meta.duration_ms,
                width: meta.width,
                height: meta.height,
            });
        }

        let store = self.clone();
        let task_id = video_id.clone();
        let bytes = bytes.to_vec();
        let meta = tokio::task::spawn_blocking(move || store.transcode(&task_id, &bytes))
            .await
            .context("transcode task panicked")??;

        Ok(Ingested {
            video_id: video_id.clone(),
            playlist_url: playlist_url(&video_id),
            play_url: play_url(&video_id),
            thumb_url: thumb_url(&video_id),
            duration_ms: meta.duration_ms,
            width: meta.width,
            height: meta.height,
        })
    }

    /// The blocking pipeline: probe → transcode → thumb → rename into place.
    fn transcode(&self, video_id: &str, bytes: &[u8]) -> anyhow::Result<VideoMeta> {
        let final_dir = self.dir(video_id);
        let temp_dir = self
            .root
            .join(format!(".tmp-{video_id}-{}", wxlike_core::types::now_ms()));
        std::fs::create_dir_all(temp_dir.join("hls"))
            .with_context(|| "create temp transcode dir".to_string())?;

        let result = self.run_pipeline(video_id, bytes, &temp_dir);
        match result {
            Ok(meta) => {
                std::fs::rename(&temp_dir, &final_dir)
                    .with_context(|| "move transcoded artifacts into place".to_string())?;
                Ok(meta)
            }
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
        let scale_h = src_h.clamp(16, TARGET_HEIGHT);
        // libx264 needs even dimensions; -2 keeps aspect and rounds to even
        let scale = format!("scale=-2:{scale_h}");

        // transcode once to a progressive MP4, then cut HLS from it with
        // `-c copy` (remux only): one encode total, and the MP4 doubles as
        // the native `<video>` playback source for the webview (倍速 needs
        // an element-controlled rate, which only a plain file gives us).
        let mp4 = temp_dir.join("play.mp4");
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
        .context("ffmpeg transcode to mp4")?;
        anyhow::ensure!(mp4.is_file(), "ffmpeg produced no mp4");

        let hls_dir = temp_dir.join("hls");
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
                hls_dir.join("seg_%03d.ts").to_str().context("seg path")?,
                hls_dir
                    .join("playlist.m3u8")
                    .to_str()
                    .context("playlist path")?,
            ],
        )
        .context("ffmpeg remux to hls")?;
        anyhow::ensure!(
            hls_dir.join("playlist.m3u8").is_file(),
            "ffmpeg produced no playlist"
        );

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
            width: probe.width_of(&scale),
            height: scale_h,
            video_codec: probe.video_codec(),
            audio_codec: probe.audio_codec(),
            transcode: TranscodeInfo {
                resolution: format!("-2x{scale_h}"),
                video_codec: "h264".into(),
                audio_codec: "aac".into(),
                created_at_ms: wxlike_core::types::now_ms(),
            },
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

    /// Output width after the `-2:{h}` scale filter (even, aspect kept).
    pub fn width_of(&self, _scale: &str) -> u32 {
        let (w, h) = self.video_size();
        if w == 0 || h == 0 {
            return 0;
        }
        let target_h = h.clamp(16, TARGET_HEIGHT);
        ((u64::from(w) * u64::from(target_h) + u64::from(h) / 2) / u64::from(h)) as u32 & !1
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
    Router::new()
        .route("/api/video", post(upload))
        .route("/api/video/{video_id}", get(meta))
        .route("/api/video/{video_id}/playlist.m3u8", get(playlist))
        .route("/api/video/{video_id}/seg/{name}", get(segment))
        .route("/api/video/{video_id}/play.mp4", get(play_mp4))
        .route("/api/video/{video_id}/thumb.png", get(thumb))
        .layer(DefaultBodyLimit::max(MAX_VIDEO_BYTES))
        .with_state(state)
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

async fn play_mp4(
    State(state): State<VideoState>,
    AxumPath(video_id): AxumPath<String>,
) -> std::result::Result<impl IntoResponse, (StatusCode, String)> {
    check_id(&video_id)?;
    let body = std::fs::read(state.store.dir_public(&video_id).join("play.mp4"))
        .map_err(|e| not_found_if_missing(e.into()))?;
    Ok(([(header::CONTENT_TYPE, "video/mp4")], body))
}

async fn thumb(
    State(state): State<VideoState>,
    AxumPath(video_id): AxumPath<String>,
) -> std::result::Result<impl IntoResponse, (StatusCode, String)> {
    check_id(&video_id)?;
    let body = state.store.thumb(&video_id).map_err(not_found_if_missing)?;
    Ok(([(header::CONTENT_TYPE, "image/png")], body))
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

/// Segment names come from the playlist ffmpeg wrote (`seg_NNN.ts`); refuse
/// everything else before it ever touches the filesystem.
pub fn check_segment_name(name: &str) -> Result<(), (StatusCode, String)> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    if ok {
        Ok(())
    } else {
        Err((StatusCode::NOT_FOUND, "malformed segment name".into()))
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
