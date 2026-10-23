//! AR-5（第四轮审计 #29）：写接口风控中间件的对抗测试。
//!
//! 覆盖三件事：
//! 1. 好友请求轰炸在窗口内撞上 429（4xx 的重复请求也计数——轰炸者用
//!    409 刷库同样是攻击面）；
//! 2. 朋友圈刷写在窗口内撞上 429；
//! 3. 正常用量（阈值内）不受影响，读接口不进闸，未认证请求不吃配额。

mod common;

use axum::http::StatusCode;
use serde_json::json;

use common::{app, call, platform, req, signup};

#[tokio::test]
async fn friend_request_bombing_hits_the_gate() {
    let state = platform();
    let app = app(state.clone());
    let (_, alice_token) = signup(&app, "alice").await;
    let (bob_id, _) = signup(&app, "bob").await;

    let mut saw_throttle = false;
    let mut processed = 0usize;
    for round in 0..40 {
        let (status, _) = call(
            &app,
            req(
                "POST",
                "/api/friends/requests",
                Some(json!({"to_user_id": bob_id})),
                Some(&alice_token),
            ),
        )
        .await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            // 默认 throttle=30：前 29 次被处理并计数，第 30 次检查命中
            assert!(round >= 29, "throttled too early: round {round}");
            saw_throttle = true;
            break;
        }
        assert!(
            status.is_success() || status.is_client_error(),
            "round {round}: {status}"
        );
        processed += 1;
    }
    assert!(
        saw_throttle,
        "bombing must hit the gate (processed {processed})"
    );
}

#[tokio::test]
async fn moment_spam_hits_the_gate() {
    let state = platform();
    let app = app(state.clone());
    let (_, token) = signup(&app, "alice").await;

    let mut saw_throttle = false;
    for round in 0..40 {
        let (status, _) = call(
            &app,
            req(
                "POST",
                "/api/moments",
                Some(json!({"text": format!("post {round}")})),
                Some(&token),
            ),
        )
        .await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            assert!(round >= 29, "throttled too early: round {round}");
            saw_throttle = true;
            break;
        }
        assert!(status.is_success() || status.is_client_error(), "{status}");
    }
    assert!(saw_throttle, "moment spam must hit the gate");
}

#[tokio::test]
async fn normal_usage_below_threshold_is_unaffected() {
    let state = platform();
    let app = app(state.clone());
    let (_, token) = signup(&app, "alice").await;

    for i in 0..5 {
        let (status, _) = call(
            &app,
            req(
                "POST",
                "/api/moments",
                Some(json!({"text": format!("normal {i}")})),
                Some(&token),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "round {i}");
    }
    // 读接口不进闸门：不会被写配额连坐
    let (status, _) = call(&app, req("GET", "/api/moments/feed", None, Some(&token))).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn unauthenticated_writes_are_not_gated() {
    let state = platform();
    let app = app(state.clone());

    // 无令牌：handler 给 401，中间件不得把它变成 429，也不得计数
    for _ in 0..35 {
        let (status, _) = call(
            &app,
            req(
                "POST",
                "/api/friends/requests",
                Some(json!({"to_user_id": "someone"})),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    // 伪造令牌同理
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"to_user_id": "someone"})),
            Some("forged"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
