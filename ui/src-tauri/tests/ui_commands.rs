//! WP-11 A2 + P1: the command layer is exercised against a REAL server — an
//! in-process stack (platform HTTP + SQLite + MQ hub with storage) and a live
//! QUIC hub. Mocks are banned at this boundary by the task book; these tests
//! drive the same `AppState` methods the Tauri commands call.

use std::sync::Arc;
use std::time::Duration;

use wxlike_platform::Platform;
use wxlike_ui_lib::AppState;

/// Boots a fresh real server stack per test: platform HTTP + QUIC hub **with
/// message storage**, so history/search/typing semantics are the deployed
/// ones. Per-test isolation is not optional here — wallet and friend events
/// are broadcast on the hub, so a shared hub would leak one test's traffic
/// into another's event stream. The hub + HTTP tasks live on this test's
/// runtime, which outlives the assertions.
async fn stack() -> (String, std::net::SocketAddr, Arc<Platform>) {
    let stack = wxlike_server::start_stack(wxlike_server::StackOptions::default())
        .await
        .unwrap();
    (
        format!("http://{}", stack.http_addr),
        stack.quic_addr,
        stack.platform.clone(),
    )
}

/// Distinct usernames per test would matter only on a shared platform; kept
/// so tests stay copy-pasteable if the stack ever becomes shared again.
fn who_name(base: &str) -> String {
    base.to_string()
}

