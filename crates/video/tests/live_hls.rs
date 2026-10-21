//! E2：直播接入与滚动窗口 HLS。
//!
//! 只有**一个** `#[tokio::test]`：这个文件要读写 `WXLIKE_LIVE_INGEST_SECRET` 与
//! `WXLIKE_LIVE_WINDOW_SEGMENTS`，环境变量是进程级的，并行用例会互相踩。
//!
//! 这里**不跑 RTMP**：本仓库按 ADR-21 不在进程内跑媒体服务器，接入的是
//! "外部终止 RTMP 之后"推上来的分片（`live_ffmpeg.rs` 用真 ffmpeg 产物验收）。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;
use wxlike_video::{
    ingest_token, live_url, routes, VideoState, VideoStore, LIVE_SECRET_ENV, LIVE_TOKEN_HEADER,
    LIVE_WINDOW_ENV,
};

const KEY: &str = "a1b2c3d4e5f6a1b2c3d4e5f6";
const SECRET: &str = "unit-test-live-secret";
const WINDOW: usize = 3;

async fn push(
    app: &axum::Router,
    path: &str,
    method: &str,
    body: Vec<u8>,
    token: Option<&str>,
    duration_ms: Option<&str>,
) -> (StatusCode, serde_json::Value, Vec<(String, String)>) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(value) = token {
        builder = builder.header(LIVE_TOKEN_HEADER, value);
    }
    if let Some(value) = duration_ms {
        builder = builder.header("x-wxlike-segment-duration-ms", value);
    }
    let res = app
        .clone()
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let headers: Vec<(String, String)> = res
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let bytes = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let json =
        serde_json::from_str::<serde_json::Value>(&text).unwrap_or(serde_json::Value::String(text));
    (status, json, headers)
}

