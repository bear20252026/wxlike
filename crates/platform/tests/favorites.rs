//! P1 favorites (收藏): snapshot storage, ownership, paging, limits.

use wxlike_platform::{FavoriteInput, Platform};

fn user(p: &Platform, name: &str) -> String {
    p.register(name, "pw").unwrap()
}

fn snap(text: &str) -> FavoriteInput {
    FavoriteInput {
        source_conv: "d-alice~bob".into(),
        source_msg_id: "0123456789abcdef0123456789abcdef".into(),
        kind: "text".into(),
        text: text.into(),
        media_id: String::new(),
        sender: "alice".into(),
        sent_at_ms: 123,
    }
}

#[test]
fn add_and_fetch_roundtrip() {
    let p = Platform::in_memory().unwrap();
    let alice = user(&p, "alice");
    let fav = p.add_favorite(&alice, &snap("要记住的话")).unwrap();
    assert_eq!(fav.id.len(), 16, "bare hex id like every other platform id");
    assert_eq!(fav.text, "要记住的话");

    let got = p.favorite(&alice, &fav.id).unwrap();
    assert_eq!(got, fav);
}

#[test]
fn list_is_newest_first_and_pages_by_created_at() {
    let p = Platform::in_memory().unwrap();
    let alice = user(&p, "alice");
    let mut ids = Vec::new();
    for i in 0..5 {
        let fav = p.add_favorite(&alice, &snap(&format!("note {i}"))).unwrap();
        ids.push(fav.id);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let page1 = p.list_favorites(&alice, None, 2).unwrap();
    assert_eq!(page1.len(), 2);
    assert_eq!(page1[0].text, "note 4", "newest first");
    assert_eq!(page1[1].text, "note 3");
    let page2 = p
        .list_favorites(&alice, Some(page1[1].created_at), 2)
        .unwrap();
    assert_eq!(page2[0].text, "note 2");
    assert_eq!(page2[1].text, "note 1");
    assert_ne!(page1[0].id, page1[1].id);
    let _ = ids;
}

#[test]
fn favorites_are_private_to_their_owner() {
    let p = Platform::in_memory().unwrap();
    let alice = user(&p, "alice");
    let bob = user(&p, "bob");
    let fav = p.add_favorite(&alice, &snap("mine")).unwrap();

    // bob cannot read, list or delete alice's favorite
    assert!(p.favorite(&bob, &fav.id).is_err());
    assert!(p.remove_favorite(&bob, &fav.id).is_err());
    assert!(p.list_favorites(&bob, None, 10).unwrap().is_empty());

    // the owner can
    p.remove_favorite(&alice, &fav.id).unwrap();
    assert!(p.favorite(&alice, &fav.id).is_err());
}

#[test]
fn snapshot_shape_is_validated() {
    let p = Platform::in_memory().unwrap();
    let alice = user(&p, "alice");

    let mut bad_conv = snap("ok");
    bad_conv.source_conv = "../etc/passwd".into();
    assert!(p.add_favorite(&alice, &bad_conv).is_err());

    let mut too_long = snap("ok");
    too_long.text = "x".repeat(5001);
    assert!(p.add_favorite(&alice, &too_long).is_err());

    let mut bad_msg = snap("ok");
    bad_msg.source_msg_id = "x".repeat(65);
    assert!(p.add_favorite(&alice, &bad_msg).is_err());

    // the happy path still works after the refusals
    assert!(p.add_favorite(&alice, &snap("fine")).is_ok());
}

// ---- HTTP surface -----------------------------------------------------------

mod http {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt as _;

    fn app() -> (axum::Router, std::sync::Arc<Platform>) {
        let platform = Platform::in_memory().unwrap().shared();
        (Platform::router(platform.clone()), platform)
    }

    fn bearer(token: &str) -> String {
        format!("Bearer {token}")
    }

    #[tokio::test]
    async fn endpoints_require_auth_and_scope_to_owner() {
        let (app, platform) = app();
        let alice_id = platform.register("alice", "pw").unwrap();
        let token = platform.create_session(&alice_id, 60_000).unwrap();

        // no token -> 401
        let res = app
            .clone()
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/favorites")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        // create via HTTP
        let body = serde_json::json!({
            "source_conv": "d-alice~bob",
            "source_msg_id": "0123456789abcdef0123456789abcdef",
            "kind": "text",
            "text": "http favorite",
            "sender": "alice",
            "sent_at_ms": 1,
        });
        let res = app
            .clone()
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/favorites")
                    .header("authorization", bearer(&token))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let fav: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let fav_id = fav["id"].as_str().unwrap().to_string();

        // another user's token cannot read it -> 404 (anti-probing)
        let bob_id = platform.register("bob", "pw").unwrap();
        let bob_token = platform.create_session(&bob_id, 60_000).unwrap();
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/favorites/{fav_id}"))
                    .header("authorization", bearer(&bob_token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        // owner deletes it -> 204, then it is 404 for the owner too
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/favorites/{fav_id}"))
                    .header("authorization", bearer(&token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }
}
