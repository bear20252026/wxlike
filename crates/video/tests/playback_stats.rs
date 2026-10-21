//! E1 之三：播放时长统计（"谁看了多久"）。
//!
//! 一个 `#[tokio::test]` 跑完所有阶段——这个文件会读写
//! `WXLIKE_VIDEO_STATS_TOKEN`，而进程环境变量是全局的：同二进制里再开一个并行
//! 测试就会互相踩。阶段顺序本身就是断言的一部分（没配 token → 503，配了 → 认头）。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use tower::ServiceExt as _;
use wxlike_video::{
    routes, VideoState, VideoStore, MAX_WATCHED_MS, STATS_TOKEN_ENV, STATS_TOKEN_HEADER,
};

/// 手搭一个"已转码完成"的对象：`duration_ms = 3000`，完成度才算得出。
///
/// 目录名带进程内自增序号：只靠 `pid + 毫秒` 时，同一毫秒启动的两个并行用例会共用
/// 一个目录，一个在写 `meta.json`、另一个在读，读到半截就是随机失败。
fn fake_object() -> (VideoState, String) {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "wxlike-video-stats-{}-{}-{}",
        std::process::id(),
        wxlike_core::types::now_ms(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let store = VideoStore::new(dir);
    let id = "d".repeat(64);
    let obj = store.dir_public(&id);
    std::fs::create_dir_all(&obj).unwrap();
    let meta = wxlike_video::VideoMeta {
        video_id: id.clone(),
        source_size: 1024,
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
    (VideoState { store }, id)
}

async fn post_json(
    app: &axum::Router,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

async fn get_json_with(
    app: &axum::Router,
    uri: &str,
    headers: &[(&'static str, &str)],
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder().uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let res = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// 错误体在本 crate 里是纯文本（沿用 `upload`/`meta` 既有的
/// `(StatusCode, String)` 形状），断言文案要用原文而不是 JSON。
async fn get_raw(
    app: &axum::Router,
    uri: &str,
    headers: &[(&'static str, &str)],
) -> (StatusCode, String) {
    let mut builder = Request::builder().uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let res = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn watch_events_are_recorded_aggregated_and_gated() {
    // 阶段 0：确保没配名单 token（开发者本机可能配了）
    std::env::remove_var(STATS_TOKEN_ENV);
    let (state, id) = fake_object();
    let app = routes(state.clone());
    let view = format!("/api/video/{id}/view");

    // ---- 拒绝：非法上报不能被悄悄收下 ----
    // 不存在的对象：不给它编统计
    let ghost = "e".repeat(64);
    let (status, _) = post_json(
        &app,
        &format!("/api/video/{ghost}/view"),
        serde_json::json!({"viewer": "u1", "watched_ms": 1000}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown video");

    // id 形状不对：404，且在碰文件系统之前
    let (status, _) = post_json(
        &app,
        "/api/video/../etc/view",
        serde_json::json!({"viewer": "u1", "watched_ms": 1000}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 0 时长心跳不是观看
    let (status, body) = post_json(
        &app,
        &view,
        serde_json::json!({"viewer": "u1", "watched_ms": 0}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // 空 viewer
    let (status, _) = post_json(
        &app,
        &view,
        serde_json::json!({"viewer": "   ", "watched_ms": 500}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // 数量级错误（把秒当毫秒报 40 小时）
    let (status, _) = post_json(
        &app,
        &view,
        serde_json::json!({"viewer": "u1", "watched_ms": MAX_WATCHED_MS + 1}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // 超长 session_id
    let (status, _) = post_json(
        &app,
        &view,
        serde_json::json!({
            "viewer": "u1",
            "watched_ms": 500,
            "session_id": "s".repeat(200),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // ---- 接受：同一会话的多次心跳 ----
    let (status, first) = post_json(
        &app,
        &view,
        serde_json::json!({"viewer": "u1", "watched_ms": 1_000, "position_ms": 1_000}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    // 客户端没给 session_id → 服务端补一个，回执里给出（后续要沿用同一个）
    let session = first["session_id"].as_str().unwrap().to_string();
    assert!(session.starts_with("s-"), "{first}");
    assert!(first["event_id"].as_i64().unwrap() > 0);

    let (status, second) = post_json(
        &app,
        &view,
        serde_json::json!({
            "viewer": "u1",
            "session_id": session,
            "watched_ms": 1_000,
            "position_ms": 2_000,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        second["session_id"].as_str().unwrap(),
        session,
        "沿用同一个会话"
    );

    // 第二个人：一个人看了 5 秒（比片长还长）并自报看完
    let (status, _) = post_json(
        &app,
        &view,
        serde_json::json!({
            "viewer": "u2",
            "watched_ms": 5_000,
            "position_ms": 3_000,
            "completed": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // ---- 聚合 ----
    let (status, stats) = get_json_with(&app, &format!("/api/video/{id}/stats"), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stats["video_id"].as_str().unwrap(), id);
    assert_eq!(stats["events"].as_u64().unwrap(), 3);
    assert_eq!(stats["unique_viewers"].as_u64().unwrap(), 2);
    assert_eq!(stats["total_watched_ms"].as_u64().unwrap(), 7_000);
    assert_eq!(stats["avg_watched_ms_per_viewer"].as_u64().unwrap(), 3_500);
    // 完成度：u1 = 2000/3000 = 0.667（封顶 1.0 之内），u2 = 5000/3000 → 封顶 1.0
    let completion = stats["avg_completion"].as_f64().unwrap();
    assert!(
        (completion - (2.0 / 3.0 + 1.0) / 2.0).abs() < 1e-9,
        "平均完成度应是每人封顶后取平均，实测 {completion}"
    );
    assert!(stats["first_seen_ms"].as_i64().unwrap() <= stats["last_seen_ms"].as_i64().unwrap());

    // 没人看的视频：聚合是 0 事件，而不是 404、也不是"没数据就编一个数"
    let (status, empty) = get_json_with(&app, &format!("/api/video/{ghost}/stats"), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty["events"].as_u64().unwrap(), 0);
    assert_eq!(empty["unique_viewers"].as_u64().unwrap(), 0);
    // 片长未知 → 完成度是 null（"不知道"≠"没人看完"）
    assert!(empty["avg_completion"].is_null(), "{empty}");

    // ---- 名单的门 ----
    let viewers_uri = format!("/api/video/{id}/stats/viewers");
    // 没配 token：明确 503，绝不返回空名单假装"没人看"
    let (status, body) = get_raw(&app, &viewers_uri, &[]).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(
        body.contains(STATS_TOKEN_ENV),
        "错误要说清缺哪个配置: {body}"
    );
    assert!(!body.contains("u1"), "503 的说明里不能漏出身份: {body}");

    std::env::set_var(STATS_TOKEN_ENV, "list-token");
    // 配了但没带 → 403（这是"你没权限"，跟"服务没配"不是一回事）
    let (status, _) = get_json_with(&app, &viewers_uri, &[]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = get_json_with(&app, &viewers_uri, &[(STATS_TOKEN_HEADER, "nope")]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let auth = [(STATS_TOKEN_HEADER, "list-token")];
    let (status, rows) = get_json_with(&app, &viewers_uri, &auth).await;
    assert_eq!(status, StatusCode::OK);
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    // 按看了多久排：u2(5000) 在前
    assert_eq!(rows[0]["viewer"].as_str().unwrap(), "u2");
    assert_eq!(rows[0]["watched_ms"].as_u64().unwrap(), 5_000);
    assert!(rows[0]["completed"].as_bool().unwrap());
    assert_eq!(rows[0]["sessions"].as_u64().unwrap(), 1);
    assert_eq!(rows[1]["viewer"].as_str().unwrap(), "u1");
    assert_eq!(rows[1]["watched_ms"].as_u64().unwrap(), 2_000);
    assert_eq!(rows[1]["events"].as_u64().unwrap(), 2, "两条心跳");
    assert_eq!(rows[1]["sessions"].as_u64().unwrap(), 1, "同一个会话");
    assert_eq!(rows[1]["max_position_ms"].as_u64().unwrap(), 2_000);
    assert!(!rows[1]["completed"].as_bool().unwrap());

    // 分页：limit=1 只给最长的那个人
    let (status, one) = get_json_with(&app, &format!("{viewers_uri}?limit=1"), &auth).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one.as_array().unwrap().len(), 1);
    assert_eq!(one[0]["viewer"].as_str().unwrap(), "u2");

    // 恶意 limit：夹到上限，不炸也不返回空
    let (status, many) = get_json_with(&app, &format!("{viewers_uri}?limit=999999"), &auth).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(many.as_array().unwrap().len(), 2);

    // id 校验在这条路上也一样有效
    let (status, _) = get_json_with(&app, "/api/video/zzz/stats/viewers", &auth).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    std::env::remove_var(STATS_TOKEN_ENV);
    // 名单之外的面：聚合不需要 token（它是数，不是身份）
    let (status, _) = get_json_with(&app, &format!("/api/video/{id}/stats"), &[]).await;
    assert_eq!(status, StatusCode::OK);

    // ---- 直接调 store：库坏了要冒出来，不能吞 ----
    let record = state
        .store
        .record_watch(
            &id,
            &wxlike_video::WatchEvent {
                viewer: "u3".into(),
                session_id: "explicit-session".into(),
                watched_ms: 700,
                position_ms: 700,
                completed: false,
            },
        )
        .unwrap();
    assert_eq!(record.session_id, "explicit-session");
    assert!(record.event_id > 0);
    let stats = state.store.watch_stats(&id).unwrap();
    assert_eq!(stats.events, 4);
    assert_eq!(stats.unique_viewers, 3);
}

#[test]
fn selection_route_reports_why_it_chose_a_rendition() {
    // 这条走 HTTP：验证 handler 的参数与响应形状
    let dir = std::env::temp_dir().join(format!(
        "wxlike-video-select-{}-{}",
        std::process::id(),
        wxlike_core::types::now_ms()
    ));
    let store = VideoStore::new(dir);
    let id = "a".repeat(64);
    let obj = store.dir_public(&id);
    std::fs::create_dir_all(&obj).unwrap();
    let meta = wxlike_video::VideoMeta {
        video_id: id.clone(),
        source_size: 1,
        duration_ms: 1_000,
        width: 1920,
        height: 1080,
        video_codec: "h264".into(),
        audio_codec: "aac".into(),
        transcode: wxlike_video::TranscodeInfo {
            resolution: "-2x1080".into(),
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            created_at_ms: 1,
        },
        renditions: vec![
            wxlike_video::Rendition {
                height: 360,
                width: 640,
                bandwidth_bps: 900_000,
                playlist: "360/playlist.m3u8".into(),
            },
            wxlike_video::Rendition {
                height: 720,
                width: 1280,
                bandwidth_bps: 2_400_000,
                playlist: "720/playlist.m3u8".into(),
            },
        ],
    };
    std::fs::write(obj.join("meta.json"), serde_json::to_vec(&meta).unwrap()).unwrap();
    let app = routes(VideoState { store });

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let chosen = rt.block_on(async {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/video/{id}/select?kbps=4000"))
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
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
    });
    assert_eq!(chosen["available_bps"].as_u64().unwrap(), 4_000_000);
    assert_eq!(chosen["usable_bps"].as_u64().unwrap(), 3_200_000);
    assert_eq!(chosen["rendition"]["height"].as_u64().unwrap(), 720);
    assert_eq!(
        chosen["rendition"]["playlist"].as_str().unwrap(),
        "720/playlist.m3u8"
    );
    assert_eq!(chosen["all_heights"].as_array().unwrap().len(), 2);

    // 带宽不足也照样给最低档（宁糊不黑）
    let low = rt.block_on(async {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/video/{id}/select?kbps=500"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = http_body_util::BodyExt::collect(res.into_body())
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
    });
    assert_eq!(low["rendition"]["height"].as_u64().unwrap(), 360);

    // 不给 kbps：422，不是"随便选一个"
    rt.block_on(async {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/video/{id}/select"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    });

    // kbps=0：0 带宽不是一个可用额度，报错而不是回 360p
    rt.block_on(async {
        let res = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/video/{id}/select?kbps=0"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    });
}
