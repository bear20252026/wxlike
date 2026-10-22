//! WP-01 acceptance tests: contract shapes, auth failures, concurrency and
//! adversarial input (A1 + A3 + A4). Driven through the real axum router via
//! `tower::oneshot` — no mocked transport.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt as _;
use wxlike_platform::{Platform, PlatformOptions};

fn app() -> Router {
    // low rounds keep the suite quick; the production default is asserted in
    // `production_defaults_match_contract` below.
    let opts = PlatformOptions {
        session_ttl_ms: 60_000,
        pbkdf2_rounds: 1_000,
        ..Default::default()
    };
    Platform::router(Platform::in_memory_with(opts).unwrap().shared())
}

async fn call(app: Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    // axum renders extractor rejections as plain text, so bodies are parsed
    // opportunistically rather than assumed to be JSON.
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

async fn register(app: &Router, name: &str, password: &str) -> (String, String) {
    let (status, body) = call(
        app.clone(),
        req(
            "POST",
            "/api/register",
            Some(json!({"name": name, "password": password})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register {name}: {body}");
    (
        body["user_id"].as_str().unwrap().to_string(),
        body["token"].as_str().unwrap().to_string(),
    )
}

#[tokio::test]
async fn register_returns_user_id_and_token() {
    let app = app();
    let (uid, token) = register(&app, "alice", "pw-alice").await;
    assert_eq!(uid.len(), 16, "user id is 8 random bytes hex");
    assert_eq!(token.len(), 64, "token is 32 random bytes hex");
}

#[tokio::test]
async fn duplicate_name_is_409() {
    let app = app();
    register(&app, "bob", "one").await;
    let (status, body) = call(
        app.clone(),
        req(
            "POST",
            "/api/register",
            Some(json!({"name": "bob", "password": "two"})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("name"));
}

#[tokio::test]
async fn login_accepts_right_password_and_rejects_wrong() {
    let app = app();
    let (uid, _) = register(&app, "carol", "right").await;
    let (status, body) = call(
        app.clone(),
        req(
            "POST",
            "/api/login",
            Some(json!({"name": "carol", "password": "right"})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["user_id"].as_str().unwrap(), uid);
    assert!(!body["token"].as_str().unwrap().is_empty());

    let (status, _) = call(
        app.clone(),
        req(
            "POST",
            "/api/login",
            Some(json!({"name": "carol", "password": "wrong"})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_unknown_user_is_401() {
    let app = app();
    let (status, body) = call(
        app.clone(),
        req(
            "POST",
            "/api/login",
            Some(json!({"name": "ghost", "password": "x"})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // same message as a bad password: accounts are not enumerable
    assert_eq!(body["error"].as_str().unwrap(), "invalid credentials");
}

#[tokio::test]
async fn me_requires_a_valid_bearer_token() {
    let app = app();
    let (_, token) = register(&app, "dave", "pw").await;

    let (status, _) = call(app.clone(), req("GET", "/api/me", None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "no token");

    let (status, _) = call(app.clone(), req("GET", "/api/me", None, Some("deadbeef"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "forged token");

    let (status, body) = call(app.clone(), req("GET", "/api/me", None, Some(&token))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"].as_str().unwrap(), "dave");
    assert_eq!(body["avatar"].as_str().unwrap(), "");
    assert_eq!(body["signature"].as_str().unwrap(), "");
}

#[tokio::test]
async fn profile_write_then_read() {
    let app = app();
    let (uid, token) = register(&app, "erin", "pw").await;
    let (status, _) = call(
        app.clone(),
        req(
            "PUT",
            "/api/profile",
            Some(json!({"avatar": "a.png", "signature": "hello world", "name": "Erin2"})),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, me) = call(app.clone(), req("GET", "/api/me", None, Some(&token))).await;
    assert_eq!(me["avatar"].as_str().unwrap(), "a.png");
    assert_eq!(me["signature"].as_str().unwrap(), "hello world");
    assert_eq!(me["name"].as_str().unwrap(), "Erin2");

    // partial patch leaves other fields alone
    let (status, _) = call(
        app.clone(),
        req(
            "PUT",
            "/api/profile",
            Some(json!({"signature": "later"})),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, me) = call(app.clone(), req("GET", "/api/me", None, Some(&token))).await;
    assert_eq!(me["avatar"].as_str().unwrap(), "a.png");
    assert_eq!(me["signature"].as_str().unwrap(), "later");

    // public view: name + avatar, never the signature
    let (status, pubp) = call(
        app.clone(),
        req("GET", &format!("/api/profile/{uid}"), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(pubp["name"].as_str().unwrap(), "Erin2");
    assert!(pubp.get("signature").is_none(), "signature is not public");
}

#[tokio::test]
async fn public_profile_unknown_id_is_404() {
    let app = app();
    let (status, _) = call(
        app.clone(),
        req("GET", "/api/profile/ffffffffffffffff", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // traversal-shaped id is refused, no filesystem/SQL reach
    let (status, _) = call(
        app.clone(),
        req("GET", "/api/profile/../../etc", None, None),
    )
    .await;
    assert!(status.is_client_error(), "got {status}");
}

#[tokio::test]
async fn renaming_onto_an_existing_name_is_409() {
    let app = app();
    register(&app, "frank", "pw").await;
    let (_, token) = register(&app, "gina", "pw").await;
    let (status, _) = call(
        app.clone(),
        req(
            "PUT",
            "/api/profile",
            Some(json!({"name": "frank"})),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn expired_token_is_401() {
    // session_ttl_ms = 0 => every issued session is born expired
    let platform = Platform::in_memory_with(PlatformOptions {
        session_ttl_ms: 0,
        pbkdf2_rounds: 1_000,
        ..Default::default()
    })
    .unwrap()
    .shared();
    let app = Platform::router(platform.clone());
    let (uid, stale) = register(&app, "henry", "pw").await;

    let (status, _) = call(app.clone(), req("GET", "/api/me", None, Some(&stale))).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "expired session must not auth"
    );
    assert!(
        platform.lookup_token(&stale).unwrap().is_none(),
        "expired row is garbage-collected on lookup"
    );

    // the same user can still get a live session
    let fresh = platform.create_session(&uid, 60_000).unwrap();
    let (status, _) = call(app, req("GET", "/api/me", None, Some(&fresh))).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn logout_revokes_the_session() {
    let app = app();
    let (_, token) = register(&app, "jane", "pw").await;
    let (status, _) = call(app.clone(), req("GET", "/api/me", None, Some(&token))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(app.clone(), req("POST", "/api/logout", None, Some(&token))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(app.clone(), req("GET", "/api/me", None, Some(&token))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "token dead after logout");
}

#[tokio::test]
async fn every_login_gets_a_distinct_token() {
    let app = app();
    register(&app, "kilo", "pw").await;
    let mut tokens = std::collections::HashSet::new();
    for _ in 0..8 {
        let (status, body) = call(
            app.clone(),
            req(
                "POST",
                "/api/login",
                Some(json!({"name": "kilo", "password": "pw"})),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        tokens.insert(body["token"].as_str().unwrap().to_string());
    }
    assert_eq!(tokens.len(), 8, "token uniqueness");
}

#[tokio::test]
async fn write_endpoints_reject_missing_auth() {
    let app = app();
    let (status, _) = call(
        app.clone(),
        req("PUT", "/api/profile", Some(json!({"avatar": "x"})), None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// A3: 50 concurrent registrations — no deadlock, no duplicate name.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_registers_are_serialized() {
    let platform = Platform::in_memory_with(PlatformOptions {
        session_ttl_ms: 60_000,
        pbkdf2_rounds: 500,
        ..Default::default()
    })
    .unwrap()
    .shared();
    let app = Platform::router(platform.clone());

    // 50 racing for the same name: exactly one winner
    let same = (0..50)
        .map(|_| {
            let app = app.clone();
            tokio::spawn(async move {
                let (status, _) = call(
                    app,
                    req(
                        "POST",
                        "/api/register",
                        Some(json!({"name": "racer", "password": "pw"})),
                        None,
                    ),
                )
                .await;
                status
            })
        })
        .collect::<Vec<_>>();
    let codes = futures_join(same).await;
    assert_eq!(
        codes.iter().filter(|s| **s == StatusCode::OK).count(),
        1,
        "exactly one registration wins the name"
    );
    assert!(codes
        .iter()
        .all(|s| *s == StatusCode::OK || *s == StatusCode::CONFLICT));

    // 50 distinct names: all succeed, 51 rows total
    let distinct = (0..50)
        .map(|i| {
            let app = app.clone();
            tokio::spawn(async move {
                let (status, _) = call(
                    app,
                    req(
                        "POST",
                        "/api/register",
                        Some(json!({"name": format!("u{i}"), "password": "pw"})),
                        None,
                    ),
                )
                .await;
                status
            })
        })
        .collect::<Vec<_>>();
    let codes = futures_join(distinct).await;
    assert!(
        codes.iter().all(|s| *s == StatusCode::OK),
        "no deadlock/500s"
    );

    let count: i64 = {
        let c = platform.db().lock();
        c.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(count, 51);
    let dupes: i64 = {
        let c = platform.db().lock();
        c.query_row(
            "SELECT COUNT(*) FROM (SELECT name FROM users GROUP BY name HAVING COUNT(*) > 1)",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(dupes, 0);
}

/// A4: oversized name, empty password and an injection string are all refused,
/// and the schema plus surviving rows are intact afterwards.
#[tokio::test]
async fn adversarial_inputs_are_refused_and_db_survives() {
    let app = app();
    let (_, token) = register(&app, "legit", "pw").await;

    let (status, _) = call(
        app.clone(),
        req(
            "POST",
            "/api/register",
            Some(json!({"name": "x".repeat(10_000), "password": "pw"})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "10KB name");

    let (status, _) = call(
        app.clone(),
        req(
            "POST",
            "/api/register",
            Some(json!({"name": "emptypw", "password": ""})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "empty password");

    for payload in [
        "'; DROP TABLE users;--",
        "a\" OR 1=1 --",
        "Robert'); DROP TABLE students;(",
        "name%;DELETE FROM users",
    ] {
        let (status, _) = call(
            app.clone(),
            req(
                "POST",
                "/api/register",
                Some(json!({"name": payload, "password": "pw"})),
                None,
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{payload:?} refused"
        );
    }

    // table still there, still queryable, legit session still valid
    let (status, body) = call(app.clone(), req("GET", "/api/me", None, Some(&token))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"].as_str().unwrap(), "legit");

    let (status, _) = call(
        app.clone(),
        req(
            "POST",
            "/api/register",
            Some(json!({"name": "after", "password": "pw"})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "db intact after attacks");
}

#[tokio::test]
async fn malformed_json_is_400_not_a_panic() {
    let app = app();
    let (status, _) = call(app.clone(), req("POST", "/api/register", None, None)).await;
    assert!(status.is_client_error(), "empty body: {status}");
}

#[test]
fn production_defaults_match_contract() {
    let d = PlatformOptions::default();
    // AR-3/H-3: legacy PBKDF2 floor raised to the OWASP baseline; new
    // registrations hash with argon2id outright (see users.rs::register).
    assert_eq!(
        d.pbkdf2_rounds, 600_000,
        "PBKDF2 floor >= 600k rounds (OWASP)"
    );
    assert_eq!(d.session_ttl_ms, 30 * 24 * 60 * 60 * 1000, "30 day session");
}

async fn futures_join(handles: Vec<tokio::task::JoinHandle<StatusCode>>) -> Vec<StatusCode> {
    futures_util::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.expect("task did not panic"))
        .collect()
}

/// AR-3/H-2（第四轮审计）：会话令牌不得明文入库。列里存的必须是摘要，
/// 拿走 SQLite 文件（备份/快照/容器卷）拿不到任何可用凭证。
#[test]
fn session_tokens_are_stored_as_digests() {
    let p = wxlike_platform::Platform::in_memory().unwrap();
    let uid = p.register("alice", "pw").unwrap();
    let token = p.create_session(&uid, 60_000).unwrap();
    let stored: String = {
        let c = p.db().lock();
        c.query_row("SELECT token FROM sessions LIMIT 1", [], |r| r.get(0))
            .unwrap()
    };
    assert_ne!(
        stored, token,
        "the raw bearer token must never reach the db"
    );
    assert_eq!(stored, wxlike_platform::token_digest(&token));
    assert_eq!(stored.len(), 64, "sha256 hex");
    // 摘要仍是有效的查找键：登录态照常工作
    assert_eq!(
        p.lookup_token(&token).unwrap().as_deref(),
        Some(uid.as_str())
    );
}
