//! P3 支柱 C（合同 §3.4）：社区 / 频道四类型 / 话题。
//!
//! Schema 形状借用 TangSengDaoDaoServer（Apache-2.0）的群/成员表；频道时间线
//! 直接压在 P3-1 读扩散信箱上（owner=Channel），**话题回复绝不占频道主时间
//! 线**（不变式 3 的推论，专测钉住）。

mod common;

use axum::http::StatusCode;
use common::{app, call, platform, req, signup};
use serde_json::json;
use wxlike_platform::PlatformState;

/// 建社区并让 owner 拉一个成员；返回内层 app/platform —— tokens 只对它们有效。
#[allow(clippy::type_complexity)]
async fn community_with_member() -> (String, String, String, String, PlatformState, axum::Router) {
    let p = platform();
    let app = app(p.clone());
    let (_owner, owner_tok) = signup(&app, "owner").await;
    let (member, member_tok) = signup(&app, "member").await;
    let (st, body) = call(
        &app,
        req(
            "POST",
            "/api/communities",
            Some(json!({"name": "万人社区"})),
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    let community = body["community_id"].as_str().unwrap().to_string();
    let (st, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/communities/{community}/join"),
            None,
            Some(&member_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["state"], "joined");
    (community, owner_tok, member_tok, member, p, app)
}

#[tokio::test]
async fn community_create_join_and_member_limit() {
    let p = platform();
    let app = app(p.clone());
    let (_, owner_tok) = signup(&app, "o").await;

    // member_limit 下限钳到 2：owner + 1 个名额
    let (st, body) = call(
        &app,
        req(
            "POST",
            "/api/communities",
            Some(json!({"name": "小圈子", "member_limit": 0})),
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let community = body["community_id"].as_str().unwrap().to_string();

    let (u1, t1) = signup(&app, "u1").await;
    let (u2, t2) = signup(&app, "u2").await;

    let (st, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/communities/{community}/join"),
            None,
            Some(&t1),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["state"], "joined");

    // 重复 join 幂等
    let (st, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/communities/{community}/join"),
            None,
            Some(&t1),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["state"], "already_member");

    // 第 3 人被 member_limit 挡住
    let (st, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/communities/{community}/join"),
            None,
            Some(&t2),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "limit reached");
    let _ = u2;
    let _ = u1;
}

#[tokio::test]
async fn channels_are_manager_created_with_validated_kinds() {
    let (community, owner_tok, member_tok, _member, _p, app) = community_with_member().await;

    // 成员建频道 → 403
    let (st, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/communities/{community}/channels"),
            Some(json!({"name": "成员频道", "kind": "text"})),
            Some(&member_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // 非法类型 → 422
    let (st, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/communities/{community}/channels"),
            Some(json!({"name": "坏类型", "kind": "video"})),
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    // 四类型全部可建
    for kind in ["text", "announce", "voice", "live"] {
        let (st, _) = call(
            &app,
            req(
                "POST",
                &format!("/api/communities/{community}/channels"),
                Some(json!({"name": format!("ch-{kind}"), "kind": kind})),
                Some(&owner_tok),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "kind={kind}");
    }

    // 成员可读列表（4 个）
    let (st, body) = call(
        &app,
        req(
            "GET",
            &format!("/api/communities/{community}/channels"),
            None,
            Some(&member_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["channels"].as_array().unwrap().len(), 4);
}

#[tokio::test]
async fn channel_timeline_via_mailbox_and_announce_gate() {
    let (community, owner_tok, member_tok, _member, _p, app) = community_with_member().await;

    let (_st, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/communities/{community}/channels"),
            Some(json!({"name": "一般频道", "kind": "text"})),
            Some(&owner_tok),
        ),
    )
    .await;
    let text_ch = body["channel_id"].as_str().unwrap().to_string();
    let (_st, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/communities/{community}/channels"),
            Some(json!({"name": "公告频道", "kind": "announce"})),
            Some(&owner_tok),
        ),
    )
    .await;
    let announce_ch = body["channel_id"].as_str().unwrap().to_string();

    // text 频道：成员可发，seq 单调；读取升序带游标
    let (st, b1) = call(
        &app,
        req(
            "POST",
            &format!("/api/channels/{text_ch}/messages"),
            Some(json!({"text": "第一条"})),
            Some(&member_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b1}");
    let (st, b2) = call(
        &app,
        req(
            "POST",
            &format!("/api/channels/{text_ch}/messages"),
            Some(json!({"text": "第二条"})),
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b2}");
    assert!(b2["seq"].as_u64().unwrap() > b1["seq"].as_u64().unwrap());

    let (st, body) = call(
        &app,
        req(
            "GET",
            &format!("/api/channels/{text_ch}/messages?limit=10"),
            None,
            Some(&member_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0]["text"], "第一条");
    assert!(body["next_after_seq"].is_null(), "2 < limit => 到头");

    // announce 频道：成员发送 403，owner 可发
    let (st, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/channels/{announce_ch}/messages"),
            Some(json!({"text": "成员伪装公告"})),
            Some(&member_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/channels/{announce_ch}/messages"),
            Some(json!({"text": "真公告"})),
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // 陌生人读频道 → 403
    let (_, stranger_tok) = signup(&app, "stranger").await;
    let (st, _) = call(
        &app,
        req(
            "GET",
            &format!("/api/channels/{text_ch}/messages"),
            None,
            Some(&stranger_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let _ = community;
}

#[tokio::test]
async fn thread_replies_never_touch_the_channel_timeline() {
    let (community, owner_tok, member_tok, _member, _p, app) = community_with_member().await;

    let (_st, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/communities/{community}/channels"),
            Some(json!({"name": "讨论", "kind": "text"})),
            Some(&owner_tok),
        ),
    )
    .await;
    let ch = body["channel_id"].as_str().unwrap().to_string();

    // 频道里发两条，第二条作为话题根
    call(
        &app,
        req(
            "POST",
            &format!("/api/channels/{ch}/messages"),
            Some(json!({"text": "根消息"})),
            Some(&owner_tok),
        ),
    )
    .await;
    let (_st, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/channels/{ch}/messages"),
            Some(json!({"text": "要被讨论的消息"})),
            Some(&member_tok),
        ),
    )
    .await;
    let root = body["seq"].as_u64().unwrap() as i64;

    // 话题挂根 + 两条回复
    let (st, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/channels/{ch}/threads"),
            Some(json!({"root_seq": root, "title": "展开讨论"})),
            Some(&member_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    let thread = body["thread_id"].as_str().unwrap().to_string();

    for who in [&member_tok, &owner_tok] {
        let (st, _) = call(
            &app,
            req(
                "POST",
                &format!("/api/threads/{thread}/messages"),
                Some(json!({"text": "话题内回复"})),
                Some(who),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }

    // **不变式**：话题回复后，频道时间线 head 不变（回复不占主序列）
    let (st, body) = call(
        &app,
        req(
            "GET",
            &format!("/api/channels/{ch}/messages?limit=50"),
            None,
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        body["messages"].as_array().unwrap().len(),
        2,
        "频道仍是 2 条"
    );

    // 话题内自有序列 + 成员可读
    let (st, body) = call(
        &app,
        req(
            "GET",
            &format!("/api/threads/{thread}/messages"),
            None,
            Some(&member_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["messages"].as_array().unwrap().len(), 2);

    // root_seq 越界（时间线外）→ 422
    let (st, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/channels/{ch}/threads"),
            Some(json!({"root_seq": 9999})),
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn thread_subscribe_unsubscribe_roundtrip() {
    let (community, owner_tok, member_tok, _member, _p, app) = community_with_member().await;

    let (_st, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/communities/{community}/channels"),
            Some(json!({"name": "ch", "kind": "text"})),
            Some(&owner_tok),
        ),
    )
    .await;
    let ch = body["channel_id"].as_str().unwrap().to_string();
    call(
        &app,
        req(
            "POST",
            &format!("/api/channels/{ch}/messages"),
            Some(json!({"text": "root"})),
            Some(&owner_tok),
        ),
    )
    .await;
    let (_st, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/channels/{ch}/threads"),
            Some(json!({"root_seq": 1})),
            Some(&owner_tok),
        ),
    )
    .await;
    let thread = body["thread_id"].as_str().unwrap().to_string();

    // 发起者自动订阅
    let (st, body) = call(
        &app,
        req(
            "GET",
            &format!("/api/threads/{thread}/subscribers"),
            None,
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["subscribers"].as_array().unwrap().len(), 1);

    // member 订阅 → 2 人；退订 → 回到 1
    let (st, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/threads/{thread}/subscribe"),
            None,
            Some(&member_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (_st, body) = call(
        &app,
        req(
            "GET",
            &format!("/api/threads/{thread}/subscribers"),
            None,
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(body["subscribers"].as_array().unwrap().len(), 2);
    let (st, _) = call(
        &app,
        req(
            "DELETE",
            &format!("/api/threads/{thread}/subscribe"),
            None,
            Some(&member_tok),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (_st, body) = call(
        &app,
        req(
            "GET",
            &format!("/api/threads/{thread}/subscribers"),
            None,
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(body["subscribers"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn tenant_id_reserved_column_defaults() {
    // 不变式 8：全表 tenant_id 预留（字段存在且默认 'default'，不实现隔离）
    let p = platform();
    let app = app(p.clone());
    let (_owner, owner_tok) = signup(&app, "owner").await;
    let (_st, body) = call(
        &app,
        req(
            "POST",
            "/api/communities",
            Some(json!({"name": "t"})),
            Some(&owner_tok),
        ),
    )
    .await;
    let community = body["community_id"].as_str().unwrap().to_string();

    let c = p.db().lock();
    let tenant: String = c
        .query_row(
            "SELECT tenant_id FROM communities WHERE id = ?1",
            rusqlite::params![community],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tenant, "default");
    // users 表的 tenant_id 随 P6 全表迁移补列；新 P3 表已预留
}

#[tokio::test]
async fn audit_log_separates_from_run_logs_and_self_reads() {
    let p = platform();
    let app = app(p.clone());
    let (alice, alice_tok) = signup(&app, "alice").await;
    let (bob, bob_tok) = signup(&app, "bob").await;

    // 审计动作：注册端点内部记录 + 钱包操作审计
    p.audit(&alice, "wallet.transfer", &bob, "100");
    p.audit(&bob, "redpacket.create", "", "50");

    // 自查：actor 只看到自己的审计条目（运行日志在 tracing/Loki，不在此）
    let (st, body) = call(&app, req("GET", "/api/audit/self", None, Some(&alice_tok))).await;
    assert_eq!(st, StatusCode::OK);
    let entries = body["entries"].as_array().unwrap();
    // signup 里 register 已写一条 user.register 审计 + 手动 transfer 一条
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["action"], "wallet.transfer", "最新在前");
    assert_eq!(entries[0]["target"], bob);
    assert_eq!(entries[1]["action"], "user.register");

    let (st, body) = call(&app, req("GET", "/api/audit/self", None, Some(&bob_tok))).await;
    assert_eq!(st, StatusCode::OK);
    let bob_entries = body["entries"].as_array().unwrap();
    assert_eq!(bob_entries.len(), 2, "register + redpacket.create");
    assert_eq!(bob_entries[0]["action"], "redpacket.create");
}
