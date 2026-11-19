//! G07/第四轮审计：识物/OCR/转写与翻译域**挂进真实组合根之后**的验收。
//!
//! 打的是 `Stack::app` —— 与监听端口上跑的是同一个 Router。生产默认不配置
//! 任何模型供应商凭据，仓内也没有沙箱 provider 实现，所以这两个域挂的是
//! **诚实拒绝面**：请求经真实域服务（consent/限流/参数校验）后如实报
//! `DEPENDENCY_UNAVAILABLE`（HTTP 503），绝不返回样例识别结果或样例译文。
//!
//! 底线：
//! 1. 匿名/伪造 token → 401（不是 404 —— 404 说明压根没挂）；
//! 2. 未 consent 直接跑 → 403 CONSENT_REQUIRED（域内闸门真实生效）；
//! 3. consent 后跑 → 503 DEPENDENCY_UNAVAILABLE，响应里**没有**任何结果字段；
//! 4. 参数错误先于供应商缺失被拒（400 INVALID_ARGUMENT）；
//! 5. 吊销会话 → 同一个 token 立刻 401；一键删除入口可用。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt as _;
use serde_json::{json, Value};
use tower::ServiceExt as _;
use wxlike_server::{start_stack, StackOptions};

async fn boot() -> wxlike_server::Stack {
    start_stack(StackOptions::default()).await.unwrap()
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, body)
}

