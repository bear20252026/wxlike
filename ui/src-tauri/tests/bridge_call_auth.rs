//! D6（通话域逐域退役）：通话切片经桥走认证 HTTP，与钱包域同口径。
//!
//! 缺陷本体与 #293 相同：新 UI 的通话域过去直接打 `lib/http.ts`，而那份传输的
//! `setBaseUrl`/`saveSession` 没有任何调用点，请求根本没带凭据；打包后的 CSP 又
//! 不许 webview 发 HTTP。修法是让通话域不再发 HTTP：会话留在 Rust 侧，房间控制
//! 与信令由 `core_dispatch` 代持（`call.*`，见 `core_bridge/call.inc`）。
//!
//! 全程真服务端栈（`support::stack()`，与 `bridge_wallet_auth.rs` 同一夹具）。
//! 逐条钉住的口径：
//! 1. 没登录 → `UNAUTHENTICATED`，且**一个字节都不发**（死端口判别：真发了请求
//!    会先撞上 `OFFLINE`，拿到 `OFFLINE` 就是闸门失效的实锤）；
//! 2. 控制类端点服务端回 204 → 桥读成 `{}` 如实成功，不是 decode 错误；
//! 3. 信令往返：post 投递、take 取走（读即清）；
//! 4. 畸形 conv 在发请求之前拦掉（空串/`/`/`..` 会拼出畸形路径）；
//! 5. Bearer 只出 Rust：任何 action 的返回值里都找不到它。

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
        request_id: format!("d6-call-{action}"),
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

/// 一条 core_dispatch → JSON 信封。在独立 OS 线程里跑：ureq 是阻塞的，不能占住
/// 要等 axum/服务端应答的那条 worker。
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
    assert!(
        reply.get("data").is_none(),
        "{what} 失败时不许带 data 字段（UI 会把 null 当空载荷）：{reply}"
    );
    let err = &reply["error"];
    assert_eq!(err["code"], json!(want), "{what} 的错误码不对：{reply}");
    assert!(
        !err["message"].as_str().unwrap_or_default().trim().is_empty(),
        "{what} 的失败要带上服务端说了什么：{reply}"
    );
    err
}

fn register(state: &AppState, name: &str) -> String {
    let reply = state
        .register(name, "pw")
        .unwrap_or_else(|e| panic!("{name} 注册失败：{e}"));
    reply["user_id"]
        .as_str()
        .unwrap_or_else(|| panic!("注册响应没有 user_id：{reply}"))
        .to_string()
}

