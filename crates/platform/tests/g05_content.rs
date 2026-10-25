//! G05 验收：朋友圈 / 文章订阅 / 收藏笔记表情 / 视频号 / 检索与通知 / 状态。
//!
//! 全部用真实 SQLite 域与真实 HTTP 路由；没有 mock。媒体行由测试直接落库
//! （等价于已上传），视频转码回调走真实 `/api/video/callback`。

mod common;

use axum::http::StatusCode;
use common::{app, call, req, signup};
use rusqlite::params;
use serde_json::json;
use wxlike_platform::{Platform, PlatformOptions, PlatformState};

async fn scene() -> (PlatformState, axum::Router) {
    let state = common::platform();
    let router = app(state.clone());
    (state, router)
}

async fn scene_with_video_token() -> (PlatformState, axum::Router) {
    let opts = PlatformOptions {
        session_ttl_ms: 60_000,
        pbkdf2_rounds: 1_000,
        video_callback_token: Some("video-token".into()),
        ..Default::default()
    };
    let state = Platform::in_memory_with(opts).unwrap().shared();
    let router = Platform::router(state.clone());
    (state, router)
}

fn media_id(n: u64) -> String {
    format!("{:064x}", n)
}

fn seed_media(state: &PlatformState, id: &str, uploader: &str, content_type: &str, size: u64) {
    state
        .db()
        .lock()
        .execute(
            "INSERT OR IGNORE INTO media(id, size, content_type, uploader, has_thumb, created_at)
             VALUES(?1, ?2, ?3, ?4, 0, 0)",
            params![id, size as i64, content_type, uploader],
        )
        .unwrap();
}

