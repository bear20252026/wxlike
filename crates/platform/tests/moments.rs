//! WP-07 A1 + A2: publishing, visibility (read-time), likes, comment tree,
//! author-only delete, and the descending feed cursor.

mod common;

use axum::http::StatusCode;
use common::{app, call, req, signup};
use serde_json::json;
use wxlike_platform::PlatformState;

async fn scene() -> (PlatformState, axum::Router) {
    let state = common::platform();
    let router = app(state.clone());
    (state, router)
}

/// Media ids must exist (WP-05) before a moment can reference them.
async fn seed_media(state: &PlatformState, count: usize) -> Vec<String> {
    let ids: Vec<String> = (0..count)
        .map(|i| {
            let mut s = String::new();
            for d in 0..64 {
                s.push(char::from(b'0' + ((i * 7 + d * 3) % 10) as u8));
            }
            s
        })
        .collect();
    {
        let c = state.db().lock();
        for id in &ids {
            c.execute(
                "INSERT OR IGNORE INTO media(id, size, content_type, uploader, has_thumb, created_at)
                 VALUES(?1, 10, 'image/png', 'seeder', 0, 0)",
                rusqlite::params![id],
            )
            .unwrap();
        }
    }
    ids
}

async fn publish(
    app: &axum::Router,
    token: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    call(app, req("POST", "/api/moments", Some(body), Some(token))).await
}

async fn detail(app: &axum::Router, token: &str, id: &str) -> (StatusCode, serde_json::Value) {
    call(
        app,
        req("GET", &format!("/api/moments/{id}"), None, Some(token)),
    )
    .await
}