fn request(method: &str, uri: &str, token: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    match body {
        Some(value) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(value.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

async fn register(app: &Router, name: &str) -> (String, String) {
    let (status, reply) = send(
        app,
        request("POST", "/api/register", None, Some(json!({"name": name, "password": "pw"}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register {name}: {reply}");
    (
        reply["user_id"].as_str().unwrap().to_string(),
        reply["token"].as_str().unwrap().to_string(),
    )
}

/// 1x1 PNG 的 base64（真实图片字节，供域内 mime/尺寸校验）。
const TINY_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

const GUARDED: &[(&str, &str)] = &[
    ("POST", "/api/v1/recognition/consent"),
    ("DELETE", "/api/v1/recognition/consent"),
    ("POST", "/api/v1/recognition/jobs"),
    ("GET", "/api/v1/recognition/jobs/vjob_x"),
    ("DELETE", "/api/v1/recognition/data"),
    ("POST", "/api/v1/translation/consent"),
    ("DELETE", "/api/v1/translation/consent"),
    ("POST", "/api/v1/translation"),
    ("DELETE", "/api/v1/translation/data"),
];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn anonymous_ai_requests_are_rejected() {
    let stack = boot().await;
    let app = stack.app.clone();
    for (method, uri) in GUARDED {
        let (status, body) = send(&app, request(method, uri, None, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri} → {body}");
    }
    let (status, _) = send(
        &app,
        request("POST", "/api/v1/translation", Some("obviously-not-a-session"),
            Some(json!({"text": "hi", "target": "zh-CN"}))),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "伪造 bearer 必须 401");
    stack.shutdown(std::time::Duration::from_secs(2)).await;
}

/// 识物/OCR/转写：consent 闸门真实生效；无供应商 → 503，绝无样例结果。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recognition_without_provider_fails_honestly() {
    let stack = boot().await;
    let app = stack.app.clone();
    let (_user_id, token) = register(&app, "g07-vision-user").await;
    let ocr = json!({
        "kind": "ocr", "image_base64": TINY_PNG, "mime": "image/png", "languages": ["zh-CN"],
    });

    // 未 consent → 403 CONSENT_REQUIRED（不是直接 503：闸门先于供应商）。
    let (status, body) = send(&app, request("POST", "/api/v1/recognition/jobs", Some(&token), Some(ocr.clone()))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "未授权应拒: {body}");
    assert_eq!(body["error"], "CONSENT_REQUIRED");

    // 显式 consent。
    let (status, body) = send(&app, request("POST", "/api/v1/recognition/consent", Some(&token), None)).await;
    assert_eq!(status, StatusCode::OK, "consent: {body}");
    assert!(body["expires_at_ms"].as_i64().unwrap() > 0);

    // 无供应商 → 503 DEPENDENCY_UNAVAILABLE；响应只有错误分类，没有结果。
    for kind in ["ocr", "identify"] {
        let payload = json!({
            "kind": kind, "image_base64": TINY_PNG, "mime": "image/png", "languages": ["zh-CN"],
        });
        let (status, body) = send(&app, request("POST", "/api/v1/recognition/jobs", Some(&token), Some(payload))).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{kind} 无供应商应 503: {body}");
        assert_eq!(body["error"], "DEPENDENCY_UNAVAILABLE");
        assert!(body.get("output").is_none() && body.get("job_id").is_none(),
            "诚实拒绝面不得携带任何结果字段: {body}");
    }
    // 转写（音频面）同样诚实拒绝。
    let (status, body) = send(
        &app,
        request("POST", "/api/v1/recognition/jobs", Some(&token),
            Some(json!({"kind": "transcribe", "audio_base64": "UklGRg==", "mime": "audio/wav"}))),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "transcribe: {body}");
    assert_eq!(body["error"], "DEPENDENCY_UNAVAILABLE");

    // 参数错误先于供应商缺失：mime 不合法 → 400。
    let (status, body) = send(
        &app,
        request("POST", "/api/v1/recognition/jobs", Some(&token),
            Some(json!({"kind": "ocr", "image_base64": TINY_PNG, "mime": "text/plain"}))),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "坏 mime 应 400: {body}");
    assert_eq!(body["error"], "INVALID_ARGUMENT");

    // 结果读面：没有作业 → 404（不是编造一个结果）。
    let (status, body) = send(&app, request("GET", "/api/v1/recognition/jobs/vjob_none", Some(&token), None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // 一键删除入口可用。
    let (status, body) = send(&app, request("DELETE", "/api/v1/recognition/data", Some(&token), None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["removed"].is_u64());

    // 吊销会话 → 同一个 token 立刻 401。
    stack.platform.revoke_session(&token).unwrap();
    let (status, _) = send(&app, request("POST", "/api/v1/recognition/jobs", Some(&token), Some(ocr))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "吊销后仍放行 = 有第二套鉴权");
    stack.shutdown(std::time::Duration::from_secs(2)).await;
}

/// 翻译：consent 闸门真实生效；无供应商 → 503，绝无样例译文。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn translation_without_provider_fails_honestly() {
    let stack = boot().await;
    let app = stack.app.clone();
    let (_user_id, token) = register(&app, "g07-translation-user").await;
    let ask = json!({"text": "hello world", "target": "zh-CN"});

    // 未 consent → 403。
    let (status, body) = send(&app, request("POST", "/api/v1/translation", Some(&token), Some(ask.clone()))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "未授权应拒: {body}");
    assert_eq!(body["error"], "CONSENT_REQUIRED");

    let (status, body) = send(&app, request("POST", "/api/v1/translation/consent", Some(&token), None)).await;
    assert_eq!(status, StatusCode::OK, "consent: {body}");

    // 无供应商 → 503；响应只有错误分类，没有译文。
    let (status, body) = send(&app, request("POST", "/api/v1/translation", Some(&token), Some(ask.clone()))).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "无供应商应 503: {body}");
    assert_eq!(body["error"], "DEPENDENCY_UNAVAILABLE");
    assert!(body.get("text").is_none(), "诚实拒绝面不得携带译文: {body}");

    // 参数错误先于供应商缺失：源/目标语言相同 → 400。
    let (status, body) = send(
        &app,
        request("POST", "/api/v1/translation", Some(&token),
            Some(json!({"text": "hello", "source": "en", "target": "en"}))),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "同语言应 400: {body}");
    assert_eq!(body["error"], "INVALID_ARGUMENT");

    // 撤回授权后再跑 → 回到 CONSENT_REQUIRED（授权真的被删了）。
    let (status, body) = send(&app, request("DELETE", "/api/v1/translation/consent", Some(&token), None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = send(&app, request("POST", "/api/v1/translation", Some(&token), Some(ask))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "撤回应回到授权闸门: {body}");
    assert_eq!(body["error"], "CONSENT_REQUIRED");

    // 一键删除入口可用。
    let (status, body) = send(&app, request("DELETE", "/api/v1/translation/data", Some(&token), None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    stack.shutdown(std::time::Duration::from_secs(2)).await;
}