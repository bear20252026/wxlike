//! 端到端：真的跑二进制、真的连一个本地 HTTP 桩。
//!
//! 单测只能证明"函数对"，证明不了"退出码与 stdout 形状对"——而脚本只认这两个。
//! 这里刻意不 mock 网络库：桩服务器就是一个 20 行的 TcpListener，够用且没有依赖。

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_wxlike-cli"))
}

/// 起一个"你说什么我都回这个 JSON"的桩，返回 base url。
fn stub_server(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for mut sock in listener.incoming().flatten() {
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes());
        }
    });
    format!("http://{addr}")
}

fn run(args: &[&str]) -> (i32, String, String) {
    let out = bin().args(args).output().expect("spawn");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn list_lists_every_command_without_a_server() {
    let (code, stdout, _) = run(&["list"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("msg.send"), "{stdout}");
    assert_eq!(stdout.lines().count(), 26, "每条命令一行：{stdout}");
}

#[test]
fn help_is_generated_from_the_catalog() {
    let (code, stdout, _) = run(&["help", "video"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("wxlike video job.get"), "{stdout}");
    assert!(stdout.contains("--param job_id=<string> 必填"), "{stdout}");
}

#[test]
fn usage_errors_exit_with_2() {
    for bad in [
        vec!["nope", "nada"],
        vec!["conv", "unread", "--param", "cnv=g-1"],
        vec!["--nope"],
        vec!["run"],
    ] {
        let (code, _, stderr) = run(&bad);
        assert_eq!(code, 2, "{bad:?} 应当是用法的错: {stderr}");
        assert!(!stderr.is_empty(), "用法错误必须有提示");
    }
}

/// 没 token 就执行 = 配置错误（2），而不是网络错误（6）。
#[test]
fn missing_token_is_a_config_error_not_a_network_error() {
    let (code, _, stderr) = run(&["me", "info"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("token"), "{stderr}");
}

#[test]
fn json_output_is_one_line_and_parseable() {
    let base = stub_server(r#"{"user_id":"u1","name":"alice","devices":[]}"#);
    let (code, stdout, stderr) = run(&["--server", &base, "--token", "t", "--json", "me", "info"]);
    assert_eq!(code, 0, "{stderr}");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "机器输出必须单行：{stdout}");
    let v: serde_json::Value = serde_json::from_str(lines[0]).expect("json");
    assert_eq!(v["user_id"], "u1", "{v}");
}

/// 权限拒绝发生在发请求之前：给一个连不上的地址，退出码必须是 3 而不是 6。
#[test]
fn denied_call_exits_with_3_before_any_request() {
    let (code, _, stderr) = run(&[
        "--server",
        "http://127.0.0.1:1",
        "--token",
        "t",
        "conv",
        "unread",
        "--param",
        "conv=g-1",
    ]);
    assert_eq!(code, 3, "{stderr}");
    assert!(stderr.contains("not allowed"), "{stderr}");
}

/// `--read-only` 压过环境变量：写命令被拒，退出码 3。
#[test]
fn read_only_flag_wins_over_env_grants() {
    let out = bin()
        .env("WXLIKE_AGENT_ALLOW_CONV", "g-1")
        .env("WXLIKE_AGENT_ALLOW_WRITE", "1")
        .env("WXLIKE_TOKEN", "t")
        .env("WXLIKE_SERVER", "http://127.0.0.1:1")
        .args([
            "--read-only",
            "msg",
            "send",
            "--param",
            "conv=g-1",
            "--param",
            "text=hi",
            "--param",
            "idempotency_key=k",
        ])
        .output()
        .expect("spawn");
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
