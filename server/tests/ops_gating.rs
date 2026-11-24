//! 运维面收紧（审计第 5 轮）的服务端验收：
//! * `/metrics` 只对 admin token 开放，没配 `WXLIKE_ADMIN_TOKEN` 时整个
//!   路径当不存在（404）；
//! * `/endpoints`（QUIC 拓扑）要登录；
//! * `/api/presence/{user}`（活跃画像）要登录。
//!
//! 三个面都是"匿名可达 = 泄露"：指标给出容量与积压、endpoints 给出内网
//! 地址、presence 给出谁在线。走真 stack（平台 + hub + HTTP 同进程）。

use std::time::Duration;

use wxlike_server::{start_stack, StackOptions};

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .proxy(None)
        .build()
        .into()
}

fn get(http: &str, path: &str, headers: &[(&str, &str)]) -> (u16, String) {
    let mut req = agent().get(&format!("{http}{path}"));
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    let mut resp = req.call().unwrap_or_else(|e| panic!("GET {path}: {e}"));
    let status = resp.status().as_u16();
    let body = resp.body_mut().read_to_string().unwrap_or_default();
    (status, body)
}

fn register(http: &str, name: &str) -> (String, String) {
    let mut resp = agent()
        .post(&format!("{http}/api/register"))
        .send_json(serde_json::json!({"name": name, "password": "pw"}))
        .unwrap_or_else(|e| panic!("register {name}: {e}"));
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.body_mut().read_json().unwrap();
    let user_id = body["user_id"].as_str().unwrap().to_string();
    let token = body["token"].as_str().unwrap().to_string();
    (user_id, token)
}

fn post(
    http: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: serde_json::Value,
) -> (u16, String) {
    let mut req = agent().post(&format!("{http}{path}"));
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    let mut resp = req
        .send_json(body)
        .unwrap_or_else(|e| panic!("POST {path}: {e}"));
    let status = resp.status().as_u16();
    let body = resp.body_mut().read_to_string().unwrap_or_default();
    (status, body)
}

/// 场景串行跑在一个测试里：admin token 是进程级环境变量，并行用例互相
/// 踩（与 `turn_credentials.rs` 同一约定）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn metrics_endpoints_and_presence_are_gated() {
    // ---- 场景 1：没配 admin token → /metrics 恒 404（"没配就是关"） ----
    std::env::remove_var("WXLIKE_ADMIN_TOKEN");
    let stack = start_stack(StackOptions::default()).await.unwrap();
    let http = format!("http://{}", stack.http_addr);
    let (_ops_user, token) = register(&http, "ops_user");

    let (status, body) = get(&http, "/metrics", &[]);
    assert_eq!(status, 404, "unauthenticated metrics: {body}");
    let (status, _) = get(&http, "/metrics", &[("x-admin-token", "anything")]);
    assert_eq!(
        status, 404,
        "no configured admin token = endpoint stays off"
    );

    // ---- /endpoints：登录可见 ----
    let (status, body) = get(&http, "/endpoints", &[]);
    assert_eq!(status, 401, "anonymous topology probe: {body}");
    let bearer = format!("Bearer {token}");
    let (status, body) = get(&http, "/endpoints", &[("Authorization", bearer.as_str())]);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("quic"), "{body}");

    // ---- /api/presence/{user}：登录可见（隐私预言机） ----
    let (status, body) = get(&http, "/api/presence/ops_user", &[]);
    assert_eq!(status, 401, "anonymous presence probe: {body}");
    let (status, body) = get(
        &http,
        "/api/presence/ops_user",
        &[("Authorization", bearer.as_str())],
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"online\""), "{body}");

    stack.shutdown(Duration::from_secs(2)).await;

    // ---- 场景 2：配了 admin token → 对 token 开，错 token 404 ----
    std::env::set_var("WXLIKE_ADMIN_TOKEN", "ops-secret");
    let stack2 = start_stack(StackOptions::default()).await.unwrap();
    let http2 = format!("http://{}", stack2.http_addr);
    let (status, _) = get(&http2, "/metrics", &[]);
    assert_eq!(status, 404, "no header");
    let (status, _) = get(&http2, "/metrics", &[("x-admin-token", "wrong")]);
    assert_eq!(status, 404, "wrong token is indistinguishable from off");
    let (status, body) = get(&http2, "/metrics", &[("x-admin-token", "ops-secret")]);
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains("text/plain") || !body.is_empty(),
        "prometheus exposition: {body}"
    );

    stack2.shutdown(Duration::from_secs(2)).await;

    // ---- 场景 3：开发水龙头（opt-in）也必须带 admin token ----
    // 它会凭空造虚拟货币，所以"开了 faucet 但没配 token"是关而不是裸奔。
    std::env::set_var("WXLIKE_DEV_FAUCET", "1");
    let stack3 = start_stack(StackOptions::default()).await.unwrap();
    let http3 = format!("http://{}", stack3.http_addr);
    let (faucet_id, _token) = register(&http3, "faucetuser");
    let fund = serde_json::json!({"user_id": faucet_id, "amount": 5});

    let (status, body) = post(&http3, "/api/dev/fund", &[], fund.clone());
    assert_eq!(status, 403, "faucet without token: {body}");
    let (status, body) = post(
        &http3,
        "/api/dev/fund",
        &[("x-admin-token", "wrong")],
        fund.clone(),
    );
    assert_eq!(status, 403, "faucet with wrong token: {body}");

    let mut bad = fund.clone();
    bad["amount"] = serde_json::json!(0);
    let (status, body) = post(
        &http3,
        "/api/dev/fund",
        &[("x-admin-token", "ops-secret")],
        bad,
    );
    assert_eq!(status, 422, "amount out of range: {body}");

    let mut unknown = fund.clone();
    unknown["user_id"] = serde_json::json!("nosuchuser");
    let (status, body) = post(
        &http3,
        "/api/dev/fund",
        &[("x-admin-token", "ops-secret")],
        unknown,
    );
    assert_eq!(status, 422, "unknown account cannot be funded: {body}");

    let (status, body) = post(
        &http3,
        "/api/dev/fund",
        &[("x-admin-token", "ops-secret")],
        fund,
    );
    assert_eq!(status, 204, "faucet happy path: {body}");

    stack3.shutdown(Duration::from_secs(2)).await;

    // ---- 场景 4：开了 DEV_FAUCET 但没配 admin token → 水龙头仍然关着 ----
    std::env::remove_var("WXLIKE_ADMIN_TOKEN");
    let stack4 = start_stack(StackOptions::default()).await.unwrap();
    let http4 = format!("http://{}", stack4.http_addr);
    let (status, body) = post(
        &http4,
        "/api/dev/fund",
        &[("x-admin-token", "anything")],
        serde_json::json!({"user_id": "faucetuser", "amount": 5}),
    );
    assert_eq!(
        status, 404,
        "faucet must not exist without a configured admin token: {body}"
    );
    stack4.shutdown(Duration::from_secs(2)).await;

    std::env::remove_var("WXLIKE_DEV_FAUCET");
    std::env::remove_var("WXLIKE_ADMIN_TOKEN");
}
