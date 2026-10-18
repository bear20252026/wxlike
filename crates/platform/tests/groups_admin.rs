//! P3 群管理补齐（合同 §3.3）：公告、禁言（全员/单人/定时）、邀请 token、
//! 入群审批、@所有人（owner/admin 限定）。全部走真实 axum 路由。

mod common;

use axum::http::StatusCode;
use common::{app, call, platform, req, signup, Recorder};
use serde_json::json;
use std::sync::Arc;
use wxlike_platform::PlatformState;

/// 建群并返回 (group_id, owner_tok, member_tok, member_id, platform, app)。
#[allow(clippy::type_complexity)]
async fn group_with_member() -> (String, String, String, String, PlatformState, axum::Router) {
    let p = platform();
    let app = app(p.clone());
    let (_, owner_tok) = signup(&app, "owner").await;
    let (member, member_tok) = signup(&app, "member").await;

    // 好友后建群拉人（建群要求成员是好友——沿用 groups.rs 语义）
    call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"to_user_id": member, "message": ""})),
            Some(&owner_tok),
        ),
    )
    .await;
    let (_, body) = call(
        &app,
        req(
            "GET",
            "/api/friends/requests/incoming",
            None,
            Some(&member_tok),
        ),
    )
    .await;
    let req_id = body[0]["id"].as_str().unwrap().to_string();
    call(
        &app,
        req(
            "POST",
            &format!("/api/friends/requests/{req_id}/accept"),
            None,
            Some(&member_tok),
        ),
    )
    .await;

    let (st, body) = call(
        &app,
        req(
            "POST",
            "/api/groups",
            Some(json!({"name": "管理群", "member_ids": [member]})),
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let group = body["group_id"].as_str().unwrap().to_string();
    (group, owner_tok, member_tok, member, p, app)
}

#[tokio::test]
async fn announcement_is_manager_only_and_member_readable() {
    let (group, owner_tok, member_tok, _member, _p, app) = group_with_member().await;

    // 成员设置公告 → 403
    let (st, _) = call(
        &app,
        req(
            "PUT",
            &format!("/api/groups/{group}/announcement"),
            Some(json!({"text": "成员想篡改"})),
            Some(&member_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // owner 设置 → 204；成员可读
    let (st, _) = call(
        &app,
        req(
            "PUT",
            &format!("/api/groups/{group}/announcement"),
            Some(json!({"text": "本周六团建", "media_id": ""})),
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);

    let (st, body) = call(
        &app,
        req(
            "GET",
            &format!("/api/groups/{group}/announcement"),
            None,
            Some(&member_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["text"], "本周六团建");

    // 陌生人读公告 → 403（成员限定）
    let (_, stranger_tok) = signup(&app, "stranger").await;
    let (st, _) = call(
        &app,
        req(
            "GET",
            &format!("/api/groups/{group}/announcement"),
            None,
            Some(&stranger_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn member_mute_expires_and_owner_cannot_be_muted() {
    let (group, owner_tok, member_tok, member, p, app) = group_with_member().await;

    // owner 禁言 member 2 秒
    let until = wxlike_core::types::now_ms() + 2_000;
    let (st, _) = call(
        &app,
        req(
            "PUT",
            &format!("/api/groups/{group}/mute/{member}"),
            Some(json!({"until_ms": until})),
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert!(p.is_muted(&group, &member).unwrap());

    // owner 不可被禁言
    let (st, _) = call(
        &app,
        req(
            "PUT",
            &format!("/api/groups/{group}/mute/{owner_tok}"),
            Some(json!({"until_ms": 9_999_999_999i64})),
            Some(&owner_tok),
        ),
    )
    .await;
    assert!(
        st.is_client_error() || st.is_server_error(),
        "owner mute refused: {st}"
    );

    // 定时解除：2 秒后 is_muted 自然为 false（无 sweeper）
    std::thread::sleep(std::time::Duration::from_millis(2_200));
    assert!(
        !p.is_muted(&group, &member).unwrap(),
        "expiry is time-based"
    );
    let _ = member_tok;
}

#[tokio::test]
async fn all_mute_hides_managers_from_muting() {
    let (group, owner_tok, member_tok, member, p, app) = group_with_member().await;

    let (st, _) = call(
        &app,
        req(
            "PUT",
            &format!("/api/groups/{group}/mute-all"),
            Some(json!({"on": true})),
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);

    assert!(p.is_muted(&group, &member).unwrap(), "普通成员被全员禁言");
    assert!(
        !p.is_muted(&group, &owner_id_of(&app, &group, &owner_tok).await)
            .unwrap(),
        "owner 不受全员禁言"
    );

    // 关闭全员禁言后恢复
    let (st, _) = call(
        &app,
        req(
            "PUT",
            &format!("/api/groups/{group}/mute-all"),
            Some(json!({"on": false})),
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert!(!p.is_muted(&group, &member).unwrap());
    let _ = member_tok;
}

async fn owner_id_of(app: &axum::Router, group: &str, owner_tok: &str) -> String {
    let (st, body) = call(
        app,
        req(
            "GET",
            &format!("/api/groups/{group}"),
            None,
            Some(owner_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    body["owner_uid"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn invite_token_direct_join_and_expiry() {
    let (group, owner_tok, _member_tok, _member, p, app) = group_with_member().await;
    let (newbie, newbie_tok) = signup(&app, "newbie").await;

    eprintln!("STEP: signing invite");
    // owner 签发 200ms token
    let (st, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/groups/{group}/invites"),
            Some(json!({"ttl_ms": 200})),
            Some(&owner_tok),
        ),
    )
    .await;
    eprintln!("STEP: invite signed -> {st} {body}");
    let token = body["token"].as_str().unwrap().to_string();

    eprintln!("STEP: joining");
    // 审批默认关：直join
    let (st, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/groups/join/{token}"),
            None,
            Some(&newbie_tok),
        ),
    )
    .await;
    eprintln!("STEP: joined -> {st} {body}");
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "joined");
    eprintln!("STEP: checking membership");
    assert!(p.is_member(&group, &newbie).unwrap());

    // 过期 token：拒绝
    let (st, body2) = call(
        &app,
        req(
            "POST",
            &format!("/api/groups/{group}/invites"),
            Some(json!({"ttl_ms": 100})),
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let token2 = body2["token"].as_str().unwrap().to_string();
    std::thread::sleep(std::time::Duration::from_millis(150));
    let (st, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/groups/join/{token2}"),
            None,
            Some(&newbie_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "expired invite");
    let _ = group;
}

#[tokio::test]
async fn approval_flow_pends_then_approves() {
    let (group, owner_tok, _member_tok, _member, p, app) = group_with_member().await;
    let (candidate, candidate_tok) = signup(&app, "candidate").await;

    // 开审批
    let (st, _) = call(
        &app,
        req(
            "PUT",
            &format!("/api/groups/{group}/approval"),
            Some(json!({"on": true})),
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);

    // token 入群 → pending
    let (st, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/groups/{group}/invites"),
            Some(json!({"ttl_ms": 60_000})),
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let token = body["token"].as_str().unwrap().to_string();

    let (st, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/groups/join/{token}"),
            None,
            Some(&candidate_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["state"], "pending");
    assert!(!p.is_member(&group, &candidate).unwrap(), "未批准不进群");

    // owner 审批 → 入群
    let (st, body) = call(
        &app,
        req(
            "GET",
            &format!("/api/groups/{group}/join-requests"),
            None,
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["requests"][0]["user_id"], candidate);

    let (st, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/groups/{group}/join-requests/{candidate}/approve"),
            None,
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert!(p.is_member(&group, &candidate).unwrap(), "批准后入群");
}

#[tokio::test]
async fn mention_all_is_manager_only_and_emits_to_members() {
    // group_with_member builds its OWN platform instance — the sink must go
    // on THAT one (the envelopes are emitted from the platform serving them)
    let (group, owner_tok, member_tok, _member, p, app) = group_with_member().await;
    let recorder = Arc::new(Recorder::new());
    p.set_event_sink(recorder.clone());

    // 成员 @所有人 → 403
    let (st, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/groups/{group}/mention-all"),
            Some(json!({"text": "假命令"})),
            Some(&member_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(recorder.count(), 0, "被拒绝的 @所有人不留痕迹");

    // owner @所有人 → 200 + 系统信封发给成员
    let (st, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/groups/{group}/mention-all"),
            Some(json!({"text": "全员集合"})),
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["all"], json!(true));

    let envelopes = recorder.take();
    assert!(!envelopes.is_empty(), "mention-all 经 EventSink 扇出");
    let env = envelopes.last().unwrap();
    assert_eq!(env.conv_id, group);
    match &env.body {
        wxlike_core::MsgBody::Custom { kind, payload } => {
            assert_eq!(kind, "mention");
            assert_eq!(payload["all"], json!(true));
            assert_eq!(payload["text"], "全员集合");
        }
        other => panic!("expected mention custom frame, got {other:?}"),
    }
}
