//! `wxlike-mcp`：把命令面包装成 MCP 工具（stdio JSON-RPC）。
//!
//! 这一层**不含业务逻辑**，只做三件事：读一行 JSON-RPC、按 method 分派、
//! 把结果包成 MCP 的 `content` 形状。执行全部交给
//! [`wxlike_client::execute::Executor`]——与 `wxlike-cli` 是同一份。
//!
//! 只暴露三个元工具（WordCraft 同款思路）：
//! * `im_list_commands` —— 能力地图，让 agent 自己发现；
//! * `im_execute`       —— 统一执行入口，不堆几十个工具定义；
//! * `im_inspect`       —— 结构化自查，不用截图。
//!
//! 权限由启动时的环境变量注入（`WXLIKE_AGENT_ALLOW_*`），命令实现不知道
//! 自己有没有权限——那是 `Policy` 的事。

use std::io::{BufRead as _, Write as _};
use std::process::ExitCode;

use serde_json::{json, Value};
use wxlike_client::execute::Executor;
use wxlike_client::Http;
use wxlike_cmd::policy::Policy;
use wxlike_cmd::{search, CATALOG};

/// MCP 协议版本。客户端会用自己支持的版本协商，这里报我们实现的那个。
const PROTOCOL_VERSION: &str = "2024-11-05";

fn main() -> ExitCode {
    let policy = Policy::from_env();
    let server = std::env::var("WXLIKE_SERVER").unwrap_or_else(|_| "http://127.0.0.1:8099".into());
    let mut http = Http::new(server);
    http.token = std::env::var("WXLIKE_TOKEN").ok();

    if http.token.is_none() {
        // 没有 token 不是致命错误：`tools/list` 仍然应该能回答（agent 先看能力）。
        // 但执行会被服务端拒绝，所以在这里提醒一句，别让它变成谜之 401。
        eprintln!("[wxlike-mcp] 警告：未设置 WXLIKE_TOKEN，执行类工具会被服务端拒绝");
    }
    eprintln!(
        "[wxlike-mcp] 就绪：{} 条命令，白名单 {}，写={} 破坏性={}",
        CATALOG.len(),
        if policy.conv_patterns().is_empty() {
            "（空）".to_string()
        } else {
            policy.conv_patterns().join(",")
        },
        policy.allow_write(),
        policy.allow_destructive()
    );

    let exec = Executor::new(http, policy);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(resp) = handle_line(&exec, &line) {
            if writeln!(stdout, "{resp}").is_err() {
                break;
            }
            let _ = stdout.flush();
        }
    }
    ExitCode::SUCCESS
}

/// 处理一行 JSON-RPC。返回 `None` = 通知（不需要回包）。
pub fn handle_line(exec: &Executor, line: &str) -> Option<String> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(
                json!({"jsonrpc":"2.0","id":Value::Null,
                       "error":{"code":-32700,"message":format!("parse error: {e}")}})
                .to_string(),
            )
        }
    };
    let id = msg.get("id").cloned();
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));

    // 通知没有 id，不回包（MCP 的 initialized 就是这样）。
    let id = id?;

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "wxlike", "version": env!("CARGO_PKG_VERSION")},
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => Ok(call_tool(exec, &params)),
        other => Err((-32601, format!("method not found: {other}"))),
    };

    Some(match result {
        Ok(v) => json!({"jsonrpc":"2.0","id":id,"result":v}).to_string(),
        Err((code, message)) => {
            json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}).to_string()
        }
    })
}

fn tools() -> Value {
    json!([
        {
            "name": "im_list_commands",
            "description": "列出可用的 IM 命令（id / 一句说明 / 参数 / 能力等级）。先用它发现能力，再 im_execute。",
            "inputSchema": {
                "type": "object",
                "properties": {"query": {"type": "string", "description": "按 id 或说明过滤，空=全部"}}
            }
        },
        {
            "name": "im_execute",
            "description": "执行一条 IM 命令。command 是 im_list_commands 给出的 id，params 是它的参数对象。",
            "inputSchema": {
                "type": "object",
                "required": ["command"],
                "properties": {
                    "command": {"type": "string"},
                    "params": {"type": "object"}
                }
            }
        },
        {
            "name": "im_inspect",
            "description": "不用截图就能自查：我是谁、我有哪些会话、各自未读多少。",
            "inputSchema": {"type": "object", "properties": {}}
        }
    ])
}