/// Unique pin dir per call: each test gets a fresh hub with a fresh
/// certificate, so TOFU pins must not leak across tests in one process.
fn pins() -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("wxlike-ui-pins-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("pins.json")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auth_register_login_and_profile() {
    let (http, _quic, _platform) = stack().await;
    let state = AppState::new(&http);

    let reply = state.register(&who_name("alice"), "s3cret").unwrap();
    assert!(!reply["token"].as_str().unwrap().is_empty());
    assert_eq!(reply["user_id"].as_str().unwrap().len(), 16);

    let me = state.me().unwrap();
    assert_eq!(me["name"], serde_json::json!(who_name("alice")));

    // wrong password is refused and leaves no session behind
    let fresh = AppState::new(&http);
    let err = fresh.login(&who_name("alice"), "wrong").unwrap_err();
    assert!(
        err.contains("401") || err.to_lowercase().contains("unauthorized"),
        "{err}"
    );

    // profile round trip through the command layer
    state
        .update_profile(Some(&who_name("alice2")), None, Some("hi there"))
        .unwrap();
    let me = state.me().unwrap();
    assert_eq!(me["name"], serde_json::json!(who_name("alice2")));
    assert_eq!(me["signature"], "hi there");

    // duplicate registration is a conflict the UI can show. The profile was
    // renamed to alice2 above, so THAT is the live (taken) name now.
    let other = AppState::new(&http);
    let err = other.register(&who_name("alice2"), "x").unwrap_err();
    assert!(
        err.contains("409") || err.contains("already exists"),
        "{err}"
    );
    // and the freed name is registrable again (the old row was renamed)
    let third = AppState::new(&http);
    third.register(&who_name("alice"), "pw").unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn friend_request_accept_flow() {
    let (http, _quic, _platform) = stack().await;
    let alice = AppState::new(&http);
    let bob = AppState::new(&http);
    let bob_id = bob.register(&who_name("bob"), "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice.register(&who_name("alice"), "pw").unwrap();

    // alice applies; bob sees the incoming request and accepts
    alice.friend_request(&bob_id, "加个好友？").unwrap();
    let incoming = bob.friend_requests_incoming().unwrap();
    let arr = incoming.as_array().cloned().unwrap_or_default();
    assert_eq!(arr.len(), 1, "request lands in bob's inbox: {incoming}");
    let req_id = arr[0]["id"].as_str().unwrap().to_string();
    assert_eq!(
        arr[0]["from_uid"].as_str().unwrap(),
        alice.user_id().unwrap()
    );

    bob.friend_accept(&req_id).unwrap();
    let list = alice.friends().unwrap();
    let arr = list.as_array().cloned().unwrap_or_default();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["friend_id"].as_str().unwrap(), bob_id);

    // and the friendship is mutual
    let list = bob.friends().unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);

    // duplicate requests are rejected (409) instead of spamming
    let err = alice.friend_request(&bob_id, "again").unwrap_err();
    assert!(
        err.contains("409") || err.contains("already exists"),
        "{err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn group_create_list_and_info() {
    let (http, _quic, _platform) = stack().await;
    let alice = AppState::new(&http);
    let bob = AppState::new(&http);
    let bob_id = bob.register(&who_name("bob"), "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    let carol_id = AppState::new(&http)
        .register(&who_name("carol"), "pw")
        .unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice.register(&who_name("alice"), "pw").unwrap();
    alice.friend_request(&bob_id, "").unwrap();
    let req = bob.friend_requests_incoming().unwrap().as_array().unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    bob.friend_accept(&req).unwrap();
    alice.friend_request(&carol_id, "").unwrap();
    let carol = AppState::new(&http);
    carol.login(&who_name("carol"), "pw").unwrap();
    let req = carol
        .friend_requests_incoming()
        .unwrap()
        .as_array()
        .unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    carol.friend_accept(&req).unwrap();

    let group_id = alice
        .groups_create("三人行", &[bob_id.clone(), carol_id.clone()])
        .unwrap();
    assert_eq!(
        group_id.len(),
        16,
        "group ids are 16-hex like user ids: {group_id}"
    );

    let mine = alice.groups_mine().unwrap();
    assert!(mine.to_string().contains(&group_id), "{mine}");

    let info = alice.group_info(&group_id).unwrap();
    let members = info["members"].as_array().cloned().unwrap_or_default();
    assert_eq!(members.len(), 3, "creator + two invitees: {info}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wallet_transfer_redpacket_conservation() {
    let (http, _quic, platform) = stack().await;
    let alice = AppState::new(&http);
    let bob = AppState::new(&http);
    let bob_id = bob.register(&who_name("bob"), "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    let alice_id = alice.register(&who_name("alice"), "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();

    // faucet seam (same one the server's dev endpoint uses)
    platform.fund(&alice_id, 100_000, "test").unwrap();
    assert_eq!(alice.wallet_balance().unwrap(), 100_000);

    // transfer
    alice.wallet_transfer(&bob_id, 30_000, "午饭").unwrap();
    assert_eq!(alice.wallet_balance().unwrap(), 70_000);
    assert_eq!(bob.wallet_balance().unwrap(), 30_000);

    // red packet: total frozen at creation, claim splits it
    let rp = alice.wallet_redpacket(20_000, 2, "手气红包").unwrap();
    assert_eq!(
        alice.wallet_balance().unwrap(),
        50_000,
        "frozen on creation"
    );
    let claimed = bob.wallet_claim(&rp).unwrap();
    assert!(
        (1..=19_999).contains(&claimed),
        "a random share of 20_000 over 2: {claimed}"
    );
    assert_eq!(bob.wallet_balance().unwrap(), 30_000 + claimed);

    // conservation: balances + the unclaimed frozen remainder == funded total
    let frozen = 20_000 - claimed;
    let total = alice.wallet_balance().unwrap() + bob.wallet_balance().unwrap() + frozen;
    assert_eq!(total, 100_000);

    // over-spend is refused and rolls back cleanly
    let err = alice
        .wallet_transfer(&bob_id, 999_999_999, "豪")
        .unwrap_err();
    assert!(!err.is_empty());
    assert_eq!(alice.wallet_balance().unwrap(), 50_000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn media_upload_download_roundtrip() {
    let (http, _quic, _platform) = stack().await;
    let state = AppState::new(&http);
    state.register(&who_name("alice"), "pw").unwrap();

    // 4 KiB of pseudo-random bytes: the object store is content-addressed,
    // so the returned id must be the sha256 of exactly these bytes
    let mut bytes = vec![0u8; 4096];
    let mut seed = 0x9E3779B9u32;
    for b in &mut bytes {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *b = (seed >> 24) as u8;
    }

    let up = state
        .upload_media(bytes.clone(), "application/octet-stream", None)
        .unwrap();
    let media_id = up["media_id"].as_str().unwrap().to_string();
    assert_eq!(up["size"].as_i64().unwrap(), 4096);

    let blob = state.download_media(&media_id, false).unwrap();
    let decoded =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &blob.base64).unwrap();
    assert_eq!(decoded, bytes, "download returns exactly what was uploaded");
    assert_eq!(blob.content_type, "application/octet-stream");

    // content addressing dedups: the same bytes, uploaded again, same id
    let again = state
        .upload_media(bytes.clone(), "application/octet-stream", None)
        .unwrap();
    assert_eq!(again["media_id"].as_str().unwrap(), media_id);

    // unknown ids fail with a message, not a panic
    let err = state.download_media("does-not-exist", false).unwrap_err();
    assert!(err.contains("not found"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn send_and_receive_over_live_hub() {
    let (http, quic, _platform) = stack().await;
    let alice = AppState::new(&http);
    let bob = AppState::new(&http);
    let bob_id = bob.register(&who_name("bob"), "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice.register(&who_name("alice"), "pw").unwrap();

    // both clients in this test must share one pin store: mallory connects
    // AFTER alice pinned the honest hub's certificate, so the rogue hub under
    // the same name must be refused
    let shared_pins = pins();
    alice
        .connect_hub(&quic.to_string(), &alice.auth_token().unwrap(), &shared_pins)
        .unwrap();
    bob.connect_hub(&quic.to_string(), &bob.auth_token().unwrap(), &shared_pins)
        .unwrap();
    assert!(alice.is_connected() && bob.is_connected());

    let conv = wxlike_ui_lib::direct_conv_id(&alice.user_id().unwrap(), &bob_id);

    // text
    alice
        .send_text(&conv, vec![bob_id.clone()], "在吗")
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(5), bob.next_event())
        .await
        .unwrap()
        .unwrap()
        .expect("event arrives");
    assert_eq!(ev["kind"], "text");
    assert_eq!(ev["text"], "在吗");
    assert_eq!(ev["sender"], alice.user_id().unwrap());

    // image message carries the media reference, not the bytes (WP-05)
    alice
        .send_image(&conv, vec![bob_id.clone()], "deadbeef")
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(5), bob.next_event())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(ev["kind"], "image");
    assert_eq!(ev["media_id"], "deadbeef");

    // red packet message is a wallet.redpacket custom envelope
    alice
        .send_redpacket(&conv, vec![bob_id.clone()], "rp-1", 500, 2, "拿去")
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(5), bob.next_event())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(ev["kind"], "wallet.redpacket");
    assert_eq!(ev["payload"]["redpacket_id"], "rp-1");
    assert_eq!(ev["payload"]["amount"], 500);

    // tofu: a second hub under the same pinned name would be refused
    let router = wxlike_mq::MqRouter::new(
        Arc::new(wxlike_mq::memory::MemoryMq::new()),
        wxlike_mq::memory::MemoryRoutes::new(),
    );
    let rogue = wxlike_net::run_mq_hub("rogue", "127.0.0.1:0".parse().unwrap(), router)
        .await
        .unwrap();
    let other = AppState::new(&http);
    other.register(&who_name("mallory"), "pw").unwrap();
    let err = other
        .connect_hub(
            &rogue.local_addr().unwrap().to_string(),
            "mallory",
            &shared_pins,
        )
        .unwrap_err();
    assert!(
        err.contains("fingerprint changed") || err.to_lowercase().contains("handshake"),
        "the rogue hub is refused by pinning: {err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conversation_ids_are_deterministic() {
    assert_eq!(
        wxlike_ui_lib::direct_conv_id("b", "a"),
        wxlike_ui_lib::direct_conv_id("a", "b"),
        "both sides compute the same 1:1 conversation id"
    );
    assert!(wxlike_ui_lib::direct_conv_id("a", "b").starts_with("d-"));
}

// ---------------------------------------------------------------------------
// P1: five message operations + history + secret chat (against the real stack)
// ---------------------------------------------------------------------------

fn hex_id(v: &serde_json::Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p1_history_pagination_and_read_receipts() {
    let (http, quic, _platform) = stack().await;
    let alice = AppState::new(&http);
    let bob = AppState::new(&http);
    let bob_id = bob.register(&who_name("bob"), "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    let alice_id = alice.register(&who_name("alice"), "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice
        .connect_hub(&quic.to_string(), &alice.auth_token().unwrap(), &pins())
        .unwrap();
    bob.connect_hub(&quic.to_string(), &bob.auth_token().unwrap(), &pins())
        .unwrap();
    let conv = wxlike_ui_lib::direct_conv_id(&alice_id, &bob_id);

    // six messages; bob collects them live (and thus the msg ids)
    let mut bob_saw = Vec::new();
    for i in 0..6 {
        alice
            .send_text(&conv, vec![bob_id.clone()], &format!("page {i}"))
            .unwrap();
    }
    for _ in 0..6 {
        let ev = tokio::time::timeout(Duration::from_secs(5), bob.next_event())
            .await
            .unwrap()
            .unwrap()
            .expect("event");
        bob_saw.push(ev);
    }
    tokio::time::sleep(Duration::from_millis(400)).await;

    // page 1: newest 4 in ascending order, cursor set
    let page1 = alice.load_history(&conv, 0, 4).unwrap();
    let msgs1 = page1["messages"].as_array().unwrap();
    assert_eq!(msgs1.len(), 4);
    assert_eq!(msgs1[0]["text"], "page 2");
    assert_eq!(msgs1[3]["text"], "page 5");
    let cursor = page1["next_before_seq"].as_i64().unwrap();
    let page2 = alice.load_history(&conv, cursor, 4).unwrap();
    let msgs2 = page2["messages"].as_array().unwrap();
    assert_eq!(msgs2.len(), 2, "rest of the conversation");
    assert_eq!(msgs2[0]["text"], "page 0");
    assert!(page2["next_before_seq"].is_null());

    // read receipt: bob acks the newest message; alice's hub event is a
    // receipt naming that message
    let last_id = hex_id(&bob_saw[5]["msg_id"]);
    assert_eq!(bob_saw[5]["text"], "page 5");
    bob.send_read_receipt(&conv, &last_id).unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(5), alice.next_event())
        .await
        .unwrap()
        .unwrap()
        .expect("receipt event");
    assert_eq!(ev["kind"], "receipt");
    assert_eq!(ev["receipt"]["state"], "read");
    assert_eq!(hex_id(&ev["receipt"]["target"]), last_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p1_typing_is_transient() {
    let (http, quic, platform) = stack().await;
    let alice = AppState::new(&http);
    let bob = AppState::new(&http);
    let bob_id = bob.register(&who_name("bob"), "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    let alice_id = alice.register(&who_name("alice"), "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice
        .connect_hub(&quic.to_string(), &alice.auth_token().unwrap(), &pins())
        .unwrap();
    bob.connect_hub(&quic.to_string(), &bob.auth_token().unwrap(), &pins())
        .unwrap();
    let conv = wxlike_ui_lib::direct_conv_id(&alice_id, &bob_id);

    // typing reaches bob, then a real message follows. A typing frame sent
    // before the hub binds bob is correctly dropped (transient = never
    // parked), and a real client re-sends on every keystroke — so the test
    // retries the same way instead of assuming atomic binding.
    let mut ev = None;
    for _ in 0..10 {
        alice.send_typing(&conv, vec![bob_id.clone()]).unwrap();
        match tokio::time::timeout(Duration::from_secs(2), bob.next_event()).await {
            Ok(Ok(Some(e))) => {
                ev = Some(e);
                break;
            }
            _ => continue,
        }
    }
    let ev = ev.expect("typing arrives within the retry budget");
    assert_eq!(ev["kind"], "typing");

    alice
        .send_text(&conv, vec![bob_id.clone()], "actual content")
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), bob.next_event())
        .await
        .unwrap()
        .unwrap()
        .expect("content arrives");
    tokio::time::sleep(Duration::from_millis(300)).await;

    // server history contains the content and nothing else
    let bob_token = platform.create_session(&bob_id, 60_000).unwrap();
    let rows: serde_json::Value = ureq::get(&format!("{http}/api/history/{conv}"))
        .set("Authorization", &format!("Bearer {bob_token}"))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    let msgs = rows["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1, "typing left no stored row: {msgs:?}");
    assert_eq!(msgs[0]["text"], "actual content");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p1_forward_merge_and_favorites() {
    let (http, quic, _platform) = stack().await;
    let alice = AppState::new(&http);
    let bob = AppState::new(&http);
    let bob_id = bob.register(&who_name("bob"), "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    let alice_id = alice.register(&who_name("alice"), "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice
        .connect_hub(&quic.to_string(), &alice.auth_token().unwrap(), &pins())
        .unwrap();
    bob.connect_hub(&quic.to_string(), &bob.auth_token().unwrap(), &pins())
        .unwrap();
    let conv = wxlike_ui_lib::direct_conv_id(&alice_id, &bob_id);

    // original message arrives at bob's (for the id)…
    alice
        .send_text(&conv, vec![bob_id.clone()], "要被转发的消息")
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(5), bob.next_event())
        .await
        .unwrap()
        .unwrap()
        .expect("original arrives");
    let origin_id = hex_id(&ev["msg_id"]);

    // …alice forwards it (single) — bob sees msg.forward
    let snapshot = serde_json::json!({
        "origin_conv": conv,
        "origin_msg_id": origin_id,
        "kind": "text",
        "text": "要被转发的消息",
        "sender": alice_id,
    });
    alice
        .forward_message(&conv, vec![bob_id.clone()], &snapshot)
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(5), bob.next_event())
        .await
        .unwrap()
        .unwrap()
        .expect("forward arrives");
    assert_eq!(ev["kind"], "msg.forward");
    assert_eq!(ev["payload"]["origin_msg_id"], origin_id);

    // merge forward of two items
    let mut second = snapshot.clone();
    second["text"] = "第二条".into();
    let items = vec![snapshot.clone(), second];
    alice
        .merge_forward(&conv, vec![bob_id.clone()], &items)
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(5), bob.next_event())
        .await
        .unwrap()
        .unwrap()
        .expect("merge arrives");
    assert_eq!(ev["kind"], "msg.merge");
    assert_eq!(ev["payload"]["items"].as_array().unwrap().len(), 2);

    // empty merge is refused client-side
    assert!(alice
        .merge_forward(&conv, vec![bob_id.clone()], &[])
        .is_err());

    // favorite the forward, list it, remove it
    let fav = alice
        .favorite_add(&serde_json::json!({
            "source_conv": conv, "source_msg_id": origin_id,
            "kind": "text", "text": "要被转发的消息", "sender": alice_id,
        }))
        .unwrap();
    let fav_id = hex_id(&fav["id"]);
    let list = alice.favorites_list(10).unwrap();
    let rows = list["favorites"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(hex_id(&rows[0]["id"]), fav_id);
    alice.favorite_remove(&fav_id).unwrap();
    let list = alice.favorites_list(10).unwrap();
    assert!(list["favorites"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p1_secret_chat_roundtrip_through_real_stack() {
    let (http, quic, _platform) = stack().await;
    let alice = AppState::new(&http);
    let bob = AppState::new(&http);
    let bob_id = bob.register(&who_name("bob"), "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    let alice_id = alice.register(&who_name("alice"), "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice
        .connect_hub(&quic.to_string(), &alice.auth_token().unwrap(), &pins())
        .unwrap();
    bob.connect_hub(&quic.to_string(), &bob.auth_token().unwrap(), &pins())
        .unwrap();

    // both publish pre-key bundles, then alice opens a secret conversation
    let fp_a = alice.secret_enable().unwrap();
    let fp_b = bob.secret_enable().unwrap();
    assert_eq!(fp_a.len(), 43, "ed25519 fingerprint as base64: {fp_a}");
    assert_ne!(fp_a, fp_b);
    assert!(alice.secret_enabled() && bob.secret_enabled());

    let conv = wxlike_ui_lib::direct_conv_id(&alice_id, &bob_id);
    alice
        .send_secret_text(&conv, vec![bob_id.clone()], "s3cret payload")
        .unwrap();

    let ev = tokio::time::timeout(Duration::from_secs(5), bob.next_event())
        .await
        .unwrap()
        .unwrap()
        .expect("sealed message arrives");
    assert_eq!(ev["kind"], "e2ee");
    assert_eq!(ev["encrypted"], serde_json::json!(true));
    assert_eq!(ev["text"], "s3cret payload", "decrypted at the UI layer");
}

/// 秘密聊天在拿不到对方预密钥时**必须拒绝发送**，绝不退化成明文（issue #4）。
///
/// 这条测试原来叫 `..._falls_back_to_cleartext_without_peer_bundle`，断言的是
/// `ev["kind"] == "text"`——把"秘密模式发明文"固化成了契约。护栏变成了锚：
/// 想修这个 bug 得先改一条名字看起来像"保护降级行为"的测试。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p1_secret_chat_refuses_to_send_without_peer_bundle() {
    let (http, quic, _platform) = stack().await;
    let alice = AppState::new(&http);
    let bob = AppState::new(&http);
    let bob_id = bob.register(&who_name("bob"), "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    let alice_id = alice.register(&who_name("alice"), "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice
        .connect_hub(&quic.to_string(), &alice.auth_token().unwrap(), &pins())
        .unwrap();
    bob.connect_hub(&quic.to_string(), &bob.auth_token().unwrap(), &pins())
        .unwrap();

    // only alice enables E2EE; bob never publishes a bundle
    alice.secret_enable().unwrap();
    let conv = wxlike_ui_lib::direct_conv_id(&alice_id, &bob_id);

    let err = alice
        .send_secret_text(&conv, vec![bob_id.clone()], "must not leak")
        .expect_err("没有对方预密钥时，秘密聊天必须拒绝发送");
    assert!(
        err.contains("预密钥"),
        "错误要告诉用户为什么发不出去：{err}"
    );

    // 而且真的什么都没发：bob 一秒内不该收到任何事件，更不该收到明文
    let got = tokio::time::timeout(Duration::from_millis(900), bob.next_event()).await;
    assert!(
        got.is_err(),
        "拒绝发送之后不该有消息到达（收到任何东西都说明退化了）：{got:?}"
    );
}

/// 会话建立之后，**每一条**都必须继续加密（旧实现第 2 条就发明文）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p1_secret_chat_keeps_encrypting_after_the_first_message() {
    let (http, quic, _platform) = stack().await;
    let alice = AppState::new(&http);
    let bob = AppState::new(&http);
    let bob_id = bob.register(&who_name("bob"), "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    let alice_id = alice.register(&who_name("alice"), "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice
        .connect_hub(&quic.to_string(), &alice.auth_token().unwrap(), &pins())
        .unwrap();
    bob.connect_hub(&quic.to_string(), &bob.auth_token().unwrap(), &pins())
        .unwrap();

    alice.secret_enable().unwrap();
    bob.secret_enable().unwrap();
    let conv = wxlike_ui_lib::direct_conv_id(&alice_id, &bob_id);

    for i in 1..=3 {
        let text = format!("secret-{i}");
        alice
            .send_secret_text(&conv, vec![bob_id.clone()], &text)
            .unwrap_or_else(|e| panic!("第 {i} 条发送失败：{e}"));
        let ev = tokio::time::timeout(Duration::from_secs(5), bob.next_event())
            .await
            .unwrap()
            .unwrap()
            .unwrap_or_else(|| panic!("第 {i} 条没有到达"));
        assert_eq!(
            ev["kind"], "e2ee",
            "第 {i} 条必须是加密的——旧实现在这里有会话就发明文：{ev}"
        );
        assert_eq!(ev["text"], text, "第 {i} 条解密后的正文");
    }
}
