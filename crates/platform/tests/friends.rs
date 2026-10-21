//! WP-02 acceptance tests (A1 + A3): request lifecycle, mutual friendship,
//! remarks, incremental sync, authorization holes, and the system envelopes
//! that must be emitted for every relationship change.

mod common;

use axum::http::StatusCode;
use common::{app, call, platform, req, signup, Recorder};
use serde_json::json;
use std::sync::Arc;
use wxlike_core::MsgKind;
use wxlike_platform::{Platform, PlatformState};

async fn scene() -> (PlatformState, Recorder) {
    let state = platform();
    let rec = Recorder::new();
    state.set_event_sink(Arc::new(rec.clone()));
    (state, rec)
}

#[tokio::test]
async fn request_lands_in_the_recipients_inbox() {
    let (state, rec) = scene().await;
    let app = app(state.clone());
    let (alice, at) = signup(&app, "alice").await;
    let (bob, _) = signup(&app, "bob").await;

    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"to_user_id": bob, "message": "hi it's me"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["from_uid"].as_str().unwrap(), alice);
    assert_eq!(body["to_uid"].as_str().unwrap(), bob);
    assert_eq!(body["status"].as_str().unwrap(), "pending");
    let req_id = body["id"].as_str().unwrap().to_string();

    // the recipient sees it in their inbox, with the note attached
    let bt = login(&app, "bob").await;
    let (_, incoming) = call(
        &app,
        req("GET", "/api/friends/requests/incoming", None, Some(&bt)),
    )
    .await;
    assert_eq!(incoming.as_array().unwrap().len(), 1);
    assert_eq!(incoming[0]["id"].as_str().unwrap(), req_id);
    assert_eq!(incoming[0]["message"].as_str().unwrap(), "hi it's me");

    // ...and the sender sees it as outgoing
    let (_, outgoing) = call(
        &app,
        req("GET", "/api/friends/requests/outgoing", None, Some(&at)),
    )
    .await;
    assert_eq!(outgoing.as_array().unwrap().len(), 1);

    // exactly one system envelope was pushed, addressed to the recipient
    let events = rec.take();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, MsgKind::System);
    assert_eq!(events[0].to, vec![bob.clone()]);
    assert_eq!(events[0].sender, alice);
}

async fn login(app: &axum::Router, name: &str) -> String {
    let (status, body) = call(
        app,
        req(
            "POST",
            "/api/login",
            Some(json!({"name": name, "password": "pw"})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn duplicate_request_is_409() {
    let (state, _rec) = scene().await;
    let app = app(state.clone());
    let (_, at) = signup(&app, "alice").await;
    let (bob, _) = signup(&app, "bob").await;
    let body = json!({"to_user_id": bob});

    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(body.clone()),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = call(
        &app,
        req("POST", "/api/friends/requests", Some(body), Some(&at)),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn cannot_add_yourself() {
    let (state, rec) = scene().await;
    let app = app(state.clone());
    let (alice, at) = signup(&app, "alice").await;
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"to_user_id": alice})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(rec.count(), 0, "rejected request emits nothing");
}

#[tokio::test]
async fn request_to_unknown_user_is_404() {
    let (state, _rec) = scene().await;
    let app = app(state.clone());
    let (_, at) = signup(&app, "alice").await;
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"to_user_id": "ffffffffffffffff"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn accept_makes_the_friendship_mutual() {
    let (state, rec) = scene().await;
    let app = app(state.clone());
    let (alice, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;

    let (_, created) = call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"to_user_id": bob})),
            Some(&at),
        ),
    )
    .await;
    let req_id = created["id"].as_str().unwrap().to_string();
    rec.take();

    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/friends/requests/{req_id}/accept"),
            None,
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    for (who, token, expect) in [("alice", &at, &bob), ("bob", &bt, &alice)] {
        let (_, rows) = call(&app, req("GET", "/api/friends", None, Some(token))).await;
        let peers: Vec<&str> = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["friend_id"].as_str().unwrap())
            .collect();
        assert_eq!(peers, vec![expect.as_str()], "{who} should see one friend");
    }

    // acceptance pushed a system envelope back to the requester
    let events = rec.take();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, MsgKind::System);
    assert_eq!(events[0].to, vec![alice]);
    assert!(events[0].sender == bob);
}

