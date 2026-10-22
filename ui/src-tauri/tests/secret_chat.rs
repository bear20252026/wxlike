//! #15 秘密聊天闭环：密钥持久化（重启不换身份）、开关接入发送链、
//! 禁止转发、消失消息（TTL）不再复活。
//!
//! 这些测试跑**真实进程内服务端**（platform HTTP + QUIC hub + SQLite），
//! 不用 mock——秘密聊天的 bug 全都藏在"三端 + 存储 + 重启"的交界处。

use std::time::Duration;

use wxlike_ui_lib::AppState;

/// 每个测试一套全新的真实服务端。
async fn stack() -> (String, std::net::SocketAddr) {
    let stack = wxlike_server::start_stack(wxlike_server::StackOptions::default())
        .await
        .unwrap();
    (format!("http://{}", stack.http_addr), stack.quic_addr)
}

fn pins() -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("wxlike-secret-pins-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("pins.json")
}

/// 每个客户端一个数据目录：本地库 + 秘密聊天密钥/快照都在这里。
fn data_dir(tag: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "wxlike-secret-data-{}-{tag}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("local.db")
}

/// 成为好友（P3 陌生人闸门只放行好友），并把"成为好友"的系统通知读掉。
async fn befriend(a: &AppState, a_id: &str, b: &AppState, b_id: &str) {
    a.friend_request(b_id, "test").unwrap();
    let incoming = b.friend_requests_incoming().unwrap();
    let id = incoming
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|r| r["id"].as_str())
        .unwrap_or_else(|| panic!("{a_id} 的好友请求没到 {b_id} 手里：{incoming}"))
        .to_string();
    b.friend_accept(&id).unwrap();
    for who in [a, b] {
        while let Ok(Ok(Some(_))) =
            tokio::time::timeout(Duration::from_millis(200), who.next_event()).await
        {}
    }
}

/// #15：重启后仍是**同一个身份**，已有会话还能继续（密钥与棘轮落盘）。
///
/// 旧实现把 `Device` 只放在 `AppState` 的内存里：重启后 `secret_enable`
/// 造一个新身份，对方的安全码悄悄变化，历史全部解不开。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn secret_identity_and_session_survive_a_restart() {
    let (http, quic) = stack().await;
    let dir_a = data_dir("alice");
    let dir_b = data_dir("bob");
    let bob = AppState::new(&http);
    let bob_id = bob.register("bob", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    let alice = AppState::new(&http);
    let alice_id = alice.register("alice", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice.local_open(&dir_a).unwrap();
    bob.local_open(&dir_b).unwrap();
    alice
        .connect_hub(&quic.to_string(), &alice.auth_token().unwrap(), &pins())
        .unwrap();
    bob.connect_hub(&quic.to_string(), &bob.auth_token().unwrap(), &pins())
        .unwrap();
    befriend(&alice, &alice_id, &bob, &bob_id).await;

    let fp_a = alice.secret_enable().unwrap();
    bob.secret_enable().unwrap();
    let conv = wxlike_ui_lib::direct_conv_id(&alice_id, &bob_id);
    alice
        .send_secret_text(&conv, vec![bob_id.clone()], "before restart")
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(5), bob.next_event())
        .await
        .unwrap()
        .unwrap()
        .expect("第一条到达");
    assert_eq!(ev["text"], "before restart");

    // ---- 进程重启：新 AppState + 同一个数据目录 + 同一个凭据 ----
    let token = alice.auth_token().unwrap();
    drop(alice);
    let alice = AppState::new(&http);
    alice.set_auth_token(&token);
    alice.local_open(&dir_a).unwrap();
    assert_eq!(
        alice.secret_enable().unwrap(),
        fp_a,
        "重启后身份必须还是同一个（否则安全码会变）"
    );
    alice
        .connect_hub(&quic.to_string(), &token, &pins())
        .unwrap();

    // 已有会话继续用：这条不该再走 pre-key 握手，bob 照样解得开
    alice
        .send_secret_text(&conv, vec![bob_id.clone()], "after restart")
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(5), bob.next_event())
        .await
        .unwrap()
        .unwrap()
        .expect("重启后的消息到达");
    assert_eq!(ev["kind"], "e2ee");
    assert_eq!(ev["text"], "after restart", "棘轮状态没落盘就会解不开");
}

