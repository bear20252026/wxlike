//! WP-02 A2: a friend event raised through the platform HTTP API is pushed by
//! the hub to the online recipient as a `MsgKind::System` envelope.
//!
//! This is the end-to-end proof that platform and hub are one system: the
//! relationship is stored in SQLite, the notification travels the same
//! `route_envelope` path as ordinary chat traffic, and a real QUIC client
//! receives it.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt as _;
use wxlike_core::{Envelope, MsgKind};
use wxlike_mq::{memory::MemoryMq, memory::MemoryRoutes, MqRouter};
use wxlike_net::{run_mq_hub, Client, HubHandle};
use wxlike_platform::{Platform, PlatformOptions};

/// Adapts the hub handle to the platform's event sink.
struct HubSink(HubHandle);

#[async_trait::async_trait]
impl wxlike_platform::EventSink for HubSink {
    async fn publish(&self, env: Envelope) -> anyhow::Result<()> {
        self.0.publish(&env).await
    }
}

async fn post_json(
    app: &axum::Router,
    path: &str,
    body: Value,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method("POST")
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let resp = app
        .clone()
        .oneshot(b.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn friend_request_reaches_online_peer_through_hub() {
    let router = MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new());
    let hub = run_mq_hub("hub-1", "127.0.0.1:0".parse().unwrap(), router)
        .await
        .unwrap();

    let platform = Platform::in_memory_with(PlatformOptions {
        session_ttl_ms: 60_000,
        pbkdf2_rounds: 1_000,
        ..Default::default()
    })
    .unwrap()
    .shared();
    platform.set_event_sink(Arc::new(HubSink(hub.handle())));
    let app = Platform::router(platform.clone());

    // two accounts in the platform data plane
    let (_, reply) = post_json(
        &app,
        "/api/register",
        json!({"name": "alice", "password": "pw"}),
        None,
    )
    .await;
    let alice = reply["user_id"].as_str().unwrap().to_string();
    let alice_token = reply["token"].as_str().unwrap().to_string();
    let (_, reply) = post_json(
        &app,
        "/api/register",
        json!({"name": "bob", "password": "pw"}),
        None,
    )
    .await;
    let bob = reply["user_id"].as_str().unwrap().to_string();

    // bob is online on the hub under his platform user id
    let bob_client = Client::connect(hub.local_addr().unwrap(), "localhost", bob.clone())
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(hub.handle().online_users().await, 1);

    // alice sends a friend request over HTTP
    let (status, created) = post_json(
        &app,
        "/api/friends/requests",
        json!({"to_user_id": bob, "message": "let's connect"}),
        Some(&alice_token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    // ...and bob's open QUIC stream carries the system envelope
    let got = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        bob_client.next_envelope(),
    )
    .await
    .expect("system event delivered to the online peer within 5s")
    .unwrap();
    assert_eq!(got.kind, MsgKind::System);
    assert_eq!(got.sender, alice);
    assert_eq!(got.to, vec![bob.clone()]);
    match &got.body {
        wxlike_core::MsgBody::Notice(text) => {
            assert!(text.contains("alice"), "notice names the requester: {text}");
            assert!(
                text.contains("let's connect"),
                "notice carries the note: {text}"
            );
        }
        other => panic!("expected Notice body, got {other:?}"),
    }
    // the conversation id is the deterministic pair id
    assert_eq!(got.conv_id, wxlike_platform::direct_conv_id(&alice, &bob));

    // accepting travels the same path back to alice
    let alice_client = Client::connect(hub.local_addr().unwrap(), "localhost", alice.clone())
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let (_, bt) = post_json(
        &app,
        "/api/login",
        json!({"name": "bob", "password": "pw"}),
        None,
    )
    .await;
    let bob_token = bt["token"].as_str().expect("bob can log in").to_string();
    let req_id = created["id"].as_str().unwrap();
    let (status, _) = post_json(
        &app,
        &format!("/api/friends/requests/{req_id}/accept"),
        Value::Null,
        Some(&bob_token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let accepted = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        alice_client.next_envelope(),
    )
    .await
    .expect("accept event reaches alice")
    .unwrap();
    assert_eq!(accepted.kind, MsgKind::System);
    assert_eq!(accepted.to, vec![alice.clone()]);
    assert_eq!(accepted.sender, bob);

    // and both sides now agree they are friends, straight from SQLite
    assert!(platform.are_friends(&alice, &bob).unwrap());
    assert!(platform.are_friends(&bob, &alice).unwrap());
}

#[tokio::test]
async fn friend_event_for_offline_peer_is_parked_not_dropped() {
    let router = MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new());
    let hub = run_mq_hub("hub-1", "127.0.0.1:0".parse().unwrap(), router)
        .await
        .unwrap();
    let platform = Platform::in_memory().unwrap().shared();
    platform.set_event_sink(Arc::new(HubSink(hub.handle())));
    let app = Platform::router(platform.clone());

    let (_, reply) = post_json(
        &app,
        "/api/register",
        json!({"name": "alice", "password": "pw"}),
        None,
    )
    .await;
    let alice = reply["user_id"].as_str().unwrap().to_string();
    let alice_token = reply["token"].as_str().unwrap().to_string();
    let (_, reply) = post_json(
        &app,
        "/api/register",
        json!({"name": "carol", "password": "pw"}),
        None,
    )
    .await;
    let carol = reply["user_id"].as_str().unwrap().to_string();
    drop(reply);

    // carol has never connected: the hub parks the frame in its offline store
    let (status, _) = post_json(
        &app,
        "/api/friends/requests",
        json!({"to_user_id": carol}),
        Some(&alice_token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // when she finally says Hello she gets it
    let carol_client = Client::connect(hub.local_addr().unwrap(), "localhost", carol.clone())
        .await
        .unwrap();
    let got = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        carol_client.next_envelope(),
    )
    .await
    .expect("parked system event flushed on Hello")
    .unwrap();
    assert_eq!(got.kind, MsgKind::System);
    assert_eq!(got.sender, alice);
    assert_eq!(
        platform.friends(&alice).unwrap().len(),
        0,
        "request is not a friendship"
    );
}