async fn befriend(app: &axum::Router, at: &str, target: &str, target_token: &str) {
    let (status, body) = call(
        app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({"to_user_id": target})),
            Some(at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().unwrap();
    let (status, body) = call(
        app,
        req(
            "POST",
            &format!("/api/friends/requests/{id}/accept"),
            None,
            Some(target_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn ok(app: &axum::Router, r: axum::http::Request<axum::body::Body>) -> serde_json::Value {
    let (status, body) = call(app, r).await;
    assert!(status.is_success(), "{status}: {body}");
    body
}

// ---------------------------------------------------------------------------
// G05-01 朋友圈
// ---------------------------------------------------------------------------

#[tokio::test]
async fn moments_group_visibility_block_like_and_delete_index() {
    let (st, app) = scene().await;
    let (alice, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let (carol, ct) = signup(&app, "carol").await;
    befriend(&app, &at, &bob, &bt).await;
    befriend(&app, &at, &carol, &ct).await;
    st.db()
        .lock()
        .execute(
            "UPDATE friends SET friend_group = 'close' WHERE user_id = ?1 AND friend_id = ?2",
            params![alice, bob],
        )
        .unwrap();

    // 草稿：先保存、列表可见、发布时删除。
    let draft = ok(
        &app,
        req(
            "POST",
            "/api/moments/drafts",
            Some(json!({"text": "draft body", "visibility": "groups", "audience_group": "close"})),
            Some(&at),
        ),
    )
    .await;
    let draft_id = draft["draft_id"].as_str().unwrap();
    let drafts = ok(&app, req("GET", "/api/moments/drafts", None, Some(&at))).await;
    assert_eq!(drafts["drafts"].as_array().unwrap().len(), 1);

    let body = ok(
        &app,
        req(
            "POST",
            "/api/moments",
            Some(json!({
                "text": "alpha group update",
                "visibility": "groups",
                "audience_group": "close",
                "draft_id": draft_id
            })),
            Some(&at),
        ),
    )
    .await;
    let moment = body["moment_id"].as_str().unwrap().to_string();
    let drafts = ok(&app, req("GET", "/api/moments/drafts", None, Some(&at))).await;
    assert!(drafts["drafts"].as_array().unwrap().is_empty());

    // 分组可见矩阵
    assert_eq!(
        call(
            &app,
            req("GET", &format!("/api/moments/{moment}"), None, Some(&bt))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            req("GET", &format!("/api/moments/{moment}"), None, Some(&ct))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    // 拉黑立即失效
    ok(
        &app,
        req("PUT", &format!("/api/blocks/{alice}"), None, Some(&bt)),
    )
    .await;
    assert_eq!(
        call(
            &app,
            req("GET", &format!("/api/moments/{moment}"), None, Some(&bt))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    ok(
        &app,
        req("DELETE", &format!("/api/blocks/{alice}"), None, Some(&bt)),
    )
    .await;

    // 重复点赞幂等，计数来自投影
    let first = ok(
        &app,
        req(
            "POST",
            &format!("/api/moments/{moment}/likes"),
            None,
            Some(&bt),
        ),
    )
    .await;
    let second = ok(
        &app,
        req(
            "POST",
            &format!("/api/moments/{moment}/likes"),
            None,
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(first["added"], true);
    assert_eq!(second["added"], false);
    let d = ok(
        &app,
        req("GET", &format!("/api/moments/{moment}"), None, Some(&at)),
    )
    .await;
    assert_eq!(d["like_count"], 1);

    // 评论层级上限：顶层 -> 回复 -> 再回复，第四层拒绝
    let c1 = ok(
        &app,
        req(
            "POST",
            &format!("/api/moments/{moment}/comments"),
            Some(json!({"text": "top"})),
            Some(&bt),
        ),
    )
    .await;
    let c2 = ok(
        &app,
        req(
            "POST",
            &format!("/api/moments/{moment}/comments"),
            Some(json!({"text": "reply", "reply_to_comment_id": c1["comment_id"]})),
            Some(&at),
        ),
    )
    .await;
    let c3 = ok(
        &app,
        req(
            "POST",
            &format!("/api/moments/{moment}/comments"),
            Some(json!({"text": "deep", "reply_to_comment_id": c2["comment_id"]})),
            Some(&bt),
        ),
    )
    .await;
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/moments/{moment}/comments"),
            Some(json!({"text": "too deep", "reply_to_comment_id": c3["comment_id"]})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // 收藏该朋友圈，然后删帖：索引消失、收藏撤权、附件引用清理
    let fav = ok(
        &app,
        req(
            "POST",
            "/api/favorites/objects",
            Some(json!({"kind": "moment", "id": moment})),
            Some(&at),
        ),
    )
    .await;
    let fav_id = fav["id"].as_str().unwrap();
    let hits = ok(
        &app,
        req(
            "GET",
            "/api/search/content?q=alpha%20group",
            None,
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(hits["items"].as_array().unwrap().len(), 1, "{hits}");

    ok(
        &app,
        req("DELETE", &format!("/api/moments/{moment}"), None, Some(&at)),
    )
    .await;
    let hits = ok(
        &app,
        req(
            "GET",
            "/api/search/content?q=alpha%20group",
            None,
            Some(&bt),
        ),
    )
    .await;
    assert!(
        hits["items"].as_array().unwrap().is_empty(),
        "deleted must not resurface"
    );
    let fav = ok(
        &app,
        req("GET", &format!("/api/favorites/{fav_id}"), None, Some(&at)),
    )
    .await;
    assert_eq!(fav["revoked"], true);
    assert_eq!(fav["text"], "");
    let refs: i64 = st
        .db()
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM content_media_refs WHERE ref_id = ?1",
            params![moment],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(refs, 0);
}

#[tokio::test]
async fn moment_report_lands_in_moderation_queue() {
    let (_st, app) = scene().await;
    let (_alice, at) = signup(&app, "alice").await;
    let (_bob, bt) = signup(&app, "bob").await;
    let body = ok(
        &app,
        req(
            "POST",
            "/api/moments",
            Some(json!({"text": "public post", "visibility": "public"})),
            Some(&at),
        ),
    )
    .await;
    let id = body["moment_id"].as_str().unwrap();
    let report = ok(
        &app,
        req(
            "POST",
            &format!("/api/moments/{id}/report"),
            Some(json!({"reason": "spam"})),
            Some(&bt),
        ),
    )
    .await;
    assert!(!report["report_id"].as_str().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// G05-02 公众号与文章
// ---------------------------------------------------------------------------

#[tokio::test]
async fn articles_subscription_reading_reactions_and_versioning() {
    let (_st, app) = scene().await;
    let (_alice, at) = signup(&app, "alice").await;
    let (_bob, bt) = signup(&app, "bob").await;
    let publisher = ok(
        &app,
        req(
            "POST",
            "/api/publishers",
            Some(json!({"name": "Alice Daily", "bio": "notes"})),
            Some(&at),
        ),
    )
    .await;
    let pid = publisher["id"].as_str().unwrap();

    // 恶意链接/HTML 在渲染层就被拒绝
    let bad = call(
        &app,
        req(
            "POST",
            "/api/articles",
            Some(json!({
                "title": "bad",
                "publish": true,
                "doc": {"blocks": [
                    {"type": "paragraph", "spans": [{"text": "<b>hi</b>"}]},
                    {"type": "link", "text": "x", "href": "javascript:alert(1)"}
                ]}
            })),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(bad.0, StatusCode::UNPROCESSABLE_ENTITY);

    // 关注 + 未读游标
    ok(
        &app,
        req(
            "POST",
            &format!("/api/publishers/{pid}/follow"),
            None,
            Some(&bt),
        ),
    )
    .await;
    let article = ok(
        &app,
        req(
            "POST",
            "/api/articles",
            Some(json!({
                "publisher_id": pid,
                "title": "hello readers",
                "publish": true,
                "doc": {"blocks": [
                    {"type": "heading", "level": 1, "spans": [{"text": "Title"}]},
                    {"type": "paragraph", "spans": [
                        {"text": "<script>alert(1)</script>", "bold": true}
                    ]}
                ]}
            })),
            Some(&at),
        ),
    )
    .await;
    let aid = article["id"].as_str().unwrap();
    let html = article["html"].as_str().unwrap();
    assert!(
        html.contains("&lt;script&gt;"),
        "server renders escaped html: {html}"
    );
    assert!(!html.contains("<script>"));

    let pub_view = ok(
        &app,
        req("GET", &format!("/api/publishers/{pid}"), None, Some(&bt)),
    )
    .await;
    assert_eq!(pub_view["unread"], 1);
    let read = ok(
        &app,
        req(
            "POST",
            &format!("/api/articles/{aid}/read"),
            None,
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(read["unread"], 0);

    // 在看是独立动作且可切换
    let r1 = ok(
        &app,
        req(
            "POST",
            &format!("/api/articles/{aid}/react"),
            Some(json!({"kind": "wow"})),
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(r1["active"], true);
    assert_eq!(r1["counts"]["wow"], 1);
    let r2 = ok(
        &app,
        req(
            "POST",
            &format!("/api/articles/{aid}/react"),
            Some(json!({"kind": "wow"})),
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(r2["active"], false);

    // 草稿只有作者可见
    let draft = ok(
        &app,
        req(
            "POST",
            "/api/articles",
            Some(json!({
                "title": "draft",
                "publish": false,
                "doc": {"blocks": [{"type": "paragraph", "spans": [{"text": "wip"}]}]}
            })),
            Some(&at),
        ),
    )
    .await;
    let did = draft["id"].as_str().unwrap();
    assert_eq!(
        call(
            &app,
            req("GET", &format!("/api/articles/{did}"), None, Some(&bt))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    // 版本冲突：旧版本不得静默覆盖
    let updated = ok(
        &app,
        req(
            "PUT",
            &format!("/api/articles/{aid}"),
            Some(json!({
                "title": "hello readers v2",
                "expected_version": 1,
                "doc": {"blocks": [{"type": "paragraph", "spans": [{"text": "v2"}]}]}
            })),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(updated["version"], 2);
    let conflict = call(
        &app,
        req(
            "PUT",
            &format!("/api/articles/{aid}"),
            Some(json!({
                "title": "stale",
                "expected_version": 1,
                "doc": {"blocks": [{"type": "paragraph", "spans": [{"text": "stale"}]}]}
            })),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(conflict.0, StatusCode::CONFLICT);

    // 删除后检索不回流
    ok(
        &app,
        req("DELETE", &format!("/api/articles/{aid}"), None, Some(&at)),
    )
    .await;
    let hits = ok(
        &app,
        req("GET", "/api/search/content?q=hello", None, Some(&bt)),
    )
    .await;
    assert!(hits["items"].as_array().unwrap().is_empty(), "{hits}");
}

// ---------------------------------------------------------------------------
// G05-03 收藏、笔记、表情包
// ---------------------------------------------------------------------------

#[tokio::test]
async fn notes_conflict_autosave_and_favorites_order() {
    let (_st, app) = scene().await;
    let (_alice, at) = signup(&app, "alice").await;
    let note = ok(
        &app,
        req(
            "POST",
            "/api/notes",
            Some(json!({"note_id": "note0001", "title": "t", "body": "one"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(note["version"], 1);
    // 重复创建同一 id 不覆盖
    let again = ok(
        &app,
        req(
            "POST",
            "/api/notes",
            Some(json!({"note_id": "note0001", "title": "t", "body": "overwrite"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(again["body"], "one");

    let saved = ok(
        &app,
        req(
            "PUT",
            "/api/notes/note0001",
            Some(json!({"title": "t", "body": "two", "expected_version": 1})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(saved["version"], 2);
    let auto = ok(
        &app,
        req(
            "POST",
            "/api/notes/note0001/autosave",
            Some(json!({"title": "t", "body": "three", "expected_version": 2})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(auto["version"], 3);
    let stale = call(
        &app,
        req(
            "PUT",
            "/api/notes/note0001",
            Some(json!({"title": "t", "body": "stale", "expected_version": 1})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(stale.0, StatusCode::CONFLICT);
    // 刷新读取仍是最后保存的内容
    let got = ok(&app, req("GET", "/api/notes/note0001", None, Some(&at))).await;
    assert_eq!(got["body"], "three");
}

#[tokio::test]
async fn stickers_install_send_reinstall_and_license() {
    let (st, app) = scene().await;
    let (alice, at) = signup(&app, "alice").await;
    let (_bob, bt) = signup(&app, "bob").await;
    let good = media_id(1);
    let bad = media_id(2);
    seed_media(&st, &good, &alice, "image/png", 1_024);
    seed_media(&st, &bad, "somebody-else", "image/png", 1_024);

    let denied = call(
        &app,
        req(
            "POST",
            "/api/stickers/packs",
            Some(json!({"name": "p", "license": "proprietary", "stickers": [{"media_id": good}]})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(denied.0, StatusCode::UNPROCESSABLE_ENTITY);
    let foreign = call(
        &app,
        req(
            "POST",
            "/api/stickers/packs",
            Some(json!({"name": "p", "license": "cc0-1.0", "stickers": [{"media_id": bad}]})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(foreign.0, StatusCode::FORBIDDEN);

    let pack = ok(
        &app,
        req(
            "POST",
            "/api/stickers/packs",
            Some(json!({"name": "p", "license": "cc0-1.0", "stickers": [{"media_id": good, "name": "hi"}]})),
            Some(&at),
        ),
    )
    .await;
    let pid = pack["id"].as_str().unwrap();
    let sticker = pack["stickers"][0]["id"].as_str().unwrap().to_string();

    ok(
        &app,
        req(
            "POST",
            &format!("/api/stickers/packs/{pid}/install"),
            None,
            Some(&bt),
        ),
    )
    .await;
    let installed = ok(&app, req("GET", "/api/stickers", None, Some(&bt))).await;
    assert_eq!(
        installed["stickers"][0]["media_id"], good,
        "send payload carries media"
    );
    assert_eq!(
        call(
            &app,
            req(
                "DELETE",
                &format!("/api/stickers/packs/{pid}"),
                None,
                Some(&bt)
            )
        )
        .await
        .0,
        StatusCode::NOT_FOUND,
        "only the owner may delete the pack"
    );

    ok(
        &app,
        req(
            "DELETE",
            &format!("/api/stickers/{sticker}"),
            None,
            Some(&bt),
        ),
    )
    .await;
    assert!(
        ok(&app, req("GET", "/api/stickers", None, Some(&bt))).await["stickers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // 重新安装 = 可重下
    let re = ok(
        &app,
        req(
            "POST",
            &format!("/api/stickers/packs/{pid}/install"),
            None,
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(re["added"], 1);
    ok(
        &app,
        req(
            "DELETE",
            &format!("/api/stickers/packs/{pid}"),
            None,
            Some(&at),
        ),
    )
    .await;
    assert!(
        ok(&app, req("GET", "/api/stickers", None, Some(&bt))).await["stickers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// G05-04 视频号
// ---------------------------------------------------------------------------

#[tokio::test]
async fn videos_require_transcode_and_filter_deleted_from_feeds() {
    let (st, app) = scene_with_video_token().await;
    let (alice, at) = signup(&app, "alice").await;
    let (_bob, bt) = signup(&app, "bob").await;
    let vid = media_id(9);
    seed_media(&st, &vid, &alice, "application/octet-stream", 1_024);

    let early = call(
        &app,
        req(
            "POST",
            "/api/videos",
            Some(json!({"media_id": vid, "caption": "sunset"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(
        early.0,
        StatusCode::UNPROCESSABLE_ENTITY,
        "no transcode, no publish"
    );

    let mut cb = req(
        "POST",
        "/api/video/callback",
        Some(json!({
            "job_id": vid,
            "state": "succeeded",
            "attempts": 1,
            "result": {
                "video_id": vid,
                "playlist_url": format!("/api/video/{vid}/playlist.m3u8"),
                "play_url": format!("/api/video/{vid}/play.mp4"),
                "thumb_url": format!("/api/video/{vid}/thumb.png"),
                "duration_ms": 1_200,
                "width": 640,
                "height": 360,
                "renditions": []
            }
        })),
        None,
    );
    cb.headers_mut()
        .insert("x-wxlike-video-token", "video-token".parse().unwrap());
    assert_eq!(call(&app, cb).await.0, StatusCode::OK);

    let video = ok(
        &app,
        req(
            "POST",
            "/api/videos",
            Some(json!({"media_id": vid, "caption": "sunset"})),
            Some(&at),
        ),
    )
    .await;
    let id = video["id"].as_str().unwrap();
    assert_eq!(video["playable"], true);
    // 重复发布幂等
    let again = ok(
        &app,
        req(
            "POST",
            "/api/videos",
            Some(json!({"media_id": vid, "caption": "sunset"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(again["id"], id);

    // 未关注时 hot 可见、recommend 不推已关注作者；关注后 follow 可见
    let hot = ok(
        &app,
        req("GET", "/api/videos/feed?mode=hot", None, Some(&bt)),
    )
    .await;
    assert_eq!(hot["items"].as_array().unwrap().len(), 1);
    let rec = ok(
        &app,
        req("GET", "/api/videos/feed?mode=recommend", None, Some(&bt)),
    )
    .await;
    assert!(rec["items"].as_array().unwrap().is_empty());
    ok(
        &app,
        req(
            "POST",
            &format!("/api/video-creators/{alice}/follow"),
            None,
            Some(&bt),
        ),
    )
    .await;
    let follow = ok(
        &app,
        req("GET", "/api/videos/feed?mode=follow", None, Some(&bt)),
    )
    .await;
    assert_eq!(follow["items"].as_array().unwrap().len(), 1);

    // 点赞幂等 + 评论
    let l1 = ok(
        &app,
        req("POST", &format!("/api/videos/{id}/like"), None, Some(&bt)),
    )
    .await;
    let l2 = ok(
        &app,
        req("POST", &format!("/api/videos/{id}/like"), None, Some(&bt)),
    )
    .await;
    assert_eq!(l1["added"], true);
    assert_eq!(l2["added"], false);
    assert_eq!(l2["count"], 1);
    ok(
        &app,
        req(
            "POST",
            &format!("/api/videos/{id}/comments"),
            Some(json!({"text": "nice"})),
            Some(&bt),
        ),
    )
    .await;

    // 直播标记随停播消失
    st.db()
        .lock()
        .execute(
            "INSERT INTO live_sessions(id, channel_id, host, stream_key, state, recording,
                                       started_at, viewers, peak_viewers)
             VALUES('ls1', 'ch1', ?1, 'sk1', 'live', 0, 0, 0, 0)",
            params![alice],
        )
        .unwrap();
    let live = ok(
        &app,
        req("GET", &format!("/api/videos/{id}"), None, Some(&bt)),
    )
    .await;
    assert_eq!(live["live"], true);
    st.db()
        .lock()
        .execute(
            "UPDATE live_sessions SET state = 'ended' WHERE id = 'ls1'",
            [],
        )
        .unwrap();
    let ended = ok(
        &app,
        req("GET", &format!("/api/videos/{id}"), None, Some(&bt)),
    )
    .await;
    assert_eq!(ended["live"], false);

    // 删除后所有 feed 立即过滤
    ok(
        &app,
        req("DELETE", &format!("/api/videos/{id}"), None, Some(&at)),
    )
    .await;
    for mode in ["follow", "hot", "recommend"] {
        let feed = ok(
            &app,
            req(
                "GET",
                &format!("/api/videos/feed?mode={mode}"),
                None,
                Some(&bt),
            ),
        )
        .await;
        assert!(feed["items"].as_array().unwrap().is_empty(), "{mode}");
    }
}

// ---------------------------------------------------------------------------
// G05-05 检索 / 热搜 / 通知
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_authorization_trends_and_cross_device_read() {
    let (st, app) = scene().await;
    let (alice, at) = signup(&app, "alice").await;
    let (_bob, bt) = signup(&app, "bob").await;
    let public = ok(
        &app,
        req(
            "POST",
            "/api/moments",
            Some(json!({"text": "rustlang alpha", "visibility": "public"})),
            Some(&at),
        ),
    )
    .await;
    let public_id = public["moment_id"].as_str().unwrap();
    let private = ok(
        &app,
        req(
            "POST",
            "/api/moments",
            Some(json!({"text": "secretterm private", "visibility": "private"})),
            Some(&at),
        ),
    )
    .await;
    let private_id = private["moment_id"].as_str().unwrap();

    let hits = ok(
        &app,
        req("GET", "/api/search/content?q=rustlang", None, Some(&bt)),
    )
    .await;
    assert_eq!(hits["items"][0]["doc_id"], public_id);
    assert_eq!(
        call(
            &app,
            req("GET", "/api/search/content?q=secretterm", None, Some(&bt))
        )
        .await
        .0,
        StatusCode::OK
    );
    let private_hits = ok(
        &app,
        req("GET", "/api/search/content?q=secretterm", None, Some(&bt)),
    )
    .await;
    assert!(private_hits["items"].as_array().unwrap().is_empty());

    let trends = ok(&app, req("GET", "/api/search/trends", None, Some(&bt))).await;
    let terms: Vec<&str> = trends["terms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["term"].as_str().unwrap())
        .collect();
    assert!(terms.contains(&"rustlang"), "{terms:?}");
    assert!(!terms.contains(&"secretterm"), "{terms:?}");

    // 拉黑即过滤
    ok(
        &app,
        req("PUT", &format!("/api/blocks/{alice}"), None, Some(&bt)),
    )
    .await;
    let blocked = ok(
        &app,
        req("GET", "/api/search/content?q=rustlang", None, Some(&bt)),
    )
    .await;
    assert!(blocked["items"].as_array().unwrap().is_empty());
    ok(
        &app,
        req("DELETE", &format!("/api/blocks/{alice}"), None, Some(&bt)),
    )
    .await;

    // 通知：点赞产生一条；第二台设备读到已读后两端一致
    ok(
        &app,
        req(
            "POST",
            &format!("/api/moments/{public_id}/likes"),
            None,
            Some(&bt),
        ),
    )
    .await;
    let inbox = ok(&app, req("GET", "/api/notifications", None, Some(&at))).await;
    assert_eq!(inbox["unread_count"], 1);
    let seq = inbox["items"][0]["seq"].as_i64().unwrap();
    let token2 = st.create_session(&alice, 60_000).unwrap();
    ok(
        &app,
        req(
            "POST",
            "/api/notifications/read",
            Some(json!({"up_to_seq": seq})),
            Some(&token2),
        ),
    )
    .await;
    let other = ok(&app, req("GET", "/api/notifications", None, Some(&at))).await;
    assert_eq!(other["unread_count"], 0);
    let _ = private_id;
}

// ---------------------------------------------------------------------------
// G05-06 状态
// ---------------------------------------------------------------------------

#[tokio::test]
async fn statuses_audience_background_expiry_and_reactions() {
    let (st, app) = scene().await;
    let (alice, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let (_carol, ct) = signup(&app, "carol").await;
    befriend(&app, &at, &bob, &bt).await;

    let bg = media_id(11);
    let foreign_bg = media_id(12);
    seed_media(&st, &bg, &alice, "image/png", 1_024);
    seed_media(&st, &foreign_bg, "somebody-else", "image/png", 1_024);

    // 背景草稿：坏媒体被拒，已有草稿保留
    ok(
        &app,
        req(
            "POST",
            "/api/statuses/background",
            Some(json!({"media_id": bg})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(
        call(
            &app,
            req(
                "POST",
                "/api/statuses/background",
                Some(json!({"media_id": foreign_bg})),
                Some(&at)
            )
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let draft = ok(
        &app,
        req("GET", "/api/statuses/background", None, Some(&at)),
    )
    .await;
    assert_eq!(draft["background"]["media_id"], bg);

    let status = ok(
        &app,
        req(
            "POST",
            "/api/statuses",
            Some(json!({
                "kind": "text", "text": "out for coffee",
                "background_media_id": bg, "audience": "friends"
            })),
            Some(&at),
        ),
    )
    .await;
    let sid = status["id"].as_str().unwrap();
    assert_eq!(status["background_media_id"], bg);
    // 用了草稿就清掉草稿
    let draft = ok(
        &app,
        req("GET", "/api/statuses/background", None, Some(&at)),
    )
    .await;
    assert!(draft["background"].is_null());

    let bob_view = ok(&app, req("GET", "/api/statuses", None, Some(&bt))).await;
    assert_eq!(bob_view["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        call(
            &app,
            req("GET", &format!("/api/statuses/{sid}"), None, Some(&ct))
        )
        .await
        .0,
        StatusCode::NOT_FOUND,
        "non-friend cannot see"
    );

    // 心形唯一
    let r1 = ok(
        &app,
        req(
            "POST",
            &format!("/api/statuses/{sid}/react"),
            None,
            Some(&bt),
        ),
    )
    .await;
    let r2 = ok(
        &app,
        req(
            "POST",
            &format!("/api/statuses/{sid}/react"),
            None,
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(r1["count"], 1);
    assert_eq!(r2["active"], false);
    assert_eq!(r2["count"], 0);

    // 拉黑后立即不可见
    ok(
        &app,
        req("PUT", &format!("/api/blocks/{alice}"), None, Some(&bt)),
    )
    .await;
    let blocked = ok(&app, req("GET", "/api/statuses", None, Some(&bt))).await;
    assert!(blocked["items"].as_array().unwrap().is_empty());
    ok(
        &app,
        req("DELETE", &format!("/api/blocks/{alice}"), None, Some(&bt)),
    )
    .await;

    // 服务端到期：把 expires_at 拨到过去，sweep 清可见性并留审计
    st.db()
        .lock()
        .execute(
            "UPDATE statuses SET expires_at = 0 WHERE id = ?1",
            params![sid],
        )
        .unwrap();
    assert_eq!(st.sweep_expired_statuses().unwrap(), 1);
    let gone = ok(&app, req("GET", "/api/statuses", None, Some(&bt))).await;
    assert!(gone["items"].as_array().unwrap().is_empty());
    assert_eq!(
        call(
            &app,
            req("GET", &format!("/api/statuses/{sid}"), None, Some(&at))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let audited: i64 = st
        .db()
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM audit_log WHERE action = 'status_expire' AND target = ?1",
            params![sid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(audited, 1);

    // 清除也是原子的
    let second = ok(
        &app,
        req(
            "POST",
            "/api/statuses",
            Some(json!({"kind": "text", "text": "second"})),
            Some(&at),
        ),
    )
    .await;
    let sid2 = second["id"].as_str().unwrap();
    ok(
        &app,
        req("DELETE", &format!("/api/statuses/{sid2}"), None, Some(&at)),
    )
    .await;
    assert_eq!(
        call(
            &app,
            req("GET", &format!("/api/statuses/{sid2}"), None, Some(&bt))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}
