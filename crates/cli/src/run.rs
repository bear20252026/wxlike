//! 执行与输出：**退出码是契约**，脚本靠它分支。
//!
//! | 码 | 含义 | 典型原因 |
//! |----|------|----------|
//! | 0 | 成功 | |
//! | 2 | 用法/配置错误 | 参数拼错、缺 token、未知命令 |
//! | 3 | 权限拒绝 | 会话不在白名单、没开 `--allow-write` |
//! | 4 | 命令不存在 | catalog 里没有这个 id |
//! | 5 | 执行失败 | 服务端返回错误、本地文件读不到 |
//! | 6 | 网络/超时 | 连不上服务端 |
//!
//! 3 与 5 必须分开：脚本遇到"没权限"要人改配置，遇到"业务失败"要重试或报障。

use std::process::ExitCode;

use serde_json::{json, Value};

use wxlike_client::execute::Executor;
use wxlike_client::Http;
use wxlike_cmd::catalog::{self, CommandSpec};
use wxlike_cmd::policy::Policy;

use crate::args::{Command, Invocation, Options};

pub const EXIT_OK: u8 = 0;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_DENIED: u8 = 3;
pub const EXIT_UNKNOWN: u8 = 4;
pub const EXIT_FAILED: u8 = 5;
pub const EXIT_NETWORK: u8 = 6;

/// 执行一次调用，返回退出码。
pub fn execute(inv: Invocation) -> ExitCode {
    ExitCode::from(run(inv))
}

fn run(inv: Invocation) -> u8 {
    let policy = build_policy(&inv.options);
    let Invocation { options, command } = inv;
    match command {
        Command::Help(domain) => {
            print!("{}", catalog::cli_help(domain.as_deref()));
            EXIT_OK
        }
        Command::List(query) => {
            print_list(&query, options.json);
            EXIT_OK
        }
        Command::Policy => {
            print!("{}", wxlike_cmd::policy::render(&policy));
            EXIT_OK
        }
        Command::Inspect => with_executor(&options, &policy)
            .map_or_else(|code| code, |exec| inspect(&exec, options.json)),
        Command::Run(cmds) => with_executor(&options, &policy)
            .map_or_else(|code| code, |exec| run_commands(&exec, &cmds, options.json)),
        Command::Invoke { spec, params } => with_executor(&options, &policy).map_or_else(
            |code| code,
            |exec| invoke(&exec, spec, &params, options.json),
        ),
    }
}

/// 环境变量打底 → 命令行开关 → `--read-only` 钳制。三层的顺序是刻意的：
/// 命令行只能更严（只读）或更宽（显式开写），不会因为漏传而意外收窄。
fn build_policy(options: &Options) -> Policy {
    let mut p = Policy::from_env();
    p.extend_convs(options.allow_conv.iter().cloned());
    if options.allow_write || options.allow_destructive {
        p.grant_write();
    }
    if options.allow_destructive {
        p.grant_destructive();
    }
    if options.read_only {
        p.force_read_only();
    }
    p
}

/// 执行类命令必须先有服务端地址与 token。
fn with_executor(options: &Options, policy: &Policy) -> Result<Executor, u8> {
    let token = options
        .token
        .clone()
        .or_else(|| std::env::var("WXLIKE_TOKEN").ok());
    let Some(token) = token else {
        eprintln!("缺少 token：--token 或 WXLIKE_TOKEN");
        return Err(EXIT_USAGE);
    };
    let server = options
        .server
        .clone()
        .or_else(|| std::env::var("WXLIKE_SERVER").ok())
        .unwrap_or_else(|| "http://127.0.0.1:8099".to_string());
    let mut http = Http::new(server);
    http.token = Some(token);
    Ok(Executor::new(http, policy.clone()))
}

fn invoke(exec: &Executor, spec: &CommandSpec, params: &Value, json_out: bool) -> u8 {
    match exec.run(spec.id, params) {
        Ok(v) => {
            emit(&v, json_out);
            EXIT_OK
        }
        Err(e) => fail(spec.id, &e),
    }
}

fn inspect(exec: &Executor, json_out: bool) -> u8 {
    let mut out = serde_json::Map::new();
    for id in ["me.info", "conv.list"] {
        match exec.run(id, &json!({})) {
            Ok(v) => {
                out.insert(id.to_string(), v);
            }
            Err(e) => return fail(id, &e),
        }
    }
    emit(&Value::Object(out), json_out);
    EXIT_OK
}