async fn get_raw(app: &axum::Router, path: &str) -> (StatusCode, Vec<(String, String)>, Vec<u8>) {
    let res = app
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let headers: Vec<(String, String)> = res
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let bytes = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// 现在有效、`ttl_ms` 后过期的凭据。
fn fresh_token(key: &str, ttl_ms: u64) -> String {
    let exp = wxlike_core::types::now_ms() + ttl_ms;
    ingest_token(SECRET, key, exp)
}

#[tokio::test]
async fn live_ingest_authorizes_packs_and_serves_a_rolling_window() {
    let dir = std::env::temp_dir().join(format!(
        "wxlike-video-live-{}-{}",
        std::process::id(),
        wxlike_core::types::now_ms()
    ));
    let store = VideoStore::new(dir);
    let app = routes(VideoState {
        store: store.clone(),
    });

    // ---- 阶段 1：没配共享密钥 → 拒绝一切推流（不提供 dev 默认值）----
    std::env::remove_var(LIVE_SECRET_ENV);
    std::env::remove_var(LIVE_WINDOW_ENV);
    let (status, body, _) = push(
        &app,
        &format!("/live/{KEY}/segment/seg_000.ts"),
        "PUT",
        b"ts".to_vec(),
        Some(&fresh_token(KEY, 60_000)),
        Some("4000"),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains(LIVE_SECRET_ENV),
        "错误要说清缺哪个配置: {body}"
    );

    std::env::set_var(LIVE_SECRET_ENV, SECRET);
    std::env::set_var(LIVE_WINDOW_ENV, WINDOW.to_string());

    // ---- 阶段 2：授权与参数 ----
    let uri = format!("/live/{KEY}/segment/seg_000.ts");
    // 没带凭据
    let (status, _, _) = push(&app, &uri, "PUT", b"ts".to_vec(), None, Some("4000")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "缺凭据头");
    // 凭据签名不对（接入节点配错 secret）
    let wrong = format!(
        "{}.{}",
        wxlike_core::types::now_ms() + 60_000,
        "0".repeat(64)
    );
    let (status, _, _) = push(
        &app,
        &uri,
        "PUT",
        b"ts".to_vec(),
        Some(&wrong),
        Some("4000"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "签名不符");
    // 拿别的流的凭据推这一路（凭据绑 stream_key）
    let other_key = ingest_token(
        SECRET,
        "otherstreamkey0000000000ff",
        wxlike_core::types::now_ms() + 60_000,
    );
    let (status, _, _) = push(
        &app,
        &uri,
        "PUT",
        b"ts".to_vec(),
        Some(&other_key),
        Some("4000"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "凭据不属于这路流");
    // 过期即失效（不需要谁去吊销）
    let expired = ingest_token(SECRET, KEY, wxlike_core::types::now_ms() - 1);
    let (status, _, _) = push(
        &app,
        &uri,
        "PUT",
        b"ts".to_vec(),
        Some(&expired),
        Some("4000"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "过期凭据");
    // 缺时长头：打包器不猜时长（猜错会让 TARGETDURATION 与 EXTINF 打架）
    let (status, body, _) = push(
        &app,
        &uri,
        "PUT",
        b"ts".to_vec(),
        Some(&fresh_token(KEY, 60_000)),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _, _) = push(
        &app,
        &uri,
        "PUT",
        b"ts".to_vec(),
        Some(&fresh_token(KEY, 60_000)),
        Some("abc"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "时长不是数字");
    let (status, _, _) = push(
        &app,
        &uri,
        "PUT",
        b"ts".to_vec(),
        Some(&fresh_token(KEY, 60_000)),
        Some("0"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "0 时长不合法");
    // 非法流密钥 / 分片名：在碰文件系统之前拒
    let (status, _, _) = push(
        &app,
        "/live/bad%20key/segment/seg_000.ts",
        "PUT",
        b"ts".to_vec(),
        Some(&fresh_token("bad key", 60_000)),
        Some("4000"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "非法 stream key");
    let (status, _, _) = push(
        &app,
        &format!("/live/{KEY}/segment/window.json"),
        "PUT",
        b"ts".to_vec(),
        Some(&fresh_token(KEY, 60_000)),
        Some("4000"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "分片名必须是有意义的媒体文件名"
    );

    // 播放面不需要凭据（与点播同级的公开面），但停播/推流需要
    let (status, _, _) = push(
        &app,
        &format!("/live/{KEY}/stop"),
        "POST",
        Vec::new(),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "停播也要凭据");

    // ---- 阶段 3：推 5 片，窗口只留 3 片 ----
    let mut seqs = Vec::new();
    for i in 0..5u64 {
        let name = format!("seg_{i:03}.ts");
        let payload: Vec<u8> = format!("segment-payload-{i}").into_bytes();
        let (status, body, _) = push(
            &app,
            &format!("/live/{KEY}/segment/{name}"),
            "PUT",
            payload,
            Some(&fresh_token(KEY, 60_000)),
            Some("4000"),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["seq"].as_u64().unwrap(), i);
        seqs.push(body["media_sequence"].as_u64().unwrap());
        // 第 4 片之后窗口开始裁剪
        assert_eq!(
            body["in_window"].as_u64().unwrap(),
            (i + 1).min(WINDOW as u64)
        );
        // 回执里的播放地址 = 平台 live_state().hls_url 的形状
        assert_eq!(
            body["playlist_url"].as_str().unwrap(),
            live_url(KEY),
            "接入节点拿到的地址必须能直接给观众"
        );
    }
    assert_eq!(seqs, vec![0, 0, 0, 1, 2], "MEDIA-SEQUENCE 随窗口前移");

    // 重推已经收过的分片：409，且不能在清单里写出第二条相同 EXTINF
    let (status, body, _) = push(
        &app,
        &format!("/live/{KEY}/segment/seg_004.ts"),
        "PUT",
        b"replay".to_vec(),
        Some(&fresh_token(KEY, 60_000)),
        Some("4000"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (_, _, again) = get_raw(&app, &live_url(KEY)).await;
    let text_now = String::from_utf8(again).unwrap();
    assert_eq!(
        text_now.lines().filter(|l| *l == "seg_004.ts").count(),
        1,
        "被拒的重推不该改清单: {text_now}"
    );
    assert!(
        !store.live_dir(KEY).join("seg_004.ts.replayed").exists(),
        "被拒的重推不该落盘"
    );

    // ---- 阶段 4：清单形状 ----
    let (status, headers, body) = get_raw(&app, &live_url(KEY)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header(&headers, "content-type").unwrap(),
        "application/vnd.apple.mpegurl"
    );
    // 滚动窗口绝不能被中间层缓存：缓存住就等于观众停在 48 秒以前
    assert_eq!(header(&headers, "cache-control").unwrap(), "no-store");
    let text = String::from_utf8(body).unwrap();
    assert!(text.starts_with("#EXTM3U"), "{text}");
    assert!(text.contains("#EXT-X-MEDIA-SEQUENCE:2"), "{text}");
    assert!(text.contains("#EXT-X-TARGETDURATION:4"), "{text}");
    assert_eq!(
        text.lines().filter(|l| l.starts_with("#EXTINF")).count(),
        WINDOW,
        "窗口外三片必须出清单: {text}"
    );
    assert!(
        text.contains("seg_002.ts") && !text.contains("seg_000.ts"),
        "{text}"
    );
    assert!(!text.contains("#EXT-X-ENDLIST"), "还在播不能写 ENDLIST");

    // ---- 阶段 5：分片可取、可断点续传 ----
    let (status, _, body) = get_raw(&app, &format!("/live/{KEY}/segment/seg_004.ts")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"segment-payload-4");

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/live/{KEY}/segment/seg_004.ts"))
                .header("range", "bytes=0-7")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::PARTIAL_CONTENT,
        "直播分片也要支持 Range"
    );
    let h: Vec<(String, String)> = res
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    // "segment-payload-4" 是 17 字节
    assert_eq!(header(&h, "content-range").unwrap(), "bytes 0-7/17");
    assert_eq!(header(&h, "accept-ranges").unwrap(), "bytes");

    // 出窗口的分片**仍在盘上**（那是回放/录制的原料，回收是运维的事）
    let live_dir = store.live_dir(KEY);
    for i in 0..5 {
        assert!(
            live_dir.join(format!("seg_{i:03}.ts")).is_file(),
            "seg_{i:03}.ts 应该还在盘上"
        );
    }
    assert!(live_dir.join("window.json").is_file());
    assert!(
        live_dir.join("index.m3u8").is_file(),
        "落盘目录就是播放目录"
    );

    // 不存在的东西：404，不给空清单假装在播
    let (status, _, _) = get_raw(&app, "/live/deadbeefdeadbeefdeadbeef/index.m3u8").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = get_raw(&app, &format!("/live/{KEY}/segment/seg_999.ts")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = get_raw(&app, "/live/..%2f..%2fetc/index.m3u8").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "路径穿越在拼路径之前就被拒");

    // ---- 阶段 6：停播 ----
    let (status, body, _) = push(
        &app,
        &format!("/live/{KEY}/stop"),
        "POST",
        Vec::new(),
        Some(&fresh_token(KEY, 60_000)),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["ended"].as_bool().unwrap());
    let (status, _, body) = get_raw(&app, &live_url(KEY)).await;
    let text = String::from_utf8(body).unwrap();
    assert!(text.ends_with("#EXT-X-ENDLIST\n"), "停播后清单收尾: {text}");
    assert_eq!(status, StatusCode::OK);

    // 停播后再推流：409（平台已停播，接入节点还在发是要人看的问题）
    let (status, body, _) = push(
        &app,
        &format!("/live/{KEY}/segment/seg_005.ts"),
        "PUT",
        b"late".to_vec(),
        Some(&fresh_token(KEY, 60_000)),
        Some("4000"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        !store.live_dir(KEY).join("seg_005.ts").exists(),
        "被拒的分片不该落盘"
    );

    // 没推过流的会话：停播是 404，不是"成功停了一个空会话"
    let ghost = "ffffffffffffffffffffffff";
    let (status, _, _) = push(
        &app,
        &format!("/live/{ghost}/stop"),
        "POST",
        Vec::new(),
        Some(&fresh_token(ghost, 60_000)),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // ---- 阶段 7：配坏了的窗口大小不能被悄悄当成默认值 ----
    // 静默降级在这里的代价是"直播只留 12 片而运维以为配了 3 片"，
    // 磁盘和延迟都会对不上，必须报错。
    std::env::set_var(LIVE_WINDOW_ENV, "not-a-number");
    let env_dir = std::env::temp_dir().join(format!(
        "wxlike-video-live-env-{}-{}",
        std::process::id(),
        wxlike_core::types::now_ms()
    ));
    let env_store = VideoStore::new(env_dir);
    let err = env_store
        .live_append_segment("abc12345", "seg_000.ts", b"x", 4_000)
        .unwrap_err();
    assert!(
        format!("{err:#}").contains(LIVE_WINDOW_ENV),
        "错误要点名是哪个配置坏了: {err:#}"
    );
    assert!(
        !env_store.live_dir("abc12345").join("seg_000.ts").exists(),
        "配错窗口时不该留下半份产物"
    );

    std::env::remove_var(LIVE_SECRET_ENV);
    std::env::remove_var(LIVE_WINDOW_ENV);
}
