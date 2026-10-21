//! E1 之一：`Range` 请求支持。
//!
//! 这些用例**不需要 ffmpeg**：直接在手搭的对象目录上放一个已知内容的
//! `play.mp4`，就能验收 206 / `Content-Range` / 416 这一整条链路。
//! 拖进度条这件事本来就跟转码无关，所以它也不该需要转码才能测。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use tower::ServiceExt as _;
use wxlike_video::{parse_range, routes, serve_range, ByteRange, Served, VideoState, VideoStore};

/// 4096 字节的已知内容：`b[i] == (i % 251) as u8`，断言切片对不对不用猜。
const TOTAL: u64 = 4096;

fn known_bytes() -> Vec<u8> {
    (0..TOTAL).map(|i| (i % 251) as u8).collect()
}

/// 手搭一个"已经转码完成"的对象：`meta.json` + `play.mp4` + `thumb.png`。
///
/// 目录名必须**每个用例都不同**：只靠 `pid + 毫秒` 时，同一毫秒启动的两个并行用例
/// 会共用一个目录，`std::fs::write` 先截断后写入的窗口里另一个用例正好在读，
/// 就会看到 0 字节的文件（实测在 Linux 上表现为 `thumb.png` 返回 200 而不是 206）。
/// 加一个进程内自增序号，这条竞态就没有了。
fn fake_object() -> (VideoState, String) {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "wxlike-video-range-{}-{}-{}",
        std::process::id(),
        wxlike_core::types::now_ms(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let store = VideoStore::new(dir);
    let id = "b".repeat(64);
    let obj = store.dir_public(&id);
    std::fs::create_dir_all(obj.join("hls")).unwrap();
    let meta = wxlike_video::VideoMeta {
        video_id: id.clone(),
        source_size: TOTAL,
        duration_ms: 3_000,
        width: 640,
        height: 360,
        video_codec: "h264".into(),
        audio_codec: "aac".into(),
        transcode: wxlike_video::TranscodeInfo {
            resolution: "-2x360".into(),
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            created_at_ms: 1,
        },
        renditions: vec![],
    };
    std::fs::write(
        obj.join("meta.json"),
        serde_json::to_vec_pretty(&meta).unwrap(),
    )
    .unwrap();
    std::fs::write(obj.join("play.mp4"), known_bytes()).unwrap();
    std::fs::write(obj.join("thumb.png"), b"\x89PNGfake").unwrap();
    (VideoState { store }, id)
}

async fn get(
    app: &axum::Router,
    uri: &str,
    range: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().uri(uri);
    if let Some(value) = range {
        builder = builder.header(header::RANGE, value);
    }
    let res = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

#[test]
fn parse_range_covers_the_three_forms() {
    // a-b
    assert_eq!(
        parse_range("bytes=0-1023", TOTAL),
        Some(ByteRange {
            start: 0,
            end: 1023
        })
    );
    // a-（到末尾）
    assert_eq!(
        parse_range("bytes=2048-", TOTAL),
        Some(ByteRange {
            start: 2048,
            end: TOTAL - 1
        })
    );
    // -N（最后 N 个字节）
    assert_eq!(
        parse_range("bytes=-512", TOTAL),
        Some(ByteRange {
            start: TOTAL - 512,
            end: TOTAL - 1
        })
    );
    // 末端越界夹到最后一个字节（RFC 7233：end 超总长要夹，不是报错）
    assert_eq!(
        parse_range("bytes=10-99999", TOTAL),
        Some(ByteRange {
            start: 10,
            end: TOTAL - 1
        })
    );
}

#[test]
fn parse_range_refuses_junk_and_unsatisfiable_forms() {
    // 起点已经在文件之外
    assert_eq!(parse_range("bytes=4096-", TOTAL), None);
    // end < start
    assert_eq!(parse_range("bytes=2000-1000", TOTAL), None);
    // 不是 bytes= 单元
    assert_eq!(parse_range("items=0-10", TOTAL), None);
    // 语法垃圾
    assert_eq!(parse_range("bytes=abc", TOTAL), None);
    assert_eq!(parse_range("bytes=", TOTAL), None);
    // 0 长度的后缀请求没有意义
    assert_eq!(parse_range("bytes=-0", TOTAL), None);
    // 多区间不实现：与 platform 的媒体下载同一个取舍
    assert_eq!(parse_range("bytes=0-99,200-299", TOTAL), None);
    // 空文件没有任何可满足的区间
    assert_eq!(parse_range("bytes=0-1", 0), None);
}

#[test]
fn serve_range_chooses_the_status_code() {
    assert_eq!(
        serve_range(None, TOTAL),
        Served::Whole { total: TOTAL },
        "没有 Range 头就是整体 200"
    );
    assert_eq!(
        serve_range(Some("bytes=0-4095"), TOTAL),
        Served::Whole { total: TOTAL },
        "覆盖全文件的请求没必要 206"
    );
    assert_eq!(
        serve_range(Some("bytes=100-199"), TOTAL),
        Served::Partial {
            range: ByteRange {
                start: 100,
                end: 199
            },
            total: TOTAL
        }
    );
    assert_eq!(
        serve_range(Some("bytes=9999-"), TOTAL),
        Served::Unsatisfiable { total: TOTAL },
        "带 Range 头但不可满足 → 416"
    );
    assert_eq!(
        serve_range(Some("nonsense"), TOTAL),
        Served::Unsatisfiable { total: TOTAL },
        "带 Range 头但看不懂 → 416"
    );
    assert_eq!(
        serve_range(Some("bytes=0-10"), 0),
        Served::Whole { total: 0 },
        "空文件永远整体：0 字节的 206 会让播放器以为拿到了某段"
    );
}

#[tokio::test]
async fn play_mp4_answers_range_requests() {
    let (state, id) = fake_object();
    let app = routes(state);
    let uri = format!("/api/video/{id}/play.mp4");

    // 无 Range：200 + Accept-Ranges + 全长
    let (status, headers, body) = get(&app, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::ACCEPT_RANGES].to_str().unwrap(), "bytes");
    assert_eq!(headers[header::CONTENT_TYPE].to_str().unwrap(), "video/mp4");
    assert_eq!(body.len() as u64, TOTAL);
    assert!(!headers.contains_key(header::CONTENT_RANGE));

    // 拖进度条：206 + 正确的 Content-Range 和**只包含那一段**的 body
    let (status, headers, body) = get(&app, &uri, Some("bytes=1024-2047")).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        headers[header::CONTENT_RANGE].to_str().unwrap(),
        format!("bytes 1024-2047/{TOTAL}")
    );
    assert_eq!(body.len(), 1024);
    let full = known_bytes();
    assert_eq!(body, &full[1024..2048], "切片内容必须是请求那一段");

    // 从中间到末尾
    let (status, headers, body) = get(&app, &uri, Some("bytes=4000-")).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        headers[header::CONTENT_RANGE].to_str().unwrap(),
        format!("bytes 4000-{}/{TOTAL}", TOTAL - 1)
    );
    assert_eq!(body, &full[4000..]);

    // 最后 16 个字节
    let (status, headers, body) = get(&app, &uri, Some("bytes=-16")).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        headers[header::CONTENT_RANGE].to_str().unwrap(),
        format!("bytes {}-{}/{TOTAL}", TOTAL - 16, TOTAL - 1)
    );
    assert_eq!(body.len(), 16);

    // 覆盖全文件 → 200（不是 206）
    let (status, _, body) = get(&app, &uri, Some("bytes=0-999999")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.len() as u64, TOTAL);
}

