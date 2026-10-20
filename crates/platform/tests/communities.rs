//! P3 支柱 C（合同 §3.4）：社区 / 频道四类型 / 话题 + Zone C（C1–C4）。
//!
//! Schema 形状借用 TangSengDaoDaoServer（Apache-2.0）的群/成员表；频道时间线
//! 直接压在 P3-1 读扩散信箱上（owner=Channel），**话题回复绝不占频道主时间
//! 线**（不变式 3 的推论，专测钉住）。
//!
//! Zone C 的四个验收点各有专测：迁移幂等（C1）、角色 × 消息类型闸门与代撤
//! （C2）、静音只影响推送不影响读（C3）、社区级话题汇总与角色端点（C4）。

mod common;

use axum::http::StatusCode;
use common::{app, call, platform, req, signup, Recorder};
use serde_json::json;
use std::sync::Arc;
use wxlike_core::MsgBody;
use wxlike_platform::{Platform, PlatformState};

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

// ---------------------------------------------------------------------------
// Zone C（C1–C4）
// ---------------------------------------------------------------------------

/// alice 群主 + bob/carol 普通成员 + 一个 text 频道 + 一个 announce 频道。
struct Ctx {
    p: PlatformState,
    app: axum::Router,
    community: String,
    text: String,
    announce: String,
    alice: (String, String),
    bob: (String, String),
    carol: (String, String),
    /// 不在社区里的外人
    stranger: (String, String),
}

async fn ctx() -> Ctx {
    let p = platform();
    let app = app(p.clone());
    let alice = signup(&app, "alice").await;
    let bob = signup(&app, "bob").await;
    let carol = signup(&app, "carol").await;
    let stranger = signup(&app, "stranger").await;
    let community = p.create_community(&alice.0, "Zone C 社区", 100).unwrap();
    p.join_community(&community, &bob.0).unwrap();
    p.join_community(&community, &carol.0).unwrap();
    let text = p
        .create_channel(&community, &alice.0, "综合", "text")
        .unwrap();
    let announce = p
        .create_channel(&community, &alice.0, "公告", "announce")
        .unwrap();
    Ctx {
        p,
        app,
        community,
        text,
        announce,
        alice,
        bob,
        carol,
        stranger,
    }
}

/// 频道发一条消息，返回 `(状态, msg_id, seq)`。
async fn post(
    c: &Ctx,
    channel: &str,
    who: &str,
    text: &str,
    kind: Option<&str>,
) -> (StatusCode, String, u64) {
    let body = match kind {
        Some(k) => json!({"text": text, "kind": k}),
        None => json!({"text": text}),
    };
    let (st, resp) = call(
        &c.app,
        req(
            "POST",
            &format!("/api/channels/{channel}/messages"),
            Some(body),
            Some(who),
        ),
    )
    .await;
    let msg_id = resp["msg_id"].as_str().unwrap_or_default().to_string();
    let seq = resp["seq"].as_u64().unwrap_or_default();
    (st, msg_id, seq)
}

async fn recall(c: &Ctx, channel: &str, who: &str, msg_id: &str) -> StatusCode {
    let (st, _) = call(
        &c.app,
        req(
            "POST",
            &format!("/api/channels/{channel}/messages/recall"),
            Some(json!({"msg_id": msg_id})),
            Some(who),
        ),
    )
    .await;
    st
}

