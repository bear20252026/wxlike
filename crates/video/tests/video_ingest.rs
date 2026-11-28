//! video-service minimal (P2): always-on unit tests for the pure logic, and
//! `#[ignore]`d integration tests that drive the **real** ffmpeg pipeline —
//! same convention as the Redis backend tests (WP-10): they are not skipped
//! by CI policy, they need the external tool present and run via
//!
//! ```bash
//! cargo test -p wxlike-video -- --ignored     # real transcode (needs ffmpeg)
//! cargo test -p wxlike-video                  # logic, no ffmpeg needed
//! ```

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use sha2::Digest as _;
use std::sync::Arc;
use tower::ServiceExt as _;
use wxlike_video::{routes, VideoAuth, VideoPolicy, VideoState, VideoStore};

/// 控制面（上传）自 issue #7 起要认证：给集成测试配一个"头即身份"的
/// 认证器（与 `video_access.rs` 同一模式），别把 401 当成流水线失败。
struct HeaderUser;

impl VideoAuth for HeaderUser {
    fn caller(&self, headers: &HeaderMap) -> Option<String> {
        headers
            .get("x-test-user")?
            .to_str()
            .ok()
            .map(str::to_string)
            .filter(|u| !u.is_empty())
    }
}

fn state() -> VideoState {
    let dir = std::env::temp_dir().join(format!(
        "wxlike-video-{}-{}",
        std::process::id(),
        wxlike_core::types::now_ms()
    ));
    VideoState {
        store: VideoStore::new(dir)
            .with_policy(VideoPolicy::default().with_auth(Arc::new(HeaderUser))),
    }
}

// ---------------------------------------------------------------------------
// H-21: near-limit concurrent upload stress (streaming multipart body)
// ---------------------------------------------------------------------------

/// 不先把整份文件攒在测试进程里的 multipart body：prefix + `zeros_left`
/// 个 1 MiB 的零块 + suffix。`poll_frame` 每次只产出一块，配合服务端的
/// 有界 channel，测试进程自身的 RSS 不会随文件大小增长。
struct StreamingMultipart {
    prefix: Bytes,
    zeros_left: u64,
    suffix: Bytes,
    phase: u8,
    /// 每产出一块后先 `Pending` 一次，模拟真实 socket 的读侧背压。
    ///
    /// 这不是测试技巧：multer 的 `poll_stream` 会一直 poll 到 `Pending` 才返回，
    /// 一个"永远 Ready"的 body 会在 multer 的缓冲区里攒完整份文件；真实
    /// hyper body 在块之间会 Pending，所以这个测试必须也这样做。
    yield_next: bool,
}

impl StreamingMultipart {
    fn new(file_bytes: u64, boundary: &str) -> Self {
        let prefix = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"bytes\"; filename=\"big.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        );
        let suffix = format!("\r\n--{boundary}--\r\n");
        Self {
            prefix: Bytes::from(prefix),
            zeros_left: file_bytes,
            suffix: Bytes::from(suffix),
            phase: 0,
            yield_next: false,
        }
    }
}

impl http_body::Body for StreamingMultipart {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let this = self.as_mut().get_mut();
        if this.yield_next {
            this.yield_next = false;
            _cx.waker().wake_by_ref();
            return std::task::Poll::Pending;
        }
        loop {
            match this.phase {
                0 => {
                    this.phase = 1;
                    this.yield_next = true;
                    return std::task::Poll::Ready(Some(Ok(http_body::Frame::data(
                        this.prefix.clone(),
                    ))));
                }
                1 => {
                    if this.zeros_left == 0 {
                        this.phase = 2;
                        continue;
                    }
                    let n = this.zeros_left.min(1024 * 1024) as usize;
                    this.zeros_left -= n as u64;
                    this.yield_next = true;
                    return std::task::Poll::Ready(Some(Ok(http_body::Frame::data(Bytes::from(
                        vec![0u8; n],
                    )))));
                }
                2 => {
                    this.phase = 3;
                    this.yield_next = true;
                    return std::task::Poll::Ready(Some(Ok(http_body::Frame::data(
                        this.suffix.clone(),
                    ))));
                }
                _ => return std::task::Poll::Ready(None),
            }
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        let exact = self.prefix.len() as u64 + self.zeros_left + self.suffix.len() as u64;
        http_body::SizeHint::with_exact(exact)
    }
}

#[cfg(target_os = "linux")]
fn rss_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

