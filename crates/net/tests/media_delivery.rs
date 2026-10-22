//! WP-05 A2: an image uploaded through the platform is referenced by
//! `MsgBody::Bytes(media_id)` over the QUIC hub, and the recipient's download
//! is byte-identical (same SHA-256) to what the sender uploaded.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tower::ServiceExt as _;
use wxlike_core::Envelope;
use wxlike_core::MsgBody;
use wxlike_mq::{memory::MemoryMq, memory::MemoryRoutes, MqRouter};
use wxlike_net::{run_mq_hub, Client, HubHandle};
use wxlike_platform::{Platform, PlatformOptions};

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

/// POST /api/media as multipart with the declared digest.
async fn upload(app: &axum::Router, token: &str, bytes: &[u8], conv: &str) -> String {
    let boundary = "wp05-integration";
    let mut body = Vec::new();
    let mut h = Sha256::new();
    h.update(bytes);
    let sha = hex::encode(h.finalize());

    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"sha256\"\r\n\r\n");
    body.extend_from_slice(sha.as_bytes());
    body.extend_from_slice(b"\r\n");
    // 归属会话（issue #14）：附件只有会话成员能下载
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"conv\"\r\n\r\n");
    body.extend_from_slice(conv.as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"blob\"\r\nContent-Type: image/png\r\n\r\n",
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/media")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let json = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&json).unwrap();
    assert_eq!(status, StatusCode::OK, "upload failed: {json}");
    json["media_id"].as_str().unwrap().to_string()
}

