//! Zone C · C4：命令面 → HTTP 的映射要能被钉住。
//!
//! 卡片写的是"CLI 能用 `wxlike-cmd` 调到"。真起一台服务端跑一遍是验收，
//! 但那条路依赖端口与一个活着的 `wxlike-server`，CI 里不可重复。这里用
//! `transport.rs` 同一手法：假服务器只把**收到的请求行**交回来——
//! 请求行对了，剩下的路（axum 路由 → 平台鉴权）已由
//! `crates/platform/tests/communities.rs` 的 `c4_*` 用例覆盖。

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::sync::mpsc;

use serde_json::json;
use wxlike_client::execute::Executor;
use wxlike_client::Http;
use wxlike_cmd::policy::Policy;

fn authed(base: String) -> Http {
    let mut http = Http::new(base);
    http.token = Some("t".to_string());
    http
}

/// 应答 200 `{}` 的服务器，把每个连接的**请求行**送进 channel。
fn recorder() -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // 一个测试里要跑多条命令，循环接：每个连接读到头、回一包、关掉。
        for stream in listener.incoming().flatten() {
            let tx = tx.clone();
            let mut stream = stream;
            let sep = [13u8, 10, 13, 10];
            let mut buf: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.windows(4).any(|w| w == sep) {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let text = String::from_utf8_lossy(&buf).to_string();
            let line = text.lines().next().unwrap_or_default().to_string();
            if tx.send(line).is_err() {
                return;
            }
            let crlf = String::from_utf8(vec![13, 10]).expect("crlf");
            let resp = format!(
                "HTTP/1.1 200 OK{crlf}Content-Type: application/json{crlf}Content-Length: 2{crlf}Connection: close{crlf}{crlf}{{}}"
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    (format!("http://{addr}"), rx)
}

fn next_line(rx: &mpsc::Receiver<String>) -> String {
    rx.recv_timeout(std::time::Duration::from_secs(5))
        .expect("服务器该收到一条请求")
}

/// `community.threads` → `GET /api/community/{id}/threads?limit=`
#[test]
fn community_threads_reaches_the_rollup_route() {
    let (base, rx) = recorder();
    let exec = Executor::new(authed(base), Policy::new(["*"], false, false));
    exec.run("community.threads", &json!({"community": "c0ffee01"}))
        .expect("命令该走到网络");
    assert_eq!(
        next_line(&rx),
        "GET /api/community/c0ffee01/threads?limit=50 HTTP/1.1"
    );

    // limit 要真的传下去，别在服务端悄悄用默认值
    exec.run(
        "community.threads",
        &json!({"community": "c0ffee01", "limit": 7}),
    )
    .unwrap();
    assert_eq!(
        next_line(&rx),
        "GET /api/community/c0ffee01/threads?limit=7 HTTP/1.1"
    );
}

/// `channel.roles` → `GET /api/channel/{conv}/roles`，且受会话白名单管。
#[test]
fn channel_roles_reaches_the_role_route_and_honours_the_conv_allowlist() {
    let (base, rx) = recorder();
    let exec = Executor::new(authed(base), Policy::new(["ch-allow*"], false, false));

    // 白名单里的频道：走到网络，打到 Zone C 的新路径
    exec.run("channel.roles", &json!({"conv": "ch-allow1"}))
        .expect("命令该走到网络");
    assert_eq!(next_line(&rx), "GET /api/channel/ch-allow1/roles HTTP/1.1");

    // 白名单外：**权限先于网络**，一个请求都不该发出去
    let err = exec
        .run("channel.roles", &json!({"conv": "other-channel"}))
        .expect_err("白名单外必须被拒");
    assert!(err.contains("not allowed"), "{err}");
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(300))
            .is_err(),
        "被拒的命令不该产生任何请求"
    );
}
