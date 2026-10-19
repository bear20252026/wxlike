//! 通话控制面的 **HTTP 层**契约（真实 axum router，`tower::oneshot`）。
//!
//! `tests/call.rs` 验的是 `Platform` 方法；这里验的是线上真正发生的事：
//! 路径拼对了没有、状态码是 204 还是 200、错误映射到 403/404/409 对不对。
//! 前端（Tauri 命令）对状态码和空 body 的假设就建立在这些断言上。

mod common;

use axum::Router;
use common::{app, call, platform, req, signup};
use serde_json::{json, Value};
use wxlike_platform::Platform;

/// 建一个 alice 拥有、bob/carol 在群、dave 群外的三人群，返回
/// `(router, alice_id, alice_token, dave_token, "conv|dave_id")`。
async fn group_room() -> (Router, String, String, String, String, String) {
    let state = platform();
    let (alice, ta) = signup(&Platform::router(state.clone()), "alice").await;
    let (bob, _tb) = signup(&Platform::router(state.clone()), "bob").await;
    let (carol, tc) = signup(&Platform::router(state.clone()), "carol").await;
    let (dave, td) = signup(&Platform::router(state.clone()), "dave").await;
    let conv = state
        .create_group(&alice, "通话群", &[bob, carol])
        .expect("create group");
    (app(state), alice, ta, td, tc, format!("{conv}|{dave}"))
}

fn split(pair: &str) -> (&str, &str) {
    let (a, b) = pair.split_once('|').unwrap();
    (a, b)
}

#[tokio::test]
async fn lifecycle_over_http() {
    let (app, _alice, ta, _td, _tc, pair) = group_room().await;
    let (conv, _dave) = split(&pair);
    let (_bob, tb) = signup(&app, "bob2").await;
    // bob2 不在群里：先确认 403
    let (status, _) = call(
        &app,
        req("POST", &format!("/api/call/{conv}/join"), None, Some(&tb)),
    )
    .await;
    assert_eq!(status, 403, "群外用户进不来");

    // 开房 204
    let (status, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/call/{conv}/start"),
            Some(json!({"capacity": 8})),
            Some(&ta),
        ),
    )
    .await;
    assert_eq!(status, 204, "{body}");
    assert_eq!(body, Value::Null, "开房不返回 body（前端按 204 处理）");

    // 重复开房 409
    let (status, _) = call(
        &app,
        req("POST", &format!("/api/call/{conv}/start"), None, Some(&ta)),
    )
    .await;
    assert_eq!(status, 409);

    // 状态 200
    let (status, st) = call(
        &app,
        req("GET", &format!("/api/call/{conv}"), None, Some(&ta)),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(st["live"], json!(true));
    assert_eq!(st["participants"].as_array().unwrap().len(), 1);

    // 静音 204，状态里能看到
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/call/{conv}/mute"),
            Some(json!({"target": _alice, "track": "audio", "muted": true})),
            Some(&ta),
        ),
    )
    .await;
    assert_eq!(status, 204);
    let (_, st) = call(
        &app,
        req("GET", &format!("/api/call/{conv}"), None, Some(&ta)),
    )
    .await;
    assert_eq!(st["participants"][0]["audio_muted"], json!(true));

    // 全员静音返回 200 + affected
    let (status, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/call/{conv}/mute-all"),
            Some(json!({"track": "audio", "muted": false})),
            Some(&ta),
        ),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["affected"], json!(1));

    // 出口分配 200
    let (status, plan) = call(
        &app,
        req(
            "GET",
            &format!("/api/call/{conv}/egress?estimate_bps=800000"),
            None,
            Some(&ta),
        ),
    )
    .await;
    assert_eq!(status, 200, "{plan}");
    assert_eq!(plan["tracks"], json!([]));

    // 挂断后房间消失
    let (status, _) = call(
        &app,
        req("POST", &format!("/api/call/{conv}/leave"), None, Some(&ta)),
    )
    .await;
    assert_eq!(status, 204);
    let (_, st) = call(
        &app,
        req("GET", &format!("/api/call/{conv}"), None, Some(&ta)),
    )
    .await;
    assert_eq!(st["live"], json!(false));
}

#[tokio::test]
async fn outsider_and_missing_room_errors() {
    let (app, _alice, ta, td, tc, pair) = group_room().await;
    let (conv, _dave) = split(&pair);

    // 群内成员在没开房时 join → 404（成员资格先过，房间不存在才 404）
    let (status, _) = call(
        &app,
        req("POST", &format!("/api/call/{conv}/join"), None, Some(&tc)),
    )
    .await;
    assert_eq!(status, 404);

    // 群外用户开房 → 403
    let (status, _) = call(
        &app,
        req("POST", &format!("/api/call/{conv}/start"), None, Some(&td)),
    )
    .await;
    assert_eq!(status, 403);

    // 未登录 → 401
    let (status, _) = call(&app, req("GET", &format!("/api/call/{conv}"), None, None)).await;
    assert_eq!(status, 401);

    // 房主开房后，群外用户仍然进不来 → 403
    call(
        &app,
        req("POST", &format!("/api/call/{conv}/start"), None, Some(&ta)),
    )
    .await;
    let (_bob2, tb2) = signup(&app, "bob4").await;
    let (status, _) = call(
        &app,
        req("POST", &format!("/api/call/{conv}/join"), None, Some(&tb2)),
    )
    .await;
    assert_eq!(status, 403, "不在群里仍然进不来");

    // 非法 track → 422
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/call/{conv}/mute"),
            Some(json!({"target": _alice, "track": "hologram", "muted": true})),
            Some(&ta),
        ),
    )
    .await;
    assert_eq!(status, 422);
}

/// 直聊会话也能开房：成员资格按 `d-a~b` 判定，不查群表。
#[tokio::test]
async fn direct_conversation_call() {
    let state = platform();
    let app = app(state);
    let (alice, ta) = signup(&app, "a").await;
    let (bob, tb) = signup(&app, "b").await;
    let (_, carol, _) = {
        let (c, tc) = signup(&app, "c").await;
        ((), c, tc)
    };
    let conv = format!("d-{}~{}", alice, bob);

    let (status, _) = call(
        &app,
        req("POST", &format!("/api/call/{conv}/start"), None, Some(&ta)),
    )
    .await;
    assert_eq!(status, 204, "直聊可以开房");
    let (status, _) = call(
        &app,
        req("POST", &format!("/api/call/{conv}/join"), None, Some(&tb)),
    )
    .await;
    assert_eq!(status, 204, "对端可以进");

    let (_c, tc) = signup(&app, "c2").await;
    let (status, _) = call(
        &app,
        req("POST", &format!("/api/call/{conv}/join"), None, Some(&tc)),
    )
    .await;
    assert_eq!(status, 403, "第三方进不了直聊");
    let _ = carol;
    let _ = Platform::router;
}
