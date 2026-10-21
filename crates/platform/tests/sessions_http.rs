//! 设备管理 HTTP 面：列表、按 sid 下线、一键退出其他设备。
//!
//! 库层（`Platform::sessions_of` / `revoke_session_by_sid`）已有测试，
//! 但**接口层一行没测**——覆盖率只有 50%，缺的正是这里。
//!
//! 三条不能破的性质：
//! 1. **永不返回 token**：页面能下线设备，但拿不到能冒用的凭证；
//! 2. **下线立即失效**：删行即失效，不等过期（ADR-10 选 DB session 的理由）；
//! 3. **不泄露存在性**：别人的 sid 与不存在的 sid 返回同一个 404。

mod common;

use axum::http::StatusCode;
use serde_json::json;
use wxlike_platform::Platform;

async fn login(app: &axum::Router, name: &str) -> String {
    let (status, body) = common::call(
        app,
        common::req(
            "POST",
            "/api/login",
            Some(json!({"name": name, "password": "pw"})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["token"]
        .as_str()
        .expect("login returns token")
        .to_string()
}

async fn sessions(app: &axum::Router, token: &str) -> Vec<serde_json::Value> {
    let (status, body) =
        common::call(app, common::req("GET", "/api/sessions", None, Some(token))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["sessions"].as_array().cloned().unwrap_or_default()
}

#[tokio::test]
async fn list_never_returns_tokens_and_marks_exactly_one_current() {
    let app = Platform::router(common::platform());
    let (_uid, phone) = common::signup(&app, "alice").await;
    let desk = login(&app, "alice").await;

    let rows = sessions(&app, &phone).await;
    assert_eq!(rows.len(), 2, "两个登录 = 两个会话：{rows:?}");

    for row in &rows {
        assert!(
            row.get("token").is_none(),
            "列表**绝不能**返回 token，否则页面被 XSS 就等于凭证泄露：{row}"
        );
        assert!(row["sid"].as_str().unwrap_or_default().len() <= 32);
    }
    let current_of = |rows: &[serde_json::Value]| -> String {
        let current: Vec<_> = rows
            .iter()
            .filter(|r| r["current"].as_bool().unwrap_or(false))
            .collect();
        assert_eq!(current.len(), 1, "当前设备只能标一个：{rows:?}");
        current[0]["sid"].as_str().unwrap_or_default().to_string()
    };
    let phone_sid = current_of(&rows);

    // 换一个 token 看：current 必须跟着调用者换，而不是固定标第一个
    let rows = sessions(&app, &desk).await;
    let desk_sid = current_of(&rows);
    assert_ne!(phone_sid, desk_sid, "current 必须跟着调用者的 token 走");
}

#[tokio::test]
async fn revoking_a_session_invalidates_it_immediately() {
    let app = Platform::router(common::platform());
    let (_uid, phone) = common::signup(&app, "alice").await;
    let desk = login(&app, "alice").await;

    // 先确认桌面端是能用的
    let (status, _) = common::call(&app, common::req("GET", "/api/me", None, Some(&desk))).await;
    assert_eq!(status, StatusCode::OK);

    let sid = sessions(&app, &phone)
        .await
        .into_iter()
        .find(|r| !r["current"].as_bool().unwrap_or(true))
        .and_then(|r| r["sid"].as_str().map(str::to_string))
        .expect("桌面端有 sid");

    let (status, _) = common::call(
        &app,
        common::req(
            "POST",
            &format!("/api/sessions/{sid}/revoke"),
            None,
            Some(&phone),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // 关键性质：**立即**失效，不是等过期
    let (status, _) = common::call(&app, common::req("GET", "/api/me", None, Some(&desk))).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "被下线的 token 必须立刻失效"
    );

    // 自己还活着
    let (status, _) = common::call(&app, common::req("GET", "/api/me", None, Some(&phone))).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn revoking_an_unknown_sid_is_404_and_does_not_leak_other_users() {
    let app = Platform::router(common::platform());
    let (_alice, alice_token) = common::signup(&app, "alice").await;
    let (_bob, bob_token) = common::signup(&app, "bob").await;

    // bob 拿 alice 的 sid 去踢：必须和"不存在"同一个结果，否则 sid 变成探测面
    let alice_sid = sessions(&app, &alice_token).await[0]["sid"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, _) = common::call(
        &app,
        common::req(
            "POST",
            &format!("/api/sessions/{alice_sid}/revoke"),
            None,
            Some(&bob_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = common::call(
        &app,
        common::req(
            "POST",
            "/api/sessions/ffffffffffff/revoke",
            None,
            Some(&bob_token),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "不存在的 sid 与别人的 sid 同码"
    );

    // alice 的会话没被 bob 踢掉
    let (status, _) = common::call(
        &app,
        common::req("GET", "/api/me", None, Some(&alice_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn revoke_others_keeps_the_caller_and_counts_what_it_killed() {
    let app = Platform::router(common::platform());
    let (_uid, phone) = common::signup(&app, "alice").await;
    let tablet = login(&app, "alice").await;
    let desk = login(&app, "alice").await;
    assert_eq!(sessions(&app, &phone).await.len(), 3);

    let (status, body) = common::call(
        &app,
        common::req("POST", "/api/sessions/revoke-others", None, Some(&phone)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revoked"], 2, "只该踢掉另外两台：{body}");

    for token in [&tablet, &desk] {
        let (status, _) =
            common::call(&app, common::req("GET", "/api/me", None, Some(token))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, _) = common::call(&app, common::req("GET", "/api/me", None, Some(&phone))).await;
    assert_eq!(status, StatusCode::OK, "调用者自己不能被踢掉");
    assert_eq!(sessions(&app, &phone).await.len(), 1);
}

#[tokio::test]
async fn malformed_sid_is_rejected_before_touching_the_database() {
    let app = Platform::router(common::platform());
    let (_uid, token) = common::signup(&app, "alice").await;
    let too_long = "a".repeat(64);

    let (status, body) = common::call(
        &app,
        common::req(
            "POST",
            &format!("/api/sessions/{too_long}/revoke"),
            None,
            Some(&token),
        ),
    )
    .await;
    // `BadInput` 在本仓库统一映射成 422（语法对、内容不合法）
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
}
