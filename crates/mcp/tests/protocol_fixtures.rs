//! 契约 fixture 回放：**同一批用例，两条通道，逐字节相同**。
//!
//! 两条通道是：
//! 1. 进程内协议核心（`wxlike_mcp::server::Server`）——将来远程 transport 走的也是它；
//! 2. 真正的 `wxlike-mcp` 二进制（stdio）——MCP 客户端看到的就是它。
//!
//! 只测一条是不够的：协议核心对、二进制错，说明搬运层把字节弄坏了（例如有人
//! `println!` 到 stdout）；二进制对、核心错，说明 fixture 没打在语义上。
//!
//! fixture 是**可审查的 JSON**（`tests/fixtures/*.json`）：每条 = 一次请求 +
//! 一组期望。产品经理也能读，改一条用例不需要会 Rust。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;
use wxlike_mcp::server::Server;

use wxlike_client::execute::Executor;
use wxlike_client::Http;
use wxlike_cmd::policy::Policy;

/// 一个 fixture：请求 + 期望。
#[derive(Debug, serde::Deserialize)]
struct Fixture {
    name: String,
    #[allow(dead_code)]
    description: String,
    setup: Setup,
    request: Value,
    expect: Expect,
}

#[derive(Debug, serde::Deserialize)]
struct Setup {
    convs: Vec<String>,
    write: bool,
    destructive: bool,
    /// 是否先发 `initialize`（未握手的用例要 false）。
    initialize: bool,
}

#[derive(Debug, Default, serde::Deserialize)]
struct Expect {
    /// 路径 → 期望值（`.0` 表示数组下标）。
    #[serde(default)]
    equals: std::collections::BTreeMap<String, Value>,
    /// 整帧文本里必须出现的片段（用于不关心具体位置的断言）。
    #[serde(default)]
    contains_text: Vec<String>,
    /// 期望的 JSON-RPC 错误码。
    error_code: Option<i64>,
    /// 期望 `result.isError`。
    is_error: Option<bool>,
    /// 路径必须存在。
    #[serde(default)]
    present: Vec<String>,
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn load_fixtures() -> Vec<(PathBuf, Fixture)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(fixtures_dir())
        .expect("fixtures dir")
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let raw = std::fs::read_to_string(&path).expect("read fixture");
        let fixture: Fixture = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("{} 不是合法 fixture：{e}", path.display()));
        out.push((path, fixture));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// 路径取值，支持 `a.b.0.c`（数字按数组下标处理）。
