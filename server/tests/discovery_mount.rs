//! G07/第四轮审计：发现域**挂进真实组合根之后**的验收。
//!
//! 打的是 `Stack::app` —— 与监听端口上跑的是同一个 Router。域侧 30 个用例
//! （discovery crate 内）已钉死扫码/附近/摇一摇内核；这里证明的是"路由真的
//! 挂上了、鉴权真的走平台 sessions 表、隐私口径在 HTTP 面仍然成立"。
//!
//! 底线：
//! 1. 匿名/伪造 token → 401（不是 404 —— 404 说明压根没挂）；
//! 2. 位置默认关闭：未 opt-in 直接发布 → 401 UNAUTHORIZED（域内拒绝）；
//! 3. opt-in → 发布 → 附近读面只回粗网格与距离档，**没有精确坐标字段**；
//! 4. 撤回授权后立刻不可发现；拉黑双向过滤；
//! 5. 扫码：签发 → PNG → 图片识码往返；恶意 scheme 拒绝；外链白名单不配 = 拒；
//! 6. 摇一摇：单方确认停在 Pending，双边确认才 Matched。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt as _;
use serde_json::{json, Value};
use tower::ServiceExt as _;
use wxlike_platform::AccountSessions;
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

/// 真注册：走的是平台 `/api/register`，token 来自 `sessions` 表。
async fn register(app: &Router, name: &str) -> (String, String) {
    let (status, reply) = send(
        app,
        request(
            "POST",
            "/api/register",
            None,
            Some(json!({"name": name, "password": "pw"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register {name}: {reply}");
    (
        reply["user_id"].as_str().unwrap().to_string(),
        reply["token"].as_str().unwrap().to_string(),
    )
}

/// 发现域的全部受保护面（每条都抽查匿名拒绝）。
const GUARDED: &[(&str, &str)] = &[
    ("POST", "/api/v1/scan"),
    ("POST", "/api/v1/scan/confirm"),
    ("POST", "/api/v1/qr"),
    ("POST", "/api/v1/nearby/opt-in"),
    ("POST", "/api/v1/nearby/presence"),
    ("GET", "/api/v1/nearby/presence"),
    ("DELETE", "/api/v1/nearby/presence"),
    ("POST", "/api/v1/nearby"),
    ("POST", "/api/v1/nearby/block"),
    ("POST", "/api/v1/nearby/report"),
    ("POST", "/api/v1/shake/start"),
    ("POST", "/api/v1/shake/ticket-x/join"),
    ("GET", "/api/v1/shake/ticket-x"),
    ("DELETE", "/api/v1/shake/ticket-x/cancel"),
    ("POST", "/api/v1/shake/proposals/p-x/confirm"),
];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn anonymous_discovery_requests_are_rejected() {
    let stack = boot().await;
    let app = stack.app.clone();
    for (method, uri) in GUARDED {
        let (status, body) = send(&app, request(method, uri, None, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri} → {body}");
    }
    // 假 token 也不行：认证问的是库，不是"有没有 Authorization 头"。
    let (status, _) = send(
        &app,
        request(
            "GET",
            "/api/v1/nearby/presence",
            Some("obviously-not-a-session"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "伪造 bearer 必须 401");
    stack.shutdown(std::time::Duration::from_secs(2)).await;
}

/// 核心读面：opt-in → 发布 → 附近。位置默认关闭；附近只回粗网格/距离档。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nearby_read_surface_respects_opt_in_and_coarse_grid() {
    let stack = boot().await;
    let app = stack.app.clone();
    let (alice_id, alice) = register(&app, "g07-nearby-alice").await;
    let (bob_id, bob) = register(&app, "g07-nearby-bob").await;

    // 未 opt-in 直接发布 → 域内拒绝（位置默认关闭）。
    let (status, body) = send(
        &app,
        request(
            "POST",
            "/api/v1/nearby/presence",
            Some(&alice),
            Some(json!({"lat": 31.2304, "lng": 121.4737, "visibility": "everyone"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "未授权发布应被拒: {body}");
    assert_eq!(body["error"], "UNAUTHORIZED");

    // 双方显式 opt-in 后发布（同一粗网格附近）。
    for token in [&alice, &bob] {
        let (status, body) = send(
            &app,
            request(
                "POST",
                "/api/v1/nearby/opt-in",
                Some(token),
                Some(json!({"enabled": true})),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "opt-in: {body}");
    }
    let (status, body) = send(
        &app,
        request(
            "POST",
            "/api/v1/nearby/presence",
            Some(&bob),
            Some(json!({"lat": 31.2309, "lng": 121.4742, "visibility": "everyone"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "发布: {body}");
    assert!(body["cell"]["lat_idx"].is_i64(), "只回粗网格: {body}");
    assert!(
        body.get("lat").is_none() && body.get("lng").is_none(),
        "不得回精确坐标: {body}"
    );

    // alice 读附近：能看到 bob，且只有距离档，没有精确距离/坐标。
    let (status, body) = send(
        &app,
        request(
            "POST",
            "/api/v1/nearby",
            Some(&alice),
            Some(json!({"lat": 31.2304, "lng": 121.4737, "radius_m": 1000.0})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "nearby: {body}");
    let peers = body["peers"].as_array().unwrap();
    assert!(
        peers.iter().any(|p| p["user_id"] == json!(bob_id)),
        "应看到 bob: {body}"
    );
    assert!(
        !peers.iter().any(|p| p["user_id"] == json!(alice_id)),
        "不含自己"
    );
    for peer in peers {
        assert!(peer["distance_band"].is_string(), "只有距离档: {peer}");
        assert!(peer.get("distance_m").is_none(), "不得回精确距离: {peer}");
        assert!(
            peer.get("lat").is_none() && peer.get("lng").is_none(),
            "不得回精确坐标: {peer}"
        );
    }

    // 拉黑后双向不可见。
    let (status, body) = send(
        &app,
        request(
            "POST",
            "/api/v1/nearby/block",
            Some(&alice),
            Some(json!({"target": bob_id})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "block: {body}");
    let (status, body) = send(
        &app,
        request(
            "POST",
            "/api/v1/nearby",
            Some(&alice),
            Some(json!({"lat": 31.2304, "lng": 121.4737, "radius_m": 1000.0})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body["peers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["user_id"] == json!(bob_id)),
        "拉黑后不应再看到 bob: {body}"
    );

    // bob 撤回授权 → alice 的附近立刻为空（撤回即删）。
    let (status, body) = send(
        &app,
        request("DELETE", "/api/v1/nearby/presence", Some(&bob), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "revoke: {body}");
    let (status, body) = send(
        &app,
        request("GET", "/api/v1/nearby/presence", Some(&bob), None),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "撤回后不应有可发现状态: {body}"
    );

    // 吊销会话 → 同一个 token 立刻 401（鉴权只有一条路）。
    stack.platform.revoke_session(&alice).unwrap();
    let (status, _) = send(
        &app,
        request(
            "POST",
            "/api/v1/nearby",
            Some(&alice),
            Some(json!({"lat": 31.2304, "lng": 121.4737, "radius_m": 1000.0})),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "吊销后仍放行 = 有第二套鉴权"
    );
    stack.shutdown(std::time::Duration::from_secs(2)).await;
}

/// 扫码面：好友码签发 → PNG → 图片识码往返；恶意 scheme 拒绝；外链默认不放行。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scan_surface_round_trips_and_blocks_unsafe_links() {
    let stack = boot().await;
    let app = stack.app.clone();
    let (user_id, token) = register(&app, "g07-scan-user").await;

    // 签发好友码（带 PNG）。
    let (status, body) = send(
        &app,
        request(
            "POST",
            "/api/v1/qr",
            Some(&token),
            Some(json!({"target": {"kind": "friend", "user_id": user_id}, "png": true})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "签发好友码: {body}");
    let wire = body["wire"].as_str().unwrap().to_string();
    assert!(wire.starts_with("wxlike://v1/friend"), "wire 形状: {wire}");
    let png_b64 = body["png_base64"].as_str().unwrap().to_string();

    // 文本扫码：同一 wire 解出签发目标，且要求确认（不自动加好友）。
    let (status, body) = send(
        &app,
        request(
            "POST",
            "/api/v1/scan",
            Some(&token),
            Some(json!({"text": wire})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "文本扫码: {body}");
    assert_eq!(body["action"]["type"], "signed");
    assert_eq!(body["action"]["target"]["kind"], "friend");
    assert_eq!(body["action"]["target"]["user_id"], json!(user_id));
    assert_eq!(body["requires_confirmation"], json!(true));

    // 图片识码：PNG base64 → 同一目标（真实 rqrr 解码，不是回显）。
    let (status, body) = send(
        &app,
        request(
            "POST",
            "/api/v1/scan",
            Some(&token),
            Some(json!({"image_base64": png_b64})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "图片识码: {body}");
    assert_eq!(body["action"]["target"]["user_id"], json!(user_id));

    // 恶意 scheme：javascript: 一律拒绝。
    let (status, body) = send(
        &app,
        request(
            "POST",
            "/api/v1/scan",
            Some(&token),
            Some(json!({"text": "javascript:alert(1)"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "恶意 scheme: {body}");
    assert_eq!(body["error"], "BLOCKED_EXTERNAL");

    // 外链：白名单未配置 → 确认后仍不放行（诚实拒绝，不自动跳转）。
    let (status, body) = send(
        &app,
        request(
            "POST",
            "/api/v1/scan",
            Some(&token),
            Some(json!({"text": "https://example.com/x"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "外链扫码应给出待确认动作: {body}");
    assert_eq!(body["action"]["type"], "external");
    let (status, body) = send(
        &app,
        request(
            "POST",
            "/api/v1/scan/confirm",
            Some(&token),
            Some(json!({"url": "https://example.com/x"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "白名单未配应拒绝: {body}");
    assert_eq!(body["error"], "BLOCKED_EXTERNAL");
    stack.shutdown(std::time::Duration::from_secs(2)).await;
}

/// 摇一摇：有时限窗口 + 双边确认才产生匹配。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shake_requires_two_party_confirmation() {
    let stack = boot().await;
    let app = stack.app.clone();
    let (alice_id, alice) = register(&app, "g07-shake-alice").await;
    let (bob_id, bob) = register(&app, "g07-shake-bob").await;

    let (status, body) = send(
        &app,
        request(
            "POST",
            "/api/v1/shake/start",
            Some(&alice),
            Some(json!({"magnitude_g": 3.0})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "start: {body}");
    let ticket_id = body["ticket_id"].as_str().unwrap().to_string();

    // 读面：窗口可见。
    let (status, body) = send(
        &app,
        request(
            "GET",
            &format!("/api/v1/shake/{ticket_id}"),
            Some(&bob),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "ticket 读面: {body}");
    assert_eq!(body["user_id"], json!(alice_id));

    // 自己加入自己的窗口 → 域内拒绝。
    let (status, _) = send(
        &app,
        request(
            "POST",
            &format!("/api/v1/shake/{ticket_id}/join"),
            Some(&alice),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "self join 应拒绝");

    let (status, body) = send(
        &app,
        request(
            "POST",
            &format!("/api/v1/shake/{ticket_id}/join"),
            Some(&bob),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "join: {body}");
    let proposal_id = body["proposal_id"].as_str().unwrap().to_string();

    // 单方确认：停在 Pending。
    let uri = format!("/api/v1/shake/proposals/{proposal_id}/confirm");
    let (status, body) = send(&app, request("POST", &uri, Some(&alice), None)).await;
    assert_eq!(status, StatusCode::OK, "host confirm: {body}");
    assert_eq!(body["state"], "pending");
    assert_eq!(body["needs_guest"], json!(true));

    // 双边确认：Matched，且双方都在记录里。
    let (status, body) = send(&app, request("POST", &uri, Some(&bob), None)).await;
    assert_eq!(status, StatusCode::OK, "guest confirm: {body}");
    assert_eq!(body["state"], "matched");
    let users = body["users"].as_array().unwrap();
    assert!(users.contains(&json!(alice_id)) && users.contains(&json!(bob_id)));
    stack.shutdown(std::time::Duration::from_secs(2)).await;
}
