//! 举报工单 HTTP 面：提交、自查、管理端队列与流转。
//!
//! 状态机本身（`wxlike_report`）和库层方法（`Platform::triage_report` 等）
//! 已有测试，但**接口层几乎没测**——覆盖率只有 55%，缺的正是这里。
//!
//! 接口层最容易出的错不是状态机算错，而是：**权限没接上**（未配令牌也放行）、
//! **越权可见**（别人的工单出现在我的列表里）、**错误码不对**（非法流转给 500
//! 而不是 409，客户端无法区分"你操作错了"和"服务炸了"）。

mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};
use wxlike_platform::{Platform, PlatformOptions};

const ADMIN_TOKEN: &str = "s3cret";

fn platform_with_admin() -> wxlike_platform::PlatformState {
    Platform::in_memory_with(PlatformOptions {
        session_ttl_ms: 60_000,
        pbkdf2_rounds: 1_000,
        admin_token: Some(ADMIN_TOKEN.into()),
        ..Default::default()
    })
    .unwrap()
    .shared()
}

/// 管理端请求：`x-admin-token` 与 `x-admin-actor` 是两条独立的头，
/// 前者是凭据、后者只用于审计留痕。
fn admin_req(method: &str, path: &str, body: Option<Value>, token: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-admin-actor", "mod-alice");
    if let Some(t) = token {
        b = b.header("x-admin-token", t);
    }
    b.body(match body {
        Some(v) => Body::from(v.to_string()),
        None => Body::empty(),
    })
    .unwrap()
}

async fn report_as(app: &axum::Router, token: &str, target: &str) -> String {
    let (status, body) = common::call(
        app,
        common::req(
            "POST",
            "/api/reports",
            Some(json!({
                "target_kind": "user",
                "target_id": target,
                "reason": "spam",
                "detail": "一直刷屏"
            })),
            Some(token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "open");
    body["ticket_id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn a_report_is_visible_only_to_its_reporter() {
    let app = Platform::router(platform_with_admin());
    let (_alice, alice) = common::signup(&app, "alice").await;
    let (bob_id, bob) = common::signup(&app, "bob").await;

    let id = report_as(&app, &alice, &bob_id).await;

    let (status, body) =
        common::call(&app, common::req("GET", "/api/reports", None, Some(&alice))).await;
    assert_eq!(status, StatusCode::OK);
    let rows = body["tickets"].as_array().cloned().unwrap_or_default();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], id.as_str());

    // bob 不是举报人，看不到这条
    let (_, body) = common::call(&app, common::req("GET", "/api/reports", None, Some(&bob))).await;
    assert!(
        body["tickets"]
            .as_array()
            .map(Vec::is_empty)
            .unwrap_or(false),
        "别人的工单不该出现在我的列表里：{body}"
    );
}

#[tokio::test]
async fn admin_queue_is_closed_without_the_right_token() {
    let app = Platform::router(platform_with_admin());
    let (_alice, alice) = common::signup(&app, "alice").await;
    let (bob_id, _bob) = common::signup(&app, "bob").await;
    report_as(&app, &alice, &bob_id).await;

    // 无令牌 / 错令牌都 403，且**不泄露**队列里有几条
    for token in [None, Some("wrong")] {
        let (status, _) =
            common::call(&app, admin_req("GET", "/api/admin/reports", None, token)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "token={token:?}");
    }

    let (status, body) = common::call(
        &app,
        admin_req(
            "GET",
            "/api/admin/reports?state=open",
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 管理端队列是 `{ queue: [{ ticket, overdue }] }`：外面多一层是为了
    // 让"是否超 SLA"跟着工单一起返回，前端不用自己算。
    let queue = body["queue"].as_array().cloned().unwrap_or_default();
    assert_eq!(queue.len(), 1, "{body}");
    assert!(queue[0]["ticket"]["id"].is_string(), "{body}");
}

#[tokio::test]
async fn lifecycle_over_http_moves_through_every_state() {
    let app = Platform::router(platform_with_admin());
    let (_alice, alice) = common::signup(&app, "alice").await;
    let (bob_id, _bob) = common::signup(&app, "bob").await;
    let id = report_as(&app, &alice, &bob_id).await;

    // 分诊
    let (status, body) = common::call(
        &app,
        admin_req(
            "POST",
            &format!("/api/admin/reports/{id}/triage"),
            Some(json!({"note": "受理"})),
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "triaged");
    assert_eq!(body["history"][0]["actor"], "mod-alice", "审计要记到具体人");

    // 处置（`Action` 是 internally-tagged 枚举，所以是嵌套的
    // `{"action": {"action": "warn"}}`——形状确实别扭，但这是现状，不是测试写错）
    let (status, body) = common::call(
        &app,
        admin_req(
            "POST",
            &format!("/api/admin/reports/{id}/act"),
            Some(json!({"action": {"action": "warn"}, "note": "警告一次"})),
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "actioned");

    // 关闭
    let (status, body) = common::call(
        &app,
        admin_req(
            "POST",
            &format!("/api/admin/reports/{id}/close"),
            Some(json!({"note": "已通知举报人"})),
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "closed");
    assert_eq!(body["history"].as_array().map(Vec::len), Some(3));

    // 终态之后不能再改判
    let (status, _) = common::call(
        &app,
        admin_req(
            "POST",
            &format!("/api/admin/reports/{id}/reject"),
            Some(json!({"note": "改判"})),
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert!(
        status.is_client_error(),
        "终态后的非法流转必须是 4xx（客户端要能区分「操作错了」与「服务炸了」），拿到 {status}"
    );

    // 队列里已经没有它了
    let (_, body) = common::call(
        &app,
        admin_req("GET", "/api/admin/reports", None, Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(body["queue"].as_array().map(Vec::len), Some(0), "{body}");
}

#[tokio::test]
async fn acting_before_triage_is_a_client_error_not_a_500() {
    let app = Platform::router(platform_with_admin());
    let (_alice, alice) = common::signup(&app, "alice").await;
    let (bob_id, _bob) = common::signup(&app, "bob").await;
    let id = report_as(&app, &alice, &bob_id).await;

    let (status, body) = common::call(
        &app,
        admin_req(
            "POST",
            &format!("/api/admin/reports/{id}/act"),
            Some(json!({"action": {"action": "ban"}, "note": "跳过分诊"})),
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert!(
        status.is_client_error(),
        "未分诊直接处置应当是 4xx，实际 {status}: {body}"
    );
}

#[tokio::test]
async fn unknown_reason_is_422() {
    let app = Platform::router(platform_with_admin());
    let (_alice, alice) = common::signup(&app, "alice").await;
    let (bob_id, _bob) = common::signup(&app, "bob").await;

    let (status, body) = common::call(
        &app,
        common::req(
            "POST",
            "/api/reports",
            Some(json!({
                "target_kind": "user",
                "target_id": bob_id,
                "reason": "i-just-dont-like-it",
                "detail": ""
            })),
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
}
