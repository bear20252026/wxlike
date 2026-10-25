//! `wxlike-mcp`：把命令面包装成 MCP 服务（stdio JSON-RPC）。
//!
//! 这一层**不含业务逻辑**：解析一帧、交给 [`server::Server`]、写回一帧；
//! 执行全部交给 [`wxlike_client::execute::Executor`]——与 `wxlike-cli` 是同一份。
//!
//! 工具面由 catalog 生成：**一条命令一个工具**（`msg.send` → `msg-send`），
//! 外加三个元工具（发现 / 兼容执行 / 自查）。不再手写 `tools/list`。
//!
//! 权限由启动时的环境变量或命令行开关注入（`WXLIKE_AGENT_ALLOW_*` /
//! `--allow-conv` / `--allow-write` / `--allow-destructive`），命令实现不知道
//! 自己有没有权限——那是 `Policy` 的事。

use std::process::ExitCode;

use wxlike_client::execute::Executor;
use wxlike_client::Http;
use wxlike_cmd::auth::{AuthGate, ScopeSet};
use wxlike_cmd::policy::Policy;
use wxlike_cmd::CATALOG;

use wxlike_mcp::server::{self, Server};
use wxlike_mcp::transport::{serve_stdio, Transport};

fn main() -> ExitCode {
    match Flags::parse(std::env::args().skip(1).collect()) {
        Ok(flags) => run(&flags),
        Err(e) => {
            eprintln!("[wxlike-mcp] {e}");
            ExitCode::from(2)
        }
    }
}

fn run(flags: &Flags) -> ExitCode {
    let transport = match Transport::parse(&flags.transport) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[wxlike-mcp] {e}");
            return ExitCode::from(2);
        }
    };

    let policy = flags.policy();
    let mut http = Http::new(flags.server.clone());
    http.token = flags.token.clone();
    if http.token.is_none() {
        // 没有 token 不是致命错误：`tools/list` 仍应能回答（agent 先看能力）。
        eprintln!("[wxlike-mcp] 警告：未设置 WXLIKE_TOKEN，执行类工具会被服务端拒绝");
    }
    eprintln!(
        "[wxlike-mcp] 就绪：{} 条命令，transport={:?}，协议={}，写={} 破坏性={}",
        CATALOG.len(),
        transport,
        server::DEFAULT_PROTOCOL,
        policy.allow_write(),
        policy.allow_destructive()
    );

    let gate = match flags.gate() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("[wxlike-mcp] 授权配置错误：{e}");
            return ExitCode::from(2);
        }
    };
    if gate.is_none() {
        eprintln!(
            "[wxlike-mcp] 未配置 --scope：会话/scope 判定交由服务端与本地策略；远程 transport 必须配置"
        );
    }
    let mut srv = Server::new(Executor::new(http, policy.clone()), policy).with_gate(gate);
    if flags.print_tools {
        // 诊断用：把 tools/list 打到 stdout 后退出，便于对 fixture 比快照
        println!("{}", srv.tools_dump());
        return ExitCode::SUCCESS;
    }

    match transport {
        Transport::Stdio => {
            serve_stdio(&mut srv);
            ExitCode::SUCCESS
        }
    }
}

/// 命令行开关。所有带值的 flag 都形如 `--name value`。
#[derive(Debug, Default)]
struct Flags {
    transport: String,
    server: String,
    token: Option<String>,
    allow_conv: Vec<String>,
    allow_write: bool,
    allow_destructive: bool,
    print_tools: bool,
    actor: String,
    session_id: Option<String>,
    scope: Option<String>,
    origin_allow: Vec<String>,
}