fn signed_in(http: &str, name: &str) -> (MockApp, MockHandle, String) {
    let (app, handle) = boot();
    let state = AppState::new(http);
    let user_id = register(&state, name);
    app.manage(state);
    (app, handle, user_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unauthenticated_call_actions_never_touch_the_network() {
    let (app, handle) = boot();
    // 死端口：请求真发出去会先得到 OFFLINE，所以拿到 OFFLINE == "闸门失效"。
    app.manage(AppState::new("http://127.0.0.1:1"));

    let cases: Vec<(&str, Value)> = vec![
        ("call.state", json!({"conv": "c1"})),
        ("call.start", json!({"conv": "c1"})),
        ("call.join", json!({"conv": "c1"})),
        ("call.leave", json!({"conv": "c1"})),
        ("call.mute", json!({"conv": "c1", "track": "audio", "muted": true})),
        ("call.mute_all", json!({"conv": "c1", "track": "audio", "muted": true})),
        ("call.lock", json!({"conv": "c1", "value": true})),
        ("call.record", json!({"conv": "c1", "value": true})),
        ("call.hand", json!({"conv": "c1", "value": true})),
        ("call.kick", json!({"conv": "c1", "target": "u_bob"})),
        ("call.signal.post", json!({"conv": "c1", "signal": {"kind": "offer"}})),
        ("call.signal.take", json!({"conv": "c1"})),
    ];
    for (action, params) in cases {
        let reply = call(&handle, action, params);
        let err = error(&reply, action, "UNAUTHENTICATED");
        assert!(
            !err["message"].as_str().unwrap_or_default().contains("传输失败"),
            "{action} 必须没发出任何请求（OFFLINE/传输失败都算发了）：{reply}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn control_endpoints_204_are_honest_success() {
    let (http, _quic, _platform) = stack().await;
    let (alice_app, alice, alice_id) = signed_in(&http, "c_alice");
    let (bob_app, bob, bob_id) = signed_in(&http, "c_bob");
    let conv = wxlike_ui_lib::direct_conv_id(&alice_id, &bob_id);

    // start：空 body 合法（默认容量），服务端 204 → 桥读成 {}。
    let started = call_with(&alice, "call.start", json!({"conv": conv}), Some("call-start-1"));
    ok_data(&started, "开房");

    // state：开房后 live=true。
    let state = ok_data(&call(&alice, "call.state", json!({"conv": conv})), "通话状态").clone();
    assert_eq!(state["conv_id"], json!(conv), "{state}");
    assert_eq!(state["live"], json!(true), "开房后应是 live：{state}");

    // bob join（204），随后 leave（204）。
    ok_data(&call_with(&bob, "call.join", json!({"conv": conv}), Some("call-join-1")), "加入");
    ok_data(&call_with(&bob, "call.leave", json!({"conv": conv}), Some("call-leave-1")), "离开");

    // lock / record / hand：{value} → 204。
    ok_data(&call(&alice, "call.lock", json!({"conv": conv, "value": true})), "锁定");
    ok_data(&call(&alice, "call.record", json!({"conv": conv, "value": true})), "录制");
    ok_data(&call(&alice, "call.hand", json!({"conv": conv, "value": true})), "举手");

    let _ = (alice_app, bob_app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signal_post_and_take_round_trip() {
    let (http, _quic, _platform) = stack().await;
    let (alice_app, alice, alice_id) = signed_in(&http, "s_alice");
    let (bob_app, bob, bob_id) = signed_in(&http, "s_bob");
    let conv = wxlike_ui_lib::direct_conv_id(&alice_id, &bob_id);

    ok_data(&call(&alice, "call.start", json!({"conv": conv})), "开房");
    ok_data(&call(&bob, "call.join", json!({"conv": conv})), "加入");

    // alice 投一条 invite（服务端 Signal 契约：{v, call_id, from, to, content}，
    // from 必须等于发送者）；bob 取走（读即清）。桥原样透传，不改写信封。
    let invite = json!({
        "v": 1,
        "call_id": conv,
        "from": alice_id,
        "to": bob_id,
        "content": {"type": "invite", "sdp": {"sdp": "offer-sdp"}, "video": true},
    });
    ok_data(&call(&alice, "call.signal.post", json!({"conv": conv, "signal": invite})), "投递信令");

    let taken = ok_data(&call(&bob, "call.signal.take", json!({"conv": conv})), "取信令").clone();
    let signals = taken["signals"].as_array().cloned().unwrap_or_default();
    assert!(
        signals.iter().any(|s| s["content"]["type"] == json!("invite")),
        "bob 要取到 alice 投的 invite：{taken}"
    );

    // 再取一次：读即清，队列已空（不重复投递）。
    let again = ok_data(&call(&bob, "call.signal.take", json!({"conv": conv})), "再取信令").clone();
    let empty = again["signals"].as_array().cloned().unwrap_or_default();
    assert!(
        !empty.iter().any(|s| s["content"]["type"] == json!("invite")),
        "读即清：同一条 invite 不该被取第二次：{again}"
    );

    let _ = (alice_app, bob_app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn malformed_conv_is_refused_before_the_socket() {
    let (http, _quic, _platform) = stack().await;
    let (_app, handle, _alice_id) = signed_in(&http, "v_alice");

    // 空/畸形 conv 会拼出 `/api/call//start` 这种请求，甚至改写请求目标。
    // 全部在发请求之前拦掉（VALIDATION），不是 OFFLINE。
    let cases: Vec<(&str, Value)> = vec![
        ("call.state", json!({})),
        ("call.state", json!({"conv": ""})),
        ("call.start", json!({"conv": "../admin"})),
        ("call.join", json!({"conv": "c1?x=1"})),
        ("call.signal.take", json!({"conv": "a b"})),
        ("call.mute", json!({"conv": "c1", "track": "audio"})),
        ("call.lock", json!({"conv": "c1"})),
        ("call.kick", json!({"conv": "c1"})),
        ("call.signal.post", json!({"conv": "c1"})),
    ];
    for (action, params) in cases {
        let reply = call(&handle, action, params);
        error(&reply, &format!("{action} 的畸形参数"), "VALIDATION");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bearer_token_never_crosses_the_bridge() {
    let (http, _quic, _platform) = stack().await;
    let (alice_app, alice, alice_id) = signed_in(&http, "t_alice");
    let (_bob_app, _bob, bob_id) = signed_in(&http, "t_bob");
    let conv = wxlike_ui_lib::direct_conv_id(&alice_id, &bob_id);
    let token = alice
        .state::<AppState>()
        .auth_token()
        .expect("登录后要有凭据");
    assert!(!token.is_empty(), "夹具凭据不该是空串");

    ok_data(&call(&alice, "call.start", json!({"conv": conv})), "开房");
    let seen = vec![
        call(&alice, "call.state", json!({"conv": conv})),
        call(&alice, "call.hand", json!({"conv": conv, "value": true})),
        call(&alice, "call.signal.post", json!({"conv": conv, "signal": {"kind": "offer"}})),
        call(&alice, "call.signal.take", json!({"conv": conv})),
        // 失败信封也不许把凭据回出去。
        call(&alice, "call.state", json!({"conv": "nope-not-here"})),
    ];
    for reply in seen {
        let text = reply.to_string();
        assert!(!text.contains(&token), "凭据不许跨过桥（会直接进 webview）：{text}");
        assert!(!text.contains("Bearer"), "桥不该把鉴权头本身回给 UI：{text}");
    }
    let _ = alice_app;
}

#[test]
fn call_actions_are_advertised_in_the_catalog() {
    for action in [
        "call.state",
        "call.start",
        "call.join",
        "call.leave",
        "call.mute",
        "call.mute_all",
        "call.lock",
        "call.record",
        "call.hand",
        "call.kick",
        "call.signal.post",
        "call.signal.take",
    ] {
        assert!(
            IMPLEMENTED_ACTIONS.contains(&action),
            "{action} 是真实现了的，必须让 UI 能发现"
        );
    }
}