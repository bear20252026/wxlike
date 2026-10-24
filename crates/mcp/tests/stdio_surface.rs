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
    assert_eq!(tools.len(), 29, "26 条命令 + 3 个元工具");
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