impl Flags {
    fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut f = Flags {
            transport: "stdio".into(),
            server: std::env::var("WXLIKE_SERVER")
                .unwrap_or_else(|_| "http://127.0.0.1:8099".into()),
            token: std::env::var("WXLIKE_TOKEN").ok(),
            actor: std::env::var("WXLIKE_ACTOR").unwrap_or_else(|_| "local-agent".into()),
            session_id: std::env::var("WXLIKE_SESSION_ID").ok(),
            scope: std::env::var("WXLIKE_SCOPE").ok(),
            ..Flags::default()
        };
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--print-tools" => {
                    f.print_tools = true;
                    i += 1;
                }
                "--allow-write" => {
                    f.allow_write = true;
                    i += 1;
                }
                "--allow-destructive" => {
                    f.allow_destructive = true;
                    i += 1;
                }
                flag @ ("--transport" | "--server" | "--token" | "--allow-conv" | "--actor"
                | "--session-id" | "--scope" | "--origin-allow") => {
                    let value = args
                        .get(i + 1)
                        .ok_or_else(|| format!("{flag} 需要一个值"))?
                        .clone();
                    f.apply_value_flag(flag, value);
                    i += 2;
                }
                other => return Err(format!("未知参数 {other:?}（--help 看用法）")),
            }
        }
        if f.server.trim().is_empty() {
            return Err("--server 不能为空".into());
        }
        Ok(f)
    }

    /// 带值 flag 的落位。单独一个方法，是为了让 `parse` 本体保持一层循环。
    fn apply_value_flag(&mut self, flag: &str, value: String) {
        match flag {
            "--transport" => self.transport = value,
            "--server" => self.server = value,
            "--token" => self.token = Some(value),
            "--actor" => self.actor = value,
            "--session-id" => self.session_id = Some(value),
            "--scope" => self.scope = Some(value),
            "--origin-allow" => self.origin_allow.push(value),
            _ => self.allow_conv.push(value),
        }
    }

    /// 会话/scope 闸门：没给 `--scope` 就不挂（本地单用户场景）。
    fn gate(&self) -> Result<Option<AuthGate>, String> {
        let Some(scope) = &self.scope else {
            return Ok(None);
        };
        if !ScopeSet::parse(scope).invalid().is_empty() {
            return Err(format!("--scope 里有无法解析的条目：{scope}"));
        }
        let session = self
            .session_id
            .clone()
            .ok_or_else(|| "--scope 需要配合 --session-id（会话必须绑定）".to_string())?;
        AuthGate::from_parts(
            &self.actor,
            &session,
            scope,
            self.session_id.as_deref(),
            self.origin_allow.clone(),
        )
        .map(Some)
        .map_err(|e| e.to_string())
    }

    /// 环境变量打底，命令行覆盖。stdout 是协议通道，提示只能走 stderr。
    fn policy(&self) -> Policy {
        let mut p = Policy::from_env();
        p.extend_convs(self.allow_conv.iter().cloned());
        if self.allow_write || self.allow_destructive {
            p.grant_write();
        }
        if self.allow_destructive {
            p.grant_destructive();
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flags_in_any_order() {
        let f = Flags::parse(vec![
            "--allow-conv".into(),
            "g-1".into(),
            "--transport".into(),
            "stdio".into(),
            "--print-tools".into(),
        ])
        .expect("parse");
        assert_eq!(f.transport, "stdio");
        assert_eq!(f.allow_conv, vec!["g-1".to_string()]);
        assert!(f.print_tools);
    }

    #[test]
    fn unknown_flag_is_a_startup_error() {
        let err = Flags::parse(vec!["--nope".into()]).unwrap_err();
        assert!(err.contains("未知参数"), "{err}");
    }

    #[test]
    fn gate_requires_a_session_and_parsable_scope() {
        let f = Flags::parse(vec!["--scope".into(), "conv:*".into()]).expect("parse");
        let err = f.gate().unwrap_err();
        assert!(err.contains("session-id"), "{err}");

        let f = Flags::parse(vec![
            "--scope".into(),
            "conv:* money:*".into(),
            "--session-id".into(),
            "s-1".into(),
        ])
        .expect("parse");
        assert!(f.gate().unwrap_err().contains("无法解析"), "未知类别要拒");

        let f = Flags::parse(vec![
            "--scope".into(),
            "conv:*".into(),
            "--session-id".into(),
            "s-1".into(),
        ])
        .expect("parse");
        assert!(f.gate().expect("gate").is_some());
    }

    #[test]
    fn destructive_flag_implies_write() {
        let f = Flags::parse(vec!["--allow-destructive".into()]).expect("parse");
        let p = f.policy();
        assert!(p.allow_destructive());
        assert!(p.allow_write(), "只开破坏性不开写是配置错误");
    }
}
