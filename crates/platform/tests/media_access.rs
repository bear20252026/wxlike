//! 媒体下载授权（issue #14）：内容地址不是权限。
//!
//! 三条验收：
//! 1. 非成员知道 `media_id` 也不能下载私有附件（匿名连"存在"都不告诉）；
//! 2. 短期签名 URL 可以替代 `Authorization`（浏览器 `<img>` 用），过期/篡改都拒绝；
//! 3. 公开资源（头像）登录用户都能看，但**仍要登录**。

mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use common::call;
use serde_json::json;
use wxlike_platform::{Platform, PlatformOptions, PlatformState};

fn test_platform() -> PlatformState {
    let opts = PlatformOptions {
        session_ttl_ms: 60_000,
        pbkdf2_rounds: 1_000,
        ..Default::default()
    };
    Platform::in_memory_with(opts).unwrap().shared()
}

fn app() -> axum::Router {
    common::app(test_platform())
}

async fn signup(app: &axum::Router, name: &str) -> (String, String) {
    let (status, body) = call(
        app,
        common::req(
            "POST",
            "/api/register",
            Some(json!({ "name": name, "password": "pw" })),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    (
        body["user_id"].as_str().unwrap().to_string(),
        body["token"].as_str().unwrap().to_string(),
    )
}

/// 上传一段字节，可选声明归属会话 / 公开可见。
async fn upload(
    app: &axum::Router,
    token: &str,
    bytes: &[u8],
    conv: Option<&str>,
    public: bool,
) -> String {
    let boundary = "wxlike-access-boundary";
    let mut body = Vec::new();
    if let Some(conv) = conv {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"conv\"\r\n\r\n{conv}\r\n"
            )
            .as_bytes(),
        );
    }
    if public {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"visibility\"\r\n\r\npublic\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"blob\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

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
    let (status, json) = call(app, req).await;
    assert_eq!(status, StatusCode::OK, "upload: {json}");
    json["media_id"].as_str().unwrap().to_string()
}

async fn get(app: &axum::Router, path: &str, token: Option<&str>) -> StatusCode {
    let mut b = Request::builder().method("GET").uri(path);
    if let Some(token) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let (status, _) = call(app, b.body(Body::empty()).unwrap()).await;
    status
}

/// 非成员知道 id 也拿不到：匿名 401、外人 404。
#[tokio::test]
async fn a_non_member_cannot_download_a_chat_attachment() {
    let app = app();
    let (alice, alice_token) = signup(&app, "alice").await;
    let (bob, bob_token) = signup(&app, "bob").await;
    let (_carol, carol_token) = signup(&app, "carol").await;

    let conv = format!("d-{alice}~{bob}");
    let id = upload(&app, &alice_token, b"private photo", Some(&conv), false).await;
    let path = format!("/api/media/{id}");

    // 匿名：401（连"这个 id 存不存在"都不说）
    assert_eq!(get(&app, &path, None).await, StatusCode::UNAUTHORIZED);
    // 会话另一端：可以
    assert_eq!(get(&app, &path, Some(&bob_token)).await, StatusCode::OK);
    // 上传者自己：可以
    assert_eq!(get(&app, &path, Some(&alice_token)).await, StatusCode::OK);
    // 第三方知道 id 也没用：404（不确认存在性）
    assert_eq!(
        get(&app, &path, Some(&carol_token)).await,
        StatusCode::NOT_FOUND
    );
}

/// 没声明会话的上传 = 只有上传者能取（不是"默认公开"）。
#[tokio::test]
async fn unattached_media_is_owner_only() {
    let app = app();
    let (_alice, alice_token) = signup(&app, "alice").await;
    let (_bob, bob_token) = signup(&app, "bob").await;
    let id = upload(&app, &alice_token, b"loose file", None, false).await;
    let path = format!("/api/media/{id}");
    assert_eq!(get(&app, &path, Some(&alice_token)).await, StatusCode::OK);
    assert_eq!(
        get(&app, &path, Some(&bob_token)).await,
        StatusCode::NOT_FOUND
    );
}

/// 签名 URL：能替代 Authorization；篡改与过期都拒绝。
#[tokio::test]
async fn a_signed_url_works_without_a_token_and_expires() {
    let app = app();
    let (alice, alice_token) = signup(&app, "alice").await;
    let (bob, _bob_token) = signup(&app, "bob").await;
    let conv = format!("d-{alice}~{bob}");
    let id = upload(&app, &alice_token, b"signed bytes", Some(&conv), false).await;

    let (status, reply) = call(
        &app,
        common::req(
            "GET",
            &format!("/api/media/{id}/url?ttl_secs=60"),
            None,
            Some(&alice_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let url = reply["url"].as_str().unwrap().to_string();
    assert!(url.contains("exp=") && url.contains("sig="), "{url}");

    // 不带 Authorization 也能下（这正是签名 URL 的意义：<img src>）
    assert_eq!(get(&app, &url, None).await, StatusCode::OK);

    // 改一个字节的签名 → 401
    let tampered = url.replace("sig=", "sig=0");
    assert_eq!(get(&app, &tampered, None).await, StatusCode::UNAUTHORIZED);

    // 过期 → 401（用 1 秒 TTL，等过去）
    let (status, reply) = call(
        &app,
        common::req(
            "GET",
            &format!("/api/media/{id}/url?ttl_secs=1"),
            None,
            Some(&alice_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let short = reply["url"].as_str().unwrap().to_string();
    tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;
    assert_eq!(get(&app, &short, None).await, StatusCode::UNAUTHORIZED);

    // 没有权限的人连签都签不出来
    let (status, _) = call(
        &app,
        common::req("GET", &format!("/api/media/{id}/url"), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// 公开资源（头像）：登录用户都能看，匿名仍然不行。
#[tokio::test]
async fn public_media_is_readable_by_any_signed_in_user() {
    let app = app();
    let (_alice, alice_token) = signup(&app, "alice").await;
    let (_bob, bob_token) = signup(&app, "bob").await;
    let id = upload(&app, &alice_token, b"avatar png", None, true).await;
    let path = format!("/api/media/{id}");
    assert_eq!(get(&app, &path, None).await, StatusCode::UNAUTHORIZED);
    assert_eq!(get(&app, &path, Some(&bob_token)).await, StatusCode::OK);
}

/// 不能把附件挂到别人的会话上。
#[tokio::test]
async fn a_user_cannot_attach_media_to_someone_elses_conversation() {
    let app = app();
    let (alice, alice_token) = signup(&app, "alice").await;
    let (bob, bob_token) = signup(&app, "bob").await;
    let (carol, _carol_token) = signup(&app, "carol").await;

    // alice 想把东西塞进 bob~carol 的会话
    let foreign = format!("d-{bob}~{carol}");
    let boundary = "wxlike-forbidden-boundary";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"conv\"\r\n\r\n{foreign}\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"b\"\r\n\
         Content-Type: application/octet-stream\r\n\r\nsneaky\r\n--{boundary}--\r\n"
    );
    let req = Request::builder()
        .method("POST")
        .uri("/api/media")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header(header::AUTHORIZATION, format!("Bearer {alice_token}"))
        .body(Body::from(body))
        .unwrap();
    let (status, _) = call(&app, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 而自己的会话可以
    let own = format!("d-{alice}~{bob}");
    let id = upload(&app, &alice_token, b"fine", Some(&own), false).await;
    assert_eq!(
        get(&app, &format!("/api/media/{id}"), Some(&bob_token)).await,
        StatusCode::OK
    );
}
