//! G08 / D6：小程序域挂进组合根后的验收。
//!
//! 用 mini_mount::router 直接打（预置目录），鉴权走真平台 sessions 表。钉住：
//! 1. 匿名 401；
//! 2. 目录投影（含 origins/latest）；授权→列举→撤回往返；卸载同时撤权；
//! 3. install 与包体下载诚实 501（二进制无流式提供者）；
//! 4. 授权按用户隔离（别人读不到）。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt as _;
use serde_json::{json, Value};
use tower::ServiceExt as _;
use wxlike_platform::{Platform, PlatformOptions};
use wxlike_server::mini_mount::{self, AppState, MiniApp, MiniRelease, MiniService};

fn platform() -> wxlike_platform::PlatformState {
    Platform::in_memory_with(PlatformOptions {
        session_ttl_ms: 60_000,
        pbkdf2_rounds: 1_000,
        ..Default::default()
    })
    .unwrap()
    .shared()
}

fn sample_app() -> MiniApp {
    MiniApp {
        app_id: "wxmini-0001".to_owned(),
        name: "出行助手".to_owned(),
        icon: Some("🧭".to_owned()),
        description: None,
        origins: vec!["https://mini.example.com".to_owned()],
        releases: vec![MiniRelease {
            version: "1.2.0".to_owned(),
            manifest_digest: "aa".repeat(32),
            signature: "sig".to_owned(),
            package_url: "https://mini.example.com/pkgs/1.2.0".to_owned(),
            released_at: 1_700_000_000_000,
            revoked: false,
        }],
    }
}

/// 装配一个预置目录的 mini 路由（落盘到唯一临时目录）。
fn app_with_catalog(platform: &wxlike_platform::PlatformState) -> Router {
    let dir = std::env::temp_dir().join(format!(
        "wxlike-mini-test-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    let service = Arc::new(MiniService::open(dir.join("mini-state.json")).unwrap());
    service.upsert_app(sample_app()).unwrap();
    mini_mount::router(Arc::new(AppState {
        service,
        platform: platform.clone(),
    }))
}

fn rand_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&bytes).unwrap_or(json!({}))
    };
    (status, body)
}

fn request(method: &str, uri: &str, token: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    match body {
        Some(v) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

async fn register(app: &Router, name: &str) -> String {
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
    reply["token"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn anonymous_mini_requests_are_rejected() {
    let plat = platform();
    let app = app_with_catalog(&plat);
    // 注册需要一个能打到平台的路由；这里单独验证 mini 面匿名 401。
    for (method, uri) in [
        ("GET", "/api/miniapps"),
        ("GET", "/api/mini-grants"),
        ("POST", "/api/mini-grants"),
        ("DELETE", "/api/mini-grants/wxmini-0001"),
        ("DELETE", "/api/mini-installs/wxmini-0001"),
        ("POST", "/api/mini-installs"),
    ] {
        let (status, _) = send(&app, request(method, uri, None, None)).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {uri} 匿名必须 401"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn catalog_grant_revoke_uninstall_round_trip() {
    let plat = platform();
    // 平台注册路由 + mini 路由合并，token 来自真 sessions 表。
    let mut app = Platform::router(plat.clone());
    let mini = app_with_catalog(&plat);
    app = app.merge(mini);

    let token = register(&app, "mini-user").await;

    // 目录：含预置应用，origins/latest 投影正确。
    let (status, list) = send(&app, request("GET", "/api/miniapps", Some(&token), None)).await;
    assert_eq!(status, StatusCode::OK);
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["app_id"], json!("wxmini-0001"));
    assert_eq!(items[0]["origins"][0], json!("https://mini.example.com"));
    assert_eq!(items[0]["latest"]["version"], json!("1.2.0"));

    // releases。
    let (status, rel) = send(
        &app,
        request(
            "GET",
            "/api/miniapps/wxmini-0001/releases",
            Some(&token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rel["items"].as_array().unwrap().len(), 1);

    // 授权。
    let (status, grant) = send(
        &app,
        request(
            "POST",
            "/api/mini-grants",
            Some(&token),
            Some(json!({"app_id": "wxmini-0001", "scopes": ["profile.read", "location.read"]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{grant}");
    assert_eq!(grant["app_id"], json!("wxmini-0001"));
    assert_eq!(grant["scopes"].as_array().unwrap().len(), 2);

    // 授权列举：含刚授予的。
    let (status, grants) = send(&app, request("GET", "/api/mini-grants", Some(&token), None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(grants["items"].as_array().unwrap().len(), 1);

    // 撤回授权。
    let (status, _) = send(
        &app,
        request("DELETE", "/api/mini-grants/wxmini-0001", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, grants) = send(&app, request("GET", "/api/mini-grants", Some(&token), None)).await;
    assert_eq!(grants["items"].as_array().unwrap().len(), 0);

    // 再授权后卸载：卸载同时撤权。
    send(
        &app,
        request(
            "POST",
            "/api/mini-grants",
            Some(&token),
            Some(json!({"app_id": "wxmini-0001", "scopes": ["profile.read"]})),
        ),
    )
    .await;
    let (status, _) = send(
        &app,
        request(
            "DELETE",
            "/api/mini-installs/wxmini-0001",
            Some(&token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, grants) = send(&app, request("GET", "/api/mini-grants", Some(&token), None)).await;
    assert_eq!(
        grants["items"].as_array().unwrap().len(),
        0,
        "卸载应同时撤权"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn install_and_package_download_are_honest_501() {
    let plat = platform();
    let mut app = Platform::router(plat.clone());
    app = app.merge(app_with_catalog(&plat));
    let token = register(&app, "mini-install").await;

    let (status, body) = send(&app, request("POST", "/api/mini-installs", Some(&token), Some(json!({"app_id": "wxmini-0001", "version": "1.2.0", "manifest_digest": "aa".repeat(32)})))).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("流式核心提供者"),
        "{body}"
    );

    let (status, _) = send(
        &app,
        request(
            "GET",
            "/api/miniapps/wxmini-0001/releases/1.2.0/package",
            Some(&token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grants_are_owner_scoped() {
    let plat = platform();
    let mut app = Platform::router(plat.clone());
    app = app.merge(app_with_catalog(&plat));
    let alice = register(&app, "mini-alice").await;
    let bob = register(&app, "mini-bob").await;

    send(
        &app,
        request(
            "POST",
            "/api/mini-grants",
            Some(&alice),
            Some(json!({"app_id": "wxmini-0001", "scopes": ["profile.read"]})),
        ),
    )
    .await;

    // bob 读不到 alice 的授权。
    let (_, grants) = send(&app, request("GET", "/api/mini-grants", Some(&bob), None)).await;
    assert_eq!(grants["items"].as_array().unwrap().len(), 0);
    let (_, alice_grants) =
        send(&app, request("GET", "/api/mini-grants", Some(&alice), None)).await;
    assert_eq!(alice_grants["items"].as_array().unwrap().len(), 1);
}