/// 4 路并发、每路 480 MiB（低于 500 MiB 上限但接近），body 逐块产出。
/// 旧实现会把每路整份 body 读成 `Vec`（再加 `to_vec`/clone），峰值应超过
/// 4×480 MiB；新实现只应看到有界 channel + 1 MiB 块。手动跑：
///
/// ```bash
/// cargo test -p wxlike-video --test video_ingest -- --ignored near_limit
/// ```
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "stress: 4 x 480 MiB, run manually: cargo test -p wxlike-video --test video_ingest -- --ignored near_limit"]
async fn near_limit_concurrent_uploads_do_not_buffer_the_bodies() {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    const FILE_BYTES: u64 = 480 * 1024 * 1024;
    const CONCURRENCY: usize = 4;

    let st = state();
    let baseline = rss_kib();
    let peak = Arc::new(AtomicU64::new(baseline));
    let stop = Arc::new(AtomicBool::new(false));
    let monitor = {
        let peak = Arc::clone(&peak);
        let stop = Arc::clone(&stop);
        tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                peak.fetch_max(rss_kib(), Ordering::Relaxed);
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
    };

    let mut tasks = Vec::new();
    for idx in 0..CONCURRENCY {
        let app = routes(st.clone());
        tasks.push(tokio::spawn(async move {
            let boundary = format!("wxlike-h21-{idx}");
            let body = Body::new(StreamingMultipart::new(FILE_BYTES, &boundary));
            let req = Request::builder()
                .method("POST")
                .uri("/api/video")
                .header("x-test-user", "stress")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(body)
                .unwrap();
            let response = app.oneshot(req).await.unwrap();
            response.status()
        }));
    }
    for task in tasks {
        let status = task.await.unwrap();
        assert!(
            !status.is_success(),
            "garbage 480 MiB upload must fail, got {status}"
        );
    }

    stop.store(true, Ordering::Relaxed);
    monitor.await.unwrap();
    let observed_peak = peak.load(Ordering::Relaxed);
    let growth_mib = observed_peak.saturating_sub(baseline) / 1024;
    eprintln!(
        "near-limit stress: concurrency={CONCURRENCY} file={FILE_BYTES} bytes baseline={baseline} KiB peak={observed_peak} KiB growth={growth_mib} MiB"
    );
    assert!(
        growth_mib < 384,
        "four 480 MiB uploads must not be buffered: RSS grew {growth_mib} MiB (baseline {baseline} KiB, peak {observed_peak} KiB)"
    );

    // 四路失败后不能留下半个 body
    let leftovers: Vec<_> = std::fs::read_dir(st.store.root())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with(".upload-") || n.starts_with(".ingest-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "stress upload leaked staging: {leftovers:?}"
    );
}

// ---------------------------------------------------------------------------
// pure logic (always run)
// ---------------------------------------------------------------------------

#[test]
fn malformed_ids_and_segments_never_reach_the_filesystem() {
    // the checks are pure functions; assert the contract directly
    let good_id = "a".repeat(64);
    assert!(wxlike_video::check_id_ok(&good_id));
    assert!(!wxlike_video::check_id_ok("../etc/passwd"));
    assert!(!wxlike_video::check_id_ok(&"g".repeat(64))); // not hex
    assert!(!wxlike_video::check_id_ok(&"a".repeat(63))); // one short

    assert!(wxlike_video::check_segment_ok("seg_001.ts"));
    assert!(!wxlike_video::check_segment_ok("..%2fpasswd"));
    assert!(!wxlike_video::check_segment_ok("../hls/playlist.m3u8"));
    assert!(!wxlike_video::check_segment_ok(""));
}

#[test]
fn probe_json_parses_into_meta_fields() {
    // a trimmed-down ffprobe output: the parser only touches the fields we use
    let json = serde_json::json!({
        "format": { "duration": "3.125000" },
        "streams": [
            { "codec_type": "video", "codec_name": "h264", "width": 640, "height": 360 },
            { "codec_type": "audio", "codec_name": "aac" }
        ]
    });
    let probe: wxlike_video::Probe = serde_json::from_value(json).unwrap();
    assert_eq!(probe.duration_ms().unwrap(), 3125);
    assert_eq!(probe.video_size(), (640, 360));
    assert_eq!(probe.video_codec(), "h264");
    assert_eq!(probe.audio_codec(), "aac");
    // 720 cap: a 360p source keeps 360; width recomputes even
    assert_eq!(probe.width_of("scale=-2:360"), 640);
}

#[test]
fn probe_missing_duration_is_an_error_not_a_zero() {
    let json = serde_json::json!({
        "format": {},
        "streams": []
    });
    let probe: wxlike_video::Probe = serde_json::from_value(json).unwrap();
    assert!(
        probe.duration_ms().is_err(),
        "no duration -> ingest must fail"
    );
}

#[test]
fn urls_are_relative_api_paths() {
    let id = "a".repeat(64);
    assert_eq!(
        wxlike_video::playlist_url(&id),
        format!("/api/video/{id}/playlist.m3u8")
    );
    assert_eq!(
        wxlike_video::thumb_url(&id),
        format!("/api/video/{id}/thumb.png")
    );
}

