//! WP-04 A1 + A4: group model, permissions, member cap, idempotent adds,
//! membership-cache invalidation, and the system envelopes group ops emit.

mod common;

use axum::http::StatusCode;
use common::{app, call, platform, req, signup, Recorder};
use serde_json::json;
use std::sync::Arc;
use wxlike_core::MsgKind;
use wxlike_platform::PlatformState;

async fn mk_app() -> (PlatformState, axum::Router, Recorder) {
    let state = platform();
    let rec = Recorder::new();
    state.set_event_sink(Arc::new(rec.clone()));
    let router = app(state.clone());
    (state, router, rec)
}

async fn create(app: &axum::Router, token: &str, name: &str, members: &[String]) -> String {
    let (status, body) = call(
        app,
        req(
            "POST",
            "/api/groups",
            Some(json!({"name": name, "member_ids": members})),
            Some(token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create group: {body}");
    body["group_id"].as_str().unwrap().to_string()
}

async fn roster(app: &axum::Router, token: &str, group: &str) -> Vec<(String, String)> {
    let (_, body) = call(
        app,
        req("GET", &format!("/api/groups/{group}"), None, Some(token)),
    )
    .await;
    body["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["user_id"].as_str().unwrap().to_string(),
                m["role"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// Grants a role directly: WP-04's contract has no promotion endpoint, but the
/// permission checks for `admin` are still under test.
async fn promote(state: &PlatformState, group: &str, user: &str, role: &str) {
    {
        let c = state.db().lock();
        c.execute(
            "UPDATE group_members SET role = ?3 WHERE group_id = ?1 AND user_id = ?2",
            rusqlite::params![group, user, role],
        )
        .unwrap();
    }
    state.invalidate_group(group);
}

#[tokio::test]
async fn creator_is_the_owner_and_a_member() {
    let (_state, app, _rec) = mk_app().await;
    let (alice, at) = signup(&app, "alice").await;
    let id = create(&app, &at, "rust fans", &[]).await;

    let members = roster(&app, &at, &id).await;
    assert_eq!(members.len(), 1, "creator is included by default");
    assert_eq!(members[0], (alice.clone(), "owner".to_string()));

    let (_, info) = call(
        &app,
        req("GET", &format!("/api/groups/{id}"), None, Some(&at)),
    )
    .await;
    assert_eq!(info["name"].as_str().unwrap(), "rust fans");
    assert_eq!(info["my_role"].as_str().unwrap(), "owner");
    assert_eq!(info["owner_uid"].as_str().unwrap(), alice);
}

#[tokio::test]
async fn creation_with_members_is_visible_to_all() {
    let (_state, app, rec) = mk_app().await;
    let (_, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let (carol, ct) = signup(&app, "carol").await;

    let id = create(&app, &at, "trio", &[bob.clone(), carol.clone()]).await;
    for token in [&at, &bt, &ct] {
        assert_eq!(roster(&app, token, &id).await.len(), 3, "same roster");
    }

    // announced inside the group, and not to the creator
    let events = rec.take();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, MsgKind::System);
    assert_eq!(events[0].conv_id, id, "conv_id is the group id");
    let mut to = events[0].to.clone();
    to.sort();
    let mut expected = vec![bob.clone(), carol.clone()];
    expected.sort();
    assert_eq!(to, expected);
}

#[tokio::test]
async fn adding_an_existing_member_is_idempotent() {
    let (_state, app, _rec) = mk_app().await;
    let (_, at) = signup(&app, "alice").await;
    let (bob, _) = signup(&app, "bob").await;
    let id = create(&app, &at, "g", std::slice::from_ref(&bob)).await;

    let (status, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/groups/{id}/members"),
            Some(json!({"user_ids": [bob]})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["added"].as_i64(), Some(0), "duplicate add adds nobody");
    assert_eq!(roster(&app, &at, &id).await.len(), 2);
}

#[tokio::test]
async fn plain_member_may_not_invite() {
    let (_state, app, rec) = mk_app().await;
    let (_, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let (carol, _) = signup(&app, "carol").await;
    let id = create(&app, &at, "g", &[bob]).await;
    rec.take();

    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/groups/{id}/members"),
            Some(json!({"user_ids": [carol]})),
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(roster(&app, &at, &id).await.len(), 2);
    assert_eq!(rec.count(), 0, "a refused invite announces nothing");
}

#[tokio::test]
async fn admin_may_invite() {
    let (state, app, _rec) = mk_app().await;
    let (_, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let (carol, _) = signup(&app, "carol").await;
    let id = create(&app, &at, "g", std::slice::from_ref(&bob)).await;
    promote(&state, &id, &bob, "admin").await;

    let (status, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/groups/{id}/members"),
            Some(json!({"user_ids": [carol]})),
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin invites: {body}");
    assert_eq!(body["added"].as_i64(), Some(1));
    assert_eq!(roster(&app, &at, &id).await.len(), 3);
}

#[tokio::test]
async fn only_the_owner_may_remove() {
    let (state, app, _rec) = mk_app().await;
    let (_, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let (carol, _) = signup(&app, "carol").await;
    let id = create(&app, &at, "g", &[bob.clone(), carol.clone()]).await;
    promote(&state, &id, &bob, "admin").await;

    // an admin can invite but not kick (contract: DELETE is owner-only)
    let (status, _) = call(
        &app,
        req(
            "DELETE",
            &format!("/api/groups/{id}/members/{carol}"),
            None,
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(roster(&app, &at, &id).await.len(), 3);

    let (status, _) = call(
        &app,
        req(
            "DELETE",
            &format!("/api/groups/{id}/members/{carol}"),
            None,
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let after: Vec<String> = roster(&app, &at, &id)
        .await
        .into_iter()
        .map(|(u, _)| u)
        .collect();
    assert!(!after.contains(&carol), "removed");
}

#[tokio::test]
async fn the_owner_row_cannot_be_removed() {
    let (_state, app, _rec) = mk_app().await;
    let (alice, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let id = create(&app, &at, "g", &[bob]).await;

    // a member cannot remove the owner
    let (status, _) = call(
        &app,
        req(
            "DELETE",
            &format!("/api/groups/{id}/members/{alice}"),
            None,
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // the owner cannot delete her own membership row either
    let (status, _) = call(
        &app,
        req(
            "DELETE",
            &format!("/api/groups/{id}/members/{alice}"),
            None,
            Some(&at),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "group keeps an owner"
    );
    assert_eq!(roster(&app, &at, &id).await.len(), 2);
}

#[tokio::test]
async fn leaving_a_group_stops_membership() {
    let (_state, app, rec) = mk_app().await;
    let (_, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let (carol, _) = signup(&app, "carol").await;
    let id = create(&app, &at, "g", &[bob.clone(), carol.clone()]).await;
    rec.take();

    let (status, _) = call(
        &app,
        req("POST", &format!("/api/groups/{id}/leave"), None, Some(&bt)),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let after = roster(&app, &at, &id).await;
    assert_eq!(after.len(), 2, "bob is gone");
    assert!(after.iter().all(|(u, _)| *u != bob));

    let events = rec.take();
    assert_eq!(events.len(), 1, "the group was told");
    assert!(!events[0].to.contains(&bob), "the leaver is not notified");
    assert!(matches!(
        events[0].body,
        wxlike_core::MsgBody::Notice(ref t) if t.contains("退出")
    ));
}

#[tokio::test]
async fn owner_cannot_leave_without_transferring() {
    let (_state, app, _rec) = mk_app().await;
    let (_, at) = signup(&app, "alice").await;
    let id = create(&app, &at, "g", &[]).await;
    let (status, _) = call(
        &app,
        req("POST", &format!("/api/groups/{id}/leave"), None, Some(&at)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(roster(&app, &at, &id).await.len(), 1);
}

#[tokio::test]
async fn remark_is_per_member() {
    let (_state, app, _rec) = mk_app().await;
    let (_, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let id = create(&app, &at, "g", std::slice::from_ref(&bob)).await;

    let (status, _) = call(
        &app,
        req(
            "PUT",
            &format!("/api/groups/{id}/remark-mine"),
            Some(json!({"remark": "老同学"})),
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let remark_of = |v: &serde_json::Value, who: &str| -> String {
        v["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["user_id"].as_str() == Some(who))
            .map(|m| m["my_remark"].as_str().unwrap().to_string())
            .unwrap_or_default()
    };
    let (_, a_view) = call(
        &app,
        req("GET", &format!("/api/groups/{id}"), None, Some(&at)),
    )
    .await;
    let (_, b_view) = call(
        &app,
        req("GET", &format!("/api/groups/{id}"), None, Some(&bt)),
    )
    .await;
    // my_remark belongs to the member's own row (that member's nickname for the
    // group), so both views show the same roster: bob's row carries his remark,
    // alice's row stays empty until she sets her own.
    assert_eq!(remark_of(&b_view, &bob), "老同学", "bob set his own row");
    assert_eq!(remark_of(&a_view, &bob), "老同学", "same row, same value");
    assert_eq!(
        remark_of(&a_view, &alice_id(&a_view)),
        "",
        "alice has set nothing"
    );

    let (status, _) = call(
        &app,
        req(
            "PUT",
            &format!("/api/groups/{id}/remark-mine"),
            Some(json!({"remark": "项目组"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, a2) = call(
        &app,
        req("GET", &format!("/api/groups/{id}"), None, Some(&at)),
    )
    .await;
    assert_eq!(remark_of(&a2, &alice_id(&a2)), "项目组");
    assert_eq!(remark_of(&a2, &bob), "老同学", "bob's row untouched");
}

/// The owner's own user id, read back from the roster so the assertion does not
/// depend on which row is which by position.
fn alice_id(view: &serde_json::Value) -> String {
    view["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"].as_str() == Some("owner"))
        .map(|m| m["user_id"].as_str().unwrap().to_string())
        .unwrap_or_default()
}

#[tokio::test]
async fn group_info_requires_membership() {
    let (_state, app, _rec) = mk_app().await;
    let (_, at) = signup(&app, "alice").await;
    let (stranger, st) = signup(&app, "stranger").await;
    let id = create(&app, &at, "private", &[]).await;

    let (status, _) = call(
        &app,
        req("GET", &format!("/api/groups/{id}"), None, Some(&st)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{stranger} is not a member");

    let (status, _) = call(
        &app,
        req("GET", "/api/groups/ffffffffffffffff", None, Some(&at)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn my_groups_is_scoped_to_the_caller() {
    let (_state, app, _rec) = mk_app().await;
    let (_, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let (_, ct) = signup(&app, "carol").await;
    let id = create(&app, &at, "a+b", &[bob]).await;

    let (_, a_mine) = call(&app, req("GET", "/api/groups/mine", None, Some(&at))).await;
    let (_, b_mine) = call(&app, req("GET", "/api/groups/mine", None, Some(&bt))).await;
    let (_, c_mine) = call(&app, req("GET", "/api/groups/mine", None, Some(&ct))).await;
    assert_eq!(a_mine.as_array().unwrap().len(), 1);
    assert_eq!(b_mine.as_array().unwrap().len(), 1);
    assert_eq!(c_mine.as_array().unwrap().len(), 0, "carol sees nothing");
    assert_eq!(b_mine[0]["id"].as_str().unwrap(), id);
    assert_eq!(b_mine[0]["my_role"].as_str().unwrap(), "member");
    assert_eq!(b_mine[0]["member_count"].as_i64(), Some(2));
}

/// Seeds many users directly: the cap is under test, not registration.
async fn seed_users(state: &PlatformState, prefix: &str, count: usize) -> Vec<String> {
    let ids: Vec<String> = (0..count).map(|i| format!("{prefix}{i:04}")).collect();
    {
        let c = state.db().lock();
        for id in &ids {
            c.execute(
                "INSERT INTO users(id, name, pw_salt, pw_hash, pw_rounds, avatar, signature, created_at)
                 VALUES(?1, ?1, 'x', 'x', 1, '', '', 0)",
                rusqlite::params![id],
            )
            .unwrap();
        }
    }
    ids
}

#[tokio::test]
async fn member_cap_is_enforced_on_add() {
    let (state, app, _rec) = mk_app().await;
    let (_, at) = signup(&app, "alice").await;
    let ids = seed_users(&state, "u", 1_001).await;
    let id = create(&app, &at, "massive", &[ids[0].clone()]).await;

    let (status, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/groups/{id}/members"),
            Some(json!({"user_ids": ids[1..]})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "cap: {body}");
    assert_eq!(
        roster(&app, &at, &id).await.len(),
        2,
        "the oversized batch was rejected whole, not half-applied"
    );
}

#[tokio::test]
async fn creating_over_the_cap_is_refused() {
    let (state, app, _rec) = mk_app().await;
    let (_, at) = signup(&app, "alice").await;
    let ids = seed_users(&state, "c", 1_000).await;
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/groups",
            Some(json!({"name": "full", "member_ids": ids})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "1000 + creator > cap"
    );
    let (_, mine) = call(&app, req("GET", "/api/groups/mine", None, Some(&at))).await;
    assert_eq!(mine.as_array().unwrap().len(), 0, "nothing was written");
}

#[tokio::test]
async fn membership_cache_invalidates_on_change() {
    let (state, _app, _rec) = mk_app().await;
    let alice = state.register("alice", "pw").unwrap();
    let bob = state.register("bob", "pw").unwrap();
    let carol = state.register("carol", "pw").unwrap();
    let id = state
        .create_group(&alice, "g", std::slice::from_ref(&bob))
        .unwrap();

    let first = state.cached_members(&id).unwrap().unwrap();
    assert_eq!(first.len(), 2);
    assert_eq!(state.group_cache_len(), 1, "populate on first read");

    state
        .add_members(&id, &alice, std::slice::from_ref(&carol))
        .unwrap();
    let after = state.cached_members(&id).unwrap().unwrap();
    assert_eq!(after.len(), 3, "cache dropped, not served stale");
    assert!(after.contains(&carol));

    state.remove_member(&id, &alice, &carol).unwrap();
    assert_eq!(state.cached_members(&id).unwrap().unwrap().len(), 2);

    state.leave_group(&id, &bob).unwrap();
    assert_eq!(state.cached_members(&id).unwrap().unwrap().len(), 1);

    // a conversation that is not a group stays "None", preserving DM semantics
    assert_eq!(state.cached_members("not-a-group").unwrap(), None);
}

#[tokio::test]
async fn group_endpoints_require_auth() {
    let (_state, app, _rec) = mk_app().await;
    for (method, path) in [
        ("POST", "/api/groups"),
        ("GET", "/api/groups/mine"),
        ("GET", "/api/groups/ffffffffffffffff"),
        ("POST", "/api/groups/ffffffffffffffff/leave"),
        ("POST", "/api/groups/ffffffffffffffff/members"),
        (
            "DELETE",
            "/api/groups/ffffffffffffffff/members/eeeeeeeeeeeeeeee",
        ),
        ("PUT", "/api/groups/ffffffffffffffff/remark-mine"),
    ] {
        let body = match method {
            "POST" if path.ends_with("groups") => Some(json!({"name": "x"})),
            "POST" if path.ends_with("members") => Some(json!({"user_ids": []})),
            "PUT" => Some(json!({"remark": "x"})),
            _ => None,
        };
        let (status, _) = call(&app, req(method, path, body, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
    }
}

#[tokio::test]
async fn unknown_member_target_is_404() {
    let (_state, app, _rec) = mk_app().await;
    let (_, at) = signup(&app, "alice").await;
    let id = create(&app, &at, "g", &[]).await;

    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/groups/{id}/members"),
            Some(json!({"user_ids": ["ffffffffffffffff"]})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/groups",
            Some(json!({"name": "g2", "member_ids": ["eeeeeeeeeeeeeeee"]})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, mine) = call(&app, req("GET", "/api/groups/mine", None, Some(&at))).await;
    assert_eq!(mine.as_array().unwrap().len(), 1, "no half-created group");
}

#[tokio::test]
async fn listing_yourself_at_creation_does_not_duplicate() {
    let (_state, app, _rec) = mk_app().await;
    let (alice, at) = signup(&app, "alice").await;
    let id = create(&app, &at, "g", &[alice.clone(), alice.clone()]).await;
    let members = roster(&app, &at, &id).await;
    assert_eq!(members.len(), 1);
    assert_eq!(members[0], (alice, "owner".to_string()));
}

#[tokio::test]
async fn remove_unknown_member_and_remark_without_membership() {
    let (_state, app, _rec) = mk_app().await;
    let (_, at) = signup(&app, "alice").await;
    let (stranger, _st) = signup(&app, "stranger").await;
    let id = create(&app, &at, "g", &[]).await;

    let (status, _) = call(
        &app,
        req(
            "DELETE",
            &format!("/api/groups/{id}/members/{stranger}"),
            None,
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (_, ct) = signup(&app, "carol").await;
    let (status, _) = call(
        &app,
        req(
            "PUT",
            &format!("/api/groups/{id}/remark-mine"),
            Some(json!({"remark": "x"})),
            Some(&ct),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "non-member cannot set a remark"
    );
}

#[tokio::test]
async fn domain_layer_permissions() {
    let (state, _app, _rec) = mk_app().await;
    let alice = state.register("alice", "pw").unwrap();
    let bob = state.register("bob", "pw").unwrap();
    let carol = state.register("carol", "pw").unwrap();
    let id = state
        .create_group(&alice, "g", &[bob.clone(), carol.clone()])
        .unwrap();

    assert!(
        state.remove_member(&id, &bob, &carol).is_err(),
        "member cannot kick"
    );
    assert!(state.is_member(&id, &carol).unwrap(), "target still in");
    assert!(
        state
            .add_members(&id, &bob, std::slice::from_ref(&carol))
            .is_err(),
        "member cannot invite"
    );
    state.remove_member(&id, &alice, &carol).unwrap();
    assert!(!state.is_member(&id, &carol).unwrap());
    assert_eq!(state.role_of(&id, &alice).unwrap(), "owner");
    assert!(state.role_of(&id, &carol).is_err(), "removed has no role");
    assert_eq!(state.group_header(&id).unwrap().unwrap().1, "g");
    assert!(state.group_header("nosuchgroup").unwrap().is_none());
}

/// 群成员列表只对成员开放：非成员拿 403，而不是空数组。
/// 空数组会泄露"这个群存在但你没在里面"，403 连存在性都不确认。
#[tokio::test]
async fn members_list_is_for_members_only() {
    let router = app(common::platform());
    let (alice, ta) = signup(&router, "alice").await;
    let (_bob, tb) = signup(&router, "bob").await;

    let r = req(
        "POST",
        "/api/groups",
        Some(json!({"name": "team", "member_ids": []})),
        Some(&ta),
    );
    let (_, body) = common::call(&router, r).await;
    let gid = body["group_id"].as_str().expect("group_id").to_string();

    let r = req(
        "GET",
        &format!("/api/groups/{gid}/members"),
        None,
        Some(&ta),
    );
    let (status, body) = common::call(&router, r).await;
    assert_eq!(status, 200, "{body}");
    let rows = body.as_array().expect("array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["user_id"], json!(alice));

    let r = req(
        "GET",
        &format!("/api/groups/{gid}/members"),
        None,
        Some(&tb),
    );
    let (status, _) = common::call(&router, r).await;
    assert_eq!(status, 403, "非成员不该看到成员列表");
}

/// 幂等重放：agent 重试建群时带同一个 `Idempotency-Key`，
/// 必须返回**同一个群**，而不是建出两个。
#[tokio::test]
async fn idempotency_key_makes_group_create_replayable() {
    let router = app(common::platform());
    let (_alice, ta) = signup(&router, "alice").await;

    let mk = || {
        let mut r = req(
            "POST",
            "/api/groups",
            Some(json!({"name": "team", "member_ids": []})),
            Some(&ta),
        );
        r.headers_mut()
            .insert("idempotency-key", "key-1".parse().unwrap());
        r
    };

    let (s1, b1) = common::call(&router, mk()).await;
    assert_eq!(s1, 200, "{b1}");
    let (s2, b2) = common::call(&router, mk()).await;
    assert_eq!(s2, 200, "{b2}");
    assert_eq!(b1["group_id"], b2["group_id"], "同一个 key 必须是同一个群");

    // 换一个 key 才是新群
    let mut r = req(
        "POST",
        "/api/groups",
        Some(json!({"name": "team", "member_ids": []})),
        Some(&ta),
    );
    r.headers_mut()
        .insert("idempotency-key", "key-2".parse().unwrap());
    let (_, b3) = common::call(&router, r).await;
    assert_ne!(b3["group_id"], b1["group_id"]);
}
