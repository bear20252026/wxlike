//! P3 接线验收（端到端）：**直聊/群消息经 hub 落库时同时进信箱**，
//! 因此未读与多端同步有了统一真相。
//!
//! 这条测试覆盖的是"接线"，不是"算法"：算法（`unread = head − cursor`、
//! 五层 `sync_plan`）在 `crates/message` 里早就有测试。这里要证明的是——
//! 客户端通过 QUIC 发一条直聊消息后，另一个客户端从 HTTP 查未读能看到 1，
//! 读完后变 0。少了镜像这一步，未读会永远是 0。
//!
//! 用真进程内的真 hub（QUIC）+ 真 HTTP，不 mock。

use std::time::Duration;

use serde_json::Value;
use wxlike_core::Envelope;
use wxlike_net::Client;
use wxlike_server::{start_stack, StackOptions};

/// 必须是**文件**库：`db_path: None` 时 Platform 与 hub 的 MessageStore
/// 各自开一个独立的内存库，镜像写进 hub 那份、接口读 Platform 那份，
/// 永远读到 0。生产里两者是同一个文件，测试也必须这样。
fn opts(name: &str) -> StackOptions {
    // 每个测试一个**独立目录**：Tantivy 的 search-index 建在库文件同级目录，
    // 两个测试共用 %TEMP% 会撞索引锁（LockBusy），而且库也会互相干扰。
    let dir = std::env::temp_dir().join(format!("wxlike-mirror-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("wxlike.db");
    StackOptions {
        hub_id: "mirror".into(),
        db_path: Some(path.to_string_lossy().to_string()),
        mq_mode: "memory".into(),
        http_bind: "127.0.0.1:0".parse().unwrap(),
        quic_bind: "127.0.0.1:0".parse().unwrap(),
    }
}

fn register(base: &str, name: &str) -> (String, String) {
    let reply: Value = ureq::post(&format!("{base}/api/register"))
        .send_json(serde_json::json!({"name": name, "password": "pw"}))
        .unwrap()
        .into_json()
        .unwrap();
    (
        reply["user_id"].as_str().unwrap().to_string(),
        reply["token"].as_str().unwrap().to_string(),
    )
}

/// 建立好友关系。P3 的陌生人闸门会直接丢弃非好友的直发帧，
/// 不先加好友就发消息，连落库都不会发生——测试会看到未读恒为 0。
fn befriend(base: &str, a_tok: &str, b_uid: &str, b_tok: &str) {
    ureq::post(&format!("{base}/api/friends/requests"))
        .set("Authorization", &format!("Bearer {a_tok}"))
        .send_json(serde_json::json!({"to_user_id": b_uid, "message": ""}))
        .unwrap();
    let incoming: Value = ureq::get(&format!("{base}/api/friends/requests/incoming"))
        .set("Authorization", &format!("Bearer {b_tok}"))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    let id = incoming[0]["id"].as_str().unwrap().to_string();
    ureq::post(&format!("{base}/api/friends/requests/{id}/accept"))
        .set("Authorization", &format!("Bearer {b_tok}"))
        .call()
        .unwrap();
}

/// 轮询直到未读达到期望值（最多 5s）。
///
/// 固定 sleep 在慢 runner 上会 flake：CI 的 Windows 任务就出现过
/// "两条消息只镜像了一条"——不是逻辑错，是 400ms 不够。等状态而不是等时间。
fn wait_unread(base: &str, conv: &str, token: &str, want: u64) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut last = unread(base, conv, token);
    while last["unread"].as_u64() != Some(want) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        last = unread(base, conv, token);
    }
    last
}

