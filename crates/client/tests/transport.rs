//! 传输层的契约测试：用一个手写的假服务器（`TcpListener` + 固定响应）验证
//! **状态码、错误消息、鉴权头**这三件事。
//!
//! 为什么不用 mock 库：这里要验证的正是"ureq 的错误被翻译成什么"。假服务器
//! 是最接近真实的东西，而且零依赖、零端口冲突（绑 0 端口）。

use std::io::{Read as _, Write as _};
use std::net::TcpListener;

use wxlike_client::{direct_conv_id, Http};

/// 起一个只应答一次的服务器，返回 (base_url, 收到的请求头)。
fn one_shot(
    status_line: &'static str,
    body: &'static str,
) -> (String, std::sync::mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((mut sock, _)) = listener.accept() {
            // 必须读**完整**请求再回：ureq 会把头与 body 分两次发，
            // 只读一次会拿到半截请求，回包后客户端报 "connection aborted"。
            // 用字节级分隔符（13,10,13,10）而不是字符串字面量，避免转义歧义。
            let sep = [13u8, 10, 13, 10];
            let mut buf: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                match sock.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(pos) = buf.windows(4).position(|w| w == sep) {
                            let head = String::from_utf8_lossy(&buf[..pos]).to_lowercase();
                            let need = head
                                .lines()
                                .find_map(|l| {
                                    l.strip_prefix("content-length:")
                                        .and_then(|v| v.trim().parse::<usize>().ok())
                                })
                                .unwrap_or(0);
                            if buf.len() >= pos + 4 + need {
                                break;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&buf).to_string());
            let resp = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes());
        }
    });
    (format!("http://{addr}"), rx)
}

#[test]
fn success_returns_the_parsed_body() {
    let (base, _rx) = one_shot("200 OK", r#"{"user_id":"u1","token":"t1"}"#);
    let mut http = Http::new(base);
    let reply = http.register("alice", "pw").expect("register");
    assert_eq!(reply["user_id"], "u1");
    // token 自动记住：后续请求才会带鉴权头
    assert_eq!(http.token.as_deref(), Some("t1"));
}

#[test]
fn server_error_message_is_surfaced_with_status() {
    // 4xx 的 body 里带 error 时，错误串必须是"人能读的那句 + 状态码"
    let (base, _rx) = one_shot("400 Bad Request", r#"{"error":"name taken"}"#);
    let mut http = Http::new(base);
    let err = http.register("alice", "pw").expect_err("should fail");
    assert!(err.contains("name taken"), "{err}");
    assert!(err.contains("400"), "{err}");
    // 失败不该把 token 设成空串
    assert!(http.token.is_none());
}

#[test]
fn bearer_header_is_sent_once_a_token_exists() {
    let (base, rx) = one_shot("200 OK", r#"{"user_id":"u1"}"#);
    let mut http = Http::new(base);
    http.token = Some("secret-token".into());
    http.me().expect("me");
    let request = rx.recv().expect("captured");
    assert!(
        request
            .to_lowercase()
            .contains("authorization: bearer secret-token"),
        "请求里没有鉴权头：{request}"
    );
}

#[test]
fn unread_and_ack_hit_the_mailbox_endpoints() {
    let (base, rx) = one_shot("200 OK", r#"{"unread":3,"head_seq":3,"read_seq":0}"#);
    let http = Http::new(base);
    let u = http.conv_unread("d-a~b").expect("unread");
    assert_eq!(u["unread"], 3);
    let request = rx.recv().expect("captured");
    assert!(
        request.starts_with("GET /api/conv/d-a~b/unread"),
        "路径不对：{request}"
    );
}

/// 会话 id 与顺序无关：谁在前谁在后都得到同一个 id。
#[test]
fn direct_conv_id_is_order_independent() {
    assert_eq!(
        direct_conv_id("bob", "alice"),
        direct_conv_id("alice", "bob")
    );
    assert_eq!(direct_conv_id("alice", "bob"), "d-alice~bob");
}

/// 命令标记必须真的发出去：服务端靠它落审计。
#[test]
fn command_header_is_sent_with_the_request() {
    let (base, rx) = one_shot("200 OK", r#"{"unread":0,"head_seq":0,"read_seq":0}"#);
    let http = Http::new(base).with_command("conv.unread");
    http.conv_unread("d-a~b").expect("unread");
    let request = rx.recv().expect("captured").to_lowercase();
    assert!(
        request.contains("x-wxlike-command: conv.unread"),
        "请求里没有命令标记：{request}"
    );
}

/// R5 #60：2xx 但响应体不是合法 JSON（空体/代理错误页）必须报错，
/// 不能把合成错误体当成功数据返回。修复前 expect_json 会 Ok 一个
/// {"error":"HTTP 200"}，调用方误以为拿到了业务结果。
#[test]
fn success_with_non_json_body_is_an_error() {
    let (base, _rx) = one_shot("200 OK", "this is not json");
    let http = Http::new(base);
    let err = http
        .expect_json("GET", "/api/me", None)
        .expect_err("2xx 非 JSON 体必须报错");
    assert!(err.contains("JSON"), "{err}");
    assert!(err.contains("200"), "{err}");
}

/// 204 No Content（空体）是合法成功：登出/切换账号这类端点靠它表达
/// "已完成、无返回体"，不能因为体不是 JSON 就报错（R5 #60 回归）。
#[test]
fn no_content_204_is_a_success() {
    let (base, _rx) = one_shot("204 No Content", "");
    let http = Http::new(base);
    let value = http
        .expect_json("POST", "/api/logout", None)
        .expect("204 空体必须算成功");
    assert!(value.is_null(), "空体应返回 Null，实际：{value}");
}

/// 非 2xx 且错误体非 JSON 时，仍按状态码分流给可读错误（兜底保留）。
#[test]
fn error_status_with_non_json_body_still_reports_status() {
    let (base, _rx) = one_shot("500 Internal Server Error", "<html>boom</html>");
    let http = Http::new(base);
    let err = http
        .expect_json("GET", "/api/me", None)
        .expect_err("5xx 必须报错");
    assert!(err.contains("500"), "{err}");
}
