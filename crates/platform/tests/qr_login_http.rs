//! 扫码登录 HTTP 面（`/api/qr/tickets*`，审计第 5 轮挂载）的集成测试。
//!
//! 契约路径在这里钉死：create → poll → scan → approve → exchange 的完整
//! 闭环（含"兑换出的 token 真的能调 /api/me"），以及过期、错 secret、
//! 非创建者轮询、重复兑换这些必须失败的分叉。规则本体（PKCE、状态机
//! 原子性）在 `wxlike-identity::qr_login` 里另有单测，这里只测 HTTP 契约。

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{app, call, req, signup};
use serde_json::{json, Value};
use tower::ServiceExt as _;

fn state_of(body: &Value) -> &str {
    body["state"].as_str().unwrap_or_else(|| panic!("no state: {body}"))
}

/// 造票（创建者视角）：`{ticket_id, secret, payload, expires_at_ms}`。
async fn create_ticket(app: &axum::Router, token: &str) -> Value {
    let (status, body) = call(app, req("POST", "/api/qr/tickets", Some(json!({})), Some(token))).await;
    assert_eq!(status, StatusCode::OK, "create: {body}");
    assert!(body["secret"].as_str().is_some(), "{body}");
    assert!(body["expires_at_ms"].as_i64().unwrap_or(0) > 0, "{body}");
    let payload = body["payload"].as_str().expect("payload").to_string();
    assert!(
        payload.starts_with("wxlike-qr://"),
        "payload 是二维码内容：{payload}"
    );
    body
}

/// 把某张票直接改成过期（identity 的 TTL 是 2 分钟，测试不等真实时钟）。
fn force_expire(platform: &wxlike_platform::Platform, ticket_id: &str) {
    let conn = platform.db().lock();
    conn.execute(
        "UPDATE identity_qr SET expires_at = 1 WHERE id = ?1",
        rusqlite::params![ticket_id],
    )
    .unwrap();
}

