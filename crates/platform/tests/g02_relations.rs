//! G02-02 HTTP 面：群可见性、群解散、关系闸门、并发接受。
//!
//! 平台原本就有好友 / 拉黑 / 星标标签（`PATCH /api/friends/{id}`）与群角色，
//! 这轮补的是卡面里真正缺的两块：**群可见性**与**解散**，以及把
//! `RelationsPort` 暴露成 `Platform::may_message`（G03/G05 不再自己查表）。

mod common;

use axum::http::StatusCode;
use axum::Router;
use serde_json::json;

use common::{app, call, platform, req, signup, Recorder};

async fn create_group(app: &Router, token: &str, name: &str, members: &[String]) -> String {
    let (status, body) = call(
        app,
        req(
            "POST",
            "/api/groups",
            Some(json!({ "name": name, "member_ids": members })),
            Some(token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create group: {body}");
    body["group_id"].as_str().unwrap().to_string()
}

async fn group_info(app: &Router, token: &str, gid: &str) -> (StatusCode, serde_json::Value) {
    call(
        app,
        req("GET", &format!("/api/groups/{gid}"), None, Some(token)),
    )
    .await
}

#[tokio::test]
async fn visibility_is_owner_only_and_round_trips() {
    let state = platform();
    let app = app(state);
    let (alice, alice_tok) = signup(&app, "alice").await;
    let (bob, bob_tok) = signup(&app, "bob").await;
    let gid = create_group(&app, &alice_tok, "私密群", &[bob.clone()]).await;

    // 默认 public
    let (status, body) = group_info(&app, &alice_tok, &gid).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["visibility"], "public");
    assert_eq!(body["dissolved"], false);

    // 成员（非群主）不能改
    let (status, _) = call(
        &app,
        req(
            "PUT",
            &format!("/api/groups/{gid}/visibility"),
            Some(json!({ "visibility": "private" })),
            Some(&bob_tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 非法值
    let (status, _) = call(
        &app,
        req(
            "PUT",
            &format!("/api/groups/{gid}/visibility"),
            Some(json!({ "visibility": "secret" })),
            Some(&alice_tok),
        ),
    )
    .await;
    // 平台口径：语法对、内容不合法 = 422（400 留给请求体读不出来）
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // 群主改 private
    let (status, _) = call(
        &app,
        req(
            "PUT",
            &format!("/api/groups/{gid}/visibility"),
            Some(json!({ "visibility": "private" })),
            Some(&alice_tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, body) = group_info(&app, &alice_tok, &gid).await;
    assert_eq!(body["visibility"], "private");
    let _ = alice;
}

#[tokio::test]
async fn dissolve_is_owner_only_and_marks_the_group() {
    let state = platform();
    let recorder = Recorder::new();
    state.set_event_sink(std::sync::Arc::new(recorder.clone()));
    let app = app(state);
    let (_alice, alice_tok) = signup(&app, "alice").await;
    let (bob, bob_tok) = signup(&app, "bob").await;
    let gid = create_group(&app, &alice_tok, "要解散的群", &[bob.clone()]).await;

    // 非群主不能解散
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/groups/{gid}/dissolve"),
            None,
            Some(&bob_tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/groups/{gid}/dissolve"),
            None,
            Some(&alice_tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["notified"].as_u64().unwrap() >= 2, "{body}");

    // 软删：行还在，但打上 dissolved
    let (status, info) = group_info(&app, &bob_tok, &gid).await;
    assert_eq!(status, StatusCode::OK, "解散后成员仍应能读历史");
    assert_eq!(info["dissolved"], true);

    let events = recorder.take();
    let hit = events.iter().any(|env| {
        matches!(&env.body, wxlike_core::MsgBody::Custom { kind, .. } if kind == "group.dissolved")
    });
    assert!(hit, "解散要给成员发 group.dissolved 通知帧");
}

#[tokio::test]
async fn unknown_group_visibility_is_404() {
    let state = platform();
    let app = app(state);
    let (_uid, tok) = signup(&app, "carol").await;
    let (status, _) = call(
        &app,
        req(
            "PUT",
            "/api/groups/0123456789abcdef/visibility",
            Some(json!({ "visibility": "private" })),
            Some(&tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn second_accept_loses_the_race() {
    let state = platform();
    let app = app(state);
    let (alice, alice_tok) = signup(&app, "alice").await;
    let (_bob, bob_tok) = signup(&app, "bob").await;

    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({ "to_user_id": alice, "message": "hi" })),
            Some(&bob_tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let req_id = body["id"].as_str().unwrap().to_string();

    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/friends/requests/{req_id}/accept"),
            None,
            Some(&alice_tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 第二次接受：条件 UPDATE 抢不到，返回冲突而不是再插一条关系
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/friends/requests/{req_id}/accept"),
            None,
            Some(&alice_tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn may_message_gate_covers_stranger_friend_and_blocked() {
    use wxlike_relations::port::MessageGate;

    let state = platform();
    let app = app(state.clone());
    let (alice, alice_tok) = signup(&app, "alice").await;
    let (bob, bob_tok) = signup(&app, "bob").await;

    // 陌生人：进请求箱，不直接放行
    assert_eq!(
        state.may_message(&alice, &bob).unwrap(),
        MessageGate::StrangerRequest
    );

    // 成为好友
    let (_, body) = call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({ "to_user_id": alice, "message": "hi" })),
            Some(&bob_tok),
        ),
    )
    .await;
    let req_id = body["id"].as_str().unwrap().to_string();
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/friends/requests/{req_id}/accept"),
            None,
            Some(&alice_tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        state.may_message(&alice, &bob).unwrap(),
        MessageGate::Allowed
    );

    // 拉黑后两个方向都拒绝
    let (status, _) = call(
        &app,
        req("PUT", &format!("/api/blocks/{bob}"), None, Some(&alice_tok)),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "拉黑接口应成功");
    assert_eq!(
        state.may_message(&alice, &bob).unwrap(),
        MessageGate::Blocked
    );
    assert_eq!(
        state.may_message(&bob, &alice).unwrap(),
        MessageGate::Blocked
    );
}

#[tokio::test]
async fn shared_groups_exclude_outsiders() {
    let state = platform();
    let app = app(state.clone());
    let (alice, alice_tok) = signup(&app, "alice").await;
    let (bob, _bob_tok) = signup(&app, "bob").await;
    let (carol, _carol_tok) = signup(&app, "carol").await;

    let gid = create_group(&app, &alice_tok, "共同群", &[bob.clone()]).await;
    let shared = state.shared_group_ids(&alice, &bob).unwrap();
    assert_eq!(shared, vec![gid.clone()]);
    // 非成员与谁都没有共同群
    assert!(state.shared_group_ids(&alice, &carol).unwrap().is_empty());
}
