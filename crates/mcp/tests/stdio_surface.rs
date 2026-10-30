//! 端到端：把 `wxlike-mcp` 当子进程跑，喂真 JSON-RPC，读真 stdout。
//!
//! 为什么不用进程内调用：MCP 客户端看到的就是**这个二进制**。
//! 一旦有人把提示信息 `println!` 到 stdout，协议就废了——只有跑真进程能发现。

use std::io::Write as _;
use std::process::{Command, Stdio};

use serde_json::Value;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_wxlike-mcp"))
}

/// 喂一行一帧，返回解析后的响应（跳过空行）。
fn session(lines: &[&str]) -> Vec<Value> {
    let mut child = bin()
        .args(["--server", "http://127.0.0.1:1", "--token", "t"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        for line in lines {
            writeln!(stdin, "{line}").expect("write");
        }
    }
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(), "进程异常退出");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("stdout 不是 JSON: {e}\n{l}")))
        .collect()
}

const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","clientInfo":{"name":"t","version":"0"}}}"#;

#[test]
fn handshake_then_list_then_call_on_stdio() {
    let frames = session(&[
        INIT,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"conv-unread","arguments":{"conv":"g-1"}}}"#,
    ]);
    // 通知不回包：3 帧请求 → 3 帧响应
    assert_eq!(frames.len(), 3, "{frames:?}");
    assert_eq!(frames[0]["result"]["protocolVersion"], "2025-06-18");

    let tools = frames[1]["result"]["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), 46, "42 条命令 + 4 个元工具");
    assert!(tools.iter().any(|t| t["name"] == "msg-send"), "缺 msg-send");

    // 没授权：业务被拒是工具错误，不是协议错误
    assert!(frames[2].get("error").is_none(), "{:?}", frames[2]);
    assert_eq!(frames[2]["result"]["isError"], true);
}

#[test]
fn unknown_argument_is_rejected_with_invalid_params() {
    let frames = session(&[
        INIT,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"conv-unread","arguments":{"conv":"g-1","cnv":"x"}}}"#,
    ]);
    assert_eq!(frames[1]["error"]["code"], -32602, "{:?}", frames[1]);
}

/// 诊断模式：把 tools/list 打到 stdout 后退出，便于对 fixture 比快照。
#[test]
fn print_tools_dumps_the_generated_catalog() {
    let out = bin().args(["--print-tools"]).output().expect("spawn");
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).expect("json");
    let tools = v["tools"].as_array().expect("tools");
    assert!(tools
        .iter()
        .any(|t| t["_meta"]["wxlike/actionId"] == "group.kick"));
}

/// 配了 `--scope` 之后，跨会话调用必须在本地就被拒（不发请求、不发网络）。
#[test]
fn scope_gate_blocks_other_conversations_before_execution() {
    let mut child = bin()
        .args([
            "--server",
            "http://127.0.0.1:1",
            "--token",
            "t",
            "--actor",
            "u-alice",
            "--session-id",
            "s-1",
            "--scope",
            "conv:g-1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        for line in [
            INIT,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"conv-unread","arguments":{"conv":"g-2"}}}"#,
        ] {
            writeln!(stdin, "{line}").expect("write");
        }
    }
    let out = child.wait_with_output().expect("wait");
    let replies: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("json"))
        .collect();
    let call = replies.last().expect("reply");
    assert!(call.get("error").is_none(), "scope 拒绝是工具错误：{call}");
    assert_eq!(call["result"]["isError"], true);
    let text = call["result"]["content"][0]["text"].as_str().expect("text");
    assert!(text.contains("missing scope"), "{text}");
    assert!(text.contains("g-2"), "{text}");
}

/// 参数来源声明为不可信时，写操作在协议层就被拒（提示注入的第一道闸）。
#[test]
fn untrusted_provenance_cannot_trigger_writes() {
    let frames = session(&[
        INIT,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"msg-send","arguments":{"conv":"g-1","text":"hi","idempotency_key":"k"},"_meta":{"wxlike/provenance":"untrusted: message body"}}}"#,
    ]);
    let call = &frames[1];
    assert_eq!(call["result"]["isError"], true, "{call}");
    let text = call["result"]["content"][0]["text"].as_str().expect("text");
    assert!(text.contains("untrusted content"), "{text}");
}

/// 长任务进作业账：`video.job.create` 成功后能在 `im_jobs` 里查到，
/// 且响应带 request/operation id（管理台据此追踪）。
#[test]
fn long_running_task_is_tracked_and_traceable() {
    use std::io::Read as _;
    use std::net::TcpListener;

    // 桩服务端：任何请求都回同一个 job
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for mut sock in listener.incoming().flatten() {
            // 上传是多段表单：必须把 Content-Length 读完再回，否则客户端会收到 RST
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                let n = sock.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(done) = request_complete(&buf) {
                    if done {
                        break;
                    }
                }
            }
            let body = r#"{"job_id":"j-1","status":"queued"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes());
        }
    });

    // 转码命令要读本地文件：给一个真文件
    let dir = std::env::temp_dir().join("wxlike-g15-test");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let file = dir.join("sample.mp4");
    std::fs::write(&file, b"not-really-a-video").expect("write");

    let mut child = bin()
        .args([
            "--server",
            &format!("http://{addr}"),
            "--token",
            "t",
            "--allow-write",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        for line in [
            INIT,
            &format!(
                r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"video-job-create","arguments":{{"path":{}}}}}}}"#,
                serde_json::json!(file.to_string_lossy())
            ),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"im_jobs","arguments":{"action":"list"}}}"#,
        ] {
            writeln!(stdin, "{line}").expect("write");
        }
    }
    let out = child.wait_with_output().expect("wait");
    let replies: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("json"))
        .collect();
    let created = replies.get(1).expect("create reply");
    assert_eq!(created["result"]["isError"], false, "{created}");
    assert!(
        created["result"]["_meta"]["wxlike/operationId"]
            .as_str()
            .is_some_and(|s| s.contains("video.job.create")),
        "要带 operation id：{created}"
    );

    let jobs = replies.get(2).expect("jobs reply");
    let text = jobs["result"]["content"][0]["text"].as_str().expect("text");
    let parsed: Value = serde_json::from_str(text).expect("json");
    assert_eq!(parsed["count"], 1, "{parsed}");
    assert_eq!(parsed["jobs"][0]["action_id"], "video.job.create");
    assert_eq!(parsed["jobs"][0]["state"], "done");
}

/// 是否已经把整个请求读完（头部声明的 Content-Length 到了没有）。
/// 头部与正文的分隔符（写成转义，避免源码里出现裸控制字符）。
const CRLF2: &str = "\r\n\r\n";

fn request_complete(buf: &[u8]) -> Option<bool> {
    let text = String::from_utf8_lossy(buf);
    let (head, rest) = text.split_once(CRLF2)?;
    let len: usize = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|v| v.trim().parse().ok())
        })
        .unwrap_or(0);
    Some(rest.len() >= len)
}
