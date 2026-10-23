//! AR-5（第四轮审计 #29）：写接口风控中间件的对抗测试。
//!
//! 覆盖三件事：
//! 1. 好友请求轰炸在窗口内撞上 429（4xx 的重复请求也计数——轰炸者用
//!    409 刷库同样是攻击面）；
//! 2. 朋友圈刷写在窗口内撞上 429；
//! 3. 正常用量（阈值内）不受影响，读接口不进闸，未认证请求不吃配额。
//!
//! T-B2 把同样的两件套补到群管理、社区发帖、开播、通话信令四个面。

mod common;

use axum::http::StatusCode;
use serde_json::json;

use common::{app, call, platform, req, signup};

/// 默认阈值（与 `RiskConfig::default()` 对应）。第 `threshold` 次请求
/// （1 起）被闸门拦住，即 0 起的 `round == threshold - 1`。
const WRITE_THROTTLE_AT: usize = 30;
const LIVE_THROTTLE_AT: usize = 10;
const SIGNAL_THROTTLE_AT: usize = 100;

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

// ---------------------------------------------------------------------------
// T-B2：群管理 / 社区发帖 / 开播 / 通话信令
// ---------------------------------------------------------------------------

async fn make_group(app: &axum::Router, token: &str) -> String {
    let (status, body) = call(
        app,
        req(
            "POST",
            "/api/groups",
            Some(json!({"name": "测试群"})),
            Some(token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["group_id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn group_creation_bombing_hits_the_gate() {
    let state = platform();
    let app = app(state.clone());
    let (_, token) = signup(&app, "alice").await;

    let mut saw_throttle = false;
    for round in 0..40 {
        let (status, _) = call(
            &app,
            req(
                "POST",
                "/api/groups",
                Some(json!({"name": format!("群 {round}")})),
                Some(&token),
            ),
        )
        .await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            assert!(
                round >= WRITE_THROTTLE_AT - 1,
                "throttled too early: round {round}"
            );
            saw_throttle = true;
            break;
        }
        assert!(status.is_success(), "round {round}: {status}");
    }
    assert!(saw_throttle, "group flooding must hit the gate");
}

#[tokio::test]
async fn normal_group_admin_usage_is_unaffected() {
    let state = platform();
    let app = app(state.clone());
    let (_, owner) = signup(&app, "owner").await;
    let (bob, _) = signup(&app, "bob").await;

    let group = make_group(&app, &owner).await;

    // 拉人
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/groups/{group}/members"),
            Some(json!({"user_ids": [bob]})),
            Some(&owner),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 踢人
    let (status, _) = call(
        &app,
        req(
            "DELETE",
            &format!("/api/groups/{group}/members/{bob}"),
            None,
            Some(&owner),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // 读接口不进闸
    let (status, _) = call(
        &app,
        req("GET", &format!("/api/groups/{group}"), None, Some(&owner)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

async fn make_live_channel(app: &axum::Router, token: &str) -> String {
    let (status, body) = call(
        app,
        req(
            "POST",
            "/api/communities",
            Some(json!({"name": "直播社"})),
            Some(token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let community = body["community_id"].as_str().unwrap().to_string();
    let (status, body) = call(
        app,
        req(
            "POST",
            &format!("/api/communities/{community}/channels"),
            Some(json!({"name": "主舞台", "kind": "live"})),
            Some(token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["channel_id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn community_posting_bombing_hits_the_gate() {
    let state = platform();
    let app = app(state.clone());
    let (_, token) = signup(&app, "alice").await;

    let mut saw_throttle = false;
    for round in 0..40 {
        let (status, _) = call(
            &app,
            req(
                "POST",
                "/api/communities",
                Some(json!({"name": format!("社 {round}")})),
                Some(&token),
            ),
        )
        .await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            assert!(
                round >= WRITE_THROTTLE_AT - 1,
                "throttled too early: round {round}"
            );
            saw_throttle = true;
            break;
        }
        assert!(status.is_success(), "round {round}: {status}");
    }
    assert!(saw_throttle, "community posting flood must hit the gate");
}

#[tokio::test]
async fn normal_community_posting_is_unaffected() {
    let state = platform();
    let app = app(state.clone());
    let (_, token) = signup(&app, "alice").await;

    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/communities",
            Some(json!({"name": "正常社区"})),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let community = body["community_id"].as_str().unwrap().to_string();

    let (status, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/communities/{community}/channels"),
            Some(json!({"name": "闲聊", "kind": "text"})),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let channel = body["channel_id"].as_str().unwrap().to_string();

    // 频道聊天消息**不在**发帖闸门里（正常聊天不受配额连坐）
    let (status, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/channels/{channel}/messages"),
            Some(json!({"text": "今天聊点啥"})),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let root_seq = body["seq"].as_u64().unwrap();

    // 建话题属于发帖面
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/channels/{channel}/threads"),
            Some(json!({"root_seq": root_seq, "title": "展开聊聊"})),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _) = call(
        &app,
        req(
            "GET",
            &format!("/api/communities/{community}/channels"),
            None,
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn live_start_bombing_hits_the_gate() {
    let state = platform();
    let app = app(state.clone());
    let (_, token) = signup(&app, "host").await;
    let channel = make_live_channel(&app, &token).await;

    // 首次 200，之后同频道重复开播是 409——**4xx 也计数**，这正是反复
    // 硬闯的脚本会用 409 刷库的攻击面。
    let mut saw_throttle = false;
    for round in 0..40 {
        let (status, _) = call(
            &app,
            req(
                "POST",
                &format!("/api/live/{channel}/start"),
                Some(json!({"recording": false})),
                Some(&token),
            ),
        )
        .await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            assert!(
                round >= LIVE_THROTTLE_AT - 1,
                "throttled too early: round {round}"
            );
            saw_throttle = true;
            break;
        }
        assert!(
            status.is_success() || status == StatusCode::CONFLICT,
            "round {round}: {status}"
        );
    }
    assert!(saw_throttle, "live start flooding must hit the gate");
}

#[tokio::test]
async fn normal_live_start_is_unaffected() {
    let state = platform();
    let app = app(state.clone());
    let (_, token) = signup(&app, "host").await;
    let channel = make_live_channel(&app, &token).await;

    let (status, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/live/{channel}/start"),
            Some(json!({"recording": false})),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["session"]["state"], "live");

    // 读状态不占开播配额
    let (status, body) = call(
        &app,
        req("GET", &format!("/api/live/{channel}"), None, Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["live"], true);

    // 停播是低频控制动作，不在开播闸门里
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/live/{channel}/stop"),
            None,
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

/// 建直聊 + 开房 + 对方进房；返回 (conv, alice_id, bob_id, alice_token, bob_token)。
async fn call_room(app: &axum::Router) -> (String, String, String, String, String) {
    let (alice, alice_tok) = signup(app, "alice").await;
    let (bob, bob_tok) = signup(app, "bob").await;
    let conv = format!("d-{alice}~{bob}");
    let (status, _) = call(
        app,
        req(
            "POST",
            &format!("/api/call/{conv}/start"),
            None,
            Some(&alice_tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        app,
        req(
            "POST",
            &format!("/api/call/{conv}/join"),
            None,
            Some(&bob_tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    (conv, alice, bob, alice_tok, bob_tok)
}

fn signal_body(from: &str, to: &str) -> serde_json::Value {
    json!({
        "v": 1,
        "call_id": "call-1",
        "from": from,
        "to": to,
        "content": {
            "type": "candidate",
            "sdp_mid": "0",
            "sdp_mline_index": 0,
            "candidate": "candidate:1 1 UDP 1 127.0.0.1 9 typ host"
        }
    })
}

#[tokio::test]
async fn call_signal_bombing_hits_the_gate() {
    let state = platform();
    let app = app(state.clone());
    let (conv, alice, bob, alice_tok, _) = call_room(&app).await;

    let mut saw_throttle = false;
    for round in 0..140 {
        let (status, _) = call(
            &app,
            req(
                "POST",
                &format!("/api/call/{conv}/signal"),
                Some(signal_body(&alice, &bob)),
                Some(&alice_tok),
            ),
        )
        .await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            assert!(
                round >= SIGNAL_THROTTLE_AT - 1,
                "throttled too early: round {round}"
            );
            saw_throttle = true;
            break;
        }
        assert!(status.is_success(), "round {round}: {status}");
    }
    assert!(saw_throttle, "signal flooding must hit the gate");
}

#[tokio::test]
async fn normal_call_signaling_is_unaffected() {
    let state = platform();
    let app = app(state.clone());
    let (conv, alice, bob, alice_tok, bob_tok) = call_room(&app).await;

    // 两条信令远低于 10 s 窗口的阈值
    for _ in 0..2 {
        let (status, _) = call(
            &app,
            req(
                "POST",
                &format!("/api/call/{conv}/signal"),
                Some(signal_body(&alice, &bob)),
                Some(&alice_tok),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    // 读信令不进闸，且能取到刚发的两条
    let (status, body) = call(
        &app,
        req(
            "GET",
            &format!("/api/call/{conv}/signal"),
            None,
            Some(&bob_tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["signals"].as_array().unwrap().len(), 2);
}
