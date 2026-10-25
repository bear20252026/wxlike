//! G02-01 HTTP 面：刷新轮换/重放检测、step-up 门禁、按设备下线、缓存清理事件。
//!
//! 规则实现不在这个 crate（`crates/features/identity`），这里验的是**接线**：
//! 端点存在、状态码对、撤销真的让凭证失效、事件真的发出去。

mod common;

use axum::http::StatusCode;
use axum::Router;
use serde_json::json;

use common::{app, call, platform, req, signup, Recorder};

async fn login(app: &Router, name: &str, device_id: &str) -> (String, String, String) {
    // 先注册（已存在就算了），再带设备信息登录——设备维度的会话要这条路径。
    let _ = call(
        app,
        req(
            "POST",
            "/api/register",
            Some(json!({ "name": name, "password": "pw" })),
            None,
        ),
    )
    .await;
    let (status, body) = call(
        app,
        req(
            "POST",
            "/api/login",
            Some(json!({
                "name": name,
                "password": "pw",
                "device_id": device_id,
                "device_name": "test-device",
            })),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login: {body}");
    let token = body["token"].as_str().unwrap().to_string();
    let refresh = body["refresh_token"]
        .as_str()
        .unwrap_or_else(|| panic!("login 必须下发 refresh_token: {body}"))
        .to_string();
    (body["user_id"].as_str().unwrap().to_string(), token, refresh)
}

async fn refresh(app: &Router, raw: &str) -> (StatusCode, serde_json::Value) {
    call(
        app,
        req(
            "POST",
            "/api/sessions/refresh",
            Some(json!({ "refresh_token": raw })),
            None,
        ),
    )
    .await
}

async fn me_status(app: &Router, token: &str) -> StatusCode {
    call(app, req("GET", "/api/me", None, Some(token))).await.0
}

#[tokio::test]
async fn refresh_rotates_and_retires_the_old_access_token() {
    let state = platform();
    let app = app(state);
    let (_uid, token, refresh_token) = login(&app, "alice", "dev-a").await;

    let (status, body) = refresh(&app, &refresh_token).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let fresh = body["token"].as_str().unwrap().to_string();
    let next_refresh = body["refresh_token"].as_str().unwrap().to_string();
    assert_ne!(fresh, token, "刷新必须换一枚新的访问令牌");
    assert_ne!(next_refresh, refresh_token, "刷新必须轮换刷新令牌");

    assert_eq!(me_status(&app, &fresh).await, StatusCode::OK);
    assert_eq!(
        me_status(&app, &token).await,
        StatusCode::UNAUTHORIZED,
        "旧访问令牌应随轮换失效"
    );
}

#[tokio::test]
async fn replayed_refresh_token_kills_the_whole_chain() {
    let state = platform();
    let app = app(state);
    let (_uid, _token, first) = login(&app, "bob", "dev-b").await;

    let (status, body) = refresh(&app, &first).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let second = body["refresh_token"].as_str().unwrap().to_string();

    // 重放：旧令牌第二次出现 → 整条链作废
    let (status, _) = refresh(&app, &first).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 链已废，连"合法的下一枚"也不能再用
    let (status, body) = refresh(&app, &second).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "重放后链应整体失效: {body}");
}

#[tokio::test]
async fn unknown_and_empty_refresh_tokens_are_rejected() {
    let state = platform();
    let app = app(state);
    let (status, _) = refresh(&app, "deadbeef").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = refresh(&app, "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn revoking_a_session_also_kills_its_refresh_chain() {
    let state = platform();
    let app = app(state);
    let (_uid, token, refresh_token) = login(&app, "carol", "dev-c").await;

    let (status, body) = call(&app, req("GET", "/api/sessions", None, Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let sid = body["sessions"][0]["sid"].as_str().unwrap().to_string();

    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/sessions/{sid}/revoke"),
            None,
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = refresh(&app, &refresh_token).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "会话撤销后刷新令牌不能把会话续回来"
    );
}

#[tokio::test]
async fn device_revoke_requires_step_up() {
    let state = platform();
    let app = app(state);
    let (_uid, token, _refresh) = login(&app, "dave", "dev-d").await;

    // 没有 step-up：拒绝
    let (status, _) = call(
        &app,
        req("POST", "/api/devices/dev-d/revoke", None, Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 空凭据：400，而不是"当成通过"
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/sessions/step-up",
            Some(json!({})),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 错口令：401
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/sessions/step-up",
            Some(json!({ "password": "nope" })),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 对口令：发通行证
    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/sessions/step-up",
            Some(json!({ "password": "pw" })),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["purpose"], "device.revoke");

    let (status, body) = call(
        &app,
        req("POST", "/api/devices/dev-d/revoke", None, Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revoked"], 1);

    // 设备下线后，它自己的令牌立刻失效
    assert_eq!(me_status(&app, &token).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn step_up_grant_is_single_use() {
    let state = platform();
    let app = app(state);
    let (_uid, token, _refresh) = login(&app, "erin", "dev-e").await;

    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/sessions/step-up",
            Some(json!({ "password": "pw" })),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = call(
        &app,
        req("POST", "/api/devices/dev-x/revoke", None, Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 第二台设备要重新验证：授权已被消费
    let (status, _) = call(
        &app,
        req("POST", "/api/devices/dev-y/revoke", None, Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn device_revoke_emits_a_cache_clear_event() {
    let state = platform();
    let recorder = Recorder::new();
    state.set_event_sink(std::sync::Arc::new(recorder.clone()));
    let app = app(state);
    let (uid, token, _refresh) = login(&app, "frank", "dev-f").await;

    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/sessions/step-up",
            Some(json!({ "password": "pw" })),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &app,
        req("POST", "/api/devices/dev-f/revoke", None, Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let events = recorder.take();
    let hit = events
        .iter()
        .find(|env| matches!(&env.body, wxlike_core::MsgBody::Custom { kind, .. } if kind == "session.revoked"))
        .unwrap_or_else(|| panic!("没有发出 session.revoked 事件: {}", events.len()));
    assert!(hit.to.contains(&uid), "事件应发给被撤销的用户");
}

#[tokio::test]
async fn revoke_others_keeps_the_caller_alive() {
    let state = platform();
    let app = app(state);
    let (uid, first, _r1) = login(&app, "gina", "dev-g1").await;
    // 同一账号第二台设备
    let (_uid2, second, _r2) = login(&app, "gina", "dev-g2").await;
    assert_eq!(uid, _uid2);

    let (status, body) = call(
        &app,
        req("POST", "/api/sessions/revoke-others", None, Some(&first)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 注册本身也会建一条会话，所以被踢掉的是"除我之外的全部"
    assert!(body["revoked"].as_u64().unwrap() >= 1, "{body}");

    assert_eq!(me_status(&app, &first).await, StatusCode::OK);
    assert_eq!(me_status(&app, &second).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn register_also_mints_a_refresh_token() {
    let state = platform();
    let app = app(state);
    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/register",
            Some(json!({"name": "henry", "password": "pw"})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let raw = body["refresh_token"]
        .as_str()
        .expect("注册也要能刷新，否则 60 秒后就得重新输密码");
    let (status, body) = refresh(&app, raw).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn empty_state_has_no_sessions() {
    let state = platform();
    let app = app(state);
    let (_uid, token) = signup(&app, "ivy").await;
    // signup 建了会话；撤掉后列表应为空
    let (status, body) = call(&app, req("GET", "/api/sessions", None, Some(&token))).await;
    assert_eq!(status, StatusCode::OK);
    let sid = body["sessions"][0]["sid"].as_str().unwrap().to_string();
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/sessions/{sid}/revoke"),
            None,
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&app, req("GET", "/api/sessions", None, Some(&token))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "会话撤销后不能再列会话");
}
