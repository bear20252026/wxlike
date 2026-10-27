//! #293 台账（G01-293）：四个"报成功但不做那件事"的桥 handler 改成**可判定**的。
//!
//! 每条都配一条能红的断言——mock 服务端不响应 / 装了个成功但其实没做 / 少给字段时，
//! action 必须**报错**，不许交出 `{"cancelled": true}` / `{"revoked": true}` 那种
//! 凭据。写得出成功信封的地方，都必须同时给出"那件事真在服务端发生了"的证据。
//!
//! 四条口径与判据：
//! 1. `identity.qr_login.cancel`：只停本地轮询不算取消。判据 = 撤销后再查一次票据
//!    状态，必须 `REJECTED`；做不到（服务端只有扫码侧能 reject，桌面自建的票还没人扫）
//!    就返回 `NOT_IMPLEMENTED`，绝不返回 `cancelled: true`。
//! 2. `identity.session.logout` / `identity.qr_login.switch_account`：必须真的调
//!    `POST /api/logout`，并且**回查**旧凭据再打 `/api/me` 拿到 401 才算撤销；
//!    撤销没生效时本地会话原样保留（抹掉一张还生效的凭据只会让人失去重试机会）。
//! 3. `media.upload.cancel`：`upload_id` 缺失/空白/不在表里一律 `VALIDATION`（旧写法
//!    `unwrap_or_default()` 拿到空串、`remove("")` 必然落空却照样回成功）；真的取消
//!    只丢本地记账，服务端没有可通知的分片会话，所以如实回
//!    `local_only: true / server_notified: false`，不再冒充 `cancelled: true`。
//! 4. `AppState::login/register`：响应 2xx 但没有 `token` 时报错，不存 `Some("")`
//!    ——空 bearer 之后的现象是远程 401/超时，栈里查不到"登录其实没拿到凭据"。
//!
//! 真 HTTP 面：正例全部走 `support::stack()` 的真服务端（platform + hub），
//! 只有"服务端说谎"这类形状必须自建小 axum——那正是真服务端不会给的响应形状。

#![allow(dead_code)] // 分次落盘：某一段的夹具在后续段落才用到

use serde_json::{json, Value};
use tauri::Manager as _;
use wxlike_ui_lib::core_bridge::{dispatch, CoreRequest, CoreState};
use wxlike_ui_lib::AppState;

mod support;

use crate::support::stack;

type MockApp = tauri::App<tauri::test::MockRuntime>;
type MockHandle = tauri::AppHandle<tauri::test::MockRuntime>;

/// `WXLIKE_DATA_DIR` 是进程级环境变量：需要本地库的用例整测试体串行化。
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn request(action: &str, params: Value) -> CoreRequest {
    CoreRequest {
        action_id: action.to_string(),
        request_id: format!("g01-293-cancel-{action}"),
        idempotency_key: None,
        params,
    }
}

fn boot() -> (MockApp, MockHandle) {
    let app = tauri::test::mock_app();
    app.manage(CoreState::new());
    let handle = app.handle().clone();
    (app, handle)
}

/// 一条 core_dispatch → JSON 信封（与 webview 同视角）。
///
/// 独立 OS 线程执行：ureq 是阻塞的，不能占住要等服务端应答的 tokio worker
/// （与 `bridge_wallet_auth.rs` 同一口径）。
fn call(app: &MockHandle, action: &str, params: Value) -> Value {
    let handle = app.clone();
    let req = request(action, params);
    std::thread::spawn(move || {
        let state = handle.state::<AppState>();
        let core = handle.state::<CoreState>();
        serde_json::to_value(dispatch(&handle, &state, &core, req)).unwrap()
    })
    .join()
    .unwrap_or_else(|e| std::panic::resume_unwind(e))
}

/// 成功信封必须有 `data` 且不是 null——`{"data": null}` 与失败没区别，正是本卡要消灭的。
fn ok_data<'a>(reply: &'a Value, what: &str) -> &'a Value {
    assert_eq!(reply["ok"], json!(true), "{what} 必须成功，实际：{reply}");
    let data = reply
        .get("data")
        .unwrap_or_else(|| panic!("{what} 的成功信封必须带 data：{reply}"));
    assert!(!data.is_null(), "{what} 成功时 data 不许是 null：{reply}");
    data
}