#[tokio::test]
async fn decline_keeps_them_strangers() {
    let (state, _rec) = scene().await;
    let app = app(state.clone());
    let (_, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let (_, created) = call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"to_user_id": bob})),
            Some(&at),
        ),
    )
    .await;
    let req_id = created["id"].as_str().unwrap().to_string();

    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/friends/requests/{req_id}/decline"),
            None,
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, rows) = call(&app, req("GET", "/api/friends", None, Some(&at))).await;
    assert!(
        rows.as_array().unwrap().is_empty(),
        "declined = no friendship"
    );
    let (_, inc) = call(
        &app,
        req("GET", "/api/friends/requests/incoming", None, Some(&bt)),
    )
    .await;
    assert!(
        inc.as_array().unwrap().is_empty(),
        "declined leaves the inbox"
    );
}

#[tokio::test]
async fn accept_by_someone_else_is_403_and_double_accept_is_409() {
    let (state, _rec) = scene().await;
    let app = app(state.clone());
    let (_, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let (_, mallory) = signup(&app, "mallory").await;

    let (_, created) = call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"to_user_id": bob})),
            Some(&at),
        ),
    )
    .await;
    let req_id = created["id"].as_str().unwrap().to_string();

    // a third party cannot accept a request that is not addressed to them
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/friends/requests/{req_id}/accept"),
            None,
            Some(&mallory),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/friends/requests/{req_id}/accept"),
            None,
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // the sender cannot accept their own request either
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/friends/requests/{req_id}/accept"),
            None,
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // and accepting twice is refused
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/friends/requests/{req_id}/accept"),
            None,
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // still exactly one friend row per side, no dirty duplicates
    let (_, rows) = call(&app, req("GET", "/api/friends", None, Some(&bt))).await;
    assert_eq!(rows.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn accept_unknown_request_is_404() {
    let (state, _rec) = scene().await;
    let app = app(state.clone());
    let (_, bt) = signup(&app, "bob").await;
    for path in [
        "/api/friends/requests/deadbeef00000000/accept",
        "/api/friends/requests/deadbeef00000000/decline",
    ] {
        let (status, _) = call(&app, req("POST", path, None, Some(&bt))).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn forged_from_uid_is_ignored() {
    // A3: the body cannot name another requester; identity comes from the token.
    let (state, _rec) = scene().await;
    let app = app(state.clone());
    let (alice, _) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let (mallory, mt) = signup(&app, "mallory").await;

    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"from_uid": alice, "user_id": alice, "to_user_id": bob})),
            Some(&mt),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["from_uid"].as_str().unwrap(),
        mallory,
        "request is attributed to the authenticated caller only"
    );
    assert_ne!(body["from_uid"].as_str().unwrap(), alice);
    let _ = bt;
}

