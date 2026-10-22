//! P1 history + search endpoints over the real stack (platform + hub + HTTP
//! in one process). Authorization is the point of these tests: a
//! conversation you are not part of must not be readable through either
//! endpoint, not even by guessing ids.

use std::time::Duration;

use ureq::http;
use wxlike_core::custom_kind;
use wxlike_core::{Envelope, MsgBody, MsgId};
use wxlike_net::Client;
use wxlike_server::{start_stack, StackOptions};

struct TestStack {
    http: String,
    quic: std::net::SocketAddr,
    platform: std::sync::Arc<wxlike_platform::Platform>,
    hub: wxlike_net::HubHandle,
}

/// One stack per test (broadcasts must not leak across tests; a shared
/// runtime would also die with the first test that finishes).
async fn boot() -> TestStack {
    {
        let stack = start_stack(StackOptions::default()).await.unwrap();
        TestStack {
            http: format!("http://{}", stack.http_addr),
            quic: stack.quic_addr,
            platform: stack.platform.clone(),
            hub: stack.hub.clone(),
        }
    }
}

fn register(stack: &TestStack, name: &str) -> (String, String) {
    let reply: serde_json::Value = ureq::post(&format!("{}/api/register", stack.http))
        .send_json(serde_json::json!({"name": name, "password": "pw"}))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    (
        reply["user_id"]
            .as_str()
            .unwrap_or_else(|| panic!("register {name} 失败：{reply}"))
            .to_string(),
        reply["token"].as_str().unwrap().to_string(),
    )
}

async fn connect(stack: &TestStack, user_id: &str) -> Client {
    Client::connect(stack.quic, "localhost", user_id)
        .await
        .unwrap()
}

