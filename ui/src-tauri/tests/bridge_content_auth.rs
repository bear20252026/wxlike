//! D6（内容域逐域退役）：moments/favorites/report/stickers 控制面经桥走认证 HTTP。
//!
//! 与钱包/通话/视频域同口径：会话留在 Rust 侧，控制面由 core_dispatch 代持
//! （moment.*/favorite.*/report.*/sticker.*，见 core_bridge/content.inc）。
//! G05 未对齐面（articles/publishers）在消费侧显式阻塞，不在本桥。
//!
//! 全程真服务端栈（support::stack()）。逐条钉住：
//! 1. 没登录 → UNAUTHENTICATED，且一个字节都不发（死端口判别）；
//! 2. 畸形 id 在发请求之前拦掉（VALIDATION）；
//! 3. moments 发布→feed→点赞→评论→删评论→删除 往返；favorites 增→读→改→删；
//! 4. Bearer 只出 Rust：任何 action 返回值里都找不到它。

use serde_json::{json, Value};
use tauri::Manager as _;
use wxlike_ui_lib::core_bridge::{dispatch, CoreRequest, CoreState, IMPLEMENTED_ACTIONS};
use wxlike_ui_lib::AppState;

mod support;

use crate::support::stack;

type MockApp = tauri::App<tauri::test::MockRuntime>;
type MockHandle = tauri::AppHandle<tauri::test::MockRuntime>;

fn request(action: &str, params: Value, idem: Option<&str>) -> CoreRequest {
    CoreRequest {
        action_id: action.to_string(),
        request_id: format!("d6-content-{action}"),
        idempotency_key: idem.map(str::to_string),
        params,
    }
}

fn boot() -> (MockApp, MockHandle) {
    let app = tauri::test::mock_app();
    app.manage(CoreState::new());
    let handle = app.handle().clone();
    (app, handle)
}

fn call_with(app: &MockHandle, action: &str, params: Value, idem: Option<&str>) -> Value {
    let handle = app.clone();
    let req = request(action, params, idem);
    std::thread::spawn(move || {
        let state = handle.state::<AppState>();
        let core = handle.state::<CoreState>();
        serde_json::to_value(dispatch(&handle, &state, &core, req)).unwrap()
    })
    .join()
    .unwrap_or_else(|e| std::panic::resume_unwind(e))
}

fn call(app: &MockHandle, action: &str, params: Value) -> Value {
    call_with(app, action, params, None)
}

fn ok_data<'a>(reply: &'a Value, what: &str) -> &'a Value {
    assert_eq!(reply["ok"], json!(true), "{what} 必须成功，实际：{reply}");
    reply.get("data").unwrap_or_else(|| panic!("{what} 的成功信封必须带 data：{reply}"))
}

fn error<'a>(reply: &'a Value, what: &str, want: &str) -> &'a Value {
    assert_eq!(reply["ok"], json!(false), "{what} 必须失败，不许伪造成功：{reply}");
    assert!(reply.get("data").is_none(), "{what} 失败时不许带 data：{reply}");
    let err = &reply["error"];
    assert_eq!(err["code"], json!(want), "{what} 的错误码不对：{reply}");
    err
}

fn register(state: &AppState, name: &str) -> String {
    let reply = state.register(name, "pw").unwrap_or_else(|e| panic!("{name} 注册失败：{e}"));
    reply["user_id"].as_str().unwrap_or_else(|| panic!("注册响应没有 user_id：{reply}")).to_string()
}

