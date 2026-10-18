//! P3-8 投递策略端到端（真实 Platform 关系/禁言表驱动 hub 裁决）：
//! 拉黑、禁言、陌生人闸门——三条合同铁律在收件路径真正生效。

use std::time::Duration;

use wxlike_core::custom_kind;
use wxlike_core::{Envelope, MsgBody, Store};
use wxlike_net::Client;
use wxlike_server::{start_stack, StackOptions};

async fn boot() -> wxlike_server::Stack {
    start_stack(StackOptions::default()).await.unwrap()
}

fn direct(a: &str, b: &str) -> String {
    wxlike_platform::direct_conv_id(a, b)
}

async fn wait_bound(stack: &wxlike_server::Stack, n: usize) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while stack.hub.online_users().await < n {
        assert!(
            std::time::Instant::now() < deadline,
            "hub never bound {n} users"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn next(client: &Client, what: &str) -> anyhow::Result<Envelope> {
    tokio::time::timeout(Duration::from_secs(8), client.next_envelope())
        .await
        .map_err(|_| anyhow::anyhow!("timeout waiting for {what}"))?
}

/// 等待下一条**内容帧**（Text/Custom）：系统帧（Notice，好友/群事件）被
/// 策略豁免、必然先到，逐条排空后才是被测内容。
async fn next_text(client: &Client, what: &str) -> anyhow::Result<Envelope> {
    loop {
        let env = next(client, what).await?;
        if matches!(env.body, MsgBody::Text(_) | MsgBody::Custom { .. }) {
            return Ok(env);
        }
    }
}

/// 等待"没有内容帧"：窗口内只允许豁免的系统帧经过，出现内容帧即失败。
async fn expect_silence(client: &Client, what: &str) {
    loop {
        match tokio::time::timeout(Duration::from_millis(1200), client.next_envelope()).await {
            Err(_) => return, // 窗口关闭仍是静默
            Ok(env) => {
                let env = env.unwrap_or_else(|e| panic!("conn broke on {what}: {e}"));
                assert!(
                    matches!(env.body, MsgBody::Notice(_)),
                    "expected silence for {what}, got content: {env:?}"
                );
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blocked_users_direct_messages_never_arrive() {
    let stack = boot().await;
    let http = format!("http://{}", stack.http_addr);

    let alice = Client::connect(stack.quic_addr, "localhost", "alice")
        .await
        .unwrap();
    let bob = Client::connect(stack.quic_addr, "localhost", "bob")
        .await
        .unwrap();
    let mallory = Client::connect(stack.quic_addr, "localhost", "mallory")
        .await
        .unwrap();
    wait_bound(&stack, 3).await;

    // alice 经 HTTP 拉黑 mallory
    let mallory_id: serde_json::Value = ureq::post(&format!("{http}/api/register"))
        .send_json(serde_json::json!({"name": "m-id", "password": "pw"}))
        .unwrap()
        .into_json()
        .unwrap();
    let _ = mallory_id;
    // 用平台 seam 拿真实 id（QUIC 身份是注册名映射，测试内直接注册同名拿 id）
    let (mallory_uid, _mallory_tok) = {
        // mallory 已以用户名注册过吗？此测试用 QUIC 名即用户名——先注册拿 id
        let reply: serde_json::Value = ureq::post(&format!("{http}/api/register"))
            .send_json(serde_json::json!({"name": "mallory2", "password": "pw"}))
            .unwrap()
            .into_json()
            .unwrap();
        (
            reply["user_id"].as_str().unwrap().to_string(),
            reply["token"].as_str().unwrap().to_string(),
        )
    };
    let (alice_uid, alice_tok) = {
        let reply: serde_json::Value = ureq::post(&format!("{http}/api/register"))
            .send_json(serde_json::json!({"name": "alice2", "password": "pw"}))
            .unwrap()
            .into_json()
            .unwrap();
        (
            reply["user_id"].as_str().unwrap().to_string(),
            reply["token"].as_str().unwrap().to_string(),
        )
    };

    let res = ureq::put(&format!("{http}/api/blocks/{mallory_uid}"))
        .set("Authorization", &format!("Bearer {alice_tok}"))
        .call()
        .unwrap();
    assert_eq!(res.status(), 204);

    // mallory2（被拉黑者）直发 alice2 → 拦截
    let conv_bad = direct(&mallory_uid, &alice_uid);
    Client::connect(stack.quic_addr, "localhost", &mallory_uid)
        .await
        .unwrap()
        .send(&Envelope::direct(
            &conv_bad,
            &mallory_uid,
            vec![alice_uid.clone()],
            1,
            "骚扰",
        ))
        .await
        .unwrap();

    // alice2（未拉黑关系方）直发 alice2 自己？不成立——用 bob→alice 验证
    // 正常帧不受影响：bob2 注册、alice2 未拉黑 bob2，但也没有好友关系……
    // 直发闸门要求好友或已接受。此处为隔离变量：把 bob2 设为 alice2 好友。
    let (bob_uid, bob_tok) = {
        let reply: serde_json::Value = ureq::post(&format!("{http}/api/register"))
            .send_json(serde_json::json!({"name": "bob2", "password": "pw"}))
            .unwrap()
            .into_json()
            .unwrap();
        (
            reply["user_id"].as_str().unwrap().to_string(),
            reply["token"].as_str().unwrap().to_string(),
        )
    };
    ureq::post(&format!("{http}/api/friends/requests"))
        .set("Authorization", &format!("Bearer {bob_tok}"))
        .send_json(serde_json::json!({"to_user_id": alice_uid, "message": ""}))
        .unwrap();
    let incoming: serde_json::Value = ureq::get(&format!("{http}/api/friends/requests/incoming"))
        .set("Authorization", &format!("Bearer {alice_tok}"))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    let req_id = incoming[0]["id"].as_str().unwrap().to_string();
    ureq::post(&format!("{http}/api/friends/requests/{req_id}/accept"))
        .set("Authorization", &format!("Bearer {alice_tok}"))
        .call()
        .unwrap();

    let bob2 = Client::connect(stack.quic_addr, "localhost", &bob_uid)
        .await
        .unwrap();
    let alice2 = Client::connect(stack.quic_addr, "localhost", &alice_uid)
        .await
        .unwrap();
    wait_bound(&stack, 2).await;

    let conv_ok = direct(&bob_uid, &alice_uid);
    bob2.send(&Envelope::direct(
        &conv_ok,
        &bob_uid,
        vec![alice_uid.clone()],
        1,
        "正常",
    ))
    .await
    .unwrap();

    let env = next_text(&alice2, "bob's normal frame").await.unwrap();
    assert!(matches!(&env.body, MsgBody::Text(t) if t == "正常"));
    expect_silence(&alice2, "mallory's blocked frame").await;

    // 被拦帧不落库
    tokio::time::sleep(Duration::from_millis(300)).await;
    let rows: Vec<Store> = Vec::new();
    let _ = rows;
    let stored = stack.store.list_messages(&conv_bad).unwrap();
    assert!(stored.is_empty(), "blocked frame must not persist");
    assert_eq!(stack.store.list_messages(&conv_ok).unwrap().len(), 1);
    let _ = alice;
    let _ = bob;
    let _ = mallory;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn muted_members_group_frames_are_dropped() {
    let stack = boot().await;
    let http = format!("http://{}", stack.http_addr);
    let platform = stack.platform.clone();

    // 注册 owner + loud + fan，建群拉人
    let reg = |name: &str| -> (String, String) {
        let r: serde_json::Value = ureq::post(&format!("{http}/api/register"))
            .send_json(serde_json::json!({"name": name, "password": "pw"}))
            .unwrap()
            .into_json()
            .unwrap();
        (
            r["user_id"].as_str().unwrap().to_string(),
            r["token"].as_str().unwrap().to_string(),
        )
    };
    let (owner, owner_tok) = reg("owner");
    let (loud, loud_tok) = reg("loud");
    let (fan, fan_tok) = reg("fan");
    // 互为好友（建群前置）
    for (a, ta, b, tb) in [
        (&owner, &owner_tok, &loud, &loud_tok),
        (&owner, &owner_tok, &fan, &fan_tok),
    ] {
        ureq::post(&format!("{http}/api/friends/requests"))
            .set("Authorization", &format!("Bearer {ta}"))
            .send_json(serde_json::json!({"to_user_id": b, "message": ""}))
            .unwrap();
        let incoming: serde_json::Value =
            ureq::get(&format!("{http}/api/friends/requests/incoming"))
                .set("Authorization", &format!("Bearer {tb}"))
                .call()
                .unwrap()
                .into_json()
                .unwrap();
        let id = incoming[0]["id"].as_str().unwrap().to_string();
        ureq::post(&format!("{http}/api/friends/requests/{id}/accept"))
            .set("Authorization", &format!("Bearer {tb}"))
            .call()
            .unwrap();
        let _ = a;
    }
    let group: serde_json::Value = ureq::post(&format!("{http}/api/groups"))
        .set("Authorization", &format!("Bearer {owner_tok}"))
        .send_json(serde_json::json!({"name": "禁言群", "member_ids": [loud, fan]}))
        .unwrap()
        .into_json()
        .unwrap();
    let group = group["group_id"].as_str().unwrap().to_string();

    // owner 禁言 loud（+1s 窗口）
    ureq::put(&format!("{http}/api/groups/{group}/mute/{loud}"))
        .set("Authorization", &format!("Bearer {owner_tok}"))
        .send_json(serde_json::json!({"until_ms": wxlike_core::types::now_ms() + 1_000}))
        .unwrap();

    let loud_c = Client::connect(stack.quic_addr, "localhost", &loud)
        .await
        .unwrap();
    let fan_c = Client::connect(stack.quic_addr, "localhost", &fan)
        .await
        .unwrap();
    wait_bound(&stack, 2).await;

    // loud 群帧 → 丢弃
    loud_c
        .send(&Envelope::text(&group, &loud, 1, "插话"))
        .await
        .unwrap();
    // owner 群帧 → 扇出
    let owner_c = Client::connect(stack.quic_addr, "localhost", &owner)
        .await
        .unwrap();
    owner_c
        .send(&Envelope::text(&group, &owner, 2, "正常"))
        .await
        .unwrap();

    let env = next_text(&fan_c, "owner's frame").await.unwrap();
    assert!(matches!(&env.body, MsgBody::Text(t) if t == "正常"));
    expect_silence(&fan_c, "loud's muted frame").await;

    tokio::time::sleep(Duration::from_millis(300)).await;
    let rows = stack.store.list_messages(&group).unwrap();
    assert_eq!(rows.len(), 1, "muted frame not persisted");
    assert_eq!(rows[0].envelope.sender, owner);

    // 定时解除：1s 后 loud 的帧恢复投递
    std::thread::sleep(Duration::from_millis(1_100));
    loud_c
        .send(&Envelope::text(&group, &loud, 3, "解禁了"))
        .await
        .unwrap();
    let env = next_text(&fan_c, "post-mute frame").await.unwrap();
    assert!(matches!(&env.body, MsgBody::Text(t) if t == "解禁了"));
    assert!(
        !platform.is_muted(&group, &loud).unwrap(),
        "expiry is time-based"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stranger_gate_enforces_invariant_nine() {
    let stack = boot().await;
    let http = format!("http://{}", stack.http_addr);

    let reg = |name: &str| -> (String, String) {
        let r: serde_json::Value = ureq::post(&format!("{http}/api/register"))
            .send_json(serde_json::json!({"name": name, "password": "pw"}))
            .unwrap()
            .into_json()
            .unwrap();
        (
            r["user_id"].as_str().unwrap().to_string(),
            r["token"].as_str().unwrap().to_string(),
        )
    };
    let (alice, alice_tok) = reg("alice");
    let (carol, carol_tok) = reg("carol");

    let alice_c = Client::connect(stack.quic_addr, "localhost", &alice)
        .await
        .unwrap();
    let carol_c = Client::connect(stack.quic_addr, "localhost", &carol)
        .await
        .unwrap();
    wait_bound(&stack, 2).await;

    // 非好友直发 → 拦截（不落库）
    let conv = direct(&alice, &carol);
    carol_c
        .send(&Envelope::direct(
            &conv,
            &carol,
            vec![alice.clone()],
            1,
            "初次打扰",
        ))
        .await
        .unwrap();
    expect_silence(&alice_c, "pre-accept stranger frame").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(stack.store.list_messages(&conv).unwrap().is_empty());

    // 走陌生人请求箱并接受（不变式 9 流程）
    ureq::post(&format!("{http}/api/stranger/messages"))
        .set("Authorization", &format!("Bearer {carol_tok}"))
        .send_json(serde_json::json!({"to_uid": alice, "text": "在吗"}))
        .unwrap();
    let inbox: serde_json::Value = ureq::get(&format!("{http}/api/stranger/requests"))
        .set("Authorization", &format!("Bearer {alice_tok}"))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    let req_id = inbox["requests"][0]["id"].as_str().unwrap().to_string();
    ureq::post(&format!("{http}/api/stranger/requests/{req_id}/accept"))
        .set("Authorization", &format!("Bearer {alice_tok}"))
        .call()
        .unwrap();

    // 接受后同一对直发 → 可达（接受→正常会话）
    carol_c
        .send(&Envelope::direct(
            &conv,
            &carol,
            vec![alice.clone()],
            2,
            "已接受",
        ))
        .await
        .unwrap();
    let env = next_text(&alice_c, "post-accept frame").await.unwrap();
    assert!(matches!(&env.body, MsgBody::Text(t) if t == "已接受"));
    assert_eq!(stack.store.list_messages(&conv).unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redpackets_from_muted_members_are_also_gated() {
    let stack = boot().await;
    let http = format!("http://{}", stack.http_addr);
    let reg = |name: &str| -> (String, String) {
        let r: serde_json::Value = ureq::post(&format!("{http}/api/register"))
            .send_json(serde_json::json!({"name": name, "password": "pw"}))
            .unwrap()
            .into_json()
            .unwrap();
        (
            r["user_id"].as_str().unwrap().to_string(),
            r["token"].as_str().unwrap().to_string(),
        )
    };
    let (owner, _owner_tok) = reg("owner");
    let (loud, _loud_tok) = reg("loud");
    // 平台 seam 建群（create_group 不要求好友关系）
    let group = stack
        .platform
        .create_group(&owner, "rp", std::slice::from_ref(&loud))
        .unwrap();
    stack
        .platform
        .set_member_mute(&group, &owner, &loud, wxlike_core::types::now_ms() + 5_000)
        .unwrap();

    let loud_c = Client::connect(stack.quic_addr, "localhost", &loud)
        .await
        .unwrap();
    let owner_c = Client::connect(stack.quic_addr, "localhost", &owner)
        .await
        .unwrap();
    wait_bound(&stack, 2).await;

    // 被禁言者的红包帧（Custom content）同样过闸
    stack.platform.fund(&loud, 10_000, "test").unwrap();
    let rp = stack
        .platform
        .create_redpacket(&loud, 100, 1, "包", None)
        .unwrap();
    loud_c
        .send(&Envelope::with_body(
            &group,
            &loud,
            vec![],
            1,
            MsgBody::Custom {
                kind: custom_kind::WALLET_REDPACKET.into(),
                payload: serde_json::json!({"redpacket_id": rp, "amount": 100, "count": 1}),
            },
        ))
        .await
        .unwrap();

    expect_silence(&owner_c, "muted red packet").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let rows = stack.store.list_messages(&group).unwrap();
    assert!(rows.is_empty(), "muted custom content frame not persisted");
}
