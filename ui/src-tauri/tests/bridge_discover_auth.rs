//! D6（发现域逐域退役）：服务目录控制面经桥走认证 HTTP（G07 服务目录服务端）。
//!
//! 与钱包/通话/视频/内容/文件域同口径：会话留在 Rust 侧，控制面由 core_dispatch
//! 代持（discover.*，见 core_bridge/discover.inc）。识图/翻译（multipart + 外部
//! AI）无核心提供者，在消费侧显式阻塞，不在本桥。
//!
//! 全程真服务端栈（support::stack()，组合根已装配 discover_mount）。生产默认
//! 不配置供应商沙箱凭据，所以这里钉住的是**诚实路径**：
//! 1. 没登录 → UNAUTHENTICATED，且一个字节都不发（死端口判别）；
//! 2. 畸形 id 在发请求之前拦掉（VALIDATION）；
//! 3. 目录空数组诚实（管理后台未配置），报价无供应商凭据 → NOT_IMPLEMENTED；
//! 4. Bearer 只出 Rust：任何 action 返回值里都找不到它。
//!
//! 订单完整往返（报价→下单→查单→列举→退款）由 crate 侧 HTTP 投影测试
//! （crates/service-catalog/tests/http.rs，带可用沙箱供应商）逐条钉住。

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
        request_id: format!("d6-discover-{action}"),
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
async fn unauthenticated_discover_actions_never_touch_the_network() {
    let (app, handle) = boot();
    app.manage(AppState::new("http://127.0.0.1:1"));

    let cases: Vec<(&str, Value)> = vec![
        ("discover.services", json!({})),
        ("discover.quote", json!({"service_id": "topup-cn"})),
        ("discover.order_create", json!({"service_id": "topup-cn", "quote_id": "q1"})),
        ("discover.order_get", json!({"order_id": "ord1"})),
        ("discover.orders", json!({})),
        ("discover.order_refund", json!({"order_id": "ord1"})),
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
    let (_app, handle, _id) = signed_in(&http, "d_mal");

    let cases: Vec<(&str, Value)> = vec![
        ("discover.quote", json!({})),
        ("discover.quote", json!({"service_id": ""})),
        ("discover.order_create", json!({"service_id": "s1"})),
        ("discover.order_get", json!({"order_id": "../admin"})),
        ("discover.order_refund", json!({"order_id": "a b"})),
    ];
    for (action, params) in cases {
        let reply = call(&handle, action, params);
        error(&reply, &format!("{action} 的畸形参数"), "VALIDATION");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn empty_catalog_and_blocked_quote_are_honest() {
    let (http, _quic, _platform) = stack().await;
    let (app, handle, _id) = signed_in(&http, "d_empty");

    // 目录：管理后台未配置 → 空数组，不拿假数据填充。
    let services = ok_data(&call(&handle, "discover.services", json!({})), "目录").clone();
    assert_eq!(services["items"], json!([]), "{services}");

    // 订单列举：当前用户没有任何订单 → 空数组。
    let orders = ok_data(&call(&handle, "discover.orders", json!({})), "订单列举").clone();
    assert_eq!(orders["items"], json!([]), "{orders}");

    // 报价：生产默认目录为空，报价一个不存在的服务 → 服务端 404 → VALIDATION，
    // 不伪造价格。（无供应商凭据的 BLOCKED_EXTERNAL→NOT_IMPLEMENTED 路径由
    // crate 侧 HTTP 测试在"目录有卡但供应商无凭据"时钉住。）
    let reply = call(&handle, "discover.quote", json!({"service_id": "topup-cn"}));
    error(&reply, "空目录报价不存在的服务", "VALIDATION");

    let _ = app;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bearer_token_never_crosses_the_bridge() {
    let (http, _quic, _platform) = stack().await;
    let (app, handle, _id) = signed_in(&http, "d_tok");
    let token = handle.state::<AppState>().auth_token().expect("登录后要有凭据");
    assert!(!token.is_empty());

    let seen = vec![
        call(&handle, "discover.services", json!({})),
        call(&handle, "discover.orders", json!({})),
        call(&handle, "discover.quote", json!({"service_id": "topup-cn"})),
        call(&handle, "discover.order_get", json!({"order_id": "ord_nope"})),
    ];
    for reply in seen {
        let text = reply.to_string();
        assert!(!text.contains(&token), "凭据不许跨过桥：{text}");
        assert!(!text.contains("Bearer"), "桥不该把鉴权头回给 UI：{text}");
    }
    let _ = app;
}

#[test]
fn discover_actions_are_advertised_in_the_catalog() {
    for action in [
        "discover.services",
        "discover.quote",
        "discover.order_create",
        "discover.order_get",
        "discover.orders",
        "discover.order_refund",
    ] {
        assert!(IMPLEMENTED_ACTIONS.contains(&action), "{action} 必须让 UI 能发现");
    }
}