fn signed_in(http: &str, name: &str) -> (MockApp, MockHandle, String) {
    let (app, handle) = boot();
    let state = AppState::new(http);
    let user_id = register(&state, name);
    app.manage(state);
    (app, handle, user_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unauthenticated_content_actions_never_touch_the_network() {
    let (app, handle) = boot();
    app.manage(AppState::new("http://127.0.0.1:1"));

    let cases: Vec<(&str, Value)> = vec![
        ("moment.publish", json!({"text": "hi", "media_ids": [], "visibility": "public"})),
        ("moment.feed", json!({})),
        ("moment.detail", json!({"moment_id": "m1"})),
        ("moment.remove", json!({"moment_id": "m1"})),
        ("moment.like", json!({"moment_id": "m1"})),
        ("moment.unlike", json!({"moment_id": "m1"})),
        ("moment.comment", json!({"moment_id": "m1", "text": "hi"})),
        ("moment.comment.remove", json!({"moment_id": "m1", "comment_id": "c1"})),
        ("report.create", json!({"target_id": "m1", "reason": "spam"})),
        ("favorite.list", json!({})),
        ("favorite.one", json!({"id": "f1"})),
        ("favorite.remove", json!({"id": "f1"})),
        ("favorite.add", json!({"body": {"source_conv": "c", "source_msg_id": "m", "kind": "text", "text": "t", "media_id": "", "sender": "s", "sent_at_ms": 1}})),
        ("favorite.update", json!({"id": "f1", "snapshot_label": "x"})),
        ("sticker.packs", json!({})),
        ("moment.media_url", json!({"media_id": "m9"})),
    ];
    for (action, params) in cases {
        let reply = call(&handle, action, params);
        let err = error(&reply, action, "UNAUTHENTICATED");
        assert!(
            !err["message"].as_str().unwrap_or_default().contains("传输失败"),
            "{action} 必须没发出任何请求：{reply}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn malformed_ids_are_refused_before_the_socket() {
    let (http, _quic, _platform) = stack().await;
    let (_app, handle, _id) = signed_in(&http, "c_mal");

    let cases: Vec<(&str, Value)> = vec![
        ("moment.detail", json!({})),
        ("moment.detail", json!({"moment_id": ""})),
        ("moment.remove", json!({"moment_id": "../admin"})),
        ("moment.comment", json!({"moment_id": "m1"})),
        ("favorite.one", json!({"id": "a b"})),
        ("favorite.update", json!({"id": "f1"})),
        ("moment.media_url", json!({"media_id": "x?y=1"})),
    ];
    for (action, params) in cases {
        let reply = call(&handle, action, params);
        error(&reply, &format!("{action} 的畸形参数"), "VALIDATION");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn moment_lifecycle_round_trip() {
    let (http, _quic, _platform) = stack().await;
    let (app, handle, _id) = signed_in(&http, "c_author");

    // 发布 → {moment_id}。
    let published = call_with(&handle, "moment.publish", json!({"text": "第一条", "media_ids": [], "visibility": "public"}), Some("pub-1"));
    let moment_id = ok_data(&published, "发布")["moment_id"].as_str().unwrap().to_string();

    // feed：含刚发布的动态。
    let feed = ok_data(&call(&handle, "moment.feed", json!({"limit": 20})), "feed").clone();
    let items = feed["moments"].as_array().cloned().unwrap_or_default();
    assert!(items.iter().any(|m| m["moment_id"] == json!(moment_id)), "{feed}");

    // 点赞 → {liked:true}。
    let liked = ok_data(&call(&handle, "moment.like", json!({"moment_id": moment_id})), "点赞").clone();
    assert_eq!(liked["liked"], json!(true), "{liked}");

    // 评论 → {comment_id}。
    let commented = call_with(&handle, "moment.comment", json!({"moment_id": moment_id, "text": "好文"}), Some("cmt-1"));
    let comment_id = ok_data(&commented, "评论")["comment_id"].as_str().unwrap().to_string();

    // detail：含评论。
    let detail = ok_data(&call(&handle, "moment.detail", json!({"moment_id": moment_id})), "详情").clone();
    let comments = detail["comments"].as_array().cloned().unwrap_or_default();
    assert!(comments.iter().any(|c| c["comment_id"] == json!(comment_id)), "{detail}");

    // 删评论 → 204 → {}。
    ok_data(&call(&handle, "moment.comment.remove", json!({"moment_id": moment_id, "comment_id": comment_id})), "删评论");
    // 删除动态 → 204 → {}。
    ok_data(&call(&handle, "moment.remove", json!({"moment_id": moment_id})), "删除");

    let _ = app;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn favorite_lifecycle_round_trip() {
    let (http, _quic, _platform) = stack().await;
    let (app, handle, _id) = signed_in(&http, "c_fav");

    // 增 → Favorite。
    let added = call_with(&handle, "favorite.add", json!({"body": {"source_conv": "c1", "source_msg_id": "msg1", "kind": "text", "text": "纪要", "media_id": "", "sender": "小明", "sent_at_ms": 1}}), Some("fav-1"));
    let fav_id = ok_data(&added, "收藏")["id"].as_str().unwrap().to_string();

    // 列表：含刚收藏的。
    let list = ok_data(&call(&handle, "favorite.list", json!({})), "收藏列表").clone();
    let favs = list["favorites"].as_array().cloned().unwrap_or_default();
    assert!(favs.iter().any(|f| f["id"] == json!(fav_id)), "{list}");

    // 读 → Favorite。
    ok_data(&call(&handle, "favorite.one", json!({"id": fav_id})), "读收藏");
    // 改备注 → Favorite。
    let updated = ok_data(&call(&handle, "favorite.update", json!({"id": fav_id, "snapshot_label": "修订"})), "改备注").clone();
    assert_eq!(updated["snapshot_label"], json!("修订"), "{updated}");
    // 删 → 204 → {}。
    ok_data(&call(&handle, "favorite.remove", json!({"id": fav_id})), "删收藏");

    let _ = app;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bearer_token_never_crosses_the_bridge() {
    let (http, _quic, _platform) = stack().await;
    let (app, handle, _id) = signed_in(&http, "c_tok");
    let token = handle.state::<AppState>().auth_token().expect("登录后要有凭据");
    assert!(!token.is_empty());

    let published = call(&handle, "moment.publish", json!({"text": "t", "media_ids": [], "visibility": "public"}));
    let moment_id = ok_data(&published, "发布")["moment_id"].as_str().unwrap().to_string();
    let seen = vec![
        call(&handle, "moment.feed", json!({})),
        call(&handle, "moment.detail", json!({"moment_id": moment_id})),
        call(&handle, "favorite.list", json!({})),
        call(&handle, "sticker.packs", json!({})),
        call(&handle, "moment.detail", json!({"moment_id": "nope"})),
    ];
    for reply in seen {
        let text = reply.to_string();
        assert!(!text.contains(&token), "凭据不许跨过桥：{text}");
        assert!(!text.contains("Bearer"), "桥不该把鉴权头回给 UI：{text}");
    }
    let _ = app;
}

#[test]
fn content_actions_are_advertised_in_the_catalog() {
    for action in [
        "moment.publish",
        "moment.feed",
        "moment.detail",
        "moment.remove",
        "moment.like",
        "moment.unlike",
        "moment.comment",
        "moment.comment.remove",
        "report.create",
        "favorite.list",
        "favorite.one",
        "favorite.remove",
        "favorite.add",
        "favorite.update",
        "sticker.packs",
        "moment.media_url",
    ] {
        assert!(IMPLEMENTED_ACTIONS.contains(&action), "{action} 必须让 UI 能发现");
    }
}