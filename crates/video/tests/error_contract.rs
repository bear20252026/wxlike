//! 错误契约：**video 的每个非 2xx 响应都必须是 `{"error": "..."}`**。
//!
//! 这个文件存在的理由很具体：`wxlike-video` 原来全部用
//! `(StatusCode, String)` 返回错误，axum 把它渲染成**纯文本**。同一个服务端
//! 进程里，平台接口的 404 是 `{"error":"not found"}`、视频接口的 404 是
//! `no such video artifact` —— 客户端按 `body["error"]` 取原因，遇到后者只能
//! 拿到 `null`，于是把"这条视频还没转完"显示成"未知错误"。
//!
//! 所以这里测的不是"状态码对不对"（各模块的测试已经覆盖了），而是
//! **形状是否统一**，以及 `Content-Type` 有没有跟上。
//!
//! 一条额外约束：请求体解析失败也要走这个形状（`ApiJson`），并且要说出
//! 是**哪个字段**错了。`axum::Json` 的原生行为是纯文本 + "第 1 行第 11 列"，
//! 调用方看不出是哪个字段。

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use tower::ServiceExt as _;
use wxlike_video::{routes, VideoPolicy, VideoState, VideoStore, LIVE_SECRET_ENV};

/// 每个用例一个独立目录：视频存储会建子目录，共用目录会让并行用例互相踩。
fn app() -> axum::Router {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "wxlike-video-errcontract-{}-{}-{}",
        std::process::id(),
        wxlike_core::types::now_ms(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    // 错误契约测试关心的是"错的时候长什么样"，不是"谁能调"：装一个
    // 本地信任的认证器，让控制面进得去（401 的契约另有专门用例）。
    routes(VideoState {
        store: VideoStore::new(dir).with_policy(VideoPolicy::trusted_local("tester")),
    })
}

struct Reply {
    status: StatusCode,
    content_type: Option<String>,
    body: String,
}

impl Reply {
    /// 断言"这是个统一形状的错误响应"，并返回 `error` 那句。
    ///
    /// 三件事一起断言，因为它们是一体的：状态码非 2xx、`Content-Type` 是 JSON、
    /// body 是 `{"error": "<非空>"}` 且**只有这一个键**。少任何一条，
    /// 客户端就得写第二种解析路径。
    fn expect_error(&self, want: StatusCode) -> String {
        assert_eq!(self.status, want, "body: {}", self.body);
        let ctype = self.content_type.as_deref().unwrap_or_default();
        assert!(
            ctype.starts_with("application/json"),
            "错误体的 Content-Type 必须是 JSON，实际 {ctype:?}；body: {}",
            self.body
        );
        let value: serde_json::Value = serde_json::from_str(&self.body)
            .unwrap_or_else(|err| panic!("错误体不是 JSON（{err}）：{}", self.body));
        let obj = value
            .as_object()
            .unwrap_or_else(|| panic!("错误体必须是对象：{}", self.body));
        assert_eq!(
            obj.len(),
            1,
            "错误体只许有 error 一个键（多一个键就多一种解析分支）：{}",
            self.body
        );
        obj.get("error")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| panic!("error 必须是非空字符串：{}", self.body))
            .to_string()
    }
}

