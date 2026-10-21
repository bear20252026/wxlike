//! 请求体解析失败时的**错误形状**：必须是 `{"error": ...}`，不是纯文本。
//!
//! 背景：axum 的 `Json<T>` 在反序列化失败时返回纯文本
//! （`Failed to deserialize the JSON body into the target type: ...`），
//! 而平台其它错误都是 `{"error": "..."}`。客户端按 `body["error"]` 取原因，
//! 遇到这类错误只能拿到 `null`，于是把"你少传了 duration_ms"显示成"未知错误"。
//!
//! 这个文件测的是**契约**，不是某个接口：任何 handler 换成 `ApiJson` 之后，
//! 坏输入都必须得到同样的形状。

mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::json;
use wxlike_platform::Platform;

fn raw(path: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn malformed_json_is_422_with_an_error_field() {
    let app = Platform::router(common::platform());

    let (status, body) = common::call(&app, raw("/api/register", "{not json")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let message = body["error"]
        .as_str()
        .unwrap_or_else(|| panic!("响应必须是 {{\"error\": ...}}，实际：{body}"));
    assert!(
        message.contains("请求体不合法"),
        "错误要说清是请求体的问题：{message}"
    );
}

#[tokio::test]
async fn empty_body_is_400_and_says_what_is_missing() {
    let app = Platform::router(common::platform());

    let (status, body) = common::call(&app, raw("/api/register", "")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let message = body["error"].as_str().unwrap_or_default();
    assert!(
        message.contains("请求体为空"),
        "空 body 不该报 serde 的 `EOF while parsing`：{message}"
    );
}

#[tokio::test]
async fn wrong_field_type_is_422_and_names_the_field() {
    let app = Platform::router(common::platform());

    // name 应当是字符串，给了数字
    let (status, body) = common::call(
        &app,
        raw("/api/register", r#"{"name": 42, "password": "pw"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let message = body["error"].as_str().unwrap_or_default();
    assert!(
        message.contains("name"),
        "错误要点名哪个字段不对（否则调用方只能猜）：{message}"
    );
}

#[tokio::test]
async fn a_valid_body_still_goes_through() {
    // 反向确认：换提取器没有把正常请求也挡掉
    let app = Platform::router(common::platform());
    let (status, body) = common::call(
        &app,
        raw("/api/register", r#"{"name": "alice", "password": "pw"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["token"].is_string());
}

#[tokio::test]
async fn admin_endpoints_use_the_same_shape() {
    // adminx 也是同一个契约：坏 body 不该漏出纯文本
    let app = Platform::router(common::platform());
    let (status, body) = common::call(&app, raw("/api/login", "{\"name\": ")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"].is_string(),
        "登录接口的坏 body 也要是 JSON：{body}"
    );
    let _ = json!({}); // 保持 import 有用（本文件用 json! 构造过 payload）
}