async fn feed(app: &axum::Router, token: &str, query: &str) -> serde_json::Value {
    let (status, body) = call(
        app,
        req(
            "GET",
            &format!("/api/moments/feed{query}"),
            None,
            Some(token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// alice + bob become mutual friends through the WP-02 endpoints.
async fn befriend(app: &axum::Router, at: &str, bob_id: &str) -> String {
    let (status, body) = call(
        app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"to_user_id": bob_id})),
            Some(at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn publish_with_media_round_trips_through_detail() {
    let (st, app) = scene().await;
    let (_uid, at) = signup(&app, "alice").await;
    let media = seed_media(&st, 2).await;

    let (status, body) = publish(
        &app,
        &at,
        json!({"text": "shipped the hub", "media_ids": media, "visibility": "public"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["moment_id"].as_str().unwrap().to_string();

    let (status, d) = detail(&app, &at, &id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(d["text"].as_str().unwrap(), "shipped the hub");
    assert_eq!(d["visibility"].as_str().unwrap(), "public");
    assert_eq!(d["media_ids"].as_array().unwrap().len(), 2);
    assert_eq!(d["moment_id"].as_str().unwrap(), id);
    assert_eq!(d["like_count"].as_i64(), Some(0));
    assert_eq!(d["likes"].as_array().unwrap().len(), 0);
    assert_eq!(d["comments"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn publish_input_validation() {
    let (st, app) = scene().await;
    let (_uid, at) = signup(&app, "alice").await;
    let _media = seed_media(&st, 1).await;

    // unknown media id
    let (status, _) = publish(
        &app,
        &at,
        json!({"text": "dangling", "media_ids": ["ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"]}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "unknown media id");

    // more than the nine-grid cap
    let many: Vec<String> = seed_media(&st, 10).await;
    let (status, _) = publish(&app, &at, json!({"text": "too many", "media_ids": many})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "10 images");

    // A2: the contract allows refusing over-long text, which is what we do
    let (status, _) = publish(&app, &at, json!({"text": "x".repeat(5_001)})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "5001 chars");
    let (status, _) = publish(&app, &at, json!({"text": "x".repeat(5_000)})).await;
    assert_eq!(status, StatusCode::CREATED, "exactly at the limit is fine");

    // neither text nor media
    let (status, _) = publish(&app, &at, json!({"text": "   ", "media_ids": []})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "empty moment");

    // an unknown visibility value is a deserialization failure, not a silent default
    let (status, _) = publish(&app, &at, json!({"text": "hi", "visibility": "secret"})).await;
    assert!(status.is_client_error(), "{status}");
}

#[tokio::test]
async fn friends_visibility_is_enforced_at_read_time() {
    let (_st, app) = scene().await;
    let (_alice, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let (_carol, ct) = signup(&app, "carol").await;

    let (_, body) = publish(&app, &at, json!({"text": "only friends see this"})).await;
    let id = body["moment_id"].as_str().unwrap().to_string();

    // default is friends: a non-friend gets "no such moment"
    let (status, _) = detail(&app, &ct, &id).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "stranger cannot see a friends post"
    );

    // becoming a friend is enough — no re-publish, no snapshot refresh
    let req_id = befriend(&app, &at, &bob).await;
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
    let (status, d) = detail(&app, &bt, &id).await;
    assert_eq!(status, StatusCode::OK, "a new friend can read history");
    assert_eq!(d["text"].as_str().unwrap(), "only friends see this");

    // the author always sees their own
    let (status, _) = detail(&app, &at, &id).await;
    assert_eq!(status, StatusCode::OK);
    let _ = ct;
}

#[tokio::test]
async fn unfriending_hides_the_history_again() {
    let (_st, app) = scene().await;
    let (alice, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let req_id = befriend(&app, &at, &bob).await;
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

    let (_, body) = publish(&app, &at, json!({"text": "friend-only memory"})).await;
    let id = body["moment_id"].as_str().unwrap().to_string();
    assert_eq!(detail(&app, &bt, &id).await.0, StatusCode::OK);

    // A1: the feed must not keep showing an ex-friend's history
    let (status, _) = call(
        &app,
        req("DELETE", &format!("/api/friends/{alice}"), None, Some(&bt)),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        detail(&app, &bt, &id).await.0,
        StatusCode::NOT_FOUND,
        "read-time visibility"
    );

    let b_feed = feed(&app, &bt, "?since=0").await;
    assert!(
        b_feed["moments"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["moment_id"].as_str() != Some(&id)),
        "gone from the feed too"
    );
}

#[tokio::test]
async fn public_moment_is_readable_by_anyone_but_stays_out_of_stranger_feeds() {
    let (_st, app) = scene().await;
    let (_alice, at) = signup(&app, "alice").await;
    let (_bob, bt) = signup(&app, "bob").await;

    let (_, body) = publish(
        &app,
        &at,
        json!({"text": "hello world", "visibility": "public"}),
    )
    .await;
    let id = body["moment_id"].as_str().unwrap().to_string();

    // by id: anyone may read a public moment
    assert_eq!(detail(&app, &bt, &id).await.0, StatusCode::OK);
    // feed: contract says self + friends only, so a stranger's post is absent
    let b_feed = feed(&app, &bt, "?since=0").await;
    assert!(
        b_feed["moments"].as_array().unwrap().is_empty(),
        "no strangers in the feed"
    );
}

#[tokio::test]
async fn likes_are_idempotent_and_reversible() {
    let (_st, app) = scene().await;
    let (_alice, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let req_id = befriend(&app, &at, &bob).await;
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

    let (_, body) = publish(&app, &at, json!({"text": "like me"})).await;
    let id = body["moment_id"].as_str().unwrap().to_string();

    let (status, first) = call(
        &app,
        req("POST", &format!("/api/moments/{id}/likes"), None, Some(&bt)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["added"].as_bool(), Some(true));

    // A1: a second like is still a 200 but adds nothing
    let (status, second) = call(
        &app,
        req("POST", &format!("/api/moments/{id}/likes"), None, Some(&bt)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "idempotent");
    assert_eq!(second["added"].as_bool(), Some(false));

    let (_, d) = detail(&app, &at, &id).await;
    assert_eq!(d["like_count"].as_i64(), Some(1), "still one like");
    assert_eq!(d["likes"].as_array().unwrap().as_slice(), &[json!(bob)]);

    let (_, removed) = call(
        &app,
        req(
            "DELETE",
            &format!("/api/moments/{id}/likes"),
            None,
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(removed["removed"].as_bool(), Some(true));
    let (_, d) = detail(&app, &at, &id).await;
    assert_eq!(d["like_count"].as_i64(), Some(0));
    // unliking when not liked is a no-op, not an error
    let (_, again) = call(
        &app,
        req(
            "DELETE",
            &format!("/api/moments/{id}/likes"),
            None,
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(again["removed"].as_bool(), Some(false));
}

#[tokio::test]
async fn comment_tree_links_replies_to_their_parent() {
    let (_st, app) = scene().await;
    let (alice, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let (_carol, _ct) = signup(&app, "carol").await;
    let req_id = befriend(&app, &at, &bob).await;
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

    let (_, body) = publish(&app, &at, json!({"text": "questions?"})).await;
    let id = body["moment_id"].as_str().unwrap().to_string();

    let (_, top) = call(
        &app,
        req(
            "POST",
            &format!("/api/moments/{id}/comments"),
            Some(json!({"text": "first"})),
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(
        top["comment_id"].as_str().map(|s| !s.is_empty()),
        Some(true)
    );
    let top_id = top["comment_id"].as_str().unwrap().to_string();

    let (_, reply) = call(
        &app,
        req(
            "POST",
            &format!("/api/moments/{id}/comments"),
            Some(json!({"text": "in reply", "reply_to_comment_id": top_id})),
            Some(&at),
        ),
    )
    .await;
    let reply_id = reply["comment_id"].as_str().unwrap().to_string();

    let (_, d) = detail(&app, &bt, &id).await;
    let comments = d["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 2);
    assert_eq!(d["comment_count"].as_i64(), Some(2));
    // adjacency list: the tree is the reply_to links
    assert_eq!(comments[0]["reply_to"].as_str().unwrap(), "");
    assert_eq!(comments[1]["comment_id"].as_str().unwrap(), reply_id);
    assert_eq!(comments[1]["reply_to"].as_str().unwrap(), top_id);
    assert_eq!(
        comments[1]["reply_to_name"].as_str().unwrap(),
        "bob",
        "parent author for display"
    );
    assert_eq!(comments[1]["author"].as_str().unwrap(), alice);
}

#[tokio::test]
async fn comment_validation() {
    let (_st, app) = scene().await;
    let (_alice, at) = signup(&app, "alice").await;
    let (_bob, bt) = signup(&app, "bob").await;
    let (_, body) = publish(&app, &at, json!({"text": "hi", "visibility": "public"})).await;
    let id = body["moment_id"].as_str().unwrap().to_string();

    // empty and over-long
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/moments/{id}/comments"),
            Some(json!({"text": "  "})),
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/moments/{id}/comments"),
            Some(json!({"text": "z".repeat(1_001)})),
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // A2: a reply must point at a comment on the same moment, not somewhere else
    let (_, other) = publish(&app, &at, json!({"text": "second", "visibility": "public"})).await;
    let other_id = other["moment_id"].as_str().unwrap().to_string();
    let (_, c1) = call(
        &app,
        req(
            "POST",
            &format!("/api/moments/{other_id}/comments"),
            Some(json!({"text": "parent"})),
            Some(&bt),
        ),
    )
    .await;
    let foreign = c1["comment_id"].as_str().unwrap().to_string();
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/moments/{id}/comments"),
            Some(json!({"text": "cross-post reply", "reply_to_comment_id": foreign})),
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "reply must stay in its moment"
    );

    // a stranger cannot comment on a friends-only moment at all
    let (_, hidden) = publish(&app, &at, json!({"text": "friends only"})).await;
    let hidden_id = hidden["moment_id"].as_str().unwrap().to_string();
    let (_carol, ct) = signup(&app, "carol2").await;
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/moments/{hidden_id}/comments"),
            Some(json!({"text": "hi"})),
            Some(&ct),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "invisible moment has no comment thread"
    );
}

#[tokio::test]
async fn only_the_author_may_delete() {
    let (_st, app) = scene().await;
    let (_alice, at) = signup(&app, "alice").await;
    let (_bob, bt) = signup(&app, "bob").await;
    let (_, body) = publish(&app, &at, json!({"text": "mine", "visibility": "public"})).await;
    let id = body["moment_id"].as_str().unwrap().to_string();

    // A2: forging a delete against someone else's moment id
    let (status, _) = call(
        &app,
        req("DELETE", &format!("/api/moments/{id}"), None, Some(&bt)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        detail(&app, &at, &id).await.0,
        StatusCode::OK,
        "still there"
    );

    let (status, _) = call(
        &app,
        req("DELETE", &format!("/api/moments/{id}"), None, Some(&at)),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        detail(&app, &at, &id).await.0,
        StatusCode::NOT_FOUND,
        "gone for the author"
    );
    assert_eq!(
        detail(&app, &bt, &id).await.0,
        StatusCode::NOT_FOUND,
        "and for everyone"
    );
    // a second delete is not a surprise error
    let (status, _) = call(
        &app,
        req("DELETE", &format!("/api/moments/{id}"), None, Some(&at)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let my_feed = feed(&app, &at, "?since=0").await;
    assert!(
        my_feed["moments"].as_array().unwrap().is_empty(),
        "deleted rows leave the feed"
    );
}

#[tokio::test]
async fn feed_orders_newest_first_and_pages_without_gaps() {
    let (_st, app) = scene().await;
    let (_alice, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let req_id = befriend(&app, &at, &bob).await;
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

    let mut ids = Vec::new();
    for i in 0..7 {
        let author = if i % 2 == 0 { &at } else { &bt };
        let (_, body) = publish(&app, author, json!({"text": format!("post {i}")})).await;
        ids.push(body["moment_id"].as_str().unwrap().to_string());
    }

    let page = feed(&app, &at, "?since=0&limit=3").await;
    let rows = page["moments"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "limit respected");
    assert_eq!(page["has_more"].as_bool(), Some(true));
    // newest first: post 6, then 5, then 4
    assert_eq!(rows[0]["text"].as_str().unwrap(), "post 6");
    assert_eq!(rows[1]["text"].as_str().unwrap(), "post 5");
    assert_eq!(rows[2]["text"].as_str().unwrap(), "post 4");

    // walk every page with the returned cursor: no row twice, none missed
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<i64> = None;
    for _ in 0..10 {
        let query = match cursor {
            Some(c) => format!("?since={c}&limit=2"),
            None => "?limit=2".to_string(),
        };
        let page = feed(&app, &at, &query).await;
        let rows = page["moments"].as_array().unwrap();
        if rows.is_empty() {
            break;
        }
        for r in rows {
            seen.push(r["moment_id"].as_str().unwrap().to_string());
        }
        cursor = Some(page["next_cursor"].as_i64().unwrap());
    }
    let uniq: std::collections::HashSet<&String> = seen.iter().collect();
    assert_eq!(uniq.len(), seen.len(), "no moment delivered twice");
    assert_eq!(
        seen.len(),
        7,
        "and none missed (alice sees her 4 + bob's 3)"
    );
    let mut expected = ids.clone();
    expected.sort();
    let mut got = seen.clone();
    got.sort();
    assert_eq!(got, expected, "the walk covers exactly the published set");

    // the page cap is honoured (contract: <= 50)
    let page = feed(&app, &at, "?limit=500").await;
    assert_eq!(
        page["moments"].as_array().unwrap().len(),
        7,
        "capped at 50, only 7 exist"
    );
}

#[tokio::test]
async fn feed_shows_own_posts_even_without_friends() {
    let (_st, app) = scene().await;
    let (_alice, at) = signup(&app, "alice").await;
    publish(&app, &at, json!({"text": "solitary"})).await;
    let page = feed(&app, &at, "?since=0").await;
    assert_eq!(page["moments"].as_array().unwrap().len(), 1);
    assert_eq!(page["has_more"].as_bool(), Some(false));
}

#[tokio::test]
async fn moments_endpoints_require_auth_and_validate_ids() {
    let (_st, app) = scene().await;
    for (method, path) in [
        ("POST", "/api/moments"),
        ("GET", "/api/moments/feed"),
        ("GET", "/api/moments/deadbeef00000000"),
        ("DELETE", "/api/moments/deadbeef00000000"),
        ("POST", "/api/moments/deadbeef00000000/likes"),
        ("DELETE", "/api/moments/deadbeef00000000/likes"),
        ("POST", "/api/moments/deadbeef00000000/comments"),
    ] {
        let body = match method {
            "POST" if path.ends_with("moments") => Some(json!({"text": "x"})),
            "POST" if path.ends_with("comments") => Some(json!({"text": "x"})),
            _ => None,
        };
        let (status, _) = call(&app, req(method, path, body, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
    }

    // traversal-shaped ids are refused, not resolved against the filesystem
    let (_uid, at) = signup(&app, "alice").await;
    for evil in ["../../etc/passwd", "..%2f..%2f", "not-hex-id", ""] {
        let (status, _) = call(
            &app,
            req("GET", &format!("/api/moments/{evil}"), None, Some(&at)),
        )
        .await;
        assert!(status.is_client_error(), "{evil:?} -> {status}");
    }
}

#[tokio::test]
async fn unknown_moment_is_404_everywhere() {
    let (_st, app) = scene().await;
    let (_uid, at) = signup(&app, "alice").await;
    let ghost = "a".repeat(16);
    assert_eq!(detail(&app, &at, &ghost).await.0, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/moments/{ghost}/likes"),
            None,
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &app,
        req("DELETE", &format!("/api/moments/{ghost}"), None, Some(&at)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn like_counts_and_names_are_projected_in_detail() {
    let (_st, app) = scene().await;
    let (alice, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let (_, body) = publish(
        &app,
        &at,
        json!({"text": "public post", "visibility": "public"}),
    )
    .await;
    let id = body["moment_id"].as_str().unwrap().to_string();

    call(
        &app,
        req("POST", &format!("/api/moments/{id}/likes"), None, Some(&bt)),
    )
    .await;
    let (_, d) = detail(&app, &at, &id).await;
    assert_eq!(d["likes"].as_array().unwrap().as_slice(), &[json!(bob)]);
    assert_eq!(d["author"].as_str().unwrap(), alice);
    assert_eq!(d["author_name"].as_str().unwrap(), "alice");
    assert_eq!(d["like_count"].as_i64(), Some(1));
}

/// 私密动态：**只有作者自己看得到**，连好友也不行。
///
/// 这条同时盯两个入口：详情（`can_see_moment`）与 feed（SQL 的可见性过滤）。
/// 只挡一个入口的隐私是最难查的——用户在列表里看得见、点进去 404。
#[tokio::test]
async fn a_private_moment_is_invisible_to_friends_but_not_to_its_author() {
    let (_st, app) = scene().await;
    let (_alice, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let req_id = befriend(&app, &at, &bob).await;
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

    let (status, body) = publish(
        &app,
        &at,
        json!({"text": "只有我自己", "visibility": "private"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["moment_id"].as_str().unwrap().to_string();

    // 作者：详情 200
    assert_eq!(detail(&app, &at, &id).await.0, StatusCode::OK);
    // 好友：详情 404（不泄露"存在但你看不到"）
    assert_eq!(detail(&app, &bt, &id).await.0, StatusCode::NOT_FOUND);

    // feed：作者看得到，好友看不到
    let a_feed = feed(&app, &at, "").await;
    let ids: Vec<&str> = a_feed["moments"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["moment_id"].as_str())
        .collect();
    assert!(
        ids.contains(&id.as_str()),
        "作者自己的 feed 应当有：{a_feed}"
    );

    let b_feed = feed(&app, &bt, "").await;
    let ids: Vec<&str> = b_feed["moments"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["moment_id"].as_str())
        .collect();
    assert!(
        !ids.contains(&id.as_str()),
        "好友的 feed 不该出现私密动态：{b_feed}"
    );
}

/// 删评论：评论作者可删自己的，动态作者可删任何人的，第三人不行。
#[tokio::test]
async fn comment_deletion_follows_the_wechat_rule() {
    let (_st, app) = scene().await;
    let (_alice, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let (carol, ct) = signup(&app, "carol").await;
    for (uid, tok) in [(&bob, &bt), (&carol, &ct)] {
        let req_id = befriend(&app, &at, uid).await;
        call(
            &app,
            req(
                "POST",
                &format!("/api/friends/requests/{req_id}/accept"),
                None,
                Some(tok),
            ),
        )
        .await;
    }

    let (_, body) = publish(&app, &at, json!({"text": "动态"})).await;
    let moment_id = body["moment_id"].as_str().unwrap().to_string();

    let mut comment_ids = Vec::new();
    for text in ["bob 的评论", "又一条", "第三条"] {
        let (status, body) = call(
            &app,
            req(
                "POST",
                &format!("/api/moments/{moment_id}/comments"),
                Some(json!({"text": text})),
                Some(&bt),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        comment_ids.push(body["comment_id"].as_str().unwrap().to_string());
    }

    // 1) 评论作者删自己的
    let status = call(
        &app,
        req(
            "DELETE",
            &format!("/api/moments/{moment_id}/comments/{}", comment_ids[0]),
            None,
            Some(&bt),
        ),
    )
    .await
    .0;
    assert_eq!(status, StatusCode::NO_CONTENT, "评论作者应当能删自己的");

    // 2) 动态作者删别人的
    let status = call(
        &app,
        req(
            "DELETE",
            &format!("/api/moments/{moment_id}/comments/{}", comment_ids[1]),
            None,
            Some(&at),
        ),
    )
    .await
    .0;
    assert_eq!(status, StatusCode::NO_CONTENT, "动态作者应当能删别人的");

    // 3) 第三人不行（carol 既不是评论作者也不是动态作者）
    let status = call(
        &app,
        req(
            "DELETE",
            &format!("/api/moments/{moment_id}/comments/{}", comment_ids[2]),
            None,
            Some(&ct),
        ),
    )
    .await
    .0;
    assert_eq!(status, StatusCode::NOT_FOUND, "第三人删不了");

    // 删掉的从详情里消失，剩下的还在
    let (status, body) = detail(&app, &at, &moment_id).await;
    assert_eq!(status, StatusCode::OK);
    let texts: Vec<&str> = body["comments"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["text"].as_str())
        .collect();
    assert_eq!(texts, vec!["第三条"], "只该剩下没被删的那条：{body}");
}

/// 删评论后 feed 的评论计数要跟着降——计数是 `deleted = 0` 的子查询，
/// 所以这条同时验证了"软删除"没有留下幽灵计数。
#[tokio::test]
async fn deleting_a_comment_lowers_the_feed_count() {
    let (_st, app) = scene().await;
    let (_alice, at) = signup(&app, "alice").await;

    let (_, body) = publish(&app, &at, json!({"text": "计数"})).await;
    let moment_id = body["moment_id"].as_str().unwrap().to_string();

    let mut ids = Vec::new();
    for i in 0..3 {
        let (_, body) = call(
            &app,
            req(
                "POST",
                &format!("/api/moments/{moment_id}/comments"),
                Some(json!({"text": format!("c{i}")})),
                Some(&at),
            ),
        )
        .await;
        ids.push(body["comment_id"].as_str().unwrap().to_string());
    }

    let count = |feed_body: &serde_json::Value| -> i64 {
        feed_body["moments"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["moment_id"].as_str() == Some(moment_id.as_str()))
            .expect("moment in feed")["comment_count"]
            .as_i64()
            .unwrap()
    };
    assert_eq!(count(&feed(&app, &at, "").await), 3);

    let status = call(
        &app,
        req(
            "DELETE",
            &format!("/api/moments/{moment_id}/comments/{}", ids[0]),
            None,
            Some(&at),
        ),
    )
    .await
    .0;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(count(&feed(&app, &at, "").await), 2);
}
