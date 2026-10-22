//! 限流的**来源标识**（issue #19 / A17）。
//!
//! 以前 `client_key` 无条件读 `x-forwarded-for` / `x-real-ip` / `x-client-id`。
//! 这三个头客户端都能自己填，于是"换一个头值 = 换一个限流桶"——注册与登录
//! 失败计数形同虚设（换号 + 换头可以无限撞库）。
//!
//! 现在的规则：
//! - `trust_proxy == false`（默认）：**只看连接对端 IP**，转发头完全不读；
//! - `trust_proxy == true`：部署在受信反向代理后面，才采信 `x-forwarded-for` 首段。
//!
//! 这三条用例把两种部署分别钉住，再加"恢复入口失败也计数"。

mod common;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};
use std::net::SocketAddr;
use tower::ServiceExt as _;

use common::{app, call, platform};
use wxlike_platform::{Platform, PlatformOptions};

/// 带自定义头 + 可选 ConnectInfo 的请求。
///
/// `ConnectInfo` 必须**手动塞进 extensions**：`Router::oneshot` 不会自动带，
// 而 `PeerAddr` 正是从 extensions 里取它的。
fn req_with_peer(
    method: &str,
    path: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
    peer: Option<SocketAddr>,
) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let mut req = b
        .body(match body {
            Some(v) => Body::from(v.to_string()),
            None => Body::empty(),
        })
        .unwrap();
    if let Some(peer) = peer {
        req.extensions_mut().insert(ConnectInfo(peer));
    }
    req
}

/// 带 ConnectInfo 的调用（`common::call` 不带扩展）。
async fn call_with_peer(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

fn opts(trust_proxy: bool) -> PlatformOptions {
    PlatformOptions {
        session_ttl_ms: 60_000,
        pbkdf2_rounds: 1_000,
        trust_proxy,
        ..Default::default()
    }
}

/// 从 "7 + 3 = ?" 里算出答案（测试专用：模拟用户读题）。
fn solve(question: &str) -> String {
    let cleaned: String = question
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '+')
        .collect();
    let mut parts = cleaned.split('+');
    let a: u32 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let b: u32 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    (a + b).to_string()
}