// ---------------------------------------------------------------------------
// real ffmpeg pipeline (ignored; run with -- --ignored)
// ---------------------------------------------------------------------------

/// Synthesizes a 3s test clip with ffmpeg itself (no fixtures in the repo).
fn make_sample_video(dir: &std::path::Path) -> std::path::PathBuf {
    make_sample_video_sized(dir, 640, 360, "sample.mp4")
}

/// 指定分辨率的样片（多档阶梯用例用）。
fn make_sample_video_sized(
    dir: &std::path::Path,
    width: u32,
    height: u32,
    name: &str,
) -> std::path::PathBuf {
    let out = dir.join(name);
    let status = std::process::Command::new(wxlike_video::ffmpeg_bin())
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=duration=3:size={width}x{height}:rate=24"),
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=3",
            "-shortest",
            out.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("spawn ffmpeg to synthesize the sample");
    assert!(status.success(), "sample synthesis failed");
    out
}

fn require_tools() {
    assert!(
        wxlike_video::tools_available(),
        "ffmpeg/ffprobe not found — install ffmpeg or set WXLIKE_FFMPEG/WXLIKE_FFPROBE"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs ffmpeg on PATH (or WXLIKE_FFMPEG): cargo test -p wxlike-video -- --ignored"]
async fn ingest_produces_playlist_segments_and_thumb() {
    require_tools();
    let st = state();
    let dir = std::env::temp_dir().join(format!("wxlike-video-src-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sample = std::fs::read(make_sample_video(&dir)).unwrap();

    let first = st.store.ingest(&sample).await.unwrap();
    assert_eq!(first.video_id.len(), 64);
    assert_eq!(first.duration_ms, 3000, "ffprobe duration");
    assert_eq!(first.height, 360, "360p source is never upscaled");
    assert_eq!(first.width, 640);

    // master playlist 声明档位；每个档位的 playlist 再引用自己的分片
    let master = String::from_utf8(st.store.playlist(&first.video_id).unwrap()).unwrap();
    assert!(master.contains("#EXTM3U"));
    assert_eq!(first.renditions.len(), 1, "360p 源只产出一档");
    assert_eq!(first.renditions[0].height, 360);
    assert!(
        master.contains("EXT-X-STREAM-INF") && master.contains("360/playlist.m3u8"),
        "master 必须声明档位: {master}"
    );
    assert!(
        master.contains("RESOLUTION=640x360"),
        "分辨率由 -2 缩放算出: {master}"
    );

    let rendition = String::from_utf8(st.store.rendition_playlist(&first.video_id, 360).unwrap())
        .expect("utf8 rendition playlist");
    assert!(
        rendition.contains("seg_"),
        "档位播放列表引用分片: {rendition}"
    );
    let segs: Vec<_> = rendition
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .collect();
    assert!(!segs.is_empty());
    for seg in &segs {
        let name = format!("360/{}", seg.trim_start_matches("./"));
        let bytes = st.store.segment(&first.video_id, &name).unwrap();
        assert!(!bytes.is_empty());
    }

    let thumb = st.store.thumb(&first.video_id).unwrap();
    assert_eq!(&thumb[..4], b"\x89PNG", "thumbnail is a real PNG");

    // meta.json agrees with the ingest result
    let meta = st.store.meta(&first.video_id).unwrap();
    assert_eq!(meta.duration_ms, 3000);
    assert_eq!(meta.video_codec, "h264");
    assert_eq!(meta.audio_codec, "aac");

    // content-addressed idempotence: the same bytes ingest instantly and
    // return the same id (no second transcode)
    let again = st.store.ingest(&sample).await.unwrap();
    assert_eq!(again.video_id, first.video_id);

    // 流式落盘的临时文件在 ingest 返回后必须消失（成功和失败路径都不留半个 body）
    let leftovers: Vec<_> = std::fs::read_dir(st.store.root())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with(".upload-") || n.starts_with(".ingest-"))
        .collect();
    assert!(leftovers.is_empty(), "upload temp leaked: {leftovers:?}");

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs ffmpeg: cargo test -p wxlike-video -- --ignored"]
async fn http_surface_serves_the_full_playback_loop() {
    require_tools();
    let st = state();
    let app = routes(st.clone());

    let dir = std::env::temp_dir().join(format!("wxlike-video-src2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sample = std::fs::read(make_sample_video(&dir)).unwrap();

    // POST /api/video with multipart `bytes`
    let boundary = "wxlike-video-test";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"bytes\"; filename=\"s.mp4\"\r\nContent-Type: video/mp4\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&sample);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/video")
                .header("x-test-user", "uploader")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes();
    let ingested: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let video_id = ingested["video_id"].as_str().unwrap().to_string();
    assert_eq!(
        ingested["playlist_url"],
        serde_json::json!(wxlike_video::playlist_url(&video_id))
    );

    // GET playlist → parse a segment name → GET it
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/video/{video_id}/playlist.m3u8"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    // master 的第一条非注释行是档位播放列表（如 `360/playlist.m3u8`）
    let rendition = text
        .lines()
        .find(|l| !l.starts_with('#') && !l.is_empty())
        .expect("master declares a rendition")
        .trim_start_matches("./")
        .to_string();
    let height = rendition
        .split_once('/')
        .map(|(h, _)| h.to_string())
        .expect("rendition path carries its height");

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/video/{video_id}/{rendition}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "rendition playlist");
    let bytes = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes();
    let rtext = String::from_utf8(bytes.to_vec()).unwrap();
    let seg = rtext
        .lines()
        .find(|l| !l.starts_with('#') && !l.is_empty())
        .expect("rendition playlist references a segment")
        .trim_start_matches("./");

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/video/{video_id}/{height}/seg/{seg}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "rendition segment");

    // 审计第 5 轮：相对分片名按播放器的方式解析——档位清单里写的是
    // `seg_000.ts`（相对名），相对清单 URL 拼出的就是
    // `/api/video/{id}/{height}/seg_000.ts`（没有 `/seg/` 段）。
    let relative_uri = format!("/api/video/{video_id}/{height}/{seg}");
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&relative_uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "relative segment URI (as a player resolves it): {relative_uri}"
    );

    // thumbnail endpoint
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/video/{video_id}/thumb.png"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // traversal and junk ids are 404 before any file read
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/video/..%2fetc/playlist.m3u8")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/video/{video_id}/seg/..%2fmeta.json"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs ffmpeg: cargo test -p wxlike-video -- --ignored"]
async fn corrupt_upload_fails_cleanly_and_leaves_no_dir() {
    require_tools();
    let st = state();
    let err = st.store.ingest(b"this is not a video").await;
    assert!(err.is_err(), "garbage bytes must fail the pipeline");
    // the content-addressed dir for that hash must not exist
    let id = hex::encode(sha2::Sha256::digest(b"this is not a video"));
    assert!(!st.store.contains(&id), "failed ingest leaves no object");
    assert!(
        !st.store.root().join(format!(".tmp-{id}")).exists()
            || std::fs::read_dir(st.store.root().join(format!(".tmp-{id}"))).is_err(),
        "no temp dir remains"
    );
    let leftovers: Vec<_> = std::fs::read_dir(st.store.root())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with(".upload-") || n.starts_with(".ingest-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "failed ingest leaked staging: {leftovers:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs ffmpeg on PATH (or WXLIKE_FFMPEG): cargo test -p wxlike-video -- --ignored"]
async fn ladder_produces_multiple_renditions() {
    require_tools();
    let st = state();
    let dir = std::env::temp_dir().join(format!("wxlike-video-hd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // 720p 源 → 阶梯只应产出 360 与 720（绝不上采样到 1080）
    let sample = std::fs::read(make_sample_video_sized(&dir, 1280, 720, "hd.mp4")).unwrap();

    let ing = st.store.ingest(&sample).await.unwrap();
    let heights: Vec<u32> = ing.renditions.iter().map(|r| r.height).collect();
    assert_eq!(heights, vec![360, 720], "阶梯按升序且不上采样");
    assert_eq!(ing.height, 720, "play.mp4 取最高档");
    assert_eq!(ing.width, 1280);
    assert!(
        ing.renditions[0].bandwidth_bps > 0 && ing.renditions[1].bandwidth_bps > 0,
        "实测码率必须为正"
    );
    assert!(
        ing.renditions[1].bandwidth_bps > ing.renditions[0].bandwidth_bps,
        "720p 码率应高于 360p"
    );

    let master = String::from_utf8(st.store.playlist(&ing.video_id).unwrap()).unwrap();
    assert!(master.contains("360/playlist.m3u8"), "{master}");
    assert!(master.contains("720/playlist.m3u8"), "{master}");
    assert!(master.contains("RESOLUTION=1280x720"), "{master}");

    for r in &ing.renditions {
        let list = String::from_utf8(
            st.store
                .rendition_playlist(&ing.video_id, r.height)
                .unwrap(),
        )
        .unwrap();
        let first_seg = list
            .lines()
            .find(|l| !l.starts_with('#') && !l.is_empty())
            .expect("rendition references a segment")
            .trim_start_matches("./");
        let bytes = st
            .store
            .segment(&ing.video_id, &format!("{}/{first_seg}", r.height))
            .unwrap();
        assert!(!bytes.is_empty(), "{}p 分片可读", r.height);
    }

    // 最高档的渐进播放文件存在（webview <video> 用它）
    assert!(st
        .store
        .dir_public(&ing.video_id)
        .join("play.mp4")
        .is_file());
}