async fn download(
    app: &axum::Router,
    id: &str,
    range: Option<&str>,
    token: &str,
) -> (StatusCode, Vec<u8>) {
    let mut b = Request::builder()
        .method("GET")
        .uri(format!("/api/media/{id}"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    if let Some(r) = range {
        b = b.header(header::RANGE, r);
    }
    let resp = app
        .clone()
        .oneshot(b.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    (
        status,
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
}

fn sha_of(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

fn png_of(width: u32, height: u32) -> Vec<u8> {
    let img = image::DynamicImage::new_rgb8(width, height).into_rgba8();
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

#[tokio::test]
async fn image_reference_round_trips_through_the_hub() {
    let router = MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new());
    let hub = run_mq_hub("hub-1", "127.0.0.1:0".parse().unwrap(), router)
        .await
        .unwrap();
    let addr = hub.local_addr().unwrap();

    let platform = Platform::in_memory_with(PlatformOptions {
        session_ttl_ms: 60_000,
        pbkdf2_rounds: 1_000,
        ..Default::default()
    })
    .unwrap()
    .shared();
    platform.set_event_sink(Arc::new(HubSink(hub.handle())));
    let app = Platform::router(platform.clone());

    // two accounts
    let (_, a) = post_json(
        &app,
        "/api/register",
        json!({"name": "alice", "password": "pw"}),
        None,
    )
    .await;
    let alice = a["user_id"].as_str().unwrap().to_string();
    let alice_token = a["token"].as_str().unwrap().to_string();
    let (_, b) = post_json(
        &app,
        "/api/register",
        json!({"name": "bob", "password": "pw"}),
        None,
    )
    .await;
    let bob = b["user_id"].as_str().unwrap().to_string();
    let bob_token = b["token"].as_str().unwrap().to_string();

    // both online
    let ac = Client::connect(addr, "localhost", alice.clone())
        .await
        .unwrap();
    let bc = Client::connect(addr, "localhost", bob.clone())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // alice uploads an image and gets back a content-addressed id
    let original = png_of(800, 600);
    let conv = wxlike_platform::direct_conv_id(&alice, &bob);
    let media_id = upload(&app, &alice_token, &original, &conv).await;
    assert_eq!(media_id, sha_of(&original), "the id is the digest");

    // the message carries only the reference, not the bytes
    let msg = Envelope::with_body(
        wxlike_platform::direct_conv_id(&alice, &bob),
        alice.clone(),
        vec![bob.clone()],
        1,
        MsgBody::Bytes(media_id.as_bytes().to_vec()),
    );
    ac.send(&msg).await.unwrap();

    let got = tokio::time::timeout(Duration::from_secs(5), bc.next_envelope())
        .await
        .expect("bob receives the media reference")
        .unwrap();
    let referenced = match &got.body {
        MsgBody::Bytes(b) => String::from_utf8_lossy(b).to_string(),
        other => panic!("expected Bytes, got {other:?}"),
    };
    assert_eq!(
        referenced, media_id,
        "the frame carries the id, nothing else"
    );
    assert!(
        !matches!(&got.body, MsgBody::Bytes(b) if b.len() > 128),
        "no image bytes rode along with the message"
    );

    // bob fetches it, in one piece
    let (status, fetched) = download(&app, &media_id, None, &bob_token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched, original);
    assert_eq!(
        sha_of(&fetched),
        media_id,
        "digest matches on the receiving end"
    );

    // ...or in pieces: resume with a Range request
    let (status, head) = download(&app, &media_id, Some("bytes=0-999"), &bob_token).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    let (status2, rest) = download(&app, &media_id, Some("bytes=1000-"), &bob_token).await;
    assert_eq!(status2, StatusCode::PARTIAL_CONTENT);
    let mut rejoined = head;
    rejoined.extend_from_slice(&rest);
    assert_eq!(rejoined, original, "resumed download reassembles exactly");

    // and the thumbnail is a separate, smaller object
    let (status, thumb, _) = {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/media/{media_id}?thumb=1"))
                    .header(header::AUTHORIZATION, format!("Bearer {bob_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        (
            status,
            resp.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
            (),
        )
    };
    assert_eq!(status, StatusCode::OK);
    let img = image::load_from_memory(&thumb).expect("thumb decodes");
    assert!(img.width() <= 256 && img.height() <= 256);
    assert!(
        thumb.len() < original.len(),
        "thumb is smaller than the original"
    );
}

#[tokio::test]
async fn media_reference_survives_offline_delivery() {
    // bob connects only after the message was parked; the id must still resolve
    let router = MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new());
    let hub = run_mq_hub("hub-1", "127.0.0.1:0".parse().unwrap(), router)
        .await
        .unwrap();
    let addr = hub.local_addr().unwrap();

    let platform = Platform::in_memory_with(PlatformOptions {
        session_ttl_ms: 60_000,
        pbkdf2_rounds: 1_000,
        ..Default::default()
    })
    .unwrap()
    .shared();
    platform.set_event_sink(Arc::new(HubSink(hub.handle())));
    let app = Platform::router(platform);

    let (_, a) = post_json(
        &app,
        "/api/register",
        json!({"name": "alice", "password": "pw"}),
        None,
    )
    .await;
    let alice = a["user_id"].as_str().unwrap().to_string();
    let alice_token = a["token"].as_str().unwrap().to_string();
    let (_, b) = post_json(
        &app,
        "/api/register",
        json!({"name": "carol", "password": "pw"}),
        None,
    )
    .await;
    let carol = b["user_id"].as_str().unwrap().to_string();
    let carol_token = b["token"].as_str().unwrap().to_string();

    let ac = Client::connect(addr, "localhost", alice.clone())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;

    let payload = b"pdf-ish attachment bytes".to_vec();
    let conv = wxlike_platform::direct_conv_id(&alice, &carol);
    let media_id = upload(&app, &alice_token, &payload, &conv).await;

    ac.send(&Envelope::with_body(
        wxlike_platform::direct_conv_id(&alice, &carol),
        alice.clone(),
        vec![carol.clone()],
        1,
        MsgBody::Bytes(media_id.as_bytes().to_vec()),
    ))
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let cc = Client::connect(addr, "localhost", carol.clone())
        .await
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), cc.next_envelope())
        .await
        .expect("parked media reference is flushed on Hello")
        .unwrap();
    let id = match &got.body {
        MsgBody::Bytes(b) => String::from_utf8_lossy(b).to_string(),
        other => panic!("expected Bytes, got {other:?}"),
    };
    assert_eq!(id, media_id);

    let (status, fetched) = download(&app, &id, None, &carol_token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched, payload);
}