async fn call(app: &axum::Router, method: Method, uri: &str, body: Option<&str>) -> Reply {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    let res = app
        .clone()
        .oneshot(
            builder
                .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let content_type = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    Reply {
        status,
        content_type,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn missing_id() -> String {
    // 合法形状（64 个十六进制字符）但一定不存在
    "0".repeat(64)
}

#[tokio::test]
async fn unknown_video_is_a_json_404() {
    let app = app();
    let reply = call(
        &app,
        Method::GET,
        &format!("/api/video/{}", missing_id()),
        None,
    )
    .await;
    let msg = reply.expect_error(StatusCode::NOT_FOUND);
    // 审计第 5 轮起 404 也是固定文案（不再回显 anyhow 链——context 里可能
    // 带磁盘布局）。这条测试要防的从来不是"少了哪个字段"，而是**平台相关
    // 的错误文本**（Unix `No such file` / Windows `os error 3`）混进响应体：
    // 固定文案天然免疫；缺哪个对象由调用方自己知道（它刚发的就是这个 id）。
    assert_eq!(
        msg, "no such video artifact",
        "404 文案固定且不含平台细节：{msg}"
    );
}

/// 畸形 id 在**拼路径之前**就被拒：这条同时防路径穿越。
#[tokio::test]
async fn malformed_id_is_a_json_404() {
    let app = app();
    let reply = call(&app, Method::GET, "/api/video/not-a-hash", None).await;
    assert!(reply
        .expect_error(StatusCode::NOT_FOUND)
        .contains("malformed"));
}

/// `Range` 那条路径原来手拼 `r#"{"error":"..."}"#`，现在也走统一构造。
#[tokio::test]
async fn missing_artifact_is_a_json_404() {
    let app = app();
    let reply = call(
        &app,
        Method::GET,
        &format!("/api/video/{}/play.mp4", missing_id()),
        None,
    )
    .await;
    reply.expect_error(StatusCode::NOT_FOUND);
}

/// 选流缺参数 → 422，且**告诉调用方缺的是什么**（不是一句 "bad request"）。
#[tokio::test]
async fn select_without_kbps_is_a_json_422() {
    let app = app();
    let reply = call(
        &app,
        Method::GET,
        &format!("/api/video/{}/select", missing_id()),
        None,
    )
    .await;
    let msg = reply.expect_error(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(msg.contains("kbps"), "要说出缺哪个参数：{msg}");
}

#[tokio::test]
async fn select_with_zero_kbps_is_a_json_422() {
    let app = app();
    let reply = call(
        &app,
        Method::GET,
        &format!("/api/video/{}/select?kbps=0", missing_id()),
        None,
    )
    .await;
    assert!(reply
        .expect_error(StatusCode::UNPROCESSABLE_ENTITY)
        .contains("positive"));
}

/// 作业面：查询与重试不存在的作业都是 404，形状一致。
#[tokio::test]
async fn unknown_job_is_a_json_404_on_both_routes() {
    let app = app();
    for (method, uri) in [
        (Method::GET, format!("/api/video/jobs/{}", missing_id())),
        (
            Method::POST,
            format!("/api/video/jobs/{}/retry", missing_id()),
        ),
    ] {
        let reply = call(&app, method.clone(), &uri, None).await;
        assert!(
            reply
                .expect_error(StatusCode::NOT_FOUND)
                .contains("no such job"),
            "{method} {uri}"
        );
    }
}

/// **请求体解析失败**：422 + 点名出错字段。
///
/// 这是 `ApiJson` 相对 `axum::Json` 的全部价值：原生行为是
/// `Failed to deserialize the JSON body into the target type: ...`
/// 的纯文本，客户端只能显示"未知错误"。
#[tokio::test]
async fn bad_watch_event_names_the_offending_field() {
    let app = app();
    let reply = call(
        &app,
        Method::POST,
        &format!("/api/video/{}/view", missing_id()),
        Some(r#"{"viewer":"u1","watched_ms":"不是数字"}"#),
    )
    .await;
    let msg = reply.expect_error(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        msg.contains("watched_ms"),
        "要说出是哪个字段错了，而不是让调用方猜：{msg}"
    );
}

/// 语法错（连 JSON 都不是）同样是 422 的统一形状。
#[tokio::test]
async fn malformed_json_body_is_a_json_422() {
    let app = app();
    let reply = call(
        &app,
        Method::POST,
        &format!("/api/video/{}/view", missing_id()),
        Some(r#"{"viewer":"#),
    )
    .await;
    reply.expect_error(StatusCode::UNPROCESSABLE_ENTITY);
}

/// 直播：没有会话就是 404。这条以前是硬编码的 `r#"{"error":"no live session"}"#`。
#[tokio::test]
async fn live_playlist_without_session_is_a_json_404() {
    let app = app();
    let reply = call(&app, Method::GET, "/live/nosuchstream/index.m3u8", None).await;
    assert!(reply
        .expect_error(StatusCode::NOT_FOUND)
        .contains("no live session"));
}

/// 直播推流：没配密钥 → 503（不是 403）。
///
/// 这里显式清掉环境变量：本文件的其它用例不读它，所以不存在并行踩踏；
/// 而"CI 机器上恰好设了这个变量"会让断言随机变红。
#[tokio::test]
async fn live_push_without_secret_is_a_json_503() {
    std::env::remove_var(LIVE_SECRET_ENV);
    let app = app();
    let reply = call(&app, Method::PUT, "/live/somekey/segment/seg_000.ts", None).await;
    let msg = reply.expect_error(StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        msg.contains(LIVE_SECRET_ENV),
        "503 的说明要点名缺哪个配置，运维才知道去配：{msg}"
    );
}
