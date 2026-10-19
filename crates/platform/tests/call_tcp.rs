//! **真 socket** 冒烟：axum 起在 127.0.0.1 的真实 TCP 端口上，用裸 HTTP/1.1
//! 客户端驱动通话控制面。
//!
//! 为什么不能只靠 `tower::oneshot`：那套走的是内存中的 `Request`，**不经过
//! 真正的连接与响应写出**。之前 `POST /start` 空 body 被判 400、以及 204 被
//! 当成空 body 解码失败，都是"线上行为"层面的问题，只有在真连接上才看得见。
//!
//! 这里刻意手写 HTTP 报文（不引第三方客户端）：Tauri 桥用的是 ureq，
//! 断言的正是它依赖的两件事——状态码和**没有 body 的 204**。

mod common;

use std::time::Duration;

use common::{platform, req};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use wxlike_platform::Platform;

/// 起一个真监听，返回 `(base_addr, join_handle)`。
async fn serve() -> std::net::SocketAddr {
    let app = Platform::router(platform());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    // 等 accept 循环真正就绪（bind 之后 serve 之前有一小段窗口）
    for _ in 0..50 {
        if TcpStream::connect(addr).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    addr
}

/// 手写一条 HTTP/1.1 请求，返回 `(status, body_text)`。
async fn raw(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    body: Option<&str>,
    token: Option<&str>,
) -> (u16, String) {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let payload = body.unwrap_or("");
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n",
        payload.len()
    );
    if let Some(t) = token {
        head.push_str(&format!("Authorization: Bearer {t}\r\n"));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes()).await.unwrap();
    if !payload.is_empty() {
        s.write_all(payload.as_bytes()).await.unwrap();
    }
    s.flush().await.unwrap();

    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).to_string();
    let status: u16 = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

#[tokio::test]
async fn call_flow_over_a_real_socket() {
    let addr = serve().await;

    // 两个用户（注册接口返回 token）
    let (status, body) = raw(
        addr,
        "POST",
        "/api/register",
        Some(r#"{"name":"tcp-a","password":"pw"}"#),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let a: Value = serde_json::from_str(&body).unwrap();
    let (a_id, a_tok) = (
        a["user_id"].as_str().unwrap().to_string(),
        a["token"].as_str().unwrap().to_string(),
    );

    let (status, body) = raw(
        addr,
        "POST",
        "/api/register",
        Some(r#"{"name":"tcp-b","password":"pw"}"#),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let b: Value = serde_json::from_str(&body).unwrap();
    let b_id = b["user_id"].as_str().unwrap().to_string();

    let conv = format!("d-{a_id}~{b_id}");

    // 开房：**空 body**，真连接上必须仍是 204（不是 400）
    let (status, body) = raw(
        addr,
        "POST",
        &format!("/api/call/{conv}/start"),
        None,
        Some(&a_tok),
    )
    .await;
    assert_eq!(status, 204, "空 body 开房在真连接上也要 204: {body}");
    assert!(body.is_empty(), "204 不能带 body");

    // 状态：200 + JSON
    let (status, body) = raw(
        addr,
        "GET",
        &format!("/api/call/{conv}"),
        None,
        Some(&a_tok),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let st: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(st["live"], json!(true));
    assert_eq!(st["participants"].as_array().unwrap().len(), 1);

    // 重复开房：409，且错误体是 JSON（ureq 会读 error 字段）
    let (status, body) = raw(
        addr,
        "POST",
        &format!("/api/call/{conv}/start"),
        None,
        Some(&a_tok),
    )
    .await;
    assert_eq!(status, 409, "{body}");
    let err: Value = serde_json::from_str(&body).unwrap();
    assert!(err["error"].is_string(), "错误体要有 error 字段: {body}");

    // 未登录：401
    let (status, _) = raw(addr, "GET", &format!("/api/call/{conv}"), None, None).await;
    assert_eq!(status, 401);

    // 收尾：挂断后房间消失
    let (status, _) = raw(
        addr,
        "POST",
        &format!("/api/call/{conv}/leave"),
        None,
        Some(&a_tok),
    )
    .await;
    assert_eq!(status, 204);
    let (_, body) = raw(
        addr,
        "GET",
        &format!("/api/call/{conv}"),
        None,
        Some(&a_tok),
    )
    .await;
    let st: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(st["live"], json!(false));

    let _ = req; // 让 common 的 helper 不被当成未使用（本文件不用 oneshot）
}