/// `tools/call` 的返回。执行失败按 MCP 约定用 `isError`，**不是** JSON-RPC error——
/// 这样 agent 能读到失败原因并自己纠正，而不是把它当成协议故障。
fn call_tool(exec: &Executor, params: &Value) -> Value {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match name {
        "im_list_commands" => {
            let q = args.get("query").and_then(Value::as_str).unwrap_or("");
            let items: Vec<Value> = search(q)
                .into_iter()
                .map(|s| {
                    json!({
                        "id": s.id,
                        "summary": s.summary,
                        "params": s.params,
                        "capability": s.capability.as_str(),
                    })
                })
                .collect();
            text_ok(json!({ "count": items.len(), "commands": items }))
        }
        "im_execute" => {
            let Some(cmd) = args.get("command").and_then(Value::as_str) else {
                return text_err("im_execute 需要 command 参数");
            };
            let cmd_params = args.get("params").cloned().unwrap_or_else(|| json!({}));
            match exec.run(cmd, &cmd_params) {
                Ok(v) => text_ok(v),
                Err(e) => text_err(&e),
            }
        }
        "im_inspect" => {
            let mut out = serde_json::Map::new();
            for id in ["me.info", "conv.list"] {
                match exec.run(id, &json!({})) {
                    Ok(v) => {
                        out.insert(id.to_string(), v);
                    }
                    Err(e) => return text_err(&format!("{id}: {e}")),
                }
            }
            text_ok(Value::Object(out))
        }
        other => text_err(&format!("unknown tool: {other}")),
    }
}

fn text_ok(value: Value) -> Value {
    json!({
        "content": [{"type": "text", "text": serde_json::to_string_pretty(&value).unwrap_or_default()}],
        "isError": false
    })
}

fn text_err(message: &str) -> Value {
    json!({
        "content": [{"type": "text", "text": message}],
        "isError": true
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec(policy: Policy) -> Executor {
        Executor::new(Http::new("http://127.0.0.1:1"), policy)
    }

    fn call(exec: &Executor, line: &str) -> Value {
        serde_json::from_str(&handle_line(exec, line).expect("response")).unwrap()
    }

    #[test]
    fn initialize_reports_protocol_and_server() {
        let e = exec(Policy::none());
        let v = call(&e, r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#);
        assert_eq!(v["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(v["result"]["serverInfo"]["name"], "wxlike");
        assert!(v["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn tools_list_exposes_exactly_three_meta_tools() {
        let e = exec(Policy::none());
        let v = call(&e, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
        let names: Vec<&str> = v["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["im_list_commands", "im_execute", "im_inspect"]);
    }

    #[test]
    fn list_commands_can_filter() {
        let e = exec(Policy::none());
        let v = call(
            &e,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"im_list_commands","arguments":{"query":"unread"}}}"#,
        );
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["count"], 1);
        assert_eq!(parsed["commands"][0]["id"], "conv.unread");
    }

    /// 权限拒绝必须是 `isError: true` 的**工具结果**，而不是 JSON-RPC 错误：
    /// agent 要能读到原因并纠正。
    #[test]
    fn denied_command_is_a_tool_error_not_a_protocol_error() {
        let e = exec(Policy::none());
        let v = call(
            &e,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"im_execute","arguments":{"command":"conv.unread","params":{"conv":"d-a~b"}}}}"#,
        );
        assert!(v.get("error").is_none(), "不该是协议错误: {v}");
        assert_eq!(v["result"]["isError"], true);
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("not allowed"), "{text}");
    }

    #[test]
    fn unknown_method_is_a_protocol_error() {
        let e = exec(Policy::none());
        let v = call(&e, r#"{"jsonrpc":"2.0","id":5,"method":"nope"}"#);
        assert_eq!(v["error"]["code"], -32601);
    }

    #[test]
    fn notifications_get_no_reply() {
        let e = exec(Policy::none());
        assert!(handle_line(
            &e,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
        )
        .is_none());
    }

    #[test]
    fn broken_json_reports_parse_error_with_null_id() {
        let e = exec(Policy::none());
        let v = call(&e, "{not json");
        assert_eq!(v["error"]["code"], -32700);
        assert!(v["id"].is_null());
    }
}
