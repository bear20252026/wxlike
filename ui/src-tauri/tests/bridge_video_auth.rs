//! D6（视频/直播域逐域退役）：控制面经桥走认证 HTTP，与钱包/通话域同口径。
//!
//! 服务端把视频域分成两面（crates/video/src/lib.rs，issue #7）：播放面匿名、控制面
//! 认调用者。本桥只退役控制面里读旧会话的那部分（meta/stats/view/feed 与 live 六条），
//! 见 `core_bridge/video.inc`。上传（multipart）无流式提供者，在消费侧显式阻塞。
//!
//! 全程真服务端栈（`support::stack()`）。逐条钉住的口径：
//! 1. 没登录 → `UNAUTHENTICATED`，且一个字节都不发（死端口判别）；
//! 2. 畸形 video_id/channel 在发请求之前拦掉（VALIDATION）；
//! 3. 直播控制面往返：start→state(live)→join→danmaku→leave→stop(204→{})；
//! 4. Bearer 只出 Rust：任何 action 返回值里都找不到它（live.start 的 ingest_token
//!    是推流凭据，不是会话 Bearer）；
//! 5. 目录登记：video.*/live.* 都在 IMPLEMENTED_ACTIONS。

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
        request_id: format!("d6-video-{action}"),
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
    reply
        .get("data")
        .unwrap_or_else(|| panic!("{what} 的成功信封必须带 data：{reply}"))
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

/// 用调用者自己的 bearer 建一个 live 频道（社区→频道），返回 channel_id。
/// 这是夹具搭建（不是被测路径），故直接走 ureq；被测的直播控制面一律经桥。
fn make_live_channel(http: &str, token: &str) -> String {
    let community: Value = ureq::post(&format!("{http}/api/communities"))
        .header("Authorization", &format!("Bearer {token}"))
        .send_json(json!({ "name": "d6-live-community" }))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    let cid = community["community_id"].as_str().unwrap().to_string();
    let channel: Value = ureq::post(&format!("{http}/api/communities/{cid}/channels"))
        .header("Authorization", &format!("Bearer {token}"))
        .send_json(json!({ "name": "d6-live", "kind": "live" }))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    channel["channel_id"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unauthenticated_video_actions_never_touch_the_network() {
    let (app, handle) = boot();
    app.manage(AppState::new("http://127.0.0.1:1"));

    let cases: Vec<(&str, Value)> = vec![
        ("video.meta", json!({"video_id": "v1"})),
        ("video.stats", json!({"video_id": "v1"})),
        ("video.view", json!({"video_id": "v1", "viewer": "ui", "watched_ms": 1000})),
        ("video.feed", json!({"mode": "hot"})),
        ("live.state", json!({"channel": "ch1"})),
        ("live.start", json!({"channel": "ch1", "recording": false})),
        ("live.stop", json!({"channel": "ch1"})),
        ("live.join", json!({"channel": "ch1"})),
        ("live.leave", json!({"channel": "ch1"})),
        ("live.danmaku", json!({"channel": "ch1", "text": "hi"})),
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
    let (_app, handle, _id) = signed_in(&http, "v_mal");

    let cases: Vec<(&str, Value)> = vec![
        ("video.meta", json!({})),
        ("video.meta", json!({"video_id": ""})),
        ("video.stats", json!({"video_id": "../admin"})),
        ("video.view", json!({"video_id": "v1", "viewer": "ui"})),
        ("live.state", json!({"channel": "a b"})),
        ("live.start", json!({"channel": "ch1?x=1"})),
        ("live.danmaku", json!({"channel": "ch1"})),
    ];
    for (action, params) in cases {
        let reply = call(&handle, action, params);
        error(&reply, &format!("{action} 的畸形参数"), "VALIDATION");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_control_plane_round_trip() {
    let (http, _quic, _platform) = stack().await;
    let (app, handle, _id) = signed_in(&http, "v_host");
    let token = handle.state::<AppState>().auth_token().expect("登录后要有凭据");
    let channel = make_live_channel(&http, &token);

    // start：{recording} → {session, ingest_url, ingest_token}。
    let started = call_with(&handle, "live.start", json!({"channel": channel, "recording": true}), Some("live-start-1"));
    let data = ok_data(&started, "开播");
    assert!(data["ingest_url"].as_str().unwrap_or_default().contains("/live/"), "{data}");

    // state：开播后 live=true。
    let state = ok_data(&call(&handle, "live.state", json!({"channel": channel})), "直播状态").clone();
    assert_eq!(state["live"], json!(true), "开播后应是 live：{state}");

    // join / danmaku / leave。
    ok_data(&call(&handle, "live.join", json!({"channel": channel})), "加入");
    ok_data(&call(&handle, "live.danmaku", json!({"channel": channel, "text": "hello"})), "弹幕");
    ok_data(&call(&handle, "live.leave", json!({"channel": channel})), "离开");

    // stop：204 → {}。
    ok_data(&call(&handle, "live.stop", json!({"channel": channel})), "停播");
    let after = ok_data(&call(&handle, "live.state", json!({"channel": channel})), "停播后状态").clone();
    assert_eq!(after["live"], json!(false), "停播后应不是 live：{after}");

    let _ = app;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn video_read_paths_are_honest_on_empty_store() {
    let (http, _quic, _platform) = stack().await;
    let (app, handle, _id) = signed_in(&http, "v_read");

    // feed：空库 → 空信封（不编造条目）。
    let feed = ok_data(&call(&handle, "video.feed", json!({"mode": "hot"})), "feed").clone();
    assert_eq!(feed["items"].as_array().map(Vec::len), Some(0), "{feed}");

    // stats / view：服务端校验视频存在，不存在 → 404 → 桥归 VALIDATION，
    // 如实报错不伪造成功（不编造观看数/不假装上报成功）。
    let stats = call(&handle, "video.stats", json!({"video_id": "v-read"}));
    error(&stats, "对不存在视频的统计", "VALIDATION");
    let view = call(&handle, "video.view", json!({"video_id": "v-read", "viewer": "ui", "watched_ms": 1000}));
    error(&view, "对不存在视频的观看上报", "VALIDATION");

    let _ = app;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bearer_token_never_crosses_the_bridge() {
    let (http, _quic, _platform) = stack().await;
    let (app, handle, _id) = signed_in(&http, "v_tok");
    let token = handle.state::<AppState>().auth_token().expect("登录后要有凭据");
    assert!(!token.is_empty());
    let channel = make_live_channel(&http, &token);

    ok_data(&call(&handle, "live.start", json!({"channel": channel})), "开播");
    let seen = vec![
        call(&handle, "live.state", json!({"channel": channel})),
        call(&handle, "live.join", json!({"channel": channel})),
        call(&handle, "video.feed", json!({"mode": "hot"})),
        call(&handle, "video.meta", json!({"video_id": "nope"})),
    ];
    for reply in seen {
        let text = reply.to_string();
        assert!(!text.contains(&token), "凭据不许跨过桥：{text}");
        assert!(!text.contains("Bearer"), "桥不该把鉴权头回给 UI：{text}");
    }
    let _ = app;
}

#[test]
fn video_actions_are_advertised_in_the_catalog() {
    for action in [
        "video.meta",
        "video.stats",
        "video.view",
        "video.feed",
        "live.state",
        "live.start",
        "live.stop",
        "live.join",
        "live.leave",
        "live.danmaku",
    ] {
        assert!(IMPLEMENTED_ACTIONS.contains(&action), "{action} 必须让 UI 能发现");
    }
}