//! WP-01 A2 demo: register -> login -> update profile -> read profile, driven
//! through the real axum router. Run with:
//!
//! ```text
//! cargo run -p wxlike-platform --example wp01_demo
//! ```

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt as _;
use wxlike_platform::{Platform, PlatformOptions};

async fn call(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.expect("router never errors");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("infallible body")
        .to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("handler returns JSON")
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
    .expect("valid request")
}

fn main() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        // 1_000 rounds keeps the demo under a second; the shipped default is 100_000.
        let platform = Platform::in_memory_with(PlatformOptions {
            session_ttl_ms: 60 * 60 * 1000,
            pbkdf2_rounds: 1_000,
        })?
        .shared();
        let app = Platform::router(platform);

        let (status, body) = call(
            &app,
            req("POST", "/api/register", Some(json!({"name": "alice", "password": "hunter2"})), None),
        )
        .await;
        println!("1) POST /api/register   -> {status}");
        println!("   {body}");
        let reg_token = body["token"].as_str().expect("register returns a token").to_string();

        let (status, body) = call(
            &app,
            req("POST", "/api/login", Some(json!({"name": "alice", "password": "hunter2"})), None),
        )
        .await;
        println!("2) POST /api/login      -> {status}");
        println!("   {body}");
        let token = body["token"].as_str().expect("login returns a token").to_string();
        assert_ne!(token, reg_token, "each login mints a distinct session token");

        let (status, body) = call(
            &app,
            req(
                "PUT",
                "/api/profile",
                Some(json!({"avatar": "alice.png", "signature": "rust && quic"})),
                Some(&token),
            ),
        )
        .await;
        println!("3) PUT  /api/profile    -> {status}");
        println!("   {body}");

        let (status, body) = call(&app, req("GET", "/api/me", None, Some(&token))).await;
        println!("4) GET  /api/me         -> {status}");
        println!("   {body}");

        let (status, _) = call(&app, req("GET", "/api/me", None, None)).await;
        println!("5) GET  /api/me (no Authorization)   -> {status} (expected 401)");

        let (status, _) = call(&app, req("GET", "/api/me", None, Some("00112233445566778899"))).await;
        println!("5a) GET /api/me (forged token)       -> {status} (expected 401)");

        let (status, _) = call(&app, req("GET", "/api/me", None, Some(&reg_token))).await;
        println!("5b) GET /api/me (register's token)   -> {status} (expected 200: separate live session)");

        let (status, _) = call(&app, req("POST", "/api/logout", None, Some(&token))).await;
        println!("6) POST /api/logout                  -> {status} (expected 204)");

        let (status, _) = call(&app, req("GET", "/api/me", None, Some(&token))).await;
        println!("7) GET  /api/me (after logout)       -> {status} (expected 401)");
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}
