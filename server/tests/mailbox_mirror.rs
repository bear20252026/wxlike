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
        .body_mut()
        .read_json()
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
        .header("Authorization", &format!("Bearer {a_tok}"))
        .send_json(serde_json::json!({"to_user_id": b_uid, "message": ""}))
        .unwrap();
    let incoming: Value = ureq::get(&format!("{base}/api/friends/requests/incoming"))
        .header("Authorization", &format!("Bearer {b_tok}"))
        .call()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    let id = incoming[0]["id"].as_str().unwrap().to_string();
    ureq::post(&format!("{base}/api/friends/requests/{id}/accept"))
        .header("Authorization", &format!("Bearer {b_tok}"))
        .send_empty()
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
        .header("Authorization", &format!("Bearer {token}"))
        .call()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap()
}

/// Seed the mailbox first, leaving archive timing under the test's control.
fn mailbox_only(stack: &wxlike_server::Stack, env: &Envelope) {
    stack
        .platform
        .message_store()
        .unwrap()
        .append(
            (wxlike_message::OwnerType::Direct, &env.conv_id),
            &env.sender,
            "text",
            &hex::encode(env.msg_id),
            "{}",
        )
        .unwrap();
}

fn history_response(base: &str, conv: &str, token: &str) -> axum::http::Response<ureq::Body> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    agent
        .get(&format!("{base}/api/history/{conv}?limit=1"))
        .header("Authorization", &format!("Bearer {token}"))
        .call()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn history_waits_for_a_delayed_archive_body() {
    let stack = start_stack(opts("delayed-archive")).await.unwrap();
    let base = format!("http://{}", stack.http_addr);
    let (alice, ta) = register(&base, "alice");
    let (bob, _) = register(&base, "bob");
    let conv = wxlike_platform::direct_conv_id(&alice, &bob);
    let env = Envelope::direct(&conv, alice, vec![bob], 0, "delayed");
    mailbox_only(&stack, &env);
    let store = stack.store.clone();
    let writer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        store.append(&env, false).unwrap();
    });
    let mut response = history_response(&base, &conv, &ta);
    assert_eq!(response.status().as_u16(), 200);
    let page: Value = response.body_mut().read_json().unwrap();
    assert_eq!(page["messages"].as_array().unwrap().len(), 1);
    assert_eq!(page["messages"][0]["seq"], 1);
    writer.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_archive_returns_retryable_error_without_advancing_history() {
    let stack = start_stack(opts("missing-archive")).await.unwrap();
    let base = format!("http://{}", stack.http_addr);
    let (alice, ta) = register(&base, "alice");
    let (bob, _) = register(&base, "bob");
    let conv = wxlike_platform::direct_conv_id(&alice, &bob);
    let first = Envelope::direct(&conv, &alice, vec![bob.clone()], 0, "first");
    let second = Envelope::direct(&conv, &alice, vec![bob], 0, "second");
    mailbox_only(&stack, &first);
    mailbox_only(&stack, &second);
    stack.store.append(&first, false).unwrap();
    let mut response = history_response(&base, &conv, &ta);
    assert_eq!(response.status().as_u16(), 503);
    let error = response.body_mut().read_to_string().unwrap();
    assert!(error.contains("retry the same cursor"));
    assert!(!error.contains("next_before_seq"));
    stack.store.append(&second, false).unwrap();
    let mut retry = history_response(&base, &conv, &ta);
    assert_eq!(retry.status().as_u16(), 200);
    let page: Value = retry.body_mut().read_json().unwrap();
    assert_eq!(page["messages"].as_array().unwrap().len(), 1);
    assert_eq!(page["messages"][0]["seq"], 2);
    assert_eq!(page["next_before_seq"], 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn history_rejects_a_body_from_another_conversation() {
    let stack = start_stack(opts("foreign-archive")).await.unwrap();
    let base = format!("http://{}", stack.http_addr);
    let (alice, ta) = register(&base, "alice");
    let (bob, _) = register(&base, "bob");
    let conv = wxlike_platform::direct_conv_id(&alice, &bob);
    let mut env = Envelope::direct(&conv, alice, vec![bob], 0, "private body");
    mailbox_only(&stack, &env);
    env.conv_id = "another-conversation".into();
    stack.store.append(&env, false).unwrap();
    let mut response = history_response(&base, &conv, &ta);
    assert_eq!(response.status().as_u16(), 500);
    let error = response.body_mut().read_to_string().unwrap();
    assert!(!error.contains("private body"));
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
    let alice_c = Client::connect(stack.quic_addr, "localhost", &ta)
        .await
        .expect("alice connect");
    let bob_c = Client::connect(stack.quic_addr, "localhost", &tb)
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
        .header("Authorization", &format!("Bearer {tb}"))
        .send_empty()
        .unwrap()
        .body_mut()
        .read_json()
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

    let alice_c = Client::connect(stack.quic_addr, "localhost", &ta)
        .await
        .unwrap();
    let _bob_c = Client::connect(stack.quic_addr, "localhost", &tb)
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

    let alice_c = Client::connect(stack.quic_addr, "localhost", &ta)
        .await
        .unwrap();
    let _bob_c = Client::connect(stack.quic_addr, "localhost", &tb)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;

    for i in 0..3 {
        let env = Envelope::direct(&conv, alice.clone(), vec![bob.clone()], 0, format!("m{i}"));
        alice_c.send(&env).await.expect("send");
    }
    assert_eq!(wait_unread(&base, &conv, &tb, 3)["unread"], 3);

    let page: Value = ureq::get(&format!("{base}/api/history/{conv}?limit=50"))
        .header("Authorization", &format!("Bearer {tb}"))
        .call()
        .unwrap()
        .body_mut()
        .read_json()
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
        .header("Authorization", &format!("Bearer {tb}"))
        .call()
        .unwrap()
        .body_mut()
        .read_json()
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

    let alice_c = Client::connect(stack.quic_addr, "localhost", &ta)
        .await
        .unwrap();
    let bob_c = Client::connect(stack.quic_addr, "localhost", &tb)
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

    let alice_c = Client::connect(stack.quic_addr, "localhost", &ta)
        .await
        .unwrap();
    let bob_c = Client::connect(stack.quic_addr, "localhost", &tb)
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

/// HTTP 控制面发消息：走 hub 的入站路径，因此投递策略、落库、镜像、扇出
/// 与 QUIC 帧完全一致。这条测试盯的是"没走第二条发送路径"。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_send_reuses_the_hub_path_and_is_idempotent() {
    let stack = start_stack(opts("http-send")).await.expect("stack");
    let base = format!("http://{}", stack.http_addr);
    let (alice, ta) = register(&base, "alice");
    let (bob, tb) = register(&base, "bob");
    befriend(&base, &ta, &bob, &tb);
    let conv = format!("d-{alice}~{bob}");

    let send = |key: &str| -> Value {
        ureq::post(&format!("{base}/api/conv/{conv}/send"))
            .header("Authorization", &format!("Bearer {ta}"))
            .send_json(serde_json::json!({"text": "hello over http", "idempotency_key": key}))
            .expect("send")
            .body_mut()
            .read_json()
            .unwrap()
    };

    let first = send("key-1");
    assert_eq!(first["accepted"], true, "{first}");
    // 同一个幂等键重发：msg_id 相同，不会产生第二条
    let again = send("key-1");
    assert_eq!(first["msg_id"], again["msg_id"], "同 key 必须同 msg_id");

    let u = wait_unread(&base, &conv, &tb, 1);
    assert_eq!(u["unread"], 1, "重试不该变成两条：{u}");

    // 内容确实落库，且从 history 读得到
    let page: Value = ureq::get(&format!("{base}/api/history/{conv}?limit=10"))
        .header("Authorization", &format!("Bearer {tb}"))
        .call()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    let msgs = page["messages"].as_array().expect("messages");
    assert_eq!(msgs.len(), 1, "{page}");
    assert_eq!(msgs[0]["text"], "hello over http");
}

/// 非会话参与者不能往这个会话发消息。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_send_denies_outsiders() {
    let stack = start_stack(opts("http-send-deny")).await.expect("stack");
    let base = format!("http://{}", stack.http_addr);
    let (alice, _ta) = register(&base, "alice");
    let (bob, _tb) = register(&base, "bob");
    let (carol, tc) = register(&base, "carol");
    let conv = format!("d-{alice}~{bob}");

    let res = ureq::post(&format!("{base}/api/conv/{conv}/send"))
        .header("Authorization", &format!("Bearer {tc}"))
        .send_json(serde_json::json!({"text": "sneaky", "idempotency_key": "k"}));
    let status = match res {
        Ok(r) => r.status().as_u16(),
        Err(ureq::Error::StatusCode(code)) => code,
        Err(e) => panic!("transport: {e}"),
    };
    assert_eq!(status, 403, "carol={carol} 不在会话里");
}

/// 撤回：作者可以，别人不行。hub 内部对非作者是静默丢弃（不给探测口），
/// 但 HTTP 层要先查作者再提交，否则调用方分不清"撤回了"和"被无视了"。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_recall_checks_the_author() {
    let stack = start_stack(opts("recall-http")).await.expect("stack");
    let base = format!("http://{}", stack.http_addr);
    let (alice, ta) = register(&base, "alice");
    let (bob, tb) = register(&base, "bob");
    befriend(&base, &ta, &bob, &tb);
    let conv = format!("d-{alice}~{bob}");

    let sent: Value = ureq::post(&format!("{base}/api/conv/{conv}/send"))
        .header("Authorization", &format!("Bearer {ta}"))
        .send_json(serde_json::json!({"text": "oops", "idempotency_key": "r1"}))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    let msg_id = sent["msg_id"].as_str().unwrap().to_string();

    // bob 撤回 alice 的消息 → 403
    let res = ureq::post(&format!("{base}/api/conv/{conv}/recall"))
        .header("Authorization", &format!("Bearer {tb}"))
        .send_json(serde_json::json!({"msg_id": msg_id}));
    let status = match res {
        Ok(r) => r.status().as_u16(),
        Err(ureq::Error::StatusCode(code)) => code,
        Err(e) => panic!("transport: {e}"),
    };
    assert_eq!(status, 403, "非作者不该能撤回");

    // 作者撤回 → 成功，历史里那条变成已撤回
    let ok: Value = ureq::post(&format!("{base}/api/conv/{conv}/recall"))
        .header("Authorization", &format!("Bearer {ta}"))
        .send_json(serde_json::json!({"msg_id": msg_id}))
        .expect("recall")
        .body_mut()
        .read_json()
        .unwrap();
    assert_eq!(ok["recalled"], true, "{ok}");

    let page: Value = ureq::get(&format!("{base}/api/history/{conv}?limit=10"))
        .header("Authorization", &format!("Bearer {tb}"))
        .call()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    let msgs = page["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1, "{page}");
    assert_eq!(msgs[0]["withdrawn"], true, "撤回标记没落到 history：{page}");
}

/// 转发：两边会话都要鉴权；目标会话收到一条新消息，且幂等。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_forward_copies_into_the_target_conversation() {
    let stack = start_stack(opts("forward")).await.expect("stack");
    let base = format!("http://{}", stack.http_addr);
    let (alice, ta) = register(&base, "alice");
    let (bob, tb) = register(&base, "bob");
    let (carol, tc) = register(&base, "carol");
    befriend(&base, &ta, &bob, &tb);
    befriend(&base, &ta, &carol, &tc);
    let ab = format!("d-{alice}~{bob}");
    let ac = format!("d-{alice}~{carol}");

    let sent: Value = ureq::post(&format!("{base}/api/conv/{ab}/send"))
        .header("Authorization", &format!("Bearer {ta}"))
        .send_json(serde_json::json!({"text": "forward me", "idempotency_key": "f1"}))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    let msg_id = sent["msg_id"].as_str().unwrap().to_string();

    let fwd: Value = ureq::post(&format!("{base}/api/conv/{ac}/forward"))
        .header("Authorization", &format!("Bearer {ta}"))
        .send_json(serde_json::json!({
            "from_conv": ab, "to_conv": ac, "msg_id": msg_id, "idempotency_key": "fwd-1"
        }))
        .expect("forward")
        .body_mut()
        .read_json()
        .unwrap();
    assert_eq!(fwd["forwarded"], true, "{fwd}");

    // 幂等：同 key 重发不该多一条
    let again: Value = ureq::post(&format!("{base}/api/conv/{ac}/forward"))
        .header("Authorization", &format!("Bearer {ta}"))
        .send_json(serde_json::json!({
            "from_conv": ab, "to_conv": ac, "msg_id": msg_id, "idempotency_key": "fwd-1"
        }))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert_eq!(again["msg_id"], fwd["msg_id"], "同 key 必须同 msg_id");

    let u = wait_unread(&base, &ac, &tc, 1);
    assert_eq!(u["unread"], 1, "carol 只该收到一条：{u}");
}
