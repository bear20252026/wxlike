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
    let base = lying_login_server();
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

// ---------------------------------------------------------------------------
// 自建小服务（"服务端说谎"这类形状，真服务端不会给）
// ---------------------------------------------------------------------------

/// 在**独立 OS 线程**的 current-thread 运行时里起一个小 axum 服务。
///
/// 不用 `tokio::spawn` 挂到测试运行时上：用例里的桥调用是阻塞的（ureq），
/// 服务任务与它抢同一批 worker 时会偶发 `os error 10053`（本机 8 条用例并行
/// 实测到过一次）。服务端自己一条线程，就不看测试运行时的脸色。
fn spawn_server(router: axum::Router) -> String {
    let (tx, rx) = std::sync::mpsc::channel::<std::net::SocketAddr>();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("mock 服务的当前线程运行时");
        let listener = rt
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .expect("mock 服务绑定端口");
        let addr = listener.local_addr().expect("mock 服务地址");
        tx.send(addr).expect("mock 服务上线通知");
        rt.block_on(async move {
            let _ = axum::serve(listener, router).await;
        });
    });
    let addr = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("mock 服务没起来");
    wait_until_serving(addr);
    format!("http://{addr}")
}

/// 端口在 listen ≠ 已经在应答：先真探一次，拿到响应字节才交给用例
/// （高负载下 accept 会慢，宁可等，也不要让它"约 2% 自己红"）。
fn wait_until_serving(addr: std::net::SocketAddr) {
    use std::io::{Read as _, Write as _};
    for _ in 0..400 {
        if let Ok(mut stream) = std::net::TcpStream::connect(addr) {
            let req = "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
            if stream.write_all(req.as_bytes()).is_ok() {
                let mut buf = [0u8; 16];
                if stream.read(&mut buf).is_ok() {
                    return;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("mock 服务始终没有开始应答：{addr}");
}

/// 2xx 但不签发凭据的登录/注册端点（真服务端不会这么答，所以自建）。
fn lying_login_server() -> String {
    spawn_server(
        axum::Router::new()
            .route(
                "/api/login",
                axum::routing::post(|| async { r#"{"user_id":"u-alice"}"# }),
            )
            .route(
                "/api/register",
                axum::routing::post(|| async { r#"{"user_id":"u-bob"}"# }),
            ),
    )
}

/// 台账第 4 条的正向对照：真服务端给了凭据就必须存下来（非空、能用）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_real_token_is_stored_and_usable() {
    let (http, _quic, _platform) = stack().await;
    let (_app, _handle, token) = signed_in(&http, "t_ok");
    assert!(!token.is_empty(), "真服务端要签发凭据：{token}");
    assert!(token_works(&http, &token), "刚签发的凭据必须能用");
}

// ---------------------------------------------------------------------------
// 台账第 2 条：logout / switch_account 必须真的把凭据在服务端撤销
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn logout_revokes_the_credential_on_the_server() {
    let (http, _quic, _platform) = stack().await;
    let (_app, handle, token) = signed_in(&http, "s_alice");
    assert!(
        token_works(&http, &token),
        "夹具凭据本来必须有效，否则这条测试证不了任何事"
    );

    let reply = call(&handle, "identity.session.logout", json!({}));
    let data = ok_data(&reply, "退出登录");
    assert_eq!(data["revoked"], json!(true), "{reply}");
    assert_eq!(
        data["server_revoked"],
        json!(true),
        "撤销必须带服务端确证这一半：{reply}"
    );
    assert!(
        !reply.to_string().contains(&token),
        "Bearer 不许出现在桥的返回里：{reply}"
    );

    // 判据：拿那张旧凭据再打一次需要鉴权的端点，必须 401。
    assert!(
        !token_works(&http, &token),
        "退出之后旧凭据必须已经不被服务端接受"
    );
    // 本地也不再持有它。
    assert_eq!(
        handle.state::<AppState>().auth_token(),
        None,
        "服务端撤销确认后本地凭据必须清掉"
    );
}

/// 服务端把登出"装成成功"（2xx 但那行会话还在）⇒ 桥不许跟着报 `revoked: true`。
///
/// 这条就是台账本体：`emit(signed_out)` 之后直接回成功，一次服务端调用都没有。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lying_logout_endpoint_must_not_report_revoked() {
    let base = lying_logout_server();
    let (app, handle) = boot();
    let state = AppState::new(&base);
    state.set_auth_token("tk-1");
    app.manage(state);

    let reply = call(&handle, "identity.session.logout", json!({}));
    let err = error(&reply, "服务端假撤销的登出", "DEPENDENCY_UNAVAILABLE");
    let msg = err["message"].as_str().unwrap_or_default();
    assert!(msg.contains("/api/me"), "要说清是怎么发现的：{reply}");
    assert!(
        !reply.to_string().contains("revoked"),
        "不许把这次失败投影成已撤销：{reply}"
    );
    // 撤销没生效时**不清本地**：抹掉一张仍然生效的凭据只会让人失去重试的机会。
    assert_eq!(
        handle.state::<AppState>().auth_token().as_deref(),
        Some("tk-1"),
        "撤销没确认，本地凭据必须还在"
    );
}

/// 服务端压根不响应 ⇒ 报错（OFFLINE，可重试），且不落 signed_out 投影。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unreachable_logout_reports_offline_and_keeps_the_session() {
    let (app, handle) = boot();
    let state = AppState::new("http://127.0.0.1:1");
    state.set_auth_token("tk-dead-port");
    app.manage(state);

    let reply = call(&handle, "identity.session.logout", json!({}));
    let err = error(&reply, "登不出去的服务端", "OFFLINE");
    assert!(err["retryable"].as_bool().unwrap_or(false), "{reply}");
    assert_eq!(
        handle.state::<AppState>().auth_token().as_deref(),
        Some("tk-dead-port"),
        "没撤销成功就不许偷偷清本地"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn switch_account_revokes_before_wiping_the_local_session() {
    let (http, _quic, _platform) = stack().await;
    let (_app, handle, token) = signed_in(&http, "s_bob");

    let reply = call(&handle, "identity.qr_login.switch_account", json!({}));
    let data = ok_data(&reply, "切换账号");
    assert_eq!(data["revoked"], json!(true), "{reply}");
    assert_eq!(data["server_revoked"], json!(true), "{reply}");
    assert!(
        !token_works(&http, &token),
        "切换账号后旧凭据必须已经在服务端作废（否则旧账号还能被这台设备接着用）"
    );
}

/// 同一条判据也要钉在 switch_account 上：旧实现这里连一次服务端调用都没有，
/// `emit(signed_out)` 之后直接回 `{"revoked": true}`。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn switch_account_refuses_when_the_server_did_not_revoke() {
    let base = lying_logout_server();
    let (app, handle) = boot();
    let state = AppState::new(&base);
    state.set_auth_token("tk-1");
    app.manage(state);

    let reply = call(&handle, "identity.qr_login.switch_account", json!({}));
    error(&reply, "服务端没撤销的切换账号", "DEPENDENCY_UNAVAILABLE");
    assert_eq!(
        handle.state::<AppState>().auth_token().as_deref(),
        Some("tk-1"),
        "撤销没确证 ⇒ 本地会话原样保留"
    );
    assert!(
        !reply.to_string().contains("revoked"),
        "不许把这次失败投影成已撤销：{reply}"
    );
}

/// 没有会话时退出：报错，不许回一个 `revoked: true` 的空成功。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn logging_out_without_a_session_is_not_a_success() {
    let (app, handle) = boot();
    app.manage(AppState::new("http://127.0.0.1:1"));
    let reply = call(&handle, "identity.session.logout", json!({}));
    error(&reply, "没有会话就点退出", "UNAUTHENTICATED");
}

/// 收了 `/api/logout` 但从不删会话的服务端（台账第 2 条的形状只能自建）。
fn lying_logout_server() -> String {
    spawn_server(
        axum::Router::new()
            .route(
                "/api/logout",
                axum::routing::post(|| async { r#"{"ok":true}"# }),
            )
            .route(
                "/api/me",
                axum::routing::get(|headers: axum::http::HeaderMap| async move {
                    // 只认 Bearer 在不在，从不校验它是否已被撤销：这就是"报了成功其实没做"。
                    let bearer = headers
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    if bearer == "Bearer tk-1" {
                        return (
                            axum::http::StatusCode::OK,
                            r#"{"user_id":"u-alice","name":"alice"}"#.to_string(),
                        );
                    }
                    (
                        axum::http::StatusCode::UNAUTHORIZED,
                        r#"{"error":"expired"}"#.to_string(),
                    )
                }),
            ),
    )
}