#[tokio::test]
async fn unsatisfiable_range_is_416_with_open_range() {
    let (state, id) = fake_object();
    let app = routes(state);
    let uri = format!("/api/video/{id}/play.mp4");

    for value in ["bytes=9999-", "bytes=garbage", "items=0-10"] {
        let (status, headers, body) = get(&app, &uri, Some(value)).await;
        assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE, "{value}");
        assert_eq!(
            headers[header::CONTENT_RANGE].to_str().unwrap(),
            format!("bytes */{TOTAL}"),
            "{value}: 416 要告诉客户端总长"
        );
        assert!(body.is_empty(), "{value}: 416 不该带 body");
    }
}

#[tokio::test]
async fn thumb_supports_range_and_missing_object_is_404() {
    let (state, id) = fake_object();
    let app = routes(state);

    let (status, headers, body) = get(
        &app,
        &format!("/api/video/{id}/thumb.png"),
        Some("bytes=0-3"),
    )
    .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, b"\x89PNG");
    // 手搭的 thumb 是 8 字节（\x89PNGfake）
    assert_eq!(
        headers[header::CONTENT_RANGE].to_str().unwrap(),
        "bytes 0-3/8"
    );

    // 没有这个对象：404，不是 500、也不是空 200
    let other = "c".repeat(64);
    let (status, _, _) = get(&app, &format!("/api/video/{other}/play.mp4"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 路径穿越在碰到文件系统之前就被拒
    let (status, _, _) = get(&app, "/api/video/..%2f..%2fetc/play.mp4", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[test]
fn read_range_is_exact_and_reports_short_reads() {
    let dir = std::env::temp_dir().join(format!(
        "wxlike-video-readrange-{}-{}",
        std::process::id(),
        wxlike_core::types::now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("known.bin");
    std::fs::write(&path, known_bytes()).unwrap();

    let slice = wxlike_video::read_range(&path, ByteRange { start: 7, end: 16 }).unwrap();
    assert_eq!(slice, known_bytes()[7..17]);

    // 区间越过文件末尾：不是静默少给，是明确失败（清单/播放器要靠这个数对齐）
    let err = wxlike_video::read_range(
        &path,
        ByteRange {
            start: 10,
            end: TOTAL + 500,
        },
    );
    assert!(err.is_err(), "short read must be an error");
    assert!(
        err.unwrap_err().to_string().contains("short read"),
        "error should say what happened"
    );

    // 不存在的文件也是错误，不是空 vec
    assert!(
        wxlike_video::read_range(&dir.join("nope.bin"), ByteRange { start: 0, end: 3 }).is_err()
    );
    std::fs::remove_dir_all(&dir).ok();
}
