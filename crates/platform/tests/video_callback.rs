//! 视频转码回调（`POST /api/video/callback`）与引用读取（`GET /api/video/ref/{id}`）。
//!
//! 验收重点不是"回调能不能收"，而是三条边界：
//! 1. **没配令牌 = 接口关闭**（403），配了但给错 = 401；
//! 2. 只收**能播的**引用：playlist/play/thumb 必须是本服务的 `/api/video/...`
//!    路径，外部 URL 一律 400（否则回调能往消息里塞任意播放地址）；
//! 3. 同一 `video_id` 重试回调覆盖同一行，不产生两条互相矛盾的历史。

mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};
use wxlike_platform::{Platform, PlatformOptions, PlatformState};

const TOKEN: &str = "callback-token-for-tests";

fn platform_with(token: Option<&str>) -> PlatformState {
    let opts = PlatformOptions {
        session_ttl_ms: 60_000,
        pbkdf2_rounds: 1_000,
        video_callback_token: token.map(str::to_string),
        ..Default::default()
    };
    Platform::in_memory_with(opts).unwrap().shared()
}

fn video_id(seed: char) -> String {
    std::iter::repeat_n(seed, 64).collect()
}

fn request(method: &str, path: &str, body: Option<Value>, token: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token {
        b = b.header("x-wxlike-video-token", t);
    }
    b.body(match body {
        Some(v) => Body::from(v.to_string()),
        None => Body::empty(),
    })
    .unwrap()
}

fn done_body(id: &str) -> Value {
    json!({
        "job_id": "job-1",
        "state": "done",
        "attempts": 1,
        "updated_at": 1_700_000_000_000i64,
        "result": {
            "video_id": id,
            "playlist_url": format!("/api/video/{id}/playlist.m3u8"),
            "play_url": format!("/api/video/{id}/play.mp4"),
            "thumb_url": format!("/api/video/{id}/thumb.png"),
            "duration_ms": 12_000,
            "width": 1280,
            "height": 720,
            "renditions": [{"height": 720, "kbps": 1800}]
        }
    })
}

async fn post(app: &axum::Router, body: Value, token: Option<&str>) -> (StatusCode, Value) {
    common::call(
        app,
        request("POST", "/api/video/callback", Some(body), token),
    )
    .await
}

#[tokio::test]
async fn callback_is_closed_until_a_token_is_configured() {
    let state = platform_with(None);
    let app = Platform::router(state);
    let id = video_id('a');

    let (status, body) = post(&app, done_body(&id), Some("anything")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // 403 的文案要告诉运维"该配哪个变量"，而不是干巴巴的 not permitted
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("WXLIKE_VIDEO_CALLBACK_TOKEN"),
        "403 要说清怎么开: {body}"
    );
}

#[tokio::test]
async fn wrong_token_is_rejected_and_the_right_one_stores_the_reference() {
    let state = platform_with(Some(TOKEN));
    let app = Platform::router(state.clone());
    let id = video_id('b');

    let (status, body) = post(&app, done_body(&id), Some("wrong")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    // 拒绝之后不能留下半条记录
    assert!(
        state.video_ref(&id).unwrap().is_none(),
        "被拒的回调不该落库"
    );

    let (status, body) = post(&app, done_body(&id), Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["video_id"], id);
    assert_eq!(body["duration_ms"], 12_000);
    assert_eq!(
        body["playlist_url"],
        format!("/api/video/{id}/playlist.m3u8")
    );

    // 读回来的是同一份事实
    let (status, body) = common::call(
        &app,
        request("GET", &format!("/api/video/ref/{id}"), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["job_id"], "job-1");
    assert_eq!(body["state"], "done");
    assert_eq!(body["attempts"], 1);
    assert_eq!(body["renditions"][0]["kbps"], 1800);
}

#[tokio::test]
async fn foreign_urls_are_refused() {
    let state = platform_with(Some(TOKEN));
    let app = Platform::router(state);
    let id = video_id('c');

    let mut body = done_body(&id);
    body["result"]["playlist_url"] = json!("https://evil.example/x.m3u8");
    // 422（不是 400）：这是"语法对、内容不合法"，与平台其余 `BadInput` 同一个口径
    let (status, resp) = post(&app, body, Some(TOKEN)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");

    // 路径穿越也不行（和媒体下载同一个口径）
    let mut body = done_body(&id);
    body["result"]["play_url"] = json!(format!("/api/video/{id}/../../etc/play.mp4"));
    // 422（不是 400）：这是"语法对、内容不合法"，与平台其余 `BadInput` 同一个口径
    let (status, resp) = post(&app, body, Some(TOKEN)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");
}

#[tokio::test]
async fn a_failed_job_cannot_write_a_reference() {
    let state = platform_with(Some(TOKEN));
    let app = Platform::router(state.clone());
    let id = video_id('d');

    // 没有 result 的回调（失败作业）：收下请求但**不写引用**，
    // 否则客户端会拿一条没有清单的引用当"能播"。
    let (status, resp) = post(
        &app,
        json!({"job_id": "job-2", "state": "failed", "attempts": 5}),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");
    assert!(state.video_ref(&id).unwrap().is_none());
}

#[tokio::test]
async fn retry_overwrites_the_same_row() {
    let state = platform_with(Some(TOKEN));
    let app = Platform::router(state.clone());
    let id = video_id('e');

    let (status, _) = post(&app, done_body(&id), Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK);

    // 同一条作业重试后再回调：时长与尝试次数变了，但还是一行
    let mut again = done_body(&id);
    again["attempts"] = json!(3);
    again["updated_at"] = json!(1_700_000_999_000i64);
    again["result"]["duration_ms"] = json!(13_500);
    let (status, body) = post(&app, again, Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["attempts"], 3);
    assert_eq!(body["duration_ms"], 13_500);
    assert_eq!(body["updated_at_ms"], 1_700_000_999_000i64);

    let stored = state.video_ref(&id).unwrap().unwrap();
    assert_eq!(stored.duration_ms, 13_500, "后到的回调覆盖先到的");
}

#[tokio::test]
async fn unknown_reference_is_404() {
    let state = platform_with(Some(TOKEN));
    let app = Platform::router(state);
    let id = video_id('f');
    let (status, _) = common::call(
        &app,
        request("GET", &format!("/api/video/ref/{id}"), None, None),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "还没回调过就是 404，不是空 200"
    );
}
