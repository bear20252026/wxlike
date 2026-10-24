//! 参数解析：**纯函数**，不碰网络、不读环境。
//!
//! 支持两种写法，且互相等价（同参数同结果）：
//! ```text
//! wxlike msg send --param conv=g-1 --param text=hi --param idempotency_key=k
//! wxlike run --cmd 'msg.send={"conv":"g-1","text":"hi","idempotency_key":"k"}'
//! ```
//! 第一种是给人用的，第二种是给老脚本用的；`list` / `policy` / `inspect`
//! 保持原样，不破坏已有习惯。
//!
//! `--param k=v` 的类型由 catalog 决定，**不由 v 的写法猜**：
//! `--param conv=123` 里 conv 声明为 string，就必须是字符串 `"123"`；
//! 若按 JSON 猜成数字，调用方会收到"参数 conv 类型不对"这种莫名其妙的错。

use serde_json::{Map, Value};

use wxlike_cmd::catalog::{self, CommandSpec, ParamType};

/// 解析失败（用法错误）。**永远对应退出码 2**。
#[derive(Debug, PartialEq, Eq)]
pub struct UsageError(pub String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 解析结果：要做什么。
#[derive(Debug, PartialEq)]
pub enum Command {
    /// `help [域名]`
    Help(Option<String>),
    /// `list [关键词]`
    List(String),
    /// `policy`
    Policy,
    /// `auth`：打印身份上下文与逐命令的 scope 判定
    Auth,
    /// `coverage`：业务域覆盖报告（自动 / 宿主 / 未做）
    Coverage,
    /// `inspect`
    Inspect,
    /// `run --cmd 'id={json}'`（原样保留多条顺序执行）
    Run(Vec<String>),
    /// `<域> <动作> --param k=v`
    Invoke {
        spec: &'static CommandSpec,
        params: Value,
    },
}

/// 全局开关。
#[derive(Debug, Default, PartialEq)]
pub struct Options {
    pub server: Option<String>,
    pub token: Option<String>,
    /// 机器可读输出：单行紧凑 JSON（脚本可 `jq`）。
    pub json: bool,
    pub allow_conv: Vec<String>,
    pub allow_write: bool,
    pub allow_destructive: bool,
    /// 强制只读：即使环境变量开了写，也压回只读。
    pub read_only: bool,
    /// 身份（默认读 `WXLIKE_ACTOR`，再缺省 `local-cli`）。
    pub actor: String,
    /// 会话 id（scope 判定必须绑定会话）。
    pub session_id: Option<String>,
    /// 授予的 scope，如 `conv:g-* media:*`。
    pub scope: Option<String>,
    /// 允许的 Origin（远程调用时用）。
    pub origin_allow: Vec<String>,
}

/// 解析后的完整调用。
#[derive(Debug, PartialEq)]
pub struct Invocation {
    pub options: Options,
    pub command: Command,
}

/// 带值的 flag：形态统一是 `--name value`。
const VALUE_FLAGS: [&str; 10] = [
    "--server",
    "--token",
    "--allow-conv",
    "--param",
    "--params",
    "--cmd",
    "--actor",
    "--session-id",
    "--scope",
    "--origin-allow",
];

/// 一个位置参数被吃掉了几格，或者该收尾了。
enum Step {
    /// 前进了 n 个参数。
    Next(usize),
    /// 遇到 `-h/--help`：立即收尾，不再解析后面的东西。
    Help,
}

/// 解析过程的累加器。拆出来是为了让 [`parse`] 本体只有一层循环。
#[derive(Default)]
struct Parser {
    options: Options,
    positional: Vec<String>,
    raw_params: Vec<(String, String)>,
    raw_json: Option<String>,
    cmds: Vec<String>,
}

/// 把参数列表解析成 [`Invocation`]。
pub fn parse(args: &[String]) -> Result<Invocation, UsageError> {
    let mut p = Parser::default();
    let mut i = 0;
    while i < args.len() {
        match p.step(args, i)? {
            Step::Next(n) => i += n,
            Step::Help => {
                return Ok(Invocation {
                    options: p.options,
                    command: Command::Help(p.positional.first().cloned()),
                })
            }
        }
    }
    p.finish()
}

impl Parser {
    /// 吃一个参数（带值的吃两个）。
    fn step(&mut self, args: &[String], i: usize) -> Result<Step, UsageError> {
        let arg = args[i].as_str();
        if arg == "-h" || arg == "--help" {
            return Ok(Step::Help);
        }
        if VALUE_FLAGS.contains(&arg) {
            let value = args
                .get(i + 1)
                .ok_or_else(|| UsageError(format!("{arg} 需要一个值")))?;
            self.set_value(arg, value)?;
            return Ok(Step::Next(2));
        }
        if arg.starts_with("--") {
            self.set_switch(arg)?;
            return Ok(Step::Next(1));
        }
        self.positional.push(arg.to_string());
        Ok(Step::Next(1))
    }

    fn set_value(&mut self, flag: &str, value: &str) -> Result<(), UsageError> {
        match flag {
            "--server" => self.options.server = Some(value.to_string()),
            "--token" => self.options.token = Some(value.to_string()),
            "--allow-conv" => self.options.allow_conv.push(value.to_string()),
            "--params" => self.raw_json = Some(value.to_string()),
            "--cmd" => self.cmds.push(value.to_string()),
            "--actor" => self.options.actor = value.to_string(),
            "--session-id" => self.options.session_id = Some(value.to_string()),
            "--scope" => self.options.scope = Some(value.to_string()),
            "--origin-allow" => self.options.origin_allow.push(value.to_string()),
            _ => {
                let (k, v) = value
                    .split_once('=')
                    .ok_or_else(|| UsageError(format!("--param 要写成 k=v，收到 {value:?}")))?;
                self.raw_params.push((k.to_string(), v.to_string()));
            }
        }
        Ok(())
    }

    fn set_switch(&mut self, flag: &str) -> Result<(), UsageError> {
        match flag {
            "--json" => self.options.json = true,
            "--allow-write" => self.options.allow_write = true,
            "--allow-destructive" => self.options.allow_destructive = true,
            "--read-only" => self.options.read_only = true,
            other => return Err(UsageError(format!("未知参数 {other:?}（--help 看用法）"))),
        }
        Ok(())
    }

    fn actor_default(&mut self) {
        if self.options.actor.is_empty() {
            self.options.actor =
                std::env::var("WXLIKE_ACTOR").unwrap_or_else(|_| "local-cli".to_string());
        }
        if self.options.session_id.is_none() {
            self.options.session_id = std::env::var("WXLIKE_SESSION_ID").ok();
        }
        if self.options.scope.is_none() {
            self.options.scope = std::env::var("WXLIKE_SCOPE").ok();
        }
    }

    fn finish(mut self) -> Result<Invocation, UsageError> {
        self.actor_default();
        let command = resolve(&self.positional, self.cmds, &self.raw_params, self.raw_json)?;
        Ok(Invocation {
            options: self.options,
            command,
        })
    }
}

/// 位置参数 → 命令。`list/policy/inspect/run/help` 是保留字。
fn resolve(
    positional: &[String],
    cmds: Vec<String>,
    raw_params: &[(String, String)],
    raw_json: Option<String>,
) -> Result<Command, UsageError> {
    let head = positional.first().map(String::as_str).unwrap_or("");
    match head {
        "help" => Ok(Command::Help(positional.get(1).cloned())),
        "list" => Ok(Command::List(
            positional.get(1).cloned().unwrap_or_default(),
        )),
        "policy" => Ok(Command::Policy),
        "auth" => Ok(Command::Auth),
        "coverage" => Ok(Command::Coverage),
        "inspect" => Ok(Command::Inspect),
        "run" => {
            if cmds.is_empty() {
                return Err(UsageError(
                    "run 需要至少一个 --cmd 'id[={json}]'（或用 <域> <动作> 形式）".into(),
                ));
            }
            Ok(Command::Run(cmds))
        }
        "" if cmds.is_empty() => Ok(Command::Help(None)),
        "" => Err(UsageError(
            "--cmd 前面要给出子命令 run（或改用 <域> <动作>）".into(),
        )),
        _ => resolve_invoke(positional, raw_params, raw_json),
    }
}

/// `<域> <动作...>` → catalog 里的一条命令。
fn resolve_invoke(
    positional: &[String],
    raw_params: &[(String, String)],
    raw_json: Option<String>,
) -> Result<Command, UsageError> {
    let id = positional.join(".");
    let spec = catalog::get(&id).ok_or_else(|| {
        UsageError(format!(
            "未知命令 {id:?}；用 `wxlike list` 看全部，`wxlike help <域>` 看某域"
        ))
    })?;
    let params = build_params(spec, raw_params, raw_json)?;
    catalog::validate_args(spec, &params).map_err(|e| {
        UsageError(format!(
            "{}: {e}（`wxlike help {}`）",
            spec.id,
            spec.cli_path().0
        ))
    })?;
    Ok(Command::Invoke { spec, params })
}

/// 把 `--param` / `--params` 合成为一个参数对象，类型按 catalog 声明转换。
fn build_params(
    spec: &CommandSpec,
    raw_params: &[(String, String)],
    raw_json: Option<String>,
) -> Result<Value, UsageError> {
    let mut map = Map::new();
    if let Some(raw) = raw_json {
        let parsed: Value = serde_json::from_str(&raw)
            .map_err(|e| UsageError(format!("--params 不是合法 JSON: {e}")))?;
        let obj = parsed
            .as_object()
            .ok_or_else(|| UsageError("--params 必须是 JSON 对象".into()))?;
        map.extend(obj.clone());
    }
    for (name, raw) in raw_params {
        let param = spec
            .args
            .iter()
            .find(|p| p.name == name.as_str())
            .ok_or_else(|| {
                UsageError(format!(
                    "{} 没有参数 {name:?}；可用：{}",
                    spec.id,
                    spec.args
                        .iter()
                        .map(|p| p.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
        map.insert(name.clone(), coerce(param.ty, name, raw)?);
    }
    Ok(Value::Object(map))
}

/// 按声明类型转换命令行文本。
fn coerce(ty: ParamType, name: &str, raw: &str) -> Result<Value, UsageError> {
    match ty {
        ParamType::Str => Ok(Value::String(raw.to_string())),
        ParamType::Int | ParamType::NonNegativeInt => raw
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| UsageError(format!("参数 {name} 需要整数，收到 {raw:?}"))),
        ParamType::Bool => raw
            .parse::<bool>()
            .map(Value::from)
            .map_err(|_| UsageError(format!("参数 {name} 需要 true/false，收到 {raw:?}"))),
        ParamType::StrList => Ok(Value::Array(
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| Value::String(s.to_string()))
                .collect(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn domain_and_verb_map_to_a_command() {
        let inv = parse(&args(&["msg", "send", "--param", "conv=g-1"]));
        // 缺必填参数要在解析阶段就拦住，而不是发到服务端才发现
        let err = inv.unwrap_err();
        assert!(err.0.contains("text"), "{err}");

        let inv = parse(&args(&[
            "msg",
            "send",
            "--param",
            "conv=g-1",
            "--param",
            "text=hi",
            "--param",
            "idempotency_key=k",
        ]))
        .expect("parse");
        match inv.command {
            Command::Invoke { spec, params } => {
                assert_eq!(spec.id, "msg.send");
                assert_eq!(params["conv"], "g-1");
                assert_eq!(params["text"], "hi");
            }
            other => panic!("{other:?}"),
        }
    }

    /// 三段写法（`video job get`）与两段（`video job.get`）等价。
    #[test]
    fn multi_segment_verbs_join_with_dots() {
        for form in [&["video", "job", "get"][..], &["video", "job.get"][..]] {
            let inv = parse(&args(&[form, &["--param", "job_id=j1"][..]].concat())).expect("parse");
            match inv.command {
                Command::Invoke { spec, .. } => assert_eq!(spec.id, "video.job.get"),
                other => panic!("{other:?}"),
            }
        }
    }

    /// 字符串参数里的数字必须保持字符串：`conv=123` ≠ `conv: 123`。
    #[test]
    fn param_types_follow_the_catalog_not_the_text() {
        let inv = parse(&args(&[
            "conv", "history", "--param", "conv=123", "--param", "limit=20",
        ]))
        .expect("parse");
        match inv.command {
            Command::Invoke { params, .. } => {
                assert!(params["conv"].is_string(), "字符串参数不能变成数字");
                assert_eq!(params["limit"], 20);
            }
            other => panic!("{other:?}"),
        }

        let err = parse(&args(&[
            "conv",
            "history",
            "--param",
            "conv=g-1",
            "--param",
            "limit=abc",
        ]))
        .unwrap_err();
        assert!(err.0.contains("需要整数"), "{err}");
    }

    #[test]
    fn unknown_param_fails_before_any_request() {
        let err = parse(&args(&[
            "conv", "unread", "--param", "conv=g-1", "--param", "cnv=g-1",
        ]))
        .unwrap_err();
        assert!(err.0.contains("cnv"), "{err}");
        assert!(err.0.contains("conv"), "要列出可用参数：{err}");
    }

    #[test]
    fn coverage_is_a_subcommand_and_not_a_domain() {
        assert_eq!(
            parse(&args(&["coverage"])).unwrap().command,
            Command::Coverage
        );
        assert!(parse(&args(&["coverage", "--json"])).unwrap().options.json);
    }

    #[test]
    fn legacy_subcommands_still_work() {
        assert_eq!(
            parse(&args(&["list", "unread"])).unwrap().command,
            Command::List("unread".into())
        );
        assert_eq!(parse(&args(&["policy"])).unwrap().command, Command::Policy);
        assert_eq!(
            parse(&args(&["inspect"])).unwrap().command,
            Command::Inspect
        );
        assert_eq!(parse(&args(&[])).unwrap().command, Command::Help(None));
        assert_eq!(
            parse(&args(&["help", "video"])).unwrap().command,
            Command::Help(Some("video".into()))
        );
    }

    #[test]
    fn run_requires_at_least_one_cmd() {
        let err = parse(&args(&["run"])).unwrap_err();
        assert!(err.0.contains("--cmd"), "{err}");
        let inv = parse(&args(&["run", "--cmd", "me.info"])).expect("parse");
        assert_eq!(inv.command, Command::Run(vec!["me.info".into()]));
    }

    #[test]
    fn global_flags_are_collected() {
        let inv = parse(&args(&[
            "--server",
            "http://x",
            "--token",
            "t",
            "--json",
            "--allow-write",
            "--allow-conv",
            "g-1",
            "policy",
        ]))
        .expect("parse");
        assert_eq!(inv.options.server.as_deref(), Some("http://x"));
        assert_eq!(inv.options.token.as_deref(), Some("t"));
        assert!(inv.options.json && inv.options.allow_write);
        assert_eq!(inv.options.allow_conv, vec!["g-1".to_string()]);
    }

    #[test]
    fn unknown_flag_and_unknown_command_are_usage_errors() {
        assert!(parse(&args(&["--nope"]))
            .unwrap_err()
            .0
            .contains("未知参数"));
        assert!(parse(&args(&["nope", "nada"]))
            .unwrap_err()
            .0
            .contains("未知命令"));
    }

    #[test]
    fn params_json_object_merges_with_param_flags() {
        let inv =
            parse(&args(&["conv", "unread", "--params", r#"{"conv":"g-1"}"#])).expect("parse");
        match inv.command {
            Command::Invoke { params, .. } => assert_eq!(params["conv"], "g-1"),
            other => panic!("{other:?}"),
        }
        let err = parse(&args(&["conv", "unread", "--params", "[1,2]"])).unwrap_err();
        assert!(err.0.contains("JSON 对象"), "{err}");
    }
}