fn unread(base: &str, conv: &str, token: &str) -> Value {
    ureq::get(&format!("{base}/api/conv/{conv}/unread"))
        .set("Authorization", &format!("Bearer {token}"))
        .call()
        .unwrap()
        .into_json()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn direct_message_lands_in_the_mailbox() {
    let stack = start_stack(opts("direct")).await.expect("stack");
    let base = format!("http://{}", stack.http_addr);
    let (alice, ta) = register(&base, "alice");
    let (bob, tb) = register(&base, "bob");
    befriend(&base, &ta, &bob, &tb);
    let conv = format!("d-{alice}~{bob}");

    // 两个客户端连上 hub
    let alice_c = Client::connect(stack.quic_addr, "localhost", &alice)
        .await
        .expect("alice connect");
    let bob_c = Client::connect(stack.quic_addr, "localhost", &bob)
        .await
        .expect("bob connect");
    // 让 hello 帧先走完，避免后续帧被当成未注册连接
    tokio::time::sleep(Duration::from_millis(150)).await;

    // 发两条直聊
    for text in ["hi", "again"] {
        let env = Envelope::direct(&conv, alice.clone(), vec![bob.clone()], 0, text);
        alice_c.send(&env).await.expect("send");
    }
    // 等 hub 处理完（QUIC 投递 + 落库 + 镜像）——等状态，不等时间
    let u = wait_unread(&base, &conv, &tb, 2);
    assert_eq!(u["owner"], "direct", "{u}");
    assert_eq!(u["unread"], 2, "两条直聊必须计入未读（镜像到信箱）：{u}");
    assert_eq!(u["head_seq"], 2, "{u}");

    // 读完归零
    let after: Value = ureq::post(&format!("{base}/api/conv/{conv}/read"))
        .set("Authorization", &format!("Bearer {tb}"))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(after["unread"], 0, "{after}");

    // 发送方自己的游标不受影响
    let a = unread(&base, &conv, &ta);
    assert_eq!(a["unread"], 2, "alice 没读，仍是 2：{a}");

    drop((alice_c, bob_c));
}

/// 撤回要同时撤掉信箱里的那份，否则未读与时间线会不一致。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recall_withdraws_from_the_mailbox_too() {
    let stack = start_stack(opts("recall")).await.expect("stack");
    let base = format!("http://{}", stack.http_addr);
    let (alice, ta) = register(&base, "alice");
    let (bob, tb) = register(&base, "bob");
    befriend(&base, &ta, &bob, &tb);
    let conv = format!("d-{alice}~{bob}");

    let alice_c = Client::connect(stack.quic_addr, "localhost", &alice)
        .await
        .unwrap();
    let _bob_c = Client::connect(stack.quic_addr, "localhost", &bob)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;

    let env = Envelope::direct(&conv, alice.clone(), vec![bob.clone()], 0, "oops");
    let msg_id = env.msg_id;
    alice_c.send(&env).await.expect("send");
    assert_eq!(wait_unread(&base, &conv, &tb, 1)["unread"], 1);

    alice_c
        .send(&Envelope::recall(&conv, alice.clone(), msg_id))
        .await
        .expect("recall");
    tokio::time::sleep(Duration::from_millis(300)).await;

    // 撤回后时间线里那条变成占位，但仍占一个序列号——未读数不因此回退，
    // 关键是信箱里那条确实被标记为撤回（消息体被覆盖）。
    let u = unread(&base, &conv, &tb);
    assert_eq!(u["head_seq"], 1, "序列号不回退：{u}");
}

/// P3 步骤 3 的关键一步：**history 的顺序由信箱决定**。
///
/// 客户端拿到的 `seq` 必须是会话内序列号（1,2,3…），而不是归档表的全局
/// AUTOINCREMENT——否则前端翻页游标与未读/同步用的编号是两套，永远对不上。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn history_pages_by_mailbox_seq() {
    let stack = start_stack(opts("history")).await.expect("stack");
    let base = format!("http://{}", stack.http_addr);
    let (alice, ta) = register(&base, "alice");
    let (bob, tb) = register(&base, "bob");
    befriend(&base, &ta, &bob, &tb);
    let conv = format!("d-{alice}~{bob}");

    let alice_c = Client::connect(stack.quic_addr, "localhost", &alice)
        .await
        .unwrap();
    let _bob_c = Client::connect(stack.quic_addr, "localhost", &bob)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;

    for i in 0..3 {
        let env = Envelope::direct(&conv, alice.clone(), vec![bob.clone()], 0, format!("m{i}"));
        alice_c.send(&env).await.expect("send");
    }
    assert_eq!(wait_unread(&base, &conv, &tb, 3)["unread"], 3);

    let page: Value = ureq::get(&format!("{base}/api/history/{conv}?limit=50"))
        .set("Authorization", &format!("Bearer {tb}"))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    let msgs = page["messages"].as_array().expect("messages");
    assert_eq!(msgs.len(), 3, "{page}");
    let seqs: Vec<i64> = msgs.iter().map(|m| m["seq"].as_i64().unwrap()).collect();
    assert_eq!(
        seqs,
        vec![1, 2, 3],
        "客户端看到的应是会话内序列号，不是全局 seq：{page}"
    );
    assert_eq!(msgs[0]["text"], "m0", "{page}");

    // 往前翻一页：before_seq 也是信箱序列号
    let page: Value = ureq::get(&format!("{base}/api/history/{conv}?before_seq=2"))
        .set("Authorization", &format!("Bearer {tb}"))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    let msgs = page["messages"].as_array().expect("messages");
    assert_eq!(msgs.len(), 1, "before_seq=2 之前只有第 1 条：{page}");
    assert_eq!(msgs[0]["seq"], 1, "{page}");
}