/// **直连部署**：改 `x-forwarded-for` 不能换到新的限流桶。
///
/// 同一个真实对端 IP 把登录失败攒到阈值后，即使每次都换一个伪造的转发头，
/// 第 5 次也必须先过人机校验——否则"换头撞库"就成了。
#[tokio::test]
async fn a_spoofed_forwarded_header_cannot_reset_the_counter() {
    let state = platform();
    let router = app(state.clone());
    let peer: SocketAddr = "203.0.113.9:5000".parse().unwrap();

    let (st, body) = call_with_peer(
        &router,
        req_with_peer(
            "POST",
            "/api/register",
            Some(json!({"name": "alice", "password": "pw"})),
            &[],
            Some(peer),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "注册：{body}");

    // 前 4 次错误密码，每次换一个伪造来源头
    for i in 0..4 {
        let spoofed = format!("10.0.0.{i}");
        let (st, _) = call_with_peer(
            &router,
            req_with_peer(
                "POST",
                "/api/login",
                Some(json!({"name": "alice", "password": "bad"})),
                &[("x-forwarded-for", spoofed.as_str())],
                Some(peer),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "第 {i} 次错误密码");
    }

    // 第 5 次：密码正确也必须先过人机校验（阈值 login_fail_challenge_at = 5）
    let (st, body) = call_with_peer(
        &router,
        req_with_peer(
            "POST",
            "/api/login",
            Some(json!({"name": "alice", "password": "pw"})),
            &[("x-forwarded-for", "10.9.9.9")],
            Some(peer),
        ),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::PRECONDITION_REQUIRED,
        "换伪造来源头不该把失败计数洗掉：{body}"
    );
}

/// **代理部署**：`trust_proxy = true` 时采信 `x-forwarded-for` 首段。
///
/// 用**注册**来分桶最干净：注册只按来源计数，不掺账号键（登录会同时按
/// "账号 + 来源" 两个键记，账号键先到阈值就分不清是谁的功劳）。
/// 同一个真实对端 + 两个不同转发头 = 两个桶：一个攒到阈值，另一个还没事。
#[tokio::test]
async fn behind_a_trusted_proxy_the_forwarded_header_is_the_bucket() {
    let state = Platform::in_memory_with(opts(true)).unwrap().shared();
    let router = app(state.clone());
    let peer: SocketAddr = "10.0.0.1:5000".parse().unwrap();

    let mut challenged = false;
    for i in 0..60 {
        // 前 55 次死磕同一个转发头（阈值 register_challenge_at = 50），
        // 之后换一个——换桶这一下才是这个用例要证的事。
        let header_value = if i < 55 {
            "198.51.100.7".to_string()
        } else {
            "198.51.100.250".to_string()
        };
        let (st, body) = call_with_peer(
            &router,
            req_with_peer(
                "POST",
                "/api/register",
                Some(json!({"name": format!("proxy{i}"), "password": "pw"})),
                &[("x-forwarded-for", header_value.as_str())],
                Some(peer),
            ),
        )
        .await;
        if st == StatusCode::PRECONDITION_REQUIRED {
            challenged = true;
            assert!(i < 55, "第 {i} 次就该到阈值了，怎么才触发？");
            break;
        }
        // 换桶之后必须恢复正常：这正是"代理部署下按转发头分桶"的证据
        if i >= 55 {
            assert_eq!(st, StatusCode::OK, "代理部署下不同转发头是不同桶：{body}");
            break;
        }
        assert_eq!(st, StatusCode::OK, "第 {i} 次注册：{body}");
    }
    assert!(challenged, "同一个转发头攒到阈值必须要求人机校验");
}

/// 恢复入口：**失败也要计数**。
///
/// 原来是 `?` 提前返回，`risk_record` 只在成功后调用——于是无效恢复码可以
/// 无限猜，而真的重置成功反而给这个账号攒了一次"失败"。
/// 现在失败也记：第 5 次起先要过人机校验（阈值 login_fail_challenge_at = 5）。
#[tokio::test]
async fn wrong_recovery_codes_accumulate_failures() {
    let state = platform();
    let router = app(state.clone());

    let (st, _) = call_with_peer(
        &router,
        req_with_peer(
            "POST",
            "/api/register",
            Some(json!({"name": "carol", "password": "pw"})),
            &[],
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // 前 4 次错误恢复码：普通的"码不对"
    for i in 0..4 {
        let (st, body) = call(
            &router,
            common::req(
                "POST",
                "/api/recovery/reset",
                Some(json!({
                    "name": "carol",
                    "recovery_code": "000000",
                    "new_password": "whatever",
                })),
                None,
            ),
        )
        .await;
        assert!(
            st == StatusCode::UNAUTHORIZED || st == StatusCode::FORBIDDEN,
            "第 {i} 次错误恢复码应该是码不对而不是风控：{st} {body}"
        );
    }

    // 第 5 次：失败计数攒够了，先要过人机校验
    let (st, _) = call(
        &router,
        common::req(
            "POST",
            "/api/recovery/reset",
            Some(json!({
                "name": "carol",
                "recovery_code": "000000",
                "new_password": "whatever",
            })),
            None,
        ),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::PRECONDITION_REQUIRED,
        "错误恢复码必须积累失败计数"
    );

    // 过人机校验之后这次仍然因为码错而被拒（不是被风控挡住）
    let (_, challenge) = call(&router, common::req("POST", "/api/captcha", None, None)).await;
    let id = challenge["challenge_id"].as_str().unwrap().to_string();
    let answer = solve(challenge["question"].as_str().unwrap());
    let (st, _) = call(
        &router,
        common::req(
            "POST",
            "/api/recovery/reset",
            Some(json!({
                "name": "carol",
                "recovery_code": "000000",
                "new_password": "whatever",
                "captcha_id": id,
                "captcha_answer": answer,
            })),
            None,
        ),
    )
    .await;
    assert!(
        st == StatusCode::UNAUTHORIZED || st == StatusCode::FORBIDDEN,
        "过了风控但码还是错的：{st}"
    );
}