/// #15：秘密会话只收密文；明文与转发在客户端就被拒绝。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn secret_conversation_refuses_plaintext_and_forwarding() {
    let (http, quic) = stack().await;
    let bob = AppState::new(&http);
    let bob_id = bob.register("bob", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    let alice = AppState::new(&http);
    let alice_id = alice.register("alice", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice.local_open(&data_dir("alice")).unwrap();
    bob.local_open(&data_dir("bob")).unwrap();
    alice
        .connect_hub(&quic.to_string(), &alice.auth_token().unwrap(), &pins())
        .unwrap();
    bob.connect_hub(&quic.to_string(), &bob.auth_token().unwrap(), &pins())
        .unwrap();
    befriend(&alice, &alice_id, &bob, &bob_id).await;

    bob.secret_enable().unwrap();
    let conv = wxlike_ui_lib::direct_conv_id(&alice_id, &bob_id);
    // 开关落到服务端（以前只是前端一个 Set，服务端根本不知道）
    let state = alice.secret_configure(&conv, true, 0).unwrap();
    assert_eq!(state["enabled"], serde_json::json!(true));
    assert_eq!(alice.secret_state(&conv).unwrap()["enabled"], true);

    // 明文发送被拒
    let err = alice
        .send_text(&conv, vec![bob_id.clone()], "明文不该发出去")
        .expect_err("秘密会话必须拒绝明文");
    assert!(err.contains("加密"), "{err}");
    // 图片同样
    let err = alice
        .send_image(&conv, vec![bob_id.clone()], "media-1")
        .expect_err("秘密会话必须拒绝图片明文");
    assert!(err.contains("加密"), "{err}");

    // 加密发送照常
    alice
        .send_secret_text(&conv, vec![bob_id.clone()], "加密的")
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(5), bob.next_event())
        .await
        .unwrap()
        .unwrap()
        .expect("密文到达");
    assert_eq!(ev["text"], "加密的");

    // 转发：来源是秘密会话 → 拒绝
    let item = serde_json::json!({
        "conv_id": conv,
        "kind": "e2ee",
        "text": "加密的",
        "sender": alice_id,
    });
    let err = alice
        .forward_message(&conv, vec![bob_id.clone()], &item)
        .expect_err("秘密聊天的消息不能转发");
    assert!(err.contains("不能转发"), "{err}");
    let err = alice
        .merge_forward(&conv, vec![bob_id.clone()], &[item])
        .expect_err("合并转发同样要拦住");
    assert!(err.contains("不能转发"), "{err}");

    // 关掉之后普通消息恢复
    alice.secret_configure(&conv, false, 0).unwrap();
    alice
        .send_text(&conv, vec![bob_id.clone()], "普通消息")
        .unwrap();
}

/// #15：消失消息到点后服务端不再下发（重装/换设备拉历史不会复活）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn secret_ttl_hides_expired_history() {
    let st = wxlike_server::start_stack(wxlike_server::StackOptions::default())
        .await
        .unwrap();
    let http = format!("http://{}", st.http_addr);
    let alice = AppState::new(&http);
    let alice_id = alice.register("alice", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    let bob = AppState::new(&http);
    let bob_id = bob.register("bob", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice.local_open(&data_dir("alice")).unwrap();
    let conv = wxlike_ui_lib::direct_conv_id(&alice_id, &bob_id);

    // 直接往归档里放一条"两分钟前"的消息（信箱没有镜像行 → 走归档兜底路径）
    let mut env = wxlike_core::Envelope::with_body(
        &conv,
        &alice_id,
        vec![bob_id.clone()],
        1,
        wxlike_core::MsgBody::Text("两分钟前".into()),
    );
    env.sent_at_ms = wxlike_core::types::now_ms().saturating_sub(120_000);
    st.store.append(&env, false).unwrap();

    // 没设 TTL：历史照常可见
    let page = alice.load_history(&conv, 0, 50).unwrap();
    assert_eq!(
        page["messages"].as_array().unwrap().len(),
        1,
        "没有消失设置时不该隐藏：{page}"
    );

    // 设 30 秒 TTL：这条已经过期，不再下发
    alice.secret_configure(&conv, true, 30_000).unwrap();
    let page = alice.load_history(&conv, 0, 50).unwrap();
    assert!(
        page["messages"].as_array().unwrap().is_empty(),
        "过期的消息不能再出现在历史里：{page}"
    );
}