#[tokio::test]
async fn happy_path_creates_scan_approve_exchange() {
    let platform = common::platform();
    let app = app(platform.clone());
    let (creator, creator_token) = signup(&app, "qr_creator").await;
    let (_scanner, scanner_token) = signup(&app, "qr_scanner").await;

    let ticket = create_ticket(&app, &creator_token).await;
    let ticket_id = ticket["ticket_id"].as_str().unwrap().to_string();
    let secret = ticket["secret"].as_str().unwrap().to_string();

    // 轮询：创建者看到 CREATED
    let (status, body) = call(&app, req("GET", &format!("/api/qr/tickets/{ticket_id}"), None, Some(&creator_token))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(state_of(&body), "CREATED");
    assert!(body.get("account_hint").is_none(), "没扫过不给提示: {body}");

    // 扫码：必须是登录用户；回包带设备线索
    let (status, body) = call(&app, req(
        "POST",
        &format!("/api/qr/tickets/{ticket_id}/scan"),
        Some(json!({ "secret": secret })),
        Some(&scanner_token),
    )).await;
    assert_eq!(status, StatusCode::OK, "scan: {body}");
    assert_eq!(state_of(&body), "SCANNED");

    // 轮询：SCANNED + 账号提示（谁扫的）
    let (status, body) = call(&app, req("GET", &format!("/api/qr/tickets/{ticket_id}"), None, Some(&creator_token))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(state_of(&body), "SCANNED");
    assert_eq!(body["account_hint"].as_str(), Some("qr_scanner"), "{body}");

    // 批准前兑换：拒绝（扫码 ≠ 授权）
    let (status, _) = call(&app, req(
        "POST",
        &format!("/api/qr/tickets/{ticket_id}/exchange"),
        Some(json!({ "secret": secret })),
        None,
    )).await;
    assert_eq!(status, StatusCode::CONFLICT, "scan is not approval");

    // 批准（扫码者）
    let (status, body) = call(&app, req("POST", &format!("/api/qr/tickets/{ticket_id}/approve"), None, Some(&scanner_token))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(state_of(&body), "APPROVED");

    // 兑换：匿名 + secret → 创建者账号的真实会话
    let (status, body) = call(&app, req(
        "POST",
        &format!("/api/qr/tickets/{ticket_id}/exchange"),
        Some(json!({ "secret": secret })),
        None,
    )).await;
    assert_eq!(status, StatusCode::OK, "exchange: {body}");
    assert_eq!(body["user_id"].as_str(), Some(creator.as_str()), "{body}");
    assert_eq!(body["name"].as_str(), Some("qr_creator"), "{body}");
    let token = body["token"].as_str().expect("token").to_string();

    // 兑换出的 token 必须真的能用（与 /api/login 同一条签发路径）
    let (status, body) = call(&app, req("GET", "/api/me", None, Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "exchanged token works: {body}");
    assert_eq!(body["user_id"].as_str(), Some(creator.as_str()), "{body}");

    // 票是一次性的：再兑 = 409
    let (status, _) = call(&app, req(
        "POST",
        &format!("/api/qr/tickets/{ticket_id}/exchange"),
        Some(json!({ "secret": secret })),
        None,
    )).await;
    assert_eq!(status, StatusCode::CONFLICT, "tickets are single-use");
}

#[tokio::test]
async fn expired_ticket_polls_expired_and_refuses_exchange() {
    let platform = common::platform();
    let app = app(platform.clone());
    let (_, creator_token) = signup(&app, "qr_expiry_creator").await;
    let (_, scanner_token) = signup(&app, "qr_expiry_scanner").await;

    let ticket = create_ticket(&app, &creator_token).await;
    let ticket_id = ticket["ticket_id"].as_str().unwrap().to_string();
    let secret = ticket["secret"].as_str().unwrap().to_string();

    let (status, _) = call(&app, req(
        "POST",
        &format!("/api/qr/tickets/{ticket_id}/scan"),
        Some(json!({ "secret": secret })),
        Some(&scanner_token),
    )).await;
    assert_eq!(status, StatusCode::OK);

    force_expire(&platform, &ticket_id);

    let (status, body) = call(&app, req("GET", &format!("/api/qr/tickets/{ticket_id}"), None, Some(&creator_token))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(state_of(&body), "EXPIRED");

    let (status, _) = call(&app, req(
        "POST",
        &format!("/api/qr/tickets/{ticket_id}/exchange"),
        Some(json!({ "secret": secret })),
        None,
    )).await;
    assert_eq!(status, StatusCode::GONE, "expired ticket must not exchange");
}

#[tokio::test]
async fn wrong_secret_cannot_scan_or_exchange() {
    let platform = common::platform();
    let app = app(platform.clone());
    let (_, creator_token) = signup(&app, "qr_secret_creator").await;
    let (_, scanner_token) = signup(&app, "qr_secret_scanner").await;

    let ticket = create_ticket(&app, &creator_token).await;
    let ticket_id = ticket["ticket_id"].as_str().unwrap().to_string();

    // 错 secret 扫码：404（不确认哪张票存在）
    let (status, body) = call(&app, req(
        "POST",
        &format!("/api/qr/tickets/{ticket_id}/scan"),
        Some(json!({ "secret": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef" })),
        Some(&scanner_token),
    )).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // 用真 secret 扫码 + 批准，但兑换用错 secret：404
    let (status, _) = call(&app, req(
        "POST",
        &format!("/api/qr/tickets/{ticket_id}/scan"),
        Some(json!({ "secret": ticket["secret"].as_str().unwrap() })),
        Some(&scanner_token),
    )).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, req("POST", &format!("/api/qr/tickets/{ticket_id}/approve"), None, Some(&scanner_token))).await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = call(&app, req(
        "POST",
        &format!("/api/qr/tickets/{ticket_id}/exchange"),
        Some(json!({ "secret": "not-the-secret" })),
        None,
    )).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "wrong secret must not match a ticket");
}

#[tokio::test]
async fn only_the_creator_can_poll() {
    let platform = common::platform();
    let app = app(platform.clone());
    let (_, creator_token) = signup(&app, "qr_owner_creator").await;
    let (_, other_token) = signup(&app, "qr_owner_other").await;

    let ticket = create_ticket(&app, &creator_token).await;
    let ticket_id = ticket["ticket_id"].as_str().unwrap().to_string();

    let (status, _) = call(&app, req("GET", &format!("/api/qr/tickets/{ticket_id}"), None, Some(&other_token))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "non-creator cannot poll");

    // 匿名轮询：401（Authed 拒绝）
    let (status, _) = call(&app, req("GET", &format!("/api/qr/tickets/{ticket_id}"), None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 匿名建票：401
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/qr/tickets")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn reject_ends_the_ticket() {
    let platform = common::platform();
    let app = app(platform.clone());
    let (_, creator_token) = signup(&app, "qr_reject_creator").await;
    let (_, scanner_token) = signup(&app, "qr_reject_scanner").await;

    let ticket = create_ticket(&app, &creator_token).await;
    let ticket_id = ticket["ticket_id"].as_str().unwrap().to_string();
    let secret = ticket["secret"].as_str().unwrap().to_string();

    let (status, _) = call(&app, req(
        "POST",
        &format!("/api/qr/tickets/{ticket_id}/scan"),
        Some(json!({ "secret": secret })),
        Some(&scanner_token),
    )).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, req("POST", &format!("/api/qr/tickets/{ticket_id}/reject"), None, Some(&scanner_token))).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = call(&app, req("GET", &format!("/api/qr/tickets/{ticket_id}"), None, Some(&creator_token))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(state_of(&body), "REJECTED");

    let (status, _) = call(&app, req(
        "POST",
        &format!("/api/qr/tickets/{ticket_id}/exchange"),
        Some(json!({ "secret": secret })),
        None,
    )).await;
    assert_eq!(status, StatusCode::CONFLICT, "rejected ticket must not exchange");
}
