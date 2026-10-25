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
//! | 7 | 待确认 | 危险动作没带 `--confirm`（此时只打印计划，不执行） |
//!
//! 退出码由 `DomainError` 的分类派生（`contract::classify`）：
//! `invalid_argument`→2、`unauthorized`/`forbidden`→3、`not_found`→4、
//! `timeout`/`unavailable`→6，其余→5。这样 CLI、MCP `isError` 与 UI 的
//! 错误码三者同源，不会出现"CLI 说没权限、UI 说参数错"。
//!
//! 3 与 5 必须分开：脚本遇到"没权限"要人改配置，遇到"业务失败"要重试或报障。

use std::process::ExitCode;

use serde_json::{json, Value};

use wxlike_client::execute::Executor;
use wxlike_client::Http;
use wxlike_cmd::auth::{AuthGate, Provenance, ScopeSet};
use wxlike_cmd::catalog::{self, CommandSpec};
use wxlike_cmd::contract;
use wxlike_cmd::policy::Policy;
use wxlike_types::{ActionResult, DomainError};

use crate::args::{Command, Invocation, Options};

pub const EXIT_OK: u8 = 0;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_DENIED: u8 = 3;
pub const EXIT_UNKNOWN: u8 = 4;
pub const EXIT_FAILED: u8 = 5;
pub const EXIT_NETWORK: u8 = 6;
pub const EXIT_CONFIRMATION_REQUIRED: u8 = 7;

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
        Command::Auth => auth_report(&options, &policy),
        Command::Doctor => doctor(&options, &policy),
        Command::Coverage => {
            if options.json {
                emit(&wxlike_cmd::domains::report(), true);
            } else {
                print!("{}", wxlike_cmd::domains::render());
            }
            EXIT_OK
        }
        Command::Inspect => with_executor(&options, &policy)
            .map_or_else(|code| code, |exec| inspect(&exec, options.json)),
        Command::Run(cmds) => with_executor(&options, &policy)
            .map_or_else(|code| code, |exec| run_commands(&exec, &cmds, options.json)),
        Command::Invoke { spec, params } => invoke_command(&options, &policy, spec, &params),
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

/// 一条业务命令的执行路径：闸门 → 执行器 → 输出。
fn invoke_command(options: &Options, policy: &Policy, spec: &CommandSpec, params: &Value) -> u8 {
    if options.dry_run {
        return emit_plan(spec, params, options.json, "dry_run");
    }
    let gate = match build_gate(options) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{e}");
            return EXIT_USAGE;
        }
    };
    let env = match contract::Envelope::from_raw(
        options.trace_id.as_deref(),
        options.entry_id.as_deref(),
        options.deadline_ms,
    ) {
        Ok(env) => env,
        Err(e) => return fail_contract(spec, &e, options.json),
    };
    let exec = match with_executor(options, policy) {
        Ok(exec) => exec,
        Err(code) => return code,
    };
    invoke(
        &exec,
        spec,
        params,
        options.json,
        gate.as_ref(),
        options.confirm,
        &env,
    )
}

/// 打印"将要发生什么"。
///
/// `mode = "dry_run"` → 退出 0（这是用户主动要的计划）；
/// `mode = "confirmation_required"` → 退出 7（脚本据此知道要人确认）。
fn emit_plan(spec: &CommandSpec, params: &Value, json_out: bool, mode: &str) -> u8 {
    let plan = wxlike_cmd::confirmation::plan(spec, params);
    let (domain, verb) = spec.cli_path();
    let payload = json!({
        "mode": mode,
        "confirmation_required": spec.confirmation.is_required(),
        "capability": spec.capability.as_str(),
        "plan": plan.to_json(),
        "hint": if mode == "dry_run" {
            format!("只出计划，未执行。确认无误后去掉 --dry-run：wxlike {domain} {verb} ...")
        } else {
            format!("确认无误后加 --confirm 重新执行：wxlike {domain} {verb} --confirm ...")
        },
    });
    emit(&payload, json_out);
    if mode == "dry_run" {
        EXIT_OK
    } else {
        EXIT_CONFIRMATION_REQUIRED
    }
}

/// `wxlike doctor`：本机诊断。**不联网**——诊断本身不该制造故障。
fn doctor(options: &Options, policy: &Policy) -> u8 {
    let report = doctor_report(options, policy);
    emit(&report, options.json);
    if report["problems"].as_array().is_some_and(|p| !p.is_empty()) {
        return 1;
    }
    EXIT_OK
}

