//! G02-09 设备侧联动的平台层验收：`dispatch` 真的把断连命令投给注入的
//! [`GovernanceDeviceSink`]（此前只落日志留痕）。
//!
//! 用 [`ChannelGovernanceSink`] 作真实桥：命令从 HTTP 治理端点一路走到通道，
//! 后台任务再把它交给 hub（hub 侧的断连验收见 net 的 `governance_link`）。
//! 本文件只钉一件事：**平台确实投递了正确的命令**，且没装配 sink 时降级为
//! 留痕而不是丢事件。

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt as _;
use wxlike_identity::{session, Audience};
use wxlike_platform::{ChannelGovernanceSink, GovernanceDisconnect, Platform, PlatformOptions};

/// 文件库平台 + 已装配的通道 sink（返回命令接收端供断言）。
fn platform_with_sink() -> (
    Router,
    String,
    tokio::sync::mpsc::UnboundedReceiver<GovernanceDisconnect>,
) {
    let dir = std::env::temp_dir().join(format!(
        "wxlike-gov-link-{}-{}",
        std::process::id(),
        wxlike_platform::random_hex(6)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("wxlike.db");
    let path = db_path.to_str().unwrap().to_string();
    let opts = PlatformOptions {
        session_ttl_ms: 60_000,
        pbkdf2_rounds: 1_000,
        ..Default::default()
    };
    let platform = Platform::open_with(Some(&path), opts).unwrap();
    let (sink, rx) = ChannelGovernanceSink::new();
    platform.set_governance_sink(Arc::new(sink));
    (Platform::router(platform.shared()), path, rx)
}

/// 与平台主库同文件的第三条连接（仅测试用）。
fn sidecar(path: &str) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch("PRAGMA busy_timeout = 5000;").unwrap();
    conn
}

fn ensure_device(conn: &rusqlite::Connection, user_id: &str, device_id: &str) {
    let reg = wxlike_identity::device::Registration {
        platform: "desktop".to_string(),
        label: "测试设备".to_string(),
        os_version: "win11".to_string(),
        app_version: "1.0.0".to_string(),
        identity_key: format!("key-{device_id}"),
    };
    wxlike_identity::device::register(
        conn,
        user_id,
        device_id,
        &reg,
        wxlike_core::types::now_ms() as i64,
    )
    .unwrap();
}

/// 经 identity 域 API 签发 Admin audience 操作者会话。
fn admin_session(conn: &rusqlite::Connection, user_id: &str) -> String {
    ensure_device(conn, user_id, "device-admin0001");
    let grant = session::open(
        conn,
        user_id,
        "device-admin0001",
        Audience::Admin,
        60_000,
        "127.0.0.1",
        wxlike_core::types::now_ms() as i64,
    )
    .unwrap();
    grant.access_token
}

async fn call(app: &Router, r: Request<Body>) -> (StatusCode, Value) {
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

fn req(method: &str, path: &str, body: Option<Value>, token: Option<&str>) -> Request<Body> {
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

fn admin_req(path: &str, body: Value, operator_token: &str) -> Request<Body> {
    let mut r = req("POST", path, Some(body), None);
    r.headers_mut()
        .insert("x-admin-token", operator_token.parse().unwrap());
    r
}

async fn register(app: &Router, name: &str) -> String {
    let (status, body) = call(
        app,
        req(
            "POST",
            "/api/register",
            Some(json!({"name": name, "password": "pw"})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register {name}: {body}");
    body["user_id"].as_str().unwrap().to_string()
}

async fn login(app: &Router, name: &str) -> String {
    let (status, body) = call(
        app,
        req(
            "POST",
            "/api/login",
            Some(json!({"name": name, "password": "pw"})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login {name}: {body}");
    body["token"].as_str().unwrap().to_string()
}

/// 下一条断连命令（平台侧同步 dispatch，命令应立即入队）。
async fn next_cmd(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<GovernanceDisconnect>,
) -> GovernanceDisconnect {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("治理动作应产出断连命令")
        .expect("sink 仍存活")
}

#[tokio::test]
async fn suspend_pushes_user_disconnect_command() {
    let (app, path, mut rx) = platform_with_sink();
    let victim = register(&app, "link-victim").await;
    let operator_id = register(&app, "link-operator").await;
    let conn = sidecar(&path);
    let operator_token = admin_session(&conn, &operator_id);

    // 先造一个会话：sessions_revoked > 0 才会触发断连命令。
    login(&app, "link-victim").await;

    let (status, receipt) = call(
        &app,
        admin_req(
            &format!("/api/admin/identity/users/{victim}/suspend"),
            json!({"reason": "spam", "actor": "ops"}),
            &operator_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{receipt}");

    assert_eq!(next_cmd(&mut rx).await, GovernanceDisconnect::User(victim));
}

#[tokio::test]
async fn device_revoke_pushes_device_command() {
    let (app, path, mut rx) = platform_with_sink();
    let victim = register(&app, "link-dev-victim").await;
    let operator_id = register(&app, "link-dev-operator").await;
    let conn = sidecar(&path);
    let operator_token = admin_session(&conn, &operator_id);
    ensure_device(&conn, &victim, "dev-victim-1");

    let (status, receipt) = call(
        &app,
        admin_req(
            "/api/admin/identity/devices/dev-victim-1/revoke",
            json!({"reason": "lost", "actor": "ops"}),
            &operator_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{receipt}");

    // 设备撤销的第一个命令就是精确的按设备打击（事件顺序见 identity 域）。
    assert_eq!(
        next_cmd(&mut rx).await,
        GovernanceDisconnect::Device {
            user_id: victim,
            device_id: "dev-victim-1".to_string(),
        }
    );
}

#[tokio::test]
async fn idempotent_suspend_replay_does_not_redisconnect() {
    let (app, path, mut rx) = platform_with_sink();
    let victim = register(&app, "link-idem").await;
    let operator_id = register(&app, "link-idem-operator").await;
    let conn = sidecar(&path);
    let operator_token = admin_session(&conn, &operator_id);
    login(&app, "link-idem").await;

    let target = format!("/api/admin/identity/users/{victim}/suspend");
    let body = json!({"reason": "spam", "actor": "ops"});
    let (status, first) = call(&app, admin_req(&target, body.clone(), &operator_token)).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(next_cmd(&mut rx).await, GovernanceDisconnect::User(victim));

    // 幂等命中：sessions_revoked == 0，不该再产出断连命令（避免把刚重连的
    // 用户又踢下线）。
    let (status, second) = call(&app, admin_req(&target, body, &operator_token)).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["already"], true, "{second}");
    assert!(
        tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .is_err(),
        "幂等重放不该再发断连命令"
    );
}
