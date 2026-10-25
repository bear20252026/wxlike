//! `tests/security` 的共用夹具。
//!
//! 原则：**只搭真实的平台路由**（`Platform::router`），不打桩。多账号用
//! `signup` 真注册，越权用例就是"用 B 的 token 打 A 的对象"——
//! 这正是线上会发生的动作。需要控制时间的部分（风控窗口）用引擎的
//! `now_ms` 入参，不 sleep。
#![allow(dead_code)]

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt as _;
use wxlike_platform::{Platform, PlatformOptions, PlatformState};

/// 管理端令牌：与 `PlatformOptions.admin_token` 一致，供"管理面必须真的要求
/// 管理令牌"的用例使用。
pub const ADMIN_TOKEN: &str = "security-test-admin";

pub fn platform() -> PlatformState {
    let opts = PlatformOptions {
        session_ttl_ms: 60_000,
        pbkdf2_rounds: 1_000,
        admin_token: Some(ADMIN_TOKEN.to_string()),
        ..Default::default()
    };
    Platform::in_memory_with(opts).unwrap().shared()
}

pub fn app(state: PlatformState) -> Router {
    Platform::router(state)
}

pub fn req(method: &str, path: &str, body: Option<Value>, token: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    b.body(match body {
        Some(v) => Body::from(v.to_string()),
        None => Body::empty(),
    })
    .unwrap()
}

/// 同一个请求，额外带自定义头（幂等键、管理令牌）。
pub fn req_with_headers(
    method: &str,
    path: &str,
    body: Option<Value>,
    token: Option<&str>,
    extra: &[(&str, &str)],
) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    b.body(match body {
        Some(v) => Body::from(v.to_string()),
        None => Body::empty(),
    })
    .unwrap()
}

pub async fn call(app: &Router, r: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(r).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, body)
}

pub async fn signup(app: &Router, name: &str) -> (String, String) {
    let (status, body) = call(
        app,
        req(
            "POST",
            "/api/register",
            Some(json!({"name": name, "password": "pw-123456"})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "signup {name}: {body}");
    (
        body["user_id"].as_str().unwrap().to_string(),
        body["token"].as_str().unwrap().to_string(),
    )
}

/// 建群并返回 group_id。群主是 `owner_token` 对应的人。
pub async fn create_group(app: &Router, token: &str, name: &str) -> String {
    let (status, body) = call(
        app,
        req("POST", "/api/groups", Some(json!({"name": name})), Some(token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create group: {body}");
    body["group_id"]
        .as_str()
        .or_else(|| body["id"].as_str())
        .unwrap_or_else(|| panic!("create group 没返回 id: {body}"))
        .to_string()
}

/// multipart 上传一个对象（字段名与 `media::upload` 一致）。
pub async fn upload_media(app: &Router, token: &str, bytes: &[u8], public: bool) -> String {
    let boundary = "----wxlike-security-boundary";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"x.bin\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"visibility\"\r\n\r\n{}\r\n",
            if public { "public" } else { "private" }
        )
        .as_bytes(),
    );
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

    let req = Request::builder()
        .method("POST")
        .uri("/api/media")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(body))
        .unwrap();
    let (status, resp) = call(app, req).await;
    assert_eq!(status, StatusCode::OK, "upload: {resp}");
    resp["media_id"]
        .as_str()
        .or_else(|| resp["id"].as_str())
        .unwrap_or_else(|| panic!("upload 没返回 media_id: {resp}"))
        .to_string()
}

/// 断言"这个响应没有把对象给出去"：越权的正确形状是 401/403/404，
/// 而不是 200 + 数据，也不是 500（500 说明校验抛错而不是拒绝）。
pub fn assert_denied(status: StatusCode, body: &Value, what: &str) {
    assert!(
        matches!(
            status,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
        ),
        "{what} 应当被拒绝（401/403/404），实际 {status} body={body}"
    );
}
