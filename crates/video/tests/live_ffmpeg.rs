//! E2 端到端：真 ffmpeg 切的 MPEG-TS 分片 → 推给打包器 → 拉出来的 HLS 能播。
//!
//! 需要 ffmpeg：`cargo test -p wxlike-video --test live_ffmpeg -- --ignored`。
//! 这里**不含 RTMP**：终止 RTMP 是外部接入节点的事（ADR-21：不在进程内跑媒体
//! 服务器），本用例模拟它的产物——一串 `.ts` 加各自的实测时长。
//! "浏览器 HLS 播放 30 秒不卡"那条验收要 OBS + 真接入节点，见 PR 的待验清单。

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use support::{cut_ts_segments, make_sample_video, probe_seconds, require_tools, temp_root};
use tower::ServiceExt as _;
use wxlike_video::{
    ingest_token, live_url, routes, VideoState, VideoStore, LIVE_SECRET_ENV, LIVE_TOKEN_HEADER,
    LIVE_WINDOW_ENV,
};

const KEY: &str = "cafebabecafebabecafebabe";
const SECRET: &str = "e2e-live-secret";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs ffmpeg: cargo test -p wxlike-video --test live_ffmpeg -- --ignored"]
async fn real_ts_segments_produce_a_playable_hls_window() {
    require_tools();
    std::env::set_var(LIVE_SECRET_ENV, SECRET);
    std::env::set_var(LIVE_WINDOW_ENV, "12");

    let src = temp_root("live-e2e-src");
    let sample = make_sample_video(&src, 640, 360, 6, "clip.mp4");
    let segments = cut_ts_segments(&sample, &src.join("cut"), 2);
    assert!(
        segments.len() >= 2,
        "6 秒片子至少该切成两片，实际 {}",
        segments.len()
    );

    let store = VideoStore::new(temp_root("live-e2e-store"));
    let app = routes(VideoState {
        store: store.clone(),
    });
    let exp = wxlike_core::types::now_ms() + 10 * 60_000;
    let token = ingest_token(SECRET, KEY, exp);

    let mut pushed = 0usize;
    for (i, path) in segments.iter().enumerate() {
        let bytes = std::fs::read(path).unwrap();
        assert!(bytes.len() > 100, "分片不该是空的");
        // 时长由接入节点实测后随头带上来（打包器不猜）
        let duration_ms = (probe_seconds(path) * 1_000.0).round().max(1.0) as u64;
        let name = format!("seg_{i:03}.ts");
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/live/{KEY}/segment/{name}"))
                    .header(LIVE_TOKEN_HEADER, token.as_str())
                    .header("x-wxlike-segment-duration-ms", duration_ms.to_string())
                    .body(Body::from(bytes))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED, "第 {i} 片");
        pushed += 1;
    }

    // 清单：把所有推上来的片都列出来，TARGETDURATION 覆盖最长片
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(live_url(KEY))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    let playlist = String::from_utf8(bytes.clone()).unwrap();
    assert!(playlist.starts_with("#EXTM3U"), "{playlist}");
    assert_eq!(
        playlist
            .lines()
            .filter(|l| l.starts_with("#EXTINF"))
            .count(),
        pushed,
        "{playlist}"
    );
    assert!(playlist.contains("#EXT-X-MEDIA-SEQUENCE:0"), "{playlist}");
    let target: u64 = playlist
        .lines()
        .find(|l| l.starts_with("#EXT-X-TARGETDURATION"))
        .unwrap()
        .split(':')
        .next_back()
        .unwrap()
        .parse()
        .unwrap();
    let max_extinf = playlist
        .lines()
        .filter(|l| l.starts_with("#EXTINF"))
        .map(|l| {
            l.trim_start_matches("#EXTINF:")
                .split(',')
                .next()
                .unwrap()
                .parse::<f64>()
                .unwrap()
        })
        .fold(0.0f64, f64::max);
    assert!(
        target as f64 >= max_extinf,
        "RFC 8216：TARGETDURATION({target}) 必须 ≥ 每个 EXTINF({max_extinf})"
    );

    // 分片可取，且支持 Range（观众掉线重传）
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/live/{KEY}/segment/seg_000.ts"))
                .header("range", "bytes=0-3")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);

    let bundle = store.live_dir(KEY);

    // 停播：观众手里那份能播到底
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/live/{KEY}/stop"))
                .header(LIVE_TOKEN_HEADER, token.as_str())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let res = app
        .oneshot(
            Request::builder()
                .uri(live_url(KEY))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let text = String::from_utf8(
        http_body_util::BodyExt::collect(res.into_body())
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(text.ends_with("#EXT-X-ENDLIST\n"), "{text}");
    // 停播后窗口里的片仍在盘上（回放/录制的原料）
    assert!(bundle.join("seg_000.ts").is_file());

    // 把落盘目录里这一份直接交给 ffprobe：能读出接近样片时长，说明清单与分片
    // 是自洽的（观众拿到的就是这一份）。**必须在停播之后**：没有
    // `#EXT-X-ENDLIST` 的清单按 HLS 定义是直播流，时长本就是未知（ffprobe 给
    // N/A），那不是坏产物，拿它断言会得出假阴性。
    let measured = probe_seconds(&bundle.join("index.m3u8"));
    assert!(
        (measured - 6.0).abs() < 1.5,
        "HLS 实测时长 {measured}s，样片 6s：差太多说明清单或分片对不上"
    );

    std::env::remove_var(LIVE_SECRET_ENV);
    std::env::remove_var(LIVE_WINDOW_ENV);
    std::fs::remove_dir_all(&src).ok();
}
