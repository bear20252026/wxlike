//! Shared harness for the UI integration tests: the real server stack and
//! per-test TOFU pin dirs. `ui_commands.rs` keeps its own copy of `stack`
//! (same shape) because each test binary links its own harness.

#![allow(dead_code)] // 共享测试夹具：各测试二进制只用到其中一部分
use std::sync::Arc;
use std::time::Duration;

use wxlike_platform::Platform;
use wxlike_ui_lib::AppState;

/// Boots a fresh real server stack per test: platform HTTP + QUIC hub with
/// storage + video-service. Broadcasts on a shared hub would leak one test's
/// traffic into another's event stream, so isolation beats amortized boot.
pub async fn stack() -> (String, std::net::SocketAddr, Arc<Platform>) {
    let stack = wxlike_server::start_stack(wxlike_server::StackOptions::default())
        .await
        .unwrap();
    (
        format!("http://{}", stack.http_addr),
        stack.quic_addr,
        stack.platform.clone(),
    )
}

/// Distinct usernames per test would matter only on a shared platform.
pub fn who_name(base: &str) -> String {
    base.to_string()
}

/// Unique pin dir per call: each test gets a fresh hub certificate, so TOFU
/// pins must not leak across tests in one process.
pub fn pins() -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("wxlike-ui-pins-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("pins.json")
}

pub fn hex_id(v: &serde_json::Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

/// A connected pair sharing one conversation (the common fixture).
pub async fn connected_pair() -> (
    String,
    std::net::SocketAddr,
    Arc<Platform>,
    AppState,
    AppState,
    String,
) {
    let (http, quic, platform) = stack().await;
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
    // hub 只认注册返回的会话凭据，不认 user_id（issue #3）
    alice
        .connect_hub(&quic.to_string(), &alice.auth_token().unwrap(), &pins())
        .unwrap();
    bob.connect_hub(&quic.to_string(), &bob.auth_token().unwrap(), &pins())
        .unwrap();
    befriend(&alice, &bob_id, &bob).await;
    let conv = wxlike_ui_lib::direct_conv_id(&alice_id, &bob_id);
    (http, quic, platform, alice, bob, conv)
}

/// Bounded wait for the next event (tests fail with a clear message, not a
/// silent hang).
pub async fn next_event(state: &AppState, what: &str) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(15), state.next_event())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .expect("connection alive")
        .expect("event arrives")
}

/// 成为好友（P3 陌生人闸门只放行好友/已接受的陌生人）。
///
/// 顺手把"成为好友"的系统通知读掉：它也走 hub，留着会抢走调用方的
/// `next_event()`。
pub async fn befriend(a: &AppState, b_id: &str, b: &AppState) {
    a.friend_request(b_id, "test").unwrap();
    let incoming = b.friend_requests_incoming().unwrap();
    let id = incoming
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|r| r["id"].as_str())
        .unwrap_or_else(|| panic!("好友请求没到对方手里：{incoming}"))
        .to_string();
    b.friend_accept(&id).unwrap();
    for who in [a, b] {
        while let Ok(Ok(Some(_))) =
            tokio::time::timeout(Duration::from_millis(200), who.next_event()).await
        {}
    }
}