/// 老的 `run --cmd` 语义：**一条失败就停**，不假装成功。
fn run_commands(exec: &Executor, cmds: &[String], json_out: bool) -> u8 {
    for raw in cmds {
        let (id, params) = match parse_cmd(raw) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("{e}");
                return EXIT_USAGE;
            }
        };
        match exec.run(&id, &params) {
            Ok(v) => emit(&v, json_out),
            Err(e) => return fail(&id, &e),
        }
    }
    EXIT_OK
}

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

fn print_list(query: &str, json_out: bool) {
    let specs = catalog::search(query);
    if json_out {
        let items: Vec<Value> = specs
            .iter()
            .map(|s| {
                json!({
                    "id": s.id,
                    "tool": s.tool_name(),
                    "capability": s.capability.as_str(),
                    "scope": s.scope.as_str(),
                    "summary": s.summary,
                })
            })
            .collect();
        emit(&json!({"count": items.len(), "commands": items}), true);
        return;
    }
    for s in specs {
        println!("{:<20} {:<12} {}", s.id, s.capability, s.summary);
    }
}

/// 成功输出：`--json` 时单行紧凑（喂管道），否则缩进（给人看）。
fn emit(value: &Value, json_out: bool) {
    if json_out {
        println!("{}", serde_json::to_string(value).unwrap_or_default());
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_default()
        );
    }
}

fn fail(id: &str, error: &str) -> u8 {
    let code = classify(error);
    eprintln!("{id}: {error}");
    code
}

/// 错误文本 → 退出码。**唯一一处**做这个映射，测试直接钉住这张表。
pub fn classify(error: &str) -> u8 {
    if error.contains("unknown command") || error.contains("尚未实现") {
        return EXIT_UNKNOWN;
    }
    if error.contains("not allowed")
        || error.contains("write access was not granted")
        || error.contains("destructive access was not granted")
        || error.contains("read-only")
        || error.contains("needs a `conv` param")
    {
        return EXIT_DENIED;
    }
    if error.contains("connect")
        || error.contains("timed out")
        || error.contains("timeout")
        || error.contains("dns")
    {
        return EXIT_NETWORK;
    }
    EXIT_FAILED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_stable_per_failure_class() {
        // 权限拒绝（三种文案都来自 policy::Deny 的 Display）
        assert_eq!(classify("conversation g-2 is not allowed: outside the allowlist (grant it with --allow-conv)"), EXIT_DENIED);
        assert_eq!(
            classify("msg.send writes and write access was not granted (add --allow-write)"),
            EXIT_DENIED
        );
        assert_eq!(
            classify("group.kick is destructive and destructive access was not granted (add --allow-destructive)"),
            EXIT_DENIED
        );
        assert_eq!(
            classify(
                "msg.send is a write command and this session is read-only (drop --read-only)"
            ),
            EXIT_DENIED
        );
        assert_eq!(
            classify("conv.unread needs a `conv` param, but none was given"),
            EXIT_DENIED
        );
        // 命令不存在
        assert_eq!(
            classify("unknown command \"msg.send_for_ai\""),
            EXIT_UNKNOWN
        );
        // 网络
        assert_eq!(classify("connection refused: connect error"), EXIT_NETWORK);
        assert_eq!(classify("request timed out"), EXIT_NETWORK);
        // 其它一律业务失败
        assert_eq!(classify("server: 500 internal error"), EXIT_FAILED);
    }

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
        let exec = Executor::new(Http::new("http://127.0.0.1:1"), Policy::none());
        let err = exec
            .run("conv.unread", &json!({"conv": "d-a~b"}))
            .unwrap_err();
        assert!(err.contains("not allowed"), "{err}");
        assert!(!err.contains("connect"), "不该走到网络：{err}");
        assert_eq!(classify(&err), EXIT_DENIED);
    }

    /// `--read-only` 必须压过环境变量里开的写权限。
    #[test]
    fn read_only_flag_overrides_grants() {
        let options = Options {
            allow_write: true,
            allow_destructive: true,
            read_only: true,
            ..Options::default()
        };
        let p = build_policy(&options);
        assert!(!p.allow_write());
        assert!(!p.allow_destructive());
    }
}