/// **C1**：迁移建表 + 原地补列，并且**重开一次不炸**（老库升级路径）。
#[tokio::test]
async fn c1_channel_role_table_and_muted_column_migrate_idempotently() {
    let dir = std::env::temp_dir().join(format!("wxlike-zone-c-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("wxlike.db").to_str().unwrap().to_string();

    let p = Platform::open(Some(&path)).expect("open");
    let owner = p.register("owner", "pw").unwrap();
    let other = p.register("other", "pw").unwrap();
    let community = p.create_community(&owner, "社群", 100).unwrap();
    p.join_community(&community, &other).unwrap();
    let channel = p
        .create_channel(&community, &owner, "综合", "text")
        .unwrap();

    // 非法角色名被 CHECK 拦住：写坏的角色行比没有角色行更糟
    let err = p.db().lock().execute(
        "INSERT INTO channel_role(channel_id, user_id, role, granted_by, created_at)
         VALUES(?1, ?2, 'boss', ?3, 1)",
        rusqlite::params![channel, other, owner],
    );
    assert!(err.is_err(), "CHECK 约束必须拒绝未知角色");

    // 覆盖行落库；老库补出来的 muted 列对既有订阅行取默认 0（=不静音）
    p.set_channel_role(&channel, &owner, &other, "guest")
        .unwrap();
    let roles = p.channel_roles(&channel, &owner).unwrap();
    assert_eq!(roles.len(), 2);
    let guest = roles.iter().find(|r| r.user_id == other).unwrap();
    assert_eq!(guest.role, "guest");
    assert!(!guest.inherited, "有专属行就不是继承来的");
    assert_eq!(guest.granted_by, owner);
    assert_eq!(
        roles.iter().find(|r| r.user_id == owner).unwrap().role,
        "owner",
        "社区群主在频道里继承成 owner"
    );
    // 补列出来的 muted：老写法（不写 muted 的 INSERT）必须落到默认 0，
    // 否则升级之后所有既有订阅都会被当成静音，等于静默改了行为。
    let thread = {
        p.send_channel_message(&channel, &owner, "text", "根消息")
            .await
            .expect("send");
        p.create_thread(&channel, &owner, 1, "话题").unwrap()
    };
    p.db()
        .lock()
        .execute(
            "INSERT INTO thread_subs(thread_id, user_id) VALUES(?1, ?2)",
            rusqlite::params![thread, other],
        )
        .unwrap();
    let muted: i64 = p
        .db()
        .lock()
        .query_row(
            "SELECT muted FROM thread_subs WHERE thread_id = ?1 AND user_id = ?2",
            rusqlite::params![thread, other],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(muted, 0, "既有订阅不能被升级成静音");

    drop(p);
    // 第二次打开：CREATE IF NOT EXISTS + ALTER 补列都要能重放
    let p2 = Platform::open(Some(&path)).expect("重开必须幂等");
    let roles = p2.channel_roles(&channel, &owner).unwrap();
    assert_eq!(
        roles.iter().find(|r| r.user_id == other).unwrap().role,
        "guest",
        "重启不丢覆盖行"
    );
    assert_eq!(
        p2.effective_channel_role(&channel, &other)
            .unwrap()
            .unwrap()
            .as_str(),
        "guest"
    );
    drop(p2);
    std::fs::remove_dir_all(&dir).ok();
}

/// **C2**：频道角色 × 消息类型的闸门，以及"moderator 能撤他人消息"。
#[tokio::test]
async fn c2_channel_roles_gate_message_types_and_recall() {
    let c = ctx().await;
    // bob 在社区里提到管理员 → 频道里继承为 moderator；carol 压成 guest
    c.p.set_member_role(&c.community, &c.alice.0, &c.bob.0, "admin")
        .unwrap();
    let st = call(
        &c.app,
        req(
            "POST",
            &format!("/api/channel/{}/roles", c.text),
            Some(json!({"user_id": c.carol.0, "role": "guest"})),
            Some(&c.alice.1),
        ),
    )
    .await;
    assert_eq!(st.0, StatusCode::OK, "{st:?}");

    // 外人发消息 → 403（非成员连闸门都进不来）
    let (st, _, _) = post(&c, &c.text, &c.stranger.1, "嗨", None).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "非成员必须 403");

    // guest 能打字
    let (st, guest_msg, _) = post(&c, &c.text, &c.carol.1, "只发文字", None).await;
    assert_eq!(st, StatusCode::OK, "guest 发文字要过：{st}");
    // guest 不能发附件
    let (st, _, _) = post(&c, &c.text, &c.carol.1, "img-1", Some("image")).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "guest 不能发媒体");
    // 端点只收客户端能发的类型：放 system / 卡片进去等于让人伪造成系统口吻
    for kind in ["system", "msg.forward", "wallet.redpacket", "typing"] {
        let (st, _, _) = post(&c, &c.text, &c.alice.1, "伪造", Some(kind)).await;
        assert_eq!(
            st,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{kind} 不该从端点进来"
        );
    }
    // 普通成员（bob 是 moderator）可以
    let (st, bob_media, _) = post(&c, &c.text, &c.bob.1, "img-2", Some("image")).await;
    assert_eq!(st, StatusCode::OK, "member/moderator 发媒体要过");
    // 公告频道：guest 与普通成员都不行，moderator 行
    let (st, _, _) = post(&c, &c.announce, &c.carol.1, "伪装公告", None).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "公告频道挡住 guest");
    let (st, announce_media, _) = post(&c, &c.announce, &c.bob.1, "真公告", None).await;
    assert_eq!(st, StatusCode::OK, "moderator 能在公告频道发");

    // 撤他人消息：moderator 过，member/guest 不过
    assert_eq!(
        recall(&c, &c.text, &c.bob.1, &guest_msg).await,
        StatusCode::NO_CONTENT,
        "moderator 撤他人消息"
    );
    assert_eq!(
        recall(&c, &c.text, &c.carol.1, &bob_media).await,
        StatusCode::FORBIDDEN,
        "guest 撤不动别人的消息"
    );
    // 本人的消息自己就能撤，不需要任何管理权限点
    assert_eq!(
        recall(&c, &c.announce, &c.bob.1, &announce_media).await,
        StatusCode::NO_CONTENT,
        "作者撤自己的"
    );
    // 撤回是逻辑删除：时间线里还在，只是标记 withdrawn
    let (st, body) = call(
        &c.app,
        req(
            "GET",
            &format!("/api/channels/{}/messages?limit=50", c.text),
            None,
            Some(&c.alice.1),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let rows = body["messages"].as_array().unwrap();
    let recalled = rows.iter().find(|m| m["msg_id"] == guest_msg).unwrap();
    assert_eq!(recalled["withdrawn"], json!(true), "撤的是标记不是删行");
    assert_eq!(recalled["kind"], json!("text"), "类型要回传给客户端");

    // 提权只归 owner：moderator 授不动频道角色
    let (st, _) = call(
        &c.app,
        req(
            "POST",
            &format!("/api/channel/{}/roles", c.text),
            Some(json!({"user_id": c.carol.0, "role": "member"})),
            Some(&c.bob.1),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "moderator 不能授予角色");
}

/// **C3**：订阅/静音只动推送名单，不动读；话题回复不进频道时间线。
#[tokio::test]
async fn c3_thread_mute_moves_fanout_not_reading() {
    let c = ctx().await;
    let rec = Recorder::new();
    c.p.set_event_sink(Arc::new(rec.clone()));

    let (_st, _mid, seq) = post(&c, &c.text, &c.alice.1, "根消息", None).await;
    let (st, body) = call(
        &c.app,
        req(
            "POST",
            &format!("/api/channels/{}/threads", c.text),
            Some(json!({"root_seq": seq, "title": "展开"})),
            Some(&c.alice.1),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    let thread = body["thread_id"].as_str().unwrap().to_string();

    // 发起者自动订阅
    let (_, body) = call(
        &c.app,
        req(
            "GET",
            &format!("/api/threads/{thread}/subscribers"),
            None,
            Some(&c.alice.1),
        ),
    )
    .await;
    assert_eq!(body["fanout"].as_array().unwrap().len(), 1);

    // bob 订阅 → 扇出名单多一个人（这就是 MessageStore::members 之外的那一层）
    let (st, _) = call(
        &c.app,
        req(
            "POST",
            &format!("/api/threads/{thread}/subscribe"),
            None,
            Some(&c.bob.1),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (_, body) = call(
        &c.app,
        req(
            "GET",
            &format!("/api/threads/{thread}/subscribers"),
            None,
            Some(&c.alice.1),
        ),
    )
    .await;
    let mut fanout: Vec<String> = body["fanout"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    fanout.sort();
    let mut want = vec![c.alice.0.clone(), c.bob.0.clone()];
    want.sort();
    assert_eq!(fanout, want, "订阅进名单");
    // carol 没订阅：她在名单外，但读不受影响（下面验）
    assert!(!body["fanout"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == &json!(c.carol.0)));

    let reply = |text: String| {
        let app = c.app.clone();
        let tok = c.alice.1.clone();
        let thread = thread.clone();
        async move {
            call(
                &app,
                req(
                    "POST",
                    &format!("/api/threads/{thread}/messages"),
                    Some(json!({"text": text})),
                    Some(&tok),
                ),
            )
            .await
        }
    };

    // 回复只扇给名单里的别人（不含作者自己）
    assert_eq!(reply("一".into()).await.0, StatusCode::OK);
    let events = rec.take();
    assert_eq!(events.len(), 1, "订阅者收到一条推送");
    assert_eq!(events[0].to, vec![c.bob.0.clone()], "扇出名单就是收件人");
    assert!(
        matches!(&events[0].body, MsgBody::Custom { kind, .. } if kind == "thread.reply"),
        "thread.reply 不进消息类型注册表 = 不落频道时间线"
    );

    // bob 静音：仍在订阅名单里，但从推送名单里掉出去
    let (st, _) = call(
        &c.app,
        req(
            "PUT",
            &format!("/api/threads/{thread}/subscribe"),
            Some(json!({"muted": true})),
            Some(&c.bob.1),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (_, body) = call(
        &c.app,
        req(
            "GET",
            &format!("/api/threads/{thread}/subscribers"),
            None,
            Some(&c.alice.1),
        ),
    )
    .await;
    assert_eq!(
        body["subscribers"].as_array().unwrap().len(),
        2,
        "静音不等于退订"
    );
    assert_eq!(body["fanout"].as_array().unwrap().len(), 1);
    assert_eq!(body["fanout"][0], json!(c.alice.0));
    assert_eq!(reply("二".into()).await.0, StatusCode::OK);
    assert_eq!(rec.count(), 0, "静音后不再扇给 bob，作者自己也不算");

    // 读面不受订阅影响：没订阅过的 carol、以及静音的 bob 都能翻话题
    for who in [&c.carol.1, &c.bob.1] {
        let (st, body) = call(
            &c.app,
            req(
                "GET",
                &format!("/api/threads/{thread}/messages"),
                None,
                Some(who),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "读话题不该看订阅");
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
    }
    // 频道时间线仍然只有那条根消息（不变式 3）
    let (_, body) = call(
        &c.app,
        req(
            "GET",
            &format!("/api/channels/{}/messages?limit=50", c.text),
            None,
            Some(&c.carol.1),
        ),
    )
    .await;
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);

    // 退订 → 名单回到只有作者
    let (st, _) = call(
        &c.app,
        req(
            "DELETE",
            &format!("/api/threads/{thread}/subscribe"),
            None,
            Some(&c.bob.1),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert_eq!(c.p.thread_fanout(&thread).unwrap(), vec![c.alice.0.clone()]);
}

/// **C4**：社区级话题汇总 + 频道角色端点的 HTTP 契约。
#[tokio::test]
async fn c4_community_thread_rollup_and_role_endpoints() {
    let c = ctx().await;
    let second =
        c.p.create_channel(&c.community, &c.alice.0, "招聘", "text")
            .unwrap();
    // 另一个社区，用来验证跨社区写入被挡住
    let other = c.p.create_community(&c.alice.0, "隔壁社区", 100).unwrap();
    let foreign =
        c.p.create_channel(&other, &c.alice.0, "别的频道", "text")
            .unwrap();

    let mut ids = Vec::new();
    for ch in [&c.text, &second] {
        let (_st, _m, seq) = post(&c, ch, &c.alice.1, "根", None).await;
        let (st, body) = call(
            &c.app,
            req(
                "POST",
                &format!("/api/channels/{ch}/threads"),
                Some(json!({"root_seq": seq, "title": "讨论"})),
                Some(&c.alice.1),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "{body}");
        ids.push(body["thread_id"].as_str().unwrap().to_string());
    }
    // 第一条话题里塞两条回复，验证 replies 计数
    for t in ["a", "b"] {
        call(
            &c.app,
            req(
                "POST",
                &format!("/api/threads/{}/messages", ids[0]),
                Some(json!({"text": t})),
                Some(&c.bob.1),
            ),
        )
        .await;
    }

    let (st, body) = call(
        &c.app,
        req(
            "GET",
            &format!("/api/community/{}/threads", c.community),
            None,
            Some(&c.carol.1),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let rows = body["threads"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "跨频道汇总");
    // 排序契约是 `(created_at DESC, id DESC)`：同一毫秒建的话题也要有确定次序，
    // 否则翻页会出现"同一条在两页都出现"或"两页都不出现"。测创建先后是假断言——
    // 两个话题可以落在同一毫秒里。
    let key = |r: &serde_json::Value| {
        (
            r["created_at"].as_i64().unwrap(),
            r["thread_id"].as_str().unwrap().to_string(),
        )
    };
    assert!(key(&rows[0]) >= key(&rows[1]), "最新在前，同刻按 id 倒序");
    let by_id = |want: &str| rows.iter().find(|r| r["thread_id"] == json!(want)).unwrap();
    assert_eq!(by_id(&ids[0])["replies"], json!(2), "回复数按话题给");
    assert_eq!(by_id(&ids[0])["channel_name"], json!("综合"));
    assert_eq!(by_id(&ids[1])["channel_name"], json!("招聘"));
    assert_eq!(
        by_id(&ids[0])["subscribed"],
        json!(false),
        "carol 没订阅：列表里看得到，但不算订阅"
    );
    // 外人看不到社区话题
    let (st, _) = call(
        &c.app,
        req(
            "GET",
            &format!("/api/community/{}/threads", c.community),
            None,
            Some(&c.stranger.1),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // POST 走社区入口必须点名频道；缺 channel_id → 422，跨社区 → 404
    let (st, _) = call(
        &c.app,
        req(
            "POST",
            &format!("/api/community/{}/threads", c.community),
            Some(json!({"root_seq": 1, "title": "无频道"})),
            Some(&c.alice.1),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    let (st, _) = call(
        &c.app,
        req(
            "POST",
            &format!("/api/community/{}/threads", c.community),
            Some(json!({"root_seq": 1, "channel_id": foreign})),
            Some(&c.alice.1),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "别的社区的频道不能写进来");
    let (st, body) = call(
        &c.app,
        req(
            "POST",
            &format!("/api/community/{}/threads", c.community),
            Some(json!({"root_seq": 1, "channel_id": second, "title": "新话题"})),
            Some(&c.alice.1),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{body}");

    // 角色端点：列出（含继承标记）→ 覆盖 → 撤掉覆盖回到继承
    let (st, body) = call(
        &c.app,
        req(
            "GET",
            &format!("/api/channel/{}/roles", c.text),
            None,
            Some(&c.bob.1),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["my_role"], json!("member"));
    let roles = body["roles"].as_array().unwrap();
    assert_eq!(roles.len(), 3);
    assert!(
        roles.iter().all(|r| r["inherited"] == json!(true)),
        "没人被单独授予过角色"
    );
    assert_eq!(
        roles
            .iter()
            .find(|r| r["user_id"] == json!(c.alice.0))
            .unwrap()["role"],
        json!("owner")
    );

    call(
        &c.app,
        req(
            "POST",
            &format!("/api/channel/{}/roles", c.text),
            Some(json!({"user_id": c.bob.0, "role": "moderator"})),
            Some(&c.alice.1),
        ),
    )
    .await;
    let (_, body) = call(
        &c.app,
        req(
            "GET",
            &format!("/api/channel/{}/roles", c.text),
            None,
            Some(&c.alice.1),
        ),
    )
    .await;
    let bob_row = body["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["user_id"] == json!(c.bob.0))
        .unwrap();
    assert_eq!(bob_row["role"], json!("moderator"));
    assert_eq!(bob_row["inherited"], json!(false));
    assert_eq!(bob_row["granted_by"], json!(c.alice.0));

    let (st, _) = call(
        &c.app,
        req(
            "DELETE",
            &format!("/api/channel/{}/roles/{}", c.text, c.bob.0),
            None,
            Some(&c.alice.1),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let role =
        c.p.effective_channel_role(&c.text, &c.bob.0)
            .unwrap()
            .unwrap();
    assert_eq!(role.as_str(), "member", "撤掉覆盖 = 回到继承");
}
