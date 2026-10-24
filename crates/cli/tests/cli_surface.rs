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

/// scope 闸门在发请求之前生效：地址不可达 → 仍然必须是 3（权限），不是 6（网络）。
#[test]
fn scope_gate_denies_before_any_request() {
    let (code, _, stderr) = run(&[
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
        "conv",
        "unread",
        "--param",
        "conv=g-2",
    ]);
    assert_eq!(code, 3, "{stderr}");
    assert!(stderr.contains("missing scope"), "{stderr}");
    assert!(stderr.contains("g-2"), "{stderr}");
}

/// `wxlike auth` 是自查入口：给 scope 就列出逐命令判定，不给就是用法错误。
#[test]
fn auth_report_lists_each_command_verdict() {
    let (code, stdout, stderr) = run(&[
        "auth",
        "--actor",
        "u-alice",
        "--session-id",
        "s-1",
        "--scope",
        "conv:g-1",
    ]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("actor=u-alice"), "{stdout}");
    assert!(stdout.contains("conv.unread"), "{stdout}");
    assert!(
        stdout
            .lines()
            .filter(|l| l.contains("允许") || l.contains("拒绝"))
            .count()
            >= 26,
        "每条命令一行：{stdout}"
    );

    let (code, _, stderr) = run(&["auth"]);
    assert_eq!(code, 2, "没配 scope 是用法问题：{stderr}");
    assert!(stderr.contains("--scope"), "{stderr}");
}

/// `coverage` 是"支持全业务"的可验证形态：缺口必须写清负责组与 issue。
#[test]
fn coverage_report_lists_every_domain_and_gap_owner() {
    let (code, stdout, stderr) = run(&["coverage"]);
    assert_eq!(code, 0, "{stderr}");
    for domain in [
        "messaging",
        "relations",
        "content",
        "media",
        "payment",
        "miniprogram",
        "service",
        "admin",
    ] {
        assert!(stdout.contains(domain), "覆盖表漏了域 {domain}");
    }
    assert!(stdout.contains("阻塞于 G14 #176"), "{stdout}");

    let (code, stdout, stderr) = run(&["coverage", "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json");
    assert_eq!(v["summary"]["automated"], 26);
    assert_eq!(v["domains"].as_array().map(Vec::len), Some(8));
}

/// 危险动作：没有 `--confirm` 时只打印计划，退出码 7，且不发请求。
#[test]
fn dangerous_action_stops_for_confirmation_with_exit_7() {
    let (code, stdout, stderr) = run(&[
        "--server",
        "http://127.0.0.1:1",
        "--token",
        "t",
        "--allow-conv",
        "g-1",
        "--allow-destructive",
        "--json",
        "group",
        "kick",
        "--param",
        "conv=g-1",
        "--param",
        "user_id=u2",
    ]);
    assert_eq!(code, 7, "{stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json");
    assert_eq!(v["confirmation_required"], true);
    assert_eq!(v["plan"]["action_id"], "group.kick");
    assert_eq!(v["plan"]["params"]["user_id"], "u2");
    assert_eq!(
        v["plan"]["digest"].as_str().map(str::len),
        Some(64),
        "计划要带规范参数摘要：{v}"
    );
    assert!(v["hint"].as_str().expect("hint").contains("--confirm"));
}

/// 加了 `--confirm` 才真的发请求（这里地址不可达 → 网络错误 6，说明已经出网）。
#[test]
fn confirmed_action_reaches_the_network() {
    let (code, _, stderr) = run(&[
        "--server",
        "http://127.0.0.1:1",
        "--token",
        "t",
        "--allow-conv",
        "g-1",
        "--allow-destructive",
        "--confirm",
        "group",
        "kick",
        "--param",
        "conv=g-1",
        "--param",
        "user_id=u2",
    ]);
    assert_ne!(code, 7, "加 --confirm 后不该再停在确认：{stderr}");
    assert!(
        stderr.contains("connect") || code == 6,
        "应当已经尝试出网：{code} {stderr}"
    );
}

/// dry-run：没有 token、地址不可达也能出计划，且退出 0（只计划，不落账）。
#[test]
fn dry_run_plans_without_credentials_or_network() {
    let (code, stdout, stderr) = run(&[
        "--dry-run",
        "--json",
        "msg",
        "send",
        "--param",
        "conv=g-1",
        "--param",
        "text=hi",
        "--param",
        "idempotency_key=k",
    ]);
    assert_eq!(code, 0, "dry-run 不需要 token：{stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json");
    assert_eq!(v["mode"], "dry_run");
    assert_eq!(v["confirmation_required"], false);
    assert_eq!(v["plan"]["action_id"], "msg.send");
    assert_eq!(v["plan"]["params"]["text"], "hi");
    assert!(
        v["plan"].get("token").is_none(),
        "dry-run 不该签发令牌：{v}"
    );
}

/// doctor：本机诊断不联网，报告结构稳定；token 只报"是否配置"。
#[test]
fn doctor_reports_local_state_without_network() {
    let (code, stdout, stderr) = run(&["doctor", "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json");
    assert_eq!(v["report"], "wxlike-cli doctor");
    assert_eq!(v["catalog"]["commands"], 26);
    assert!(
        v["coverage"]["blocked"].as_u64().is_some(),
        "覆盖统计里的 blocked 必须是数字"
    );
    assert!(v["scope"].as_str().is_some());
    assert!(
        v["warnings"].as_array().is_some(),
        "缺 token 应该是 warning 而不是 problem"
    );
    assert!(v["problems"].as_array().is_some_and(|p| p.is_empty()));

    // 人的形态也给一份
    let (code, stdout, _) = run(&["doctor"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("wxlike-cli doctor"), "{stdout}");
}

/// 非法 scope 是 problem：doctor 必须退出 1，让 CI/脚本能拦住坏配置。
#[test]
fn doctor_fails_on_unparsable_scope() {
    let (code, stdout, _) = run(&[
        "doctor",
        "--json",
        "--scope",
        "conv:* money:*",
        "--session-id",
        "s-1",
    ]);
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json");
    assert!(!v["problems"].as_array().expect("problems").is_empty());
}

/// 示例脚本必须只读：出现任何写/破坏性命令就是审计事故的种子。
#[test]
fn example_scripts_stay_read_only() {
    use std::path::Path;
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("examples dir").flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sh") {
            continue;
        }
        checked += 1;
        let text = std::fs::read_to_string(&path).expect("read script");
        assert!(
            text.contains("--json"),
            "{} 应当输出可解析 JSON",
            path.display()
        );
        for spec in wxlike_cmd::CATALOG
            .iter()
            .filter(|s| s.capability != wxlike_cmd::Capability::Read)
        {
            let (domain, verb) = spec.cli_path();
            let needle = format!("{domain} {verb}");
            assert!(
                !text.contains(&needle),
                "{} 里出现了会改数据的命令 {needle}（如确需演示，必须加 --dry-run）",
                path.display()
            );
        }
    }
    assert!(checked >= 3, "示例脚本少于 3 个：{checked}");
}