fn doctor_report(options: &Options, policy: &Policy) -> Value {
    let mut problems: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    if options.token.is_none() && std::env::var("WXLIKE_TOKEN").is_err() {
        warnings.push("没有 token：执行类命令会被服务端拒绝".into());
    }
    if policy.conv_patterns().is_empty() {
        warnings.push("会话白名单为空：所有会话类命令都会被拒".into());
    }
    let scope = options
        .scope
        .clone()
        .or_else(|| std::env::var("WXLIKE_SCOPE").ok());
    let invalid: Vec<String> = scope
        .as_deref()
        .map(|s| wxlike_cmd::auth::ScopeSet::parse(s).invalid().to_vec())
        .unwrap_or_default();
    if !invalid.is_empty() {
        problems.push(format!("--scope 里有无法解析的条目：{invalid:?}"));
    }
    let server = options
        .server
        .clone()
        .or_else(|| std::env::var("WXLIKE_SERVER").ok())
        .unwrap_or_else(|| "http://127.0.0.1:8099".to_string());
    json!({
        "report": "wxlike-cli doctor",
        "version": env!("CARGO_PKG_VERSION"),
        "server": server,
        "token": if options.token.is_some() || std::env::var("WXLIKE_TOKEN").is_ok() {
            "已配置"
        } else {
            "未配置"
        },
        "policy": {
            "convs": policy.conv_patterns(),
            "write": policy.allow_write(),
            "destructive": policy.allow_destructive(),
        },
        "scope": scope.unwrap_or_else(|| "（未配置，仅按本地策略）".to_string()),
        "catalog": {
            "commands": wxlike_cmd::catalog::CATALOG.len(),
            "note": "MCP 侧另有元工具（list/execute/inspect/jobs），由 wxlike-mcp 自己报",
        },
        "coverage": wxlike_cmd::domains::summary(),
        "protocol": {
            "mcp_versions": ["2025-06-18", "2025-03-26", "2024-11-05"],
        },
        "warnings": warnings,
        "problems": problems,
    })
}

/// 会话/scope 闸门。没配 `--scope` 就不挂：本地单用户场景由 `policy` 负责，
/// 远程调用必须配（否则等于把"谁能做什么"全交给服务端一次判定）。
fn build_gate(options: &Options) -> Result<Option<AuthGate>, String> {
    let Some(scope) = &options.scope else {
        return Ok(None);
    };
    if !ScopeSet::parse(scope).invalid().is_empty() {
        return Err(format!("--scope 里有无法解析的条目：{scope}"));
    }
    if options.actor.trim().is_empty() {
        return Err("--scope 需要 --actor（身份不能为空）".to_string());
    }
    let session = options
        .session_id
        .clone()
        .ok_or_else(|| "--scope 需要配合 --session-id（会话必须绑定）".to_string())?;
    AuthGate::from_parts(
        &options.actor,
        &session,
        scope,
        Some(&session),
        options.origin_allow.clone(),
    )
    .map(Some)
    .map_err(|e| e.to_string())
}