#[tokio::test]
async fn remark_is_per_side_and_requires_friendship() {
    let (state, _rec) = scene().await;
    let app = app(state.clone());
    let (alice, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let (_, created) = call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"to_user_id": bob})),
            Some(&at),
        ),
    )
    .await;
    let req_id = created["id"].as_str().unwrap().to_string();
    call(
        &app,
        req(
            "POST",
            &format!("/api/friends/requests/{req_id}/accept"),
            None,
            Some(&bt),
        ),
    )
    .await;

    let (status, _) = call(
        &app,
        req(
            "PUT",
            &format!("/api/friends/{bob}/remark"),
            Some(json!({"remark": "老同学"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, rows) = call(&app, req("GET", "/api/friends", None, Some(&at))).await;
    assert_eq!(rows[0]["remark"].as_str().unwrap(), "老同学");
    // bob's own row is untouched: remarks are per-side
    let (_, brows) = call(&app, req("GET", "/api/friends", None, Some(&bt))).await;
    assert_eq!(brows[0]["remark"].as_str().unwrap(), "");
    assert_eq!(brows[0]["friend_id"].as_str().unwrap(), alice);

    // non-friend remark is 404
    let (carol, ct) = signup(&app, "carol").await;
    let (status, _) = call(
        &app,
        req(
            "PUT",
            &format!("/api/friends/{carol}/remark"),
            Some(json!({"remark": "x"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let _ = ct;
}

#[tokio::test]
async fn removal_is_mutual_and_logged() {
    let (state, rec) = scene().await;
    let app = app(state.clone());
    let (_alice, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let (_, created) = call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"to_user_id": bob})),
            Some(&at),
        ),
    )
    .await;
    let req_id = created["id"].as_str().unwrap().to_string();
    call(
        &app,
        req(
            "POST",
            &format!("/api/friends/requests/{req_id}/accept"),
            None,
            Some(&bt),
        ),
    )
    .await;
    rec.take();

    let (status, _) = call(
        &app,
        req("DELETE", &format!("/api/friends/{bob}"), None, Some(&at)),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, arows) = call(&app, req("GET", "/api/friends", None, Some(&at))).await;
    assert!(arows.as_array().unwrap().is_empty(), "gone from alice");
    let (_, brows) = call(&app, req("GET", "/api/friends", None, Some(&bt))).await;
    assert!(brows.as_array().unwrap().is_empty(), "gone from bob too");

    // both sides got a contact-log entry and bob got the system notice
    let (_, changes) = call(
        &app,
        req("GET", "/api/friends/changes?since=0", None, Some(&bt)),
    )
    .await;
    let kinds: Vec<&str> = changes["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["request", "accepted", "removed"]);

    let events = rec.take();
    assert_eq!(events.len(), 1);
    assert!(events[0].to == vec![bob]);
}

#[tokio::test]
async fn removing_a_non_friend_is_404() {
    let (state, _rec) = scene().await;
    let app = app(state.clone());
    let (_, at) = signup(&app, "alice").await;
    let (bob, _) = signup(&app, "bob").await;
    let (status, _) = call(
        &app,
        req("DELETE", &format!("/api/friends/{bob}"), None, Some(&at)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn changes_cursor_returns_only_the_delta() {
    let (state, _rec) = scene().await;
    let app = app(state.clone());
    let (alice, at) = signup(&app, "alice").await;
    let (bob, _) = signup(&app, "bob").await;
    let (carol, _) = signup(&app, "carol").await;

    call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"to_user_id": bob})),
            Some(&at),
        ),
    )
    .await;
    let (_, first) = call(
        &app,
        req("GET", "/api/friends/changes?since=0", None, Some(&at)),
    )
    .await;
    let cursor = first["cursor"].as_i64().unwrap();
    assert_eq!(first["changes"].as_array().unwrap().len(), 1);
    assert_eq!(first["has_more"].as_bool(), Some(false));

    // a second event after the cursor shows up; the first does not repeat
    call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"to_user_id": carol})),
            Some(&at),
        ),
    )
    .await;
    let (_, next) = call(
        &app,
        req(
            "GET",
            &format!("/api/friends/changes?since={cursor}"),
            None,
            Some(&at),
        ),
    )
    .await;
    let rows = next["changes"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "incremental, no repeats");
    assert_eq!(rows[0]["subject_id"].as_str().unwrap(), carol);
    assert!(rows[0]["seq"].as_i64().unwrap() > cursor);

    // the cursor is per-user: bob's own log is untouched by alice's actions
    let (status, brows) = call(
        &app,
        req(
            "POST",
            "/api/login",
            Some(json!({"name": "bob", "password": "pw"})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let bt = brows["token"].as_str().unwrap();
    let (_, bchanges) = call(
        &app,
        req("GET", "/api/friends/changes?since=0", None, Some(bt)),
    )
    .await;
    assert_eq!(bchanges["changes"].as_array().unwrap().len(), 1);
    assert_eq!(
        bchanges["changes"][0]["subject_id"].as_str().unwrap(),
        alice
    );
}

#[tokio::test]
async fn friend_endpoints_require_auth() {
    let (state, _rec) = scene().await;
    let app = app(state.clone());
    for (method, path) in [
        ("POST", "/api/friends/requests"),
        ("GET", "/api/friends"),
        ("GET", "/api/friends/requests/incoming"),
        ("GET", "/api/friends/requests/outgoing"),
        ("GET", "/api/friends/changes"),
        ("DELETE", "/api/friends/ffffffffffffffff"),
        ("PUT", "/api/friends/ffffffffffffffff/remark"),
    ] {
        let body = match method {
            "POST" => Some(json!({"to_user_id": "ffffffffffffffff"})),
            "PUT" => Some(json!({"remark": "x"})),
            _ => None,
        };
        let (status, _) = call(&app, req(method, path, body, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
    }
}

#[tokio::test]
async fn outgoing_inbox_lists_are_correct() {
    let (state, _rec) = scene().await;
    let app = app(state.clone());
    let (_, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"to_user_id": bob})),
            Some(&at),
        ),
    )
    .await;

    let (_, out) = call(
        &app,
        req("GET", "/api/friends/requests/outgoing", None, Some(&at)),
    )
    .await;
    assert_eq!(out.as_array().unwrap().len(), 1);
    let (_, inc) = call(
        &app,
        req("GET", "/api/friends/requests/incoming", None, Some(&bt)),
    )
    .await;
    assert_eq!(inc.as_array().unwrap().len(), 1);
    let (_, binc) = call(
        &app,
        req("GET", "/api/friends/requests/outgoing", None, Some(&bt)),
    )
    .await;
    assert!(binc.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn re_request_after_decline_is_allowed_again() {
    let (state, _rec) = scene().await;
    let app = app(state.clone());
    let (_, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let (_, created) = call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"to_user_id": bob})),
            Some(&at),
        ),
    )
    .await;
    let req_id = created["id"].as_str().unwrap().to_string();
    call(
        &app,
        req(
            "POST",
            &format!("/api/friends/requests/{req_id}/decline"),
            None,
            Some(&bt),
        ),
    )
    .await;

    let (status, again) = call(
        &app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"to_user_id": bob})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "declined request can be recreated"
    );
    assert_eq!(again["status"].as_str().unwrap(), "pending");
    // and it is acceptable again
    let rid = again["id"].as_str().unwrap().to_string();
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/friends/requests/{rid}/accept"),
            None,
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, rows) = call(&app, req("GET", "/api/friends", None, Some(&at))).await;
    assert_eq!(rows.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn domain_api_directly_persists_without_a_sink() {
    // A platform with no hub wired still stores relationships (single-node dev).
    let state = Platform::in_memory().unwrap().shared();
    let alice = state.register("a", "pw").unwrap();
    let bob = state.register("b", "pw").unwrap();
    let (rid, _) = state.send_friend_request(&alice, &bob, "").unwrap();
    state.accept_request(&bob, &rid).unwrap();
    assert!(state.are_friends(&alice, &bob).unwrap());
    assert!(state.are_friends(&bob, &alice).unwrap());
    assert_eq!(state.friends(&alice).unwrap().len(), 1);
    // seq is a global log position, not a per-user counter
    let entries = state.changes(&alice, 0, 10).unwrap();
    assert_eq!(
        entries.iter().map(|c| c.kind.as_str()).collect::<Vec<_>>(),
        vec!["request", "accepted"]
    );
    assert_eq!(state.changes_head(&alice).unwrap(), entries[1].seq);
    assert_eq!(state.changes(&alice, entries[0].seq, 10).unwrap().len(), 1);
    // self, unknown target and malformed ids are refused at the domain layer too
    assert!(state.send_friend_request(&alice, &alice, "").is_err());
    assert!(state.send_friend_request(&alice, "zz!", "").is_err());
    assert!(state
        .send_friend_request(&alice, "ffffffffffffffff", "")
        .is_err());
    assert!(state.accept_request(&alice, "0000000000000000").is_err());
}