fn at<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = value;
    for seg in path.split('.') {
        cur = match (cur, seg.parse::<usize>()) {
            (Value::Object(map), _) => map.get(seg)?,
            (Value::Array(items), Ok(i)) => items.get(i)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// 在进程内跑一遍 fixture，返回响应帧。
fn run_in_process(fixture: &Fixture) -> Value {
    let policy = Policy::new(
        fixture.setup.convs.clone(),
        fixture.setup.write,
        fixture.setup.destructive,
    );
    let exec = Executor::new(Http::new("http://127.0.0.1:1"), policy.clone());
    let mut server = Server::new(exec, policy);
    if fixture.setup.initialize {
        let init = server
            .handle_line(r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#)
            .expect("initialize reply");
        assert!(init.contains("protocolVersion"), "{init}");
    }
    let line = serde_json::to_string(&fixture.request).expect("serialize request");
    let reply = server.handle_line(&line).expect("fixture 必须有响应");
    serde_json::from_str(&reply).expect("响应必须是 JSON")
}

/// 用同一批行跑真正的二进制，返回逐行响应。
fn run_over_stdio(fixture: &Fixture) -> Vec<String> {
    let mut lines = Vec::new();
    if fixture.setup.initialize {
        lines.push(
            r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#
                .to_string(),
        );
    }
    lines.push(serde_json::to_string(&fixture.request).expect("serialize"));

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_wxlike-mcp"));
    cmd.args(["--server", "http://127.0.0.1:1", "--token", "fixture-token"]);
    if fixture.setup.write || fixture.setup.destructive {
        cmd.arg("--allow-write");
    }
    if fixture.setup.destructive {
        cmd.arg("--allow-destructive");
    }
    if !fixture.setup.convs.is_empty() {
        cmd.arg("--allow-conv").arg(fixture.setup.convs.join(","));
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn mcp");
    {
        use std::io::Write as _;
        let stdin = child.stdin.as_mut().expect("stdin");
        for line in &lines {
            writeln!(stdin, "{line}").expect("write");
        }
    }
    let out = child.wait_with_output().expect("wait");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

#[test]
fn every_fixture_holds_in_process_and_over_stdio() {
    let fixtures = load_fixtures();
    assert!(
        fixtures.len() >= 8,
        "fixture 少于 8 条说明有人删了用例：{}",
        fixtures.len()
    );

    for (path, fixture) in fixtures {
        let label = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        let in_process = run_in_process(&fixture);
        check_expectations(label, &fixture, &in_process);

        // 同一帧经二进制搬运，必须与进程内一致（时间戳/随机令牌等易变字段除外）
        let over_stdio = run_over_stdio(&fixture);
        let last = over_stdio.last().cloned().unwrap_or_default();
        assert_eq!(
            normalize_str(&last),
            normalize(&in_process).to_string(),
            "{label}: stdio 与进程内核心的回答不一致"
        );
    }
}

fn check_expectations(label: &str, fixture: &Fixture, reply: &Value) {
    let expect = &fixture.expect;
    let text = reply.to_string();
    if let Some(code) = expect.error_code {
        assert_eq!(
            at(reply, "error.code").and_then(Value::as_i64),
            Some(code),
            "{label}: 期望错误码 {code}，实际 {reply}"
        );
    }
    if let Some(is_error) = expect.is_error {
        assert_eq!(
            at(reply, "result.isError").and_then(Value::as_bool),
            Some(is_error),
            "{label}: 期望 isError={is_error}，实际 {reply}"
        );
    }
    for (path, want) in &expect.equals {
        assert_eq!(
            at(reply, path),
            Some(want),
            "{label}: 路径 {path} 不匹配（name={}）",
            fixture.name
        );
    }
    for path in &expect.present {
        assert!(
            at(reply, path).is_some(),
            "{label}: 缺少路径 {path}：{reply}"
        );
    }
    for needle in &expect.contains_text {
        assert!(
            text.contains(needle),
            "{label}: 响应里没有 {needle:?}：{reply}"
        );
    }
}

/// 每次都会变的字段：令牌与过期时间。比较前统一抹掉，
/// 但**结构必须一致**——少一个字段就说明两条通道行为不同。
const VOLATILE_KEYS: [&str; 2] = ["token", "expires_at"];

fn normalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    if VOLATILE_KEYS.contains(&k.as_str()) {
                        (k.clone(), Value::String("<volatile>".into()))
                    } else {
                        (k.clone(), normalize(v))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(normalize).collect()),
        // `content[0].text` 里嵌的是 JSON 文本：字符串也要递归，否则令牌会漏过去
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(inner) => Value::String(normalize(&inner).to_string()),
            Err(_) => value.clone(),
        },
        other => other.clone(),
    }
}

fn normalize_str(raw: &str) -> String {
    let parsed: Value = serde_json::from_str(raw).expect("response json");
    normalize(&parsed).to_string()
}

/// fixture 自己也要能读：错误码/期望值拼错时，本测试比回放更早失败。
#[test]
fn fixtures_are_self_describing() {
    for (path, fixture) in load_fixtures() {
        let label = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        assert!(!fixture.name.trim().is_empty(), "{label} 缺 name");
        assert!(
            fixture.request.get("method").is_some(),
            "{label} 的 request 没有 method"
        );
        let has_expectation = fixture.expect.error_code.is_some()
            || fixture.expect.is_error.is_some()
            || !fixture.expect.equals.is_empty()
            || !fixture.expect.contains_text.is_empty()
            || !fixture.expect.present.is_empty();
        assert!(has_expectation, "{label} 没有任何期望，等于没测");
    }
}