/// 扇出到客户端的帧必须带**信箱序列号**（1,2,3…），不是发送方填的 0。
///
/// 这是"顺序权威"的最后一环：HTTP 历史、未读、同步都用信箱序列号，
/// 如果实时帧还是 0，客户端只能按到达顺序排，多端之间就会不一致。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fanout_carries_mailbox_seq() {
    let stack = start_stack(opts("fanout-seq")).await.expect("stack");
    let base = format!("http://{}", stack.http_addr);
    let (alice, ta) = register(&base, "alice");
    let (bob, tb) = register(&base, "bob");
    befriend(&base, &ta, &bob, &tb);
    let conv = format!("d-{alice}~{bob}");

    let alice_c = Client::connect(stack.quic_addr, "localhost", &alice)
        .await
        .unwrap();
    let bob_c = Client::connect(stack.quic_addr, "localhost", &bob)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;

    for text in ["first", "second"] {
        let env = Envelope::direct(&conv, alice.clone(), vec![bob.clone()], 0, text);
        alice_c.send(&env).await.expect("send");
    }

    let mut got: Vec<(u64, String)> = Vec::new();
    while got.len() < 2 {
        let env = tokio::time::timeout(Duration::from_secs(5), bob_c.next_envelope())
            .await
            .expect("等不到帧")
            .expect("读帧失败");
        if let wxlike_core::MsgBody::Text(t) = &env.body {
            if env.conv_id == conv {
                got.push((env.seq, t.clone()));
            }
        }
    }
    assert_eq!(
        got,
        vec![(1, "first".to_string()), (2, "second".to_string())],
        "实时帧应带信箱序列号"
    );
}

/// 群消息的收件人来自**信箱的成员快照**（不是平台成员表）。
///
/// 这条测试同时验证两件事：
/// 1. 建群时平台把成员写进了信箱（否则这里会退回平台成员表，虽然也能通，
///    但那就说明步骤 5 没生效）；
/// 2. 扇出确实按信箱名单投递——bob 收到、并带信箱序列号。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn group_fans_out_from_mailbox_members() {
    let stack = start_stack(opts("group")).await.expect("stack");
    let base = format!("http://{}", stack.http_addr);
    let (alice, ta) = register(&base, "alice");
    let (bob, tb) = register(&base, "bob");

    let group = stack
        .platform
        .create_group(&alice, "team", std::slice::from_ref(&bob))
        .expect("group");

    // 成员快照已经写进信箱——步骤 5 的前提
    let snap = stack
        .platform
        .message_store()
        .expect("mailbox")
        .members((wxlike_message::OwnerType::Group, &group))
        .expect("members");
    assert!(snap.contains(&alice) && snap.contains(&bob), "{snap:?}");

    let alice_c = Client::connect(stack.quic_addr, "localhost", &alice)
        .await
        .unwrap();
    let bob_c = Client::connect(stack.quic_addr, "localhost", &bob)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;

    // 群消息不带显式收件人：由成员名单决定扇出
    let env = Envelope::direct(&group, alice.clone(), Vec::new(), 0, "hello group");
    alice_c.send(&env).await.expect("send");

    let got = tokio::time::timeout(Duration::from_secs(5), bob_c.next_envelope())
        .await
        .expect("bob 应收到群消息")
        .expect("read");
    assert_eq!(got.conv_id, group);
    assert_eq!(got.seq, 1, "群消息也应带信箱序列号");
    if let wxlike_core::MsgBody::Text(t) = &got.body {
        assert_eq!(t, "hello group");
    } else {
        panic!("expected text");
    }

    // 未读同样按信箱算
    assert_eq!(wait_unread(&base, &group, &tb, 1)["unread"], 1);
    let _ = ta;
}
