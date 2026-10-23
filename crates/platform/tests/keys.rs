//! WP-12: the pre-key directory (`/api/keys`) — publish, inspect, and the
//! claim semantics that guarantee no two callers ever share a one-time key.

mod common;

use axum::http::StatusCode;
use serde_json::json;
use wxlike_core::e2ee::{Crypto, Device, PreKeyBundle};
use wxlike_platform::Platform;

use axum::Router;

use common::{app, call, platform, req, signup};

/// 用真实 HTTP 流程把 a、b 变成好友。
async fn befriend(app: &Router, a_token: &str, b_token: &str, b_id: &str) {
    let (status, body) = call(
        app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"to_user_id": b_id})),
            Some(a_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let req_id = body["id"].as_str().unwrap_or_default().to_string();
    let (status, body) = call(
        app,
        req(
            "POST",
            &format!("/api/friends/requests/{req_id}/accept"),
            None,
            Some(b_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn publish_then_claim_round_trips_a_key() {
    let state = platform();
    let app = app(state.clone());
    let (alice_id, alice_token) = signup(&app, "alice").await;

    let bundle = Crypto::new(Device::new()).bundle(&alice_id);
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/keys",
            Some(serde_json::to_value(&bundle).unwrap()),
            Some(&alice_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, bob_token) = signup(&app, "bob").await;
    befriend(&app, &bob_token, &alice_token, &alice_id).await;
    let (status, body) = call(
        &app,
        req(
            "GET",
            &format!("/api/keys/{alice_id}"),
            None,
            Some(&bob_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let claimed: PreKeyBundle = serde_json::from_value(body).unwrap();
    assert_eq!(claimed.user_id, alice_id);
    assert_eq!(claimed.identity, bundle.identity);
    assert_eq!(
        claimed.one_time_keys.len(),
        1,
        "a claim returns exactly one key"
    );
}

#[tokio::test]
async fn two_claims_never_share_a_key() {
    let state = platform();
    let app = app(state.clone());
    let (alice_id, alice_token) = signup(&app, "alice").await;
    let (_, bob_token) = signup(&app, "bob").await;
    let (_, carol_token) = signup(&app, "carol").await;
    befriend(&app, &bob_token, &alice_token, &alice_id).await;
    befriend(&app, &carol_token, &alice_token, &alice_id).await;

    let bundle = Crypto::new(Device::new()).bundle(&alice_id);
    call(
        &app,
        req(
            "POST",
            "/api/keys",
            Some(serde_json::to_value(&bundle).unwrap()),
            Some(&alice_token),
        ),
    )
    .await;

    let first: PreKeyBundle = serde_json::from_value(
        call(
            &app,
            req(
                "GET",
                &format!("/api/keys/{alice_id}"),
                None,
                Some(&bob_token),
            ),
        )
        .await
        .1,
    )
    .unwrap();
    let second: PreKeyBundle = serde_json::from_value(
        call(
            &app,
            req(
                "GET",
                &format!("/api/keys/{alice_id}"),
                None,
                Some(&carol_token),
            ),
        )
        .await
        .1,
    )
    .unwrap();
    assert_ne!(
        first.one_time_keys, second.one_time_keys,
        "the directory hands out distinct one-time keys per claim"
    );
}

#[tokio::test]
async fn drained_bundle_reports_empty_and_refill_works() {
    let state = platform();
    let app = app(state.clone());
    let (alice_id, alice_token) = signup(&app, "alice").await;
    let (_, bob_token) = signup(&app, "bob").await;
    befriend(&app, &bob_token, &alice_token, &alice_id).await;
    // publish exactly one one-time key — a real signed bundle (the directory
    // rejects hand-crafted key strings since AR-1: shape + signature checks)
    let one = Crypto::new(Device::new()).bundle_limited(&alice_id, 1);
    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/keys",
            Some(serde_json::to_value(&one).unwrap()),
            Some(&alice_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let drain: PreKeyBundle = serde_json::from_value(
        call(
            &app,
            req(
                "GET",
                &format!("/api/keys/{alice_id}"),
                None,
                Some(&bob_token),
            ),
        )
        .await
        .1,
    )
    .unwrap();
    assert_eq!(drain.one_time_keys.len(), 1);

    let empty: PreKeyBundle = serde_json::from_value(
        call(
            &app,
            req(
                "GET",
                &format!("/api/keys/{alice_id}"),
                None,
                Some(&bob_token),
            ),
        )
        .await
        .1,
    )
    .unwrap();
    assert!(
        empty.one_time_keys.is_empty(),
        "a drained bundle says so instead of re-handing the same key"
    );
    assert!(!empty.identity.is_empty(), "identity stays fetchable");

    // the owner tops up by re-publishing
    let fresh = Crypto::new(Device::new()).bundle(&alice_id);
    call(
        &app,
        req(
            "POST",
            "/api/keys",
            Some(serde_json::to_value(&fresh).unwrap()),
            Some(&alice_token),
        ),
    )
    .await;
    let refilled: PreKeyBundle = serde_json::from_value(
        call(
            &app,
            req(
                "GET",
                &format!("/api/keys/{alice_id}"),
                None,
                Some(&bob_token),
            ),
        )
        .await
        .1,
    )
    .unwrap();
    assert_eq!(refilled.one_time_keys.len(), 1);
    assert_eq!(refilled.identity, fresh.identity, "re-publish replaces");
}

#[tokio::test]
async fn publishing_someone_elses_bundle_is_forbidden() {
    let state = platform();
    let app = app(state.clone());
    let (alice_id, _) = signup(&app, "alice").await;
    let (_, mallory_token) = signup(&app, "mallory").await;

    let forged = PreKeyBundle {
        user_id: alice_id,
        identity: "x".repeat(43),
        identity_signing: "y".repeat(43),
        one_time_keys: vec!["z".repeat(43)],
        one_time_key_signatures: Vec::new(),
        signature: String::new(),
    };
    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/keys",
            Some(serde_json::to_value(&forged).unwrap()),
            Some(&mallory_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn keys_endpoints_require_auth() {
    let state = platform();
    let app = app(state.clone());
    let (alice_id, alice_token) = signup(&app, "alice").await;

    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/keys",
            Some(
                json!({"user_id": alice_id, "identity": "i", "identity_signing": "s",
                        "one_time_keys": ["k"]}),
            ),
            Some("not-a-token"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = call(&app, req("GET", "/api/keys", None, Some("not-a-token"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // and the owner's own view shows the empty state before any publish
    let (status, body) = call(&app, req("GET", "/api/keys", None, Some(&alice_token))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["one_time_keys"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn claiming_for_an_unknown_user_is_404() {
    let state = platform();
    let app = app(state.clone());
    let (_, bob_token) = signup(&app, "bob").await;
    let (status, _) = call(
        &app,
        req("GET", "/api/keys/deadbeef00", None, Some(&bob_token)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// End-to-end through the directory: two real devices exchange a session
/// exactly the way the client stack will (publish → claim → encrypt → decrypt).
#[tokio::test]
async fn directory_backed_session_establishment_works() {
    let state = platform();
    let app = app(state.clone());
    let (alice_id, alice_token) = signup(&app, "alice").await;
    let (_, bob_token) = signup(&app, "bob").await;
    befriend(&app, &bob_token, &alice_token, &alice_id).await;
    let mut alice = Crypto::new(Device::new());
    let mut bob = Crypto::new(Device::new());

    // alice publishes her (refilled) bundle
    let bundle = alice.bundle_refilled(&alice_id);
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/keys",
            Some(serde_json::to_value(&bundle).unwrap()),
            Some(&alice_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // bob claims one key of hers and opens a session with it
    let claimed: PreKeyBundle = serde_json::from_value(
        call(
            &app,
            req(
                "GET",
                &format!("/api/keys/{alice_id}"),
                None,
                Some(&bob_token),
            ),
        )
        .await
        .1,
    )
    .unwrap();
    let sealed = bob
        .encrypt_dm("alice", &claimed, b"hello through the directory")
        .unwrap();
    assert!(sealed.pre_key);
    assert_eq!(
        alice.decrypt_dm("bob", &sealed).unwrap(),
        b"hello through the directory".to_vec()
    );
    assert!(alice.has_session_with("bob"));
}

/// Direct library-level check of the claim transaction (no HTTP), including
/// the `Platform` public API surface used by non-HTTP embedders.
#[test]
fn claim_via_library_api_pops_distinctly() {
    let platform = Platform::in_memory().unwrap();
    platform.register("alice", "pw").unwrap();
    let alice_id = platform.user_id_by_name("alice").unwrap().unwrap();

    let bundle = Crypto::new(Device::new()).bundle(&alice_id);
    platform.publish_keys(&alice_id, &bundle).unwrap();

    let mut seen = std::collections::HashSet::new();
    for _ in 0..3 {
        let claimed = platform.claim_key(&alice_id).unwrap().unwrap();
        assert_eq!(claimed.one_time_keys.len(), 1);
        assert!(seen.insert(claimed.one_time_keys[0].clone()));
    }
    let drained = platform.claim_key(&alice_id).unwrap().unwrap();
    // published 20 keys, claimed 3 — still plenty left
    assert!(!drained.one_time_keys.is_empty());
    assert_eq!(
        platform.own_keys(&alice_id).unwrap().one_time_keys.len(),
        20 - 4
    );
}

// ---------------------------------------------------------------------------
// AR-1 (audit round 4, C-1): the directory refuses to store junk.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn publishing_an_unsigned_bundle_is_rejected() {
    let state = platform();
    let app = app(state.clone());
    let (alice_id, alice_token) = signup(&app, "alice").await;

    // keys are real curve points, but nobody signed the bundle
    let mut unsigned = Crypto::new(Device::new()).bundle(&alice_id);
    unsigned.signature.clear();
    unsigned.one_time_key_signatures.clear();
    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/keys",
            Some(serde_json::to_value(&unsigned).unwrap()),
            Some(&alice_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
}

#[tokio::test]
async fn publishing_non_curve_key_material_is_rejected() {
    let state = platform();
    let app = app(state.clone());
    let (alice_id, alice_token) = signup(&app, "alice").await;

    // 43 chars of base64-looking garbage: not a Curve25519 point
    let garbage = PreKeyBundle {
        user_id: alice_id,
        identity: "a".repeat(43),
        identity_signing: "b".repeat(43),
        one_time_keys: vec!["c".repeat(43)],
        one_time_key_signatures: vec!["d".repeat(86)],
        signature: "e".repeat(86),
    };
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/keys",
            Some(serde_json::to_value(&garbage).unwrap()),
            Some(&alice_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn claim_returns_a_bundle_senders_can_verify() {
    let state = platform();
    let app = app(state.clone());
    let (alice_id, alice_token) = signup(&app, "alice").await;
    let (_, bob_token) = signup(&app, "bob").await;
    befriend(&app, &bob_token, &alice_token, &alice_id).await;

    let bundle = Crypto::new(Device::new()).bundle(&alice_id);
    call(
        &app,
        req(
            "POST",
            "/api/keys",
            Some(serde_json::to_value(&bundle).unwrap()),
            Some(&alice_token),
        ),
    )
    .await;

    let claimed: PreKeyBundle = serde_json::from_value(
        call(
            &app,
            req(
                "GET",
                &format!("/api/keys/{alice_id}"),
                None,
                Some(&bob_token),
            ),
        )
        .await
        .1,
    )
    .unwrap();
    // the claimed subset must pass the client-side trust check on its own —
    // this is the check `Crypto::encrypt_dm` runs before opening a session
    claimed.verify_signature().unwrap();
}

/// AR-2/H-4：陌生人（无好友、无同群）不能认领——关系门生效。
#[tokio::test]
async fn stranger_cannot_claim_keys() {
    let state = platform();
    let app = app(state.clone());
    let (alice_id, alice_token) = signup(&app, "alice").await;
    let (_, mallory_token) = signup(&app, "mallory").await;

    let bundle = Crypto::new(Device::new()).bundle(&alice_id);
    call(
        &app,
        req(
            "POST",
            "/api/keys",
            Some(serde_json::to_value(&bundle).unwrap()),
            Some(&alice_token),
        ),
    )
    .await;

    let (status, _) = call(
        &app,
        req(
            "GET",
            &format!("/api/keys/{alice_id}"),
            None,
            Some(&mallory_token),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "stranger gets the same answer as for an unknown user (no existence oracle)"
    );
}

/// AR-2/H-4：循环领空库存的攻击撞上 (caller,target) 限流。
#[tokio::test]
async fn key_draining_attack_is_throttled() {
    let state = platform();
    let app = app(state.clone());
    let (alice_id, alice_token) = signup(&app, "alice").await;
    let (_, bob_token) = signup(&app, "bob").await;
    befriend(&app, &bob_token, &alice_token, &alice_id).await;

    // 默认阈值 throttle=30 / deny=120（1h 窗口）。每轮：补货 + 认领一次，
    // 认领成功才计数，所以恰好需要 30 次成功才触发 Throttle。
    let mut saw_throttle = false;
    for round in 0..35 {
        let fresh = Crypto::new(Device::new()).bundle(&alice_id);
        call(
            &app,
            req(
                "POST",
                "/api/keys",
                Some(serde_json::to_value(&fresh).unwrap()),
                Some(&alice_token),
            ),
        )
        .await;
        let (status, _) = call(
            &app,
            req(
                "GET",
                &format!("/api/keys/{alice_id}"),
                None,
                Some(&bob_token),
            ),
        )
        .await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            assert!(round >= 29, "throttled too early: round {round}");
            saw_throttle = true;
            break;
        }
        assert_eq!(status, StatusCode::OK, "round {round}");
    }
    assert!(saw_throttle, "the drain loop must hit the rate limit");
}
