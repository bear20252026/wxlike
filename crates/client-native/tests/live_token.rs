//! G01-03 验收：token 的**活更新**与传输层截止时间。
//!
//! 1. `set_token` 在传输实例被 `Arc` 共享（塞进 `Ports` 之后）仍立即生效：
//!    曾经 `NativeCore::set_token` 走 `Arc::get_mut`，拿不到唯一引用就静默
//!    放弃——登录成功、请求继续带旧 token，直到进程重启。
//! 2. 已过期的 deadline 在**发请求之前**就折成 `Timeout`，不碰网络。

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use wxlike_client_core::ports::{Transport as _, TransportError};
use wxlike_client_native::transport::HttpTransport;

/// 记录到的 `Authorization` 头（按请求顺序）。
type HeaderLog = Arc<Mutex<Vec<Option<String>>>>;

/// 本机回环测试不走路上的代理：开发机的 `HTTP_PROXY` 会让 ureq 把
/// 127.0.0.1 的请求发给代理，连接直接断掉（os error 0）。
fn drop_proxy_env() {
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        std::env::remove_var(key);
    }
}

/// 起一个把每个请求的 Authorization 头记下来的本地服务器。
/// 返回 (base_url, 日志)。每个连接只服务一个请求（Connection: close）。
fn recording_server() -> (String, HeaderLog) {
    drop_proxy_env();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let log: HeaderLog = Arc::new(Mutex::new(Vec::new()));
    let writer = log.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut buffer = [0u8; 4096];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let text = String::from_utf8_lossy(&buffer[..read]).to_string();
            let auth = text.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("authorization")
                    .then(|| value.trim().to_string())
            });
            writer.lock().unwrap().push(auth);
            let body = "{\"user_id\":\"alice\"}";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (format!("http://{addr}"), log)
}

#[tokio::test]
async fn set_token_updates_requests_live_through_shared_handles() {
    let (base, log) = recording_server();
    let transport = HttpTransport::new(&base);
    // 模拟真实装配：Ports 持有另一个 Arc，`Arc::get_mut` 必然失败——
    // 修复前 `set_token` 在这条路径上是静默 no-op。
    let shared = Arc::new(transport);
    let requests = shared.clone();

    assert_eq!(shared.token(), None, "初始无 token");
    shared.set_token(Some("t-first".to_string()));
    let response = requests
        .send(wxlike_client_core::ports::TransportRequest::get("/api/me"))
        .await
        .expect("first request");
    assert!(response.is_success());
    assert_eq!(
        log.lock().unwrap().as_slice(),
        [Some("Bearer t-first".to_string())],
        "第一个请求必须带新 token"
    );

    // 换 token：不重建传输、不重开会话，后续请求立即生效
    shared.set_token(Some("t-refreshed".to_string()));
    requests
        .send(wxlike_client_core::ports::TransportRequest::get("/api/me"))
        .await
        .expect("second request");
    assert_eq!(
        log.lock().unwrap().as_slice(),
        [
            Some("Bearer t-first".to_string()),
            Some("Bearer t-refreshed".to_string())
        ],
        "换 token 必须对后续请求立即生效"
    );

    // 登出：清 token 之后不再带 Authorization
    shared.set_token(None);
    requests
        .send(wxlike_client_core::ports::TransportRequest::get("/api/me"))
        .await
        .expect("third request");
    assert_eq!(
        log.lock().unwrap()[2],
        None,
        "清 token 后不该再带 Authorization"
    );
}

#[tokio::test]
async fn expired_deadline_fails_before_touching_the_network() {
    let (base, log) = recording_server();
    let transport = HttpTransport::new(&base);
    let request = wxlike_client_core::ports::TransportRequest::get("/api/me")
        .with_deadline(wxlike_types::Deadline::at(1)); // 1970 年就过期
    let error = transport
        .send(request)
        .await
        .expect_err("过期 deadline 必须失败");
    assert!(
        matches!(error, TransportError::Timeout),
        "应当是 Timeout， got {error:?}"
    );
    assert!(log.lock().unwrap().is_empty(), "过期的请求不该发到网络");
}

/// 端到端（不依赖网络服务端，只依赖本地回环）：`NativeCore::open` 之后
/// `set_token` 必须影响后续 dispatch——修复前 `Arc::get_mut` 失败，
/// 这里第二条断言会拿到 `None`。
#[tokio::test]
async fn native_core_set_token_after_open_reaches_the_wire() {
    let (base, log) = recording_server();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = wxlike_client_native::NativeConfig::new(&base, dir.path().to_path_buf());
    let core = wxlike_client_native::NativeCore::open(config)
        .await
        .expect("open core");

    core.set_token(Some("t-after-open".to_string()));
    let request = wxlike_types::ActionRequest::new(
        wxlike_types::ActionId::parse("me.info").expect("action"),
        serde_json::json!({}),
        wxlike_types::TraceId::derive("live-token-test"),
    );
    let result = core.dispatch(request).await;
    assert!(result.is_ok(), "me.info 应当成功：{result:?}");
    core.shutdown();
    assert_eq!(
        log.lock().unwrap().as_slice(),
        [Some("Bearer t-after-open".to_string())],
        "open 之后 set_token 的值必须上线路"
    );
}
