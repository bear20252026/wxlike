//! D6（小程序域逐域退役）：目录/授权/卸载控制面经桥走认证 HTTP（G08 服务端）。
//!
//! 与其余已迁移域同口径：会话留在 Rust 侧，控制面由 core_dispatch 代持
//! （mini.*，见 core_bridge/mini.inc）。install 与包体下载（二进制 + sha256
//! 校验）无 JSON 流式提供者，在消费侧显式阻塞，不在本桥。
//!
//! 全程真服务端栈（support::stack()，组合根已装配 mini_mount）。生产默认目录
//! 为空（管理后台配置），这里钉住诚实路径：
//! 1. 没登录 → UNAUTHENTICATED，且一个字节都不发；
//! 2. 畸形 id 在发请求之前拦掉（VALIDATION）；
//! 3. 空目录诚实（空数组），给不存在的应用授权 → 404 → VALIDATION；
//! 4. Bearer 只出 Rust。
//!
//! 授权/卸载完整往返（目录有应用时）由 server 集成测试预置目录后钉住。

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
        request_id: format!("d6-mini-{action}"),
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
async fn unauthenticated_mini_actions_never_touch_the_network() {
    let (app, handle) = boot();
    app.manage(AppState::new("http://127.0.0.1:1"));

    let cases: Vec<(&str, Value)> = vec![
        ("mini.list", json!({})),
        ("mini.releases", json!({"app_id": "wxmini-0001"})),
        ("mini.grants", json!({})),
        ("mini.grant", json!({"app_id": "wxmini-0001", "scopes": ["profile.read"]})),
        ("mini.revoke", json!({"app_id": "wxmini-0001"})),
        ("mini.uninstall", json!({"app_id": "wxmini-0001"})),
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
    let (_app, handle, _id) = signed_in(&http, "m_mal");

    let cases: Vec<(&str, Value)> = vec![
        ("mini.releases", json!({})),
        ("mini.releases", json!({"app_id": "../admin"})),
        ("mini.grant", json!({"scopes": []})),
        ("mini.revoke", json!({"app_id": "a b"})),
        ("mini.uninstall", json!({"app_id": "x?y=1"})),
    ];
    for (action, params) in cases {
        let reply = call(&handle, action, params);
        error(&reply, &format!("{action} 的畸形参数"), "VALIDATION");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn empty_catalog_and_unknown_app_grant_are_honest() {
    let (http, _quic, _platform) = stack().await;
    let (app, handle, _id) = signed_in(&http, "m_empty");

    // 目录：管理后台未配置 → 空数组。
    let list = ok_data(&call(&handle, "mini.list", json!({})), "目录").clone();
    assert_eq!(list["items"], json!([]), "{list}");

    // 授权列举：当前用户没有任何授权 → 空数组。
    let grants = ok_data(&call(&handle, "mini.grants", json!({})), "授权列举").clone();
    assert_eq!(grants["items"], json!([]), "{grants}");

    // 给不存在的应用授权 → 服务端 404 → VALIDATION，不凭空授权。
    let reply = call(&handle, "mini.grant", json!({"app_id": "wxmini-nope", "scopes": ["profile.read"]}));
    error(&reply, "给不存在的应用授权", "VALIDATION");

    let _ = app;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bearer_token_never_crosses_the_bridge() {
    let (http, _quic, _platform) = stack().await;
    let (app, handle, _id) = signed_in(&http, "m_tok");
    let token = handle.state::<AppState>().auth_token().expect("登录后要有凭据");
    assert!(!token.is_empty());

    let seen = vec![
        call(&handle, "mini.list", json!({})),
        call(&handle, "mini.grants", json!({})),
        call(&handle, "mini.releases", json!({"app_id": "wxmini-nope"})),
        call(&handle, "mini.uninstall", json!({"app_id": "wxmini-nope"})),
    ];
    for reply in seen {
        let text = reply.to_string();
        assert!(!text.contains(&token), "凭据不许跨过桥：{text}");
        assert!(!text.contains("Bearer"), "桥不该把鉴权头回给 UI：{text}");
    }
    let _ = app;
}

#[test]
fn mini_actions_are_advertised_in_the_catalog() {
    for action in ["mini.list", "mini.releases", "mini.grants", "mini.grant", "mini.revoke", "mini.uninstall"] {
        assert!(IMPLEMENTED_ACTIONS.contains(&action), "{action} 必须让 UI 能发现");
    }
}