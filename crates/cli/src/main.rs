//! `wxlike-cli`：命令面的**人类入口**（P1）。
//!
//! 它做的事和将来的 MCP server 一样——把 `wxlike-cmd` 的命令 id 翻译成 HTTP
//! 调用，权限一律先过 `policy.authorize`。区别只在协议：这里是命令行，MCP 是
//! stdio JSON-RPC。**两者共用同一个 catalog 与同一个 Policy**，不重复实现。
//!
//! 用法：
//! ```text
//! wxlike-cli --server http://127.0.0.1:8099 --token $T list
//! wxlike-cli ... inspect
//! wxlike-cli ... run --cmd 'conv.unread={"conv":"d-a~b"}'
//! wxlike-cli policy
//! ```

use std::process::ExitCode;

use serde_json::{json, Value};
use wxlike_client::execute::Executor;
use wxlike_cmd::policy::Policy;
use wxlike_cmd::{search, CATALOG};

/// 解析 `--cmd`：`id` 或 `id={json}`。
fn parse_cmd(raw: &str) -> Result<(String, Value), String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("空的 --cmd".into());
    }
    match raw.split_once('=') {
        None => Ok((raw.to_string(), json!({}))),
        Some((id, rest)) => {
            let v: Value =
                serde_json::from_str(rest).map_err(|e| format!("{id} 的参数不是合法 JSON: {e}"))?;
            if !v.is_object() {
                return Err(format!("{id} 的参数必须是对象"));
            }
            Ok((id.to_string(), v))
        }
    }
}

fn main() -> ExitCode {
    // 先剥掉带值的 flag，剩下的第一个是子命令。
    // 不能直接 `args.next()`：`--server URL inspect` 会把 `--server` 当成子命令。
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        if raw[i].starts_with("--") {
            i += 2; // flag + 它的值（所有 flag 都带值）
        } else {
            positional.push(raw[i].clone());
            i += 1;
        }
    }
    let sub = positional.first().cloned().unwrap_or_default();
    let rest: Vec<String> = raw; // flag 仍从完整参数里读

    let server = flag(&rest, "--server")
        .or_else(|| std::env::var("WXLIKE_SERVER").ok())
        .unwrap_or_else(|| "http://127.0.0.1:8099".to_string());
    let token = flag(&rest, "--token").or_else(|| std::env::var("WXLIKE_TOKEN").ok());
    let policy = Policy::from_env();

    match sub.as_str() {
        "list" => {
            let q = positional.get(1).cloned().unwrap_or_default();
            for s in search(&q) {
                println!("{:<20} {:<12} {}", s.id, s.capability, s.summary);
            }
            ExitCode::SUCCESS
        }
        "policy" => {
            dump_policy(&policy);
            ExitCode::SUCCESS
        }
        "inspect" | "run" => {
            let mut http = wxlike_client::Http::new(server);
            http.token = token;
            if http.token.is_none() {
                eprintln!("缺少 token：--token 或 WXLIKE_TOKEN");
                return ExitCode::FAILURE;
            }
            let exec = Executor::new(http, policy.clone());

            if sub == "inspect" {
                let mut out = serde_json::Map::new();
                for id in ["me.info", "conv.list"] {
                    match exec.run(id, &json!({})) {
                        Ok(v) => {
                            out.insert(id.to_string(), v);
                        }
                        Err(e) => {
                            eprintln!("{id}: {e}");
                            return ExitCode::FAILURE;
                        }
                    }
                }
                println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
                return ExitCode::SUCCESS;
            }

            // run：按出现顺序执行所有 --cmd
            let cmds: Vec<String> = collect_flag_values(&rest, "--cmd");
            if cmds.is_empty() {
                eprintln!("run 需要至少一个 --cmd 'id[={{json}}]'");
                return ExitCode::FAILURE;
            }
            for raw in cmds {
                let (id, params) = match parse_cmd(&raw) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("{e}");
                        return ExitCode::FAILURE;
                    }
                };
                match exec.run(&id, &params) {
                    Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default()),
                    Err(e) => {
                        // 与 WordCraft 一致：一条失败就停，且不假装成功
                        eprintln!("{id}: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("用法:");
            eprintln!("  wxlike-cli [--server URL] [--token T] list [关键词]");
            eprintln!("  wxlike-cli [--server URL] [--token T] inspect");
            eprintln!("  wxlike-cli [--server URL] [--token T] run --cmd 'conv.unread={{\"conv\":\"d-a~b\"}}'");
            eprintln!("  wxlike-cli policy");
            eprintln!();
            eprintln!(
                "权限来自环境变量：WXLIKE_AGENT_ALLOW_CONV / _ALLOW_WRITE / _ALLOW_DESTRUCTIVE"
            );
            ExitCode::FAILURE
        }
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).cloned()
}

fn collect_flag_values(args: &[String], name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == name {
            if let Some(v) = args.get(i + 1) {
                out.push(v.clone());
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn dump_policy(p: &Policy) {
    println!("权限控制中心 · 当前生效策略");
    let convs = p.conv_patterns();
    println!(
        "  会话白名单   {}",
        if convs.is_empty() {
            "（空 = 任何会话都不允许）".to_string()
        } else {
            convs.join(", ")
        }
    );
    println!(
        "  写权限       {}",
        if p.allow_write() { "开" } else { "关" }
    );
    println!(
        "  破坏性权限   {}",
        if p.allow_destructive() { "开" } else { "关" }
    );
    println!();
    println!("{:<20} {:<12} 判定", "ID", "能力");
    for s in CATALOG {
        let probe = json!({
            "conv": p.conv_patterns().first().map(|c| c.replace('*', "probe")).unwrap_or_else(|| "probe".into()),
        });
        match p.authorize(s.id, &probe) {
            Ok(_) => println!("{:<20} {:<12} 允许", s.id, s.capability),
            Err(e) => println!("{:<20} {:<12} 拒绝 — {e}", s.id, s.capability),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cmd_accepts_bare_id_and_json() {
        assert_eq!(parse_cmd("me.info").unwrap(), ("me.info".into(), json!({})));
        let (id, p) = parse_cmd(r#"conv.unread={"conv":"d-a~b"}"#).unwrap();
        assert_eq!(id, "conv.unread");
        assert_eq!(p["conv"], "d-a~b");
    }

    #[test]
    fn parse_cmd_rejects_bad_json_and_non_object() {
        assert!(parse_cmd("conv.unread={oops}").is_err());
        assert!(parse_cmd("conv.unread=[1,2]").is_err());
        assert!(parse_cmd("  ").is_err());
    }

    /// 被权限拒绝时**不该发任何请求**：给一个必然连不上的地址，
    /// 如果错误是"连接失败"就说明权限没拦住。
    #[test]
    fn policy_denies_before_any_network_call() {
        let policy = Policy::none();
        let exec = Executor::new(wxlike_client::Http::new("http://127.0.0.1:1"), policy);
        let err = exec
            .run("conv.unread", &json!({"conv": "d-a~b"}))
            .unwrap_err();
        assert!(err.contains("not allowed"), "{err}");
        assert!(!err.contains("connect"), "不该走到网络：{err}");
    }
}