/// 失败信封：`ok:false` + 指定错误码 + 没有 `data` + 消息非空。
fn error<'a>(reply: &'a Value, what: &str, want: &str) -> &'a Value {
    assert_eq!(
        reply["ok"],
        json!(false),
        "{what} 必须失败，不许伪造成功：{reply}"
    );
    assert!(
        reply.get("data").is_none(),
        "{what} 失败时不许带 data 字段：{reply}"
    );
    let err = &reply["error"];
    assert_eq!(err["code"], json!(want), "{what} 的错误码不对：{reply}");
    assert!(
        !err["message"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .is_empty(),
        "{what} 的失败要带原因：{reply}"
    );
    err
}

/// 已注册的 AppState + 桥句柄；返回的 String 是**服务端签发的那张凭据**。
fn signed_in(http: &str, name: &str) -> (MockApp, MockHandle, String) {
    let (app, handle) = boot();
    let state = AppState::new(http);
    let reply = state
        .register(name, "pw")
        .unwrap_or_else(|e| panic!("{name} 注册失败：{e}"));
    let token = reply["token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| panic!("注册响应没有 token：{reply}"))
        .to_string();
    app.manage(state);
    (app, handle, token)
}

/// 这张凭据在服务端还活着吗？活着返回 true。
///
/// 失败时钉住**必须是 401**（"撤销生效"的判据是服务端不再认这张票，不是"请求
/// 出错了"这种含糊现象——服务挂了也会出错）。
fn token_works(http: &str, token: &str) -> bool {
    let probe = AppState::new(http);
    probe.set_auth_token(token);
    match probe.me() {
        Ok(_) => true,
        Err(err) => {
            assert!(
                err.contains("(401)"),
                "被撤销的凭据必须撞上 401，实际是：{err}"
            );
            false
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn login_refuses_to_store_an_empty_credential() {
    let base = lying_login_server().await;
    let state = AppState::new(&base);

    // 200 + 没有 token：旧写法存下 `Some("")`，之后每个请求都带一个空 bearer，
    // 现象是远程 401/超时，栈里查不到真因。现在必须是错误，且不落任何凭据。
    let err = state
        .login("alice", "pw")
        .expect_err("响应里没有 token 就不能算登录成功");
    assert!(err.contains("token"), "要指名缺哪个字段：{err}");
    assert!(
        state.auth_token().is_none(),
        "不许留下 Some(\"\") 这种看着像凭据的东西：{:?}",
        state.auth_token()
    );

    // 注册同一条规则。
    let err = state
        .register("bob", "pw")
        .expect_err("注册响应没有 token 也不算成功");
    assert!(err.contains("token"), "{err}");
    assert!(state.auth_token().is_none(), "{:?}", state.auth_token());

    // 桥侧视角：UI 拿到的必须是错误信封，不是"登录成功"的投影。
    let (app, handle) = boot();
    app.manage(AppState::new(&base));
    let reply = call(
        &handle,
        "identity.session.login",
        json!({"username": "alice", "password": "pw"}),
    );
    let err = error(&reply, "缺 token 的登录（经桥）", "INTERNAL");
    assert!(
        err["message"]
            .as_str()
            .unwrap_or_default()
            .contains("token"),
        "{reply}"
    );
    assert!(
        !reply.to_string().contains("authenticated"),
        "不许把这次失败投影成已登录：{reply}"
    );
}

/// 2xx 但不签发凭据的登录/注册端点（真服务端不会这么答，所以自建）。
async fn lying_login_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = axum::Router::new()
        .route(
            "/api/login",
            axum::routing::post(|| async { r#"{"user_id":"u-alice"}"# }),
        )
        .route(
            "/api/register",
            axum::routing::post(|| async { r#"{"user_id":"u-bob"}"# }),
        );
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
}

/// 台账第 4 条的正向对照：真服务端给了凭据就必须存下来（非空、能用）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_real_token_is_stored_and_usable() {
    let (http, _quic, _platform) = stack().await;
    let (_app, _handle, token) = signed_in(&http, "t_ok");
    assert!(!token.is_empty(), "真服务端要签发凭据：{token}");
    assert!(token_works(&http, &token), "刚签发的凭据必须能用");
}