/// `Client::connect` returns before the hub binds the stream; transient
/// frames are never parked, so tests that send them right after connecting
/// wait for the binding first (a real typing indicator is re-sent on every
/// keystroke, so an early lost ping is by design).
async fn wait_bound(stack: &TestStack, n: usize) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while stack.hub.online_users().await < n {
        assert!(
            std::time::Instant::now() < deadline,
            "hub never bound {n} users"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Distinct username prefix per test so the shared stack's users never collide.
fn who() -> String {
    std::thread::current()
        .name()
        .unwrap_or("t")
        .chars()
        .take(12)
        .collect()
}

fn conv(a: &str, b: &str) -> String {
    wxlike_platform::direct_conv_id(a, b)
}

/// ureq 2.x yields `Err(Status(code, ..))` for 4xx/5xx: normalize both
/// branches to the numeric status.
/// Eventual-consistency helper: the Tantivy writer commits in a background
/// thread (ADR-22 keeps fsync off the gateway), so tests poll for the
/// expected hit count instead of assuming synchronous visibility.
fn search_until(token: &str, q: &str, min_results: usize, base: &str) -> Vec<serde_json::Value> {
    let mut last: Option<serde_json::Value> = None;
    // 50 × 200ms = 10s 在慢 runner 上不够：CI 的 Windows 任务里，多个测试并发
    // 建 Tantivy writer，commit 明显滞后，实测 18s 才齐。断言的是"最终可见"，
    // 就该给它足够的时间——否则 flake 会把真实问题淹掉。
    for _ in 0..100 {
        let hits: serde_json::Value = ureq::get(&format!("{base}/api/search/messages?q={q}"))
            .header("Authorization", &format!("Bearer {token}"))
            .call()
            .unwrap()
            .body_mut()
            .read_json()
            .unwrap();
        let results = hits["results"].as_array().cloned().unwrap_or_default();
        if results.len() >= min_results {
            return results;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        last = Some(hits);
    }
    panic!(
        "search '{q}' never reached {min_results} hits; last response: {:?}",
        last.unwrap_or(serde_json::json!(null))
    );
}

/// 好友流程（不变式 9 生效后，直发帧要求好友或已接受陌生人）。
async fn befriend(stack: &TestStack, _a_uid: &str, a_tok: &str, b_uid: &str, b_tok: &str) {
    ureq::post(&format!("{}/api/friends/requests", stack.http))
        .header("Authorization", &format!("Bearer {a_tok}"))
        .send_json(serde_json::json!({"to_user_id": b_uid, "message": ""}))
        .unwrap();
    let incoming: serde_json::Value =
        ureq::get(&format!("{}/api/friends/requests/incoming", stack.http))
            .header("Authorization", &format!("Bearer {b_tok}"))
            .call()
            .unwrap()
            .body_mut()
            .read_json()
            .unwrap();
    let id = incoming[0]["id"].as_str().unwrap().to_string();
    ureq::post(&format!("{}/api/friends/requests/{id}/accept", stack.http))
        .header("Authorization", &format!("Bearer {b_tok}"))
        .send_empty()
        .unwrap();
}

fn status_of(res: Result<http::Response<ureq::Body>, ureq::Error>) -> u16 {
    match res {
        Ok(r) => r.status().as_u16(),
        Err(ureq::Error::StatusCode(code)) => code,
        Err(e) => panic!("transport error: {e}"),
    }
}

async fn send_text(client: &Client, conv_id: &str, to: Vec<String>, text: &str) -> MsgId {
    let env = Envelope::direct(conv_id, client.user_id.clone(), to, 0, text);
    client.send(&env).await.unwrap();
    env.msg_id
}

async fn wait_store_settles() {
    // hub persist + fanout are async; history reads the shared store
    tokio::time::sleep(Duration::from_millis(400)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn history_pages_and_marks_recalled_rows() {
    let stack = boot().await;
    let (alice, alice_tok) = register(&stack, &format!("{}-alice", who()));
    let (bob, bob_tok) = register(&stack, &format!("{}-bob", who()));
    let conv_id = conv(&alice, &bob);

    let alice_c = connect(&stack, &alice_tok).await;
    let bob_c = connect(&stack, &bob_tok).await;
    befriend(&stack, &alice, &alice_tok, &bob, &bob_tok).await;
    let _ = bob_c; // online so the frames are delivered, not parked

    let mut ids = Vec::new();
    for i in 0..6 {
        ids.push(send_text(&alice_c, &conv_id, vec![bob.clone()], &format!("msg {i}")).await);
    }
    wait_store_settles().await;

    let page = |before: i64, limit: usize| -> serde_json::Value {
        ureq::get(&format!(
            "{}/api/history/{conv_id}?before_seq={before}&limit={limit}",
            stack.http
        ))
        .header("Authorization", &format!("Bearer {alice_tok}"))
        .call()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap()
    };

    let first = page(0, 4);
    let messages = first["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0]["text"], "msg 2", "newest page, ascending order");
    assert_eq!(messages[3]["text"], "msg 5");
    let next = first["next_before_seq"].as_i64().unwrap();
    assert!(next > 0);

    let second = page(next, 4);
    let messages = second["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "the rest of the conversation");
    assert_eq!(messages[0]["text"], "msg 0");
    assert_eq!(messages[1]["text"], "msg 1");
    assert!(second["next_before_seq"].is_null());

    // recall msg 0: history shows the placeholder, never the original
    let recall = Envelope::recall(&conv_id, alice.clone(), ids[0]);
    alice_c.send(&recall).await.unwrap();
    wait_store_settles().await;
    let after = page(0, 10);
    let m0 = &after["messages"][0];
    assert_eq!(m0["withdrawn"], serde_json::json!(true));
    assert_eq!(m0["text"], serde_json::json!("消息已撤回"));
    assert!(
        m0["text"].as_str().unwrap() != "msg 0",
        "the original body must be gone"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn history_denies_outsiders() {
    let stack = boot().await;
    let (alice, alice_tok) = register(&stack, &format!("{}-alice", who()));
    let (bob, bob_tok) = register(&stack, &format!("{}-bob", who()));
    let (_mallory, mallory_tok) = register(&stack, &format!("{}-mallory", who()));
    let conv_id = conv(&alice, &bob);

    let alice_c = connect(&stack, &alice_tok).await;
    let _bob_c = connect(&stack, &bob_tok).await;
    send_text(&alice_c, &conv_id, vec![bob.clone()], "secret between us").await;
    wait_store_settles().await;

    // mallory is in neither half of the pair: 403
    let res = ureq::get(&format!("{}/api/history/{conv_id}", stack.http))
        .header("Authorization", &format!("Bearer {mallory_tok}"))
        .call();
    assert_eq!(status_of(res), 403);

    // a group mallory was never added to behaves the same
    let group = stack
        .platform
        .create_group(&alice, "g", std::slice::from_ref(&bob))
        .unwrap();
    send_text(&alice_c, &group, vec![bob.clone()], "group secret").await;
    wait_store_settles().await;
    let res = ureq::get(&format!("/api/history/{group}"))
        .header("Authorization", &format!("Bearer {mallory_tok}"))
        .call();
    assert!(res.is_err(), "relative URLs never reach the server");

    let res = ureq::get(&format!("{}/api/history/{group}", stack.http))
        .header("Authorization", &format!("Bearer {mallory_tok}"))
        .call();
    assert_eq!(status_of(res), 403);

    // and without any token: 401
    let res = ureq::get(&format!("{}/api/history/{conv_id}", stack.http)).call();
    assert_eq!(status_of(res), 401);

    // while bob (a member) reads the group fine
    let bob_tok = stack.platform.create_session(&bob, 60_000).unwrap();
    let res = ureq::get(&format!("{}/api/history/{group}?limit=5", stack.http))
        .header("Authorization", &format!("Bearer {bob_tok}"))
        .call()
        .unwrap();
    let body: serde_json::Value = res.into_body().read_json().unwrap();
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn search_only_covers_conversations_i_may_see() {
    let stack = boot().await;
    let (alice, alice_tok) = register(&stack, &format!("{}-alice", who()));
    let (bob, bob_tok) = register(&stack, &format!("{}-bob", who()));
    let (carol, carol_tok) = register(&stack, &format!("{}-carol", who()));

    let alice_c = connect(&stack, &alice_tok).await;
    let bob_c = connect(&stack, &bob_tok).await;
    let _carol_c = connect(&stack, &carol_tok).await;
    let alice_tok = stack.platform.create_session(&alice, 60_000).unwrap();
    let bob_tok2 = stack.platform.create_session(&bob, 60_000).unwrap();
    let carol_tok2 = stack.platform.create_session(&carol, 60_000).unwrap();
    befriend(&stack, &alice, &alice_tok, &bob, &bob_tok2).await;
    befriend(&stack, &alice, &alice_tok, &carol, &carol_tok2).await;

    // alice↔bob talk about pineapples; alice↔carol never do
    let ab = conv(&alice, &bob);
    let ac = conv(&alice, &carol);
    send_text(&alice_c, &ab, vec![bob.clone()], "pineapple express").await;
    send_text(&bob_c, &ab, vec![alice.clone()], "no pineapples here").await;
    send_text(&alice_c, &ac, vec![carol.clone()], "hello carol").await;
    wait_store_settles().await;

    let search = |token: &str, q: &str, min: usize| -> Vec<serde_json::Value> {
        search_until(token, q, min, &stack.http)
    };

    // bob sees both hits (his conversation with alice)
    let hits = search(
        &stack.platform.create_session(&bob, 60_000).unwrap(),
        "pineapple",
        2,
    );
    assert_eq!(hits.len(), 2, "{hits:?}");

    // carol's allowed set contains only her alice pair: zero pineapple hits
    let hits = search(&carol_tok, "pineapple", 0);
    assert!(
        hits.is_empty(),
        "carol must not read alice-bob history: {hits:?}"
    );
    // but her own conversation is searchable
    let hits = search(&carol_tok, "carol", 1);
    assert_eq!(hits.len(), 1);

    // empty query is a 422, not a full dump
    let res = ureq::get(&format!("{}/api/search/messages?q=", stack.http))
        .header("Authorization", &format!("Bearer {alice_tok}"))
        .call();
    assert_eq!(status_of(res), 422);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn search_needle_wildcards_stay_literal() {
    let stack = boot().await;
    let (alice, alice_tok) = register(&stack, &format!("{}-alice", who()));
    let (bob, bob_tok) = register(&stack, &format!("{}-bob", who()));
    let conv_id = conv(&alice, &bob);

    let alice_c = connect(&stack, &alice_tok).await;
    let bob_c = connect(&stack, &bob_tok).await;
    let _ = bob_c;
    let alice_tok = stack.platform.create_session(&alice, 60_000).unwrap();
    let bob_tok2 = stack.platform.create_session(&bob, 60_000).unwrap();
    befriend(&stack, &alice, &alice_tok, &bob, &bob_tok2).await;
    send_text(&alice_c, &conv_id, vec![bob.clone()], "100% match_").await;
    send_text(&alice_c, &conv_id, vec![bob.clone()], "1x0y0z% match_").await;
    wait_store_settles().await;

    let results = search_until(&alice_tok, "100%25%20match_", 1, &stack.http);
    assert_eq!(results.len(), 1, "wildcards are literal: {results:?}");
    assert_eq!(results[0]["text"], serde_json::json!("100% match_"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typing_and_forward_kinds_flow_through_the_hub() {
    let stack = boot().await;
    let (alice, alice_tok) = register(&stack, &format!("{}-alice", who()));
    let (bob, bob_tok) = register(&stack, &format!("{}-bob", who()));
    let conv_id = conv(&alice, &bob);

    let alice_c = connect(&stack, &alice_tok).await;
    let bob_c = connect(&stack, &bob_tok).await;
    befriend(&stack, &alice, &alice_tok, &bob, &bob_tok).await;
    wait_bound(&stack, 2).await;

    // typing: delivered, but never in history
    let typing = Envelope::with_body(
        &conv_id,
        alice.clone(),
        vec![bob.clone()],
        0,
        MsgBody::Custom {
            kind: custom_kind::TYPING.into(),
            payload: serde_json::json!({}),
        },
    );
    alice_c.send(&typing).await.unwrap();
    // befriend 的系统事件帧先于 typing 到达：逐条跳过直到 typing 帧
    let ev = loop {
        let env = tokio::time::timeout(Duration::from_secs(15), bob_c.next_envelope())
            .await
            .unwrap()
            .unwrap();
        if env.is_typing() {
            break env;
        }
    };
    assert!(ev.is_typing(), "the peer sees the typing ping");

    // forward: a Custom msg.forward lands in history like any content frame
    let fwd = Envelope::with_body(
        &conv_id,
        alice.clone(),
        vec![bob.clone()],
        0,
        MsgBody::Custom {
            kind: custom_kind::FORWARD.into(),
            payload: serde_json::json!({"text": "转发的内容", "sender": "carol"}),
        },
    );
    alice_c.send(&fwd).await.unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(15), bob_c.next_envelope())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(&ev.body, MsgBody::Custom { kind, .. } if kind == custom_kind::FORWARD));
    wait_store_settles().await;

    let history: serde_json::Value =
        ureq::get(&format!("{}/api/history/{conv_id}?limit=50", stack.http))
            .header("Authorization", &format!("Bearer {bob_tok}"))
            .call()
            .unwrap()
            .body_mut()
            .read_json()
            .unwrap();
    let messages = history["messages"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        1,
        "typing left no row, forward did: {messages:?}"
    );
    assert_eq!(messages[0]["kind"], serde_json::json!(custom_kind::FORWARD));
}
