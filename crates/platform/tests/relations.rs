//! P3 relation enhancements (contract §3.2 / ADR-24): blocklist gates,
//! the stranger-message request inbox (invariant 9), and friend metadata
//! (starred / tags / group).

mod common;

use axum::http::StatusCode;
use common::{app, call, platform, req, signup};
use serde_json::json;

#[tokio::test]
async fn blocklist_gates_friend_requests_and_roundtrips() {
    let p = platform();
    let app = app(p.clone());
    let (alice, alice_tok) = signup(&app, "alice").await;
    let (bob, bob_tok) = signup(&app, "bob").await;
    let (carol, carol_tok) = signup(&app, "carol").await;

    // alice blocks bob
    let (st, _) = call(
        &app,
        req("PUT", &format!("/api/blocks/{bob}"), None, Some(&alice_tok)),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);

    // blocked user's friend request is refused before any state lands
    let (st, body) = call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"to_user_id": alice, "message": "hi"})),
            Some(&bob_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");

    // incoming requests list of alice stays empty (no dirty state)
    let (st, body) = call(
        &app,
        req(
            "GET",
            "/api/friends/requests/incoming",
            None,
            Some(&alice_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body, json!([]), "no dirty request state (bare array)");

    // list + unblock roundtrip
    let (st, body) = call(&app, req("GET", "/api/blocks", None, Some(&alice_tok))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["blocked"], json!([bob]));

    let (st, _) = call(
        &app,
        req(
            "DELETE",
            &format!("/api/blocks/{bob}"),
            None,
            Some(&alice_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);

    // now bob's request goes through normally
    let (st, _) = call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"to_user_id": alice, "message": "again"})),
            Some(&bob_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "unblocked request succeeds");
    let _ = carol;
    let _ = carol_tok;
}

#[tokio::test]
async fn stranger_message_lands_in_inbox_and_accept_grants_direct_send() {
    let p = platform();
    let app = app(p.clone());
    let (alice, alice_tok) = signup(&app, "alice").await;
    let (mallory, mallory_tok) = signup(&app, "mallory").await;

    // 非好友首条消息：进请求箱（不变式 9），不产生会话/好友关系
    let (st, body) = call(
        &app,
        req(
            "POST",
            "/api/stranger/messages",
            Some(json!({"to_uid": alice, "text": "在吗？"})),
            Some(&mallory_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "queued");
    assert!(!p.are_friends(&mallory, &alice).unwrap());

    // alice's inbox shows it; accepting flips the state
    let (st, body) = call(
        &app,
        req("GET", "/api/stranger/requests", None, Some(&alice_tok)),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let requests = body["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["from_uid"], mallory);
    assert_eq!(requests[0]["text"], "在吗？");
    let req_id = requests[0]["id"].as_str().unwrap().to_string();

    let (st, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/stranger/requests/{req_id}/accept"),
            None,
            Some(&alice_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);

    // 接受→正常会话：accepted 许可成立（hub 发送端据此放行直发）
    assert!(p.stranger_accepted(&mallory, &alice).unwrap());
    assert!(!p.stranger_accepted(&alice, &mallory).unwrap(), "单向许可");

    // outbox shows the accepted state
    let (st, body) = call(
        &app,
        req(
            "GET",
            "/api/stranger/requests?dir=out",
            None,
            Some(&mallory_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["requests"][0]["status"], "accepted");
}

#[tokio::test]
async fn stranger_flow_honors_the_user_switch() {
    let p = platform();
    let app = app(p.clone());
    let (alice, alice_tok) = signup(&app, "alice").await;
    let (_, eve_tok) = signup(&app, "eve").await;

    // default allows strangers
    let (st, body) = call(
        &app,
        req("GET", "/api/settings/strangers", None, Some(&alice_tok)),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["allow_strangers"], json!(true));

    // alice turns strangers off: eve's message is refused outright
    let (st, _) = call(
        &app,
        req(
            "PUT",
            "/api/settings/strangers",
            Some(json!({"allow": false})),
            Some(&alice_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);

    let (st, _) = call(
        &app,
        req(
            "POST",
            "/api/stranger/messages",
            Some(json!({"to_uid": alice, "text": "打扰了"})),
            Some(&eve_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "switch off -> refused");

    // and nothing entered the inbox (流程存在但被配置拦截)
    let (st, body) = call(
        &app,
        req("GET", "/api/stranger/requests", None, Some(&alice_tok)),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["requests"], json!([]));
}

#[tokio::test]
async fn decline_closes_only_that_request() {
    let p = platform();
    let app = app(p.clone());
    let (alice, alice_tok) = signup(&app, "alice").await;
    let (zoe, zoe_tok) = signup(&app, "zoe").await;

    call(
        &app,
        req(
            "POST",
            "/api/stranger/messages",
            Some(json!({"to_uid": alice, "text": "hi"})),
            Some(&zoe_tok),
        ),
    )
    .await;
    let (_, body) = call(
        &app,
        req("GET", "/api/stranger/requests", None, Some(&alice_tok)),
    )
    .await;
    let req_id = body["requests"][0]["id"].as_str().unwrap().to_string();

    let (st, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/stranger/requests/{req_id}/decline"),
            None,
            Some(&alice_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);

    // declined: no direct-send permission, and re-request creates a fresh
    // pending row (decline != block)
    assert!(!p.stranger_accepted(&zoe, &alice).unwrap());
    let (st, _) = call(
        &app,
        req(
            "POST",
            "/api/stranger/messages",
            Some(json!({"to_uid": alice, "text": "再问一次"})),
            Some(&zoe_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, body) = call(
        &app,
        req("GET", "/api/stranger/requests", None, Some(&alice_tok)),
    )
    .await;
    assert_eq!(
        body["requests"].as_array().unwrap().len(),
        1,
        "fresh request"
    );
}

#[tokio::test]
async fn blocked_users_cannot_stranger_message_either() {
    let p = platform();
    let app = app(p.clone());
    let (alice, alice_tok) = signup(&app, "alice").await;
    let (villain, villain_tok) = signup(&app, "villain").await;

    call(
        &app,
        req(
            "PUT",
            &format!("/api/blocks/{villain}"),
            None,
            Some(&alice_tok),
        ),
    )
    .await;
    let (st, _) = call(
        &app,
        req(
            "POST",
            "/api/stranger/messages",
            Some(json!({"to_uid": alice, "text": "骚扰"})),
            Some(&villain_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "blocklist outranks the switch");
}

#[tokio::test]
async fn friend_metadata_star_tags_group_roundtrip() {
    let p = platform();
    let _ = p;
    let app = app(p.clone());
    let (alice, alice_tok) = signup(&app, "alice").await;
    let (bob, bob_tok) = signup(&app, "bob").await;

    // real friend flow: alice requests, bob accepts
    let (st, _) = call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"to_user_id": bob, "message": ""})),
            Some(&alice_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let (_, body) = call(
        &app,
        req(
            "GET",
            "/api/friends/requests/incoming",
            None,
            Some(&bob_tok),
        ),
    )
    .await;
    let req_id = body[0]["id"].as_str().unwrap().to_string();
    let (st, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/friends/requests/{req_id}/accept"),
            None,
            Some(&bob_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "accept keeps its historical 200");

    // star + tag + group bob
    let (st, _) = call(
        &app,
        req(
            "PATCH",
            &format!("/api/friends/{bob}"),
            Some(json!({"starred": true, "tags": ["同事", "重要"], "group": "工作"})),
            Some(&alice_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);

    let (st, body) = call(&app, req("GET", "/api/friends", None, Some(&alice_tok))).await;
    assert_eq!(st, StatusCode::OK);
    let bob_row = body
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["friend_id"] == json!(bob))
        .unwrap()
        .clone();
    assert_eq!(bob_row["starred"], json!(true));
    assert_eq!(bob_row["tags"], json!(["同事", "重要"]));
    assert_eq!(bob_row["friend_group"], json!("工作"));
    assert_eq!(bob_row["remark"], json!(""), "remark untouched");

    // unstar keeps the rest (partial patch semantics)
    let (st, _) = call(
        &app,
        req(
            "PATCH",
            &format!("/api/friends/{bob}"),
            Some(json!({"starred": false})),
            Some(&alice_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, body) = call(&app, req("GET", "/api/friends", None, Some(&alice_tok))).await;
    assert_eq!(st, StatusCode::OK);
    let bob_row = body
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["friend_id"] == json!(bob))
        .unwrap()
        .clone();
    assert_eq!(bob_row["starred"], json!(false));
    assert_eq!(bob_row["tags"], json!(["同事", "重要"]), "tags preserved");
    let _ = alice;
}
