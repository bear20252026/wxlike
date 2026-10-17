//! WP-11 A2: the command layer is exercised against a REAL server — an
//! in-process platform (axum + SQLite) and a live QUIC hub. Mocks are banned
//! at this boundary by the task book; these tests drive the same `AppState`
//! methods the Tauri commands call.

use std::sync::Arc;
use std::time::Duration;

use wxlike_platform::{Platform, PlatformState};
use wxlike_ui_lib::AppState;

/// Boots a real platform HTTP server + QUIC hub on OS-assigned ports.
async fn stack() -> (String, std::net::SocketAddr, Arc<Platform>) {
    let platform: PlatformState = Platform::in_memory().unwrap().shared();
    let app = Platform::router(platform.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let router = wxlike_mq::MqRouter::new(
        Arc::new(wxlike_mq::memory::MemoryMq::new()),
        wxlike_mq::memory::MemoryRoutes::new(),
    );
    let hub = wxlike_net::run_mq_hub("ui-test-hub", "127.0.0.1:0".parse().unwrap(), router)
        .await
        .unwrap();
    let quic = hub.local_addr().unwrap();
    // keep the hub alive for the whole test process
    std::mem::forget(hub);

    (format!("http://{addr}"), quic, platform)
}

fn pins() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("wxlike-ui-pins-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("pins.json")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auth_register_login_and_profile() {
    let (http, _quic, _platform) = stack().await;
    let state = AppState::new(&http);

    let reply = state.register("alice", "s3cret").unwrap();
    assert!(!reply["token"].as_str().unwrap().is_empty());
    assert_eq!(reply["user_id"].as_str().unwrap().len(), 16);

    let me = state.me().unwrap();
    assert_eq!(me["name"], "alice");

    // wrong password is refused and leaves no session behind
    let fresh = AppState::new(&http);
    let err = fresh.login("alice", "wrong").unwrap_err();
    assert!(
        err.contains("401") || err.to_lowercase().contains("unauthorized"),
        "{err}"
    );

    // profile round trip through the command layer
    state
        .update_profile(Some("alice2"), None, Some("hi there"))
        .unwrap();
    let me = state.me().unwrap();
    assert_eq!(me["name"], "alice2");
    assert_eq!(me["signature"], "hi there");

    // duplicate registration is a conflict the UI can show. The profile was
    // renamed to alice2 above, so THAT is the live (taken) name now.
    let other = AppState::new(&http);
    let err = other.register("alice2", "x").unwrap_err();
    assert!(
        err.contains("409") || err.contains("already exists"),
        "{err}"
    );
    // and the freed name is registrable again (the old row was renamed)
    let third = AppState::new(&http);
    third.register("alice", "pw").unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn friend_request_accept_flow() {
    let (http, _quic, _platform) = stack().await;
    let alice = AppState::new(&http);
    let bob = AppState::new(&http);
    let bob_id = bob.register("bob", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice.register("alice", "pw").unwrap();

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
    let bob_id = bob.register("bob", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    let carol_id = AppState::new(&http).register("carol", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice.register("alice", "pw").unwrap();
    alice.friend_request(&bob_id, "").unwrap();
    let req = bob.friend_requests_incoming().unwrap().as_array().unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    bob.friend_accept(&req).unwrap();
    alice.friend_request(&carol_id, "").unwrap();
    let carol = AppState::new(&http);
    carol.login("carol", "pw").unwrap();
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
    let bob_id = bob.register("bob", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    let alice_id = alice.register("alice", "pw").unwrap()["user_id"]
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
    state.register("alice", "pw").unwrap();

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
    let bob_id = bob.register("bob", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice.register("alice", "pw").unwrap();

    // both connect to the real hub with TOFU
    alice
        .connect_hub(&quic.to_string(), &alice.user_id().unwrap(), &pins())
        .unwrap();
    bob.connect_hub(&quic.to_string(), &bob_id, &pins())
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
    other.register("mallory", "pw").unwrap();
    let err = other
        .connect_hub(&rogue.local_addr().unwrap().to_string(), "mallory", &pins())
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