/// `wxlike auth`：把"我是谁、我能在哪些资源上做什么"一次说清。
fn auth_report(options: &Options, policy: &Policy) -> u8 {
    match build_gate(options) {
        Ok(Some(gate)) => {
            print!("{}", gate.render(policy));
            EXIT_OK
        }
        Ok(None) => {
            eprintln!("未配置 --scope（也没有 WXLIKE_SCOPE）：只按本地策略判定");
            eprintln!("示例：wxlike auth --scope 'conv:g-* media:*' --session-id s-1");
            EXIT_USAGE
        }
        Err(e) => {
            eprintln!("{e}");
            EXIT_USAGE
        }
    }
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

fn invoke(
    exec: &Executor,
    spec: &CommandSpec,
    params: &Value,
    json_out: bool,
    gate: Option<&AuthGate>,
    confirmed: bool,
    env: &contract::Envelope,
) -> u8 {
    // 危险动作先出计划：没 `--confirm` 就停在这里，**不发任何请求**。
    if spec.confirmation.is_required() && !confirmed {
        return emit_plan(spec, params, json_out, "confirmation_required");
    }
    // scope 先行：注定被服务端拒的请求不发出去，也顺便省掉一次网络往返
    if let Some(gate) = gate {
        if let Err(e) = gate.check(spec, params, Provenance::Trusted) {
            eprintln!("{}: {e}", spec.id);
            return EXIT_DENIED;
        }
    }
    // 契约层校验：写命令缺幂等键、trace/entry 不合法、deadline 已过都在这里拒
    if let Err(e) = contract::build_request(spec, params, env) {
        return fail_contract(spec, &e, json_out);
    }
    if let Err(e) = contract::check_deadline(env, contract::now_ms_i64()) {
        return fail_contract(spec, &e, json_out);
    }
    match exec.run(spec.id, params) {
        Ok(v) => {
            let result = contract::ok_result(spec, v, contract::now_ms());
            emit_result(&result, json_out);
            EXIT_OK
        }
        Err(e) => {
            let err = contract::classify(&e);
            let result = contract::failed_result(spec, err.clone(), contract::now_ms());
            emit_result(&result, json_out);
            eprintln!("{}: {e}", spec.id);
            exit_code_for(&err)
        }
    }
}

/// 契约层失败（还没发请求）：也输出 ActionResult，脚本只需认一种形状。
fn fail_contract(spec: &CommandSpec, err: &DomainError, json_out: bool) -> u8 {
    let result = contract::failed_result(spec, err.clone(), contract::now_ms());
    emit_result(&result, json_out);
    eprintln!("{}: {}", spec.id, err.message());
    exit_code_for(err)
}

/// 结果的机器形态（`--json`）与人的形态（默认只打 data）。
fn emit_result(result: &ActionResult, json_out: bool) {
    if json_out {
        println!(
            "{}",
            serde_json::to_string(&contract::result_json(result)).unwrap_or_default()
        );
        return;
    }
    match &result.outcome {
        wxlike_types::ActionOutcome::Ok { data } => {
            println!("{}", serde_json::to_string_pretty(data).unwrap_or_default());
        }
        wxlike_types::ActionOutcome::Empty => println!("（空结果）"),
        wxlike_types::ActionOutcome::Failed { error } => {
            eprintln!("{}: {}", error.code(), error.message());
        }
    }
}

/// `DomainError` → 退出码。**唯一一处**做这个映射。
pub fn exit_code_for(err: &DomainError) -> u8 {
    match err.code() {
        "invalid_argument" => EXIT_USAGE,
        "unauthorized" | "forbidden" => EXIT_DENIED,
        "not_found" => EXIT_UNKNOWN,
        "timeout" | "unavailable" => EXIT_NETWORK,
        _ => EXIT_FAILED,
    }
}

fn inspect(exec: &Executor, json_out: bool) -> u8 {
    let mut out = serde_json::Map::new();
    for id in ["me.info", "conv.list"] {
        let Some(spec) = wxlike_cmd::catalog::get(id) else {
            return fail(id, &format!("unknown command {id}"));
        };
        match exec.run(id, &json!({})) {
            Ok(v) => {
                out.insert(
                    id.to_string(),
                    contract::result_json(&contract::ok_result(spec, v, contract::now_ms())),
                );
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

/// 错误文本 → 退出码：先过 `contract::classify` 得到契约错误码，再映射。
///
/// 以前这里是一串 `contains`，与 MCP / UI 各判一套；现在分类只在契约一处。
pub fn classify(error: &str) -> u8 {
    exit_code_for(&contract::classify(error))
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
        // 缺参数是"命令写错了"（usage=2），不是"没权限"：
        // 契约把它归到 invalid_argument，脚本据此改命令行而不是找管理员。
        assert_eq!(
            classify("conv.unread needs a `conv` param, but none was given"),
            EXIT_USAGE
        );
        // 命令不存在
        assert_eq!(
            classify("unknown command \"msg.send_for_ai\""),
            EXIT_UNKNOWN
        );
        // 网络
        assert_eq!(classify("connection refused: connect error"), EXIT_NETWORK);
        assert_eq!(classify("request timed out"), EXIT_NETWORK);
        // G15-02 的会话/scope 拒绝也归 3：脚本的动作一样是"找人改授权"
        assert_eq!(
            classify("missing scope conv:g-2 (grant it in the token, not in the prompt)"),
            EXIT_DENIED
        );
        assert_eq!(
            classify("group.kick is an admin action and this actor has no admin role"),
            EXIT_DENIED
        );
        // 参数/确认类错误同样是 usage
        assert_eq!(classify("token audience \"x\" is not ours"), EXIT_DENIED);
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

    /// scope 没配就不挂闸门；配了就必须绑定会话。
    #[test]
    fn gate_needs_session_and_rejects_unknown_scope_class() {
        let none = Options::default();
        assert!(build_gate(&none).expect("ok").is_none());

        let scoped = Options {
            actor: "u-alice".into(),
            scope: Some("conv:*".into()),
            ..Options::default()
        };
        assert!(build_gate(&scoped).unwrap_err().contains("session-id"));

        let bad = Options {
            actor: "u-alice".into(),
            scope: Some("conv:* money:*".into()),
            session_id: Some("s-1".into()),
            ..Options::default()
        };
        assert!(build_gate(&bad).unwrap_err().contains("无法解析"));

        let good = Options {
            actor: "u-alice".into(),
            scope: Some("conv:g-1".into()),
            session_id: Some("s-1".into()),
            ..Options::default()
        };
        let gate = build_gate(&good).expect("ok").expect("gate");
        assert_eq!(gate.context().session_id, "s-1");
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
