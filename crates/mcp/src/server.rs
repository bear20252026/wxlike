//! MCP 协议核心：**一帧请求进、一帧响应出**，不碰 stdin/stdout。
//!
//! 拆出来是为了可测：本地 stdio 与（将来的）远程 transport 都只是"搬运字节"，
//! 协议语义只有这一份实现。G15-05 的契约 fixture 直接喂这里。
//!
//! 覆盖的方法：
//! * `initialize` / `ping`
//! * `tools/list` / `tools/call`（每个命令一个工具，schema 由 catalog 生成）
//! * `resources/list` / `resources/read`（catalog、策略、单条动作 schema）
//!
//! 刻意不做的事：
//! * **不实现第二套 MCP 框架**——协议层是手写的 JSON-RPC 分派，官方 `rmcp`
//!   的接入申请写在 `integration-requests/G15/G15-01.yaml`（根依赖与锁归 G00）；
//! * **不把执行搬进来**——真正干活的是 `wxlike_client::execute::Executor`。

use serde_json::{json, Value};

use wxlike_client::execute::Executor;
use wxlike_cmd::auth::{AuthGate, Provenance};
use wxlike_cmd::catalog::{self, ArgError, CommandSpec};
use wxlike_cmd::policy::Policy;

/// 我们实现的协议版本，**从新到旧**。客户端报的版本在这个表里就按它回。
pub const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// 默认协商版本（客户端没报或报了个我们不认识的，就回这个）。
pub const DEFAULT_PROTOCOL: &str = PROTOCOL_VERSIONS[0];

/// 元工具：不执行具体业务，只做发现与自查。
const META_TOOLS: [&str; 3] = ["im_list_commands", "im_execute", "im_inspect"];

/// JSON-RPC / MCP 错误码。
pub mod code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    /// 还没 `initialize` 就调业务方法。
    pub const NOT_INITIALIZED: i64 = -32002;
}

/// 一个协议层错误。
#[derive(Debug, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// 服务器状态：执行器 + 策略 + 协商结果。
pub struct Server {
    exec: Executor,
    policy: Policy,
    /// 会话/scope 闸门（G15-02）。`None` = 只跑本地策略（stdio 单用户场景）。
    gate: Option<AuthGate>,
    protocol: &'static str,
    initialized: bool,
}

impl Server {
    pub fn new(exec: Executor, policy: Policy) -> Self {
        Self {
            exec,
            policy,
            gate: None,
            protocol: DEFAULT_PROTOCOL,
            initialized: false,
        }
    }

    /// 挂上会话/scope 闸门。**没挂不等于没权限**：服务端仍会再验一次，
    /// 但本地就不会提前拦——所以远程 transport 必须挂。
    pub fn with_gate(mut self, gate: Option<AuthGate>) -> Self {
        self.gate = gate;
        self
    }

    /// `tools/list` 的裸 JSON（诊断/夹具用，不需要先 `initialize`）。
    pub fn tools_dump(&self) -> String {
        serde_json::to_string_pretty(&self.tools_list()).unwrap_or_default()
    }

    /// 处理一帧（可能是一行 JSON）。返回 `None` = 通知，不该回包。
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                return Some(
                    error_frame(
                        Value::Null,
                        &RpcError::new(code::PARSE_ERROR, format!("parse error: {e}")),
                    )
                    .to_string(),
                )
            }
        };
        let reply = self.handle(&msg);
        reply.map(|v| v.to_string())
    }

    /// 处理一帧已解析的 JSON。通知（无 `id`）一律返回 `None`。
    pub fn handle(&mut self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned()?;
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
        if method.is_empty() {
            return Some(error_frame(
                id,
                &RpcError::new(code::INVALID_REQUEST, "invalid request: missing method"),
            ));
        }
        Some(match self.dispatch(method, &params) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(e) => error_frame(id, &e),
        })
    }

    fn dispatch(&mut self, method: &str, params: &Value) -> Result<Value, RpcError> {
        // 除 initialize / ping 之外，业务方法必须先握手。
        if !self.initialized && !matches!(method, "initialize" | "ping") {
            return Err(RpcError::new(
                code::NOT_INITIALIZED,
                "server not initialized: send initialize first",
            ));
        }
        match method {
            "initialize" => Ok(self.initialize(params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(self.tools_list()),
            "tools/call" => self.tools_call(params),
            "resources/list" => Ok(self.resources_list()),
            "resources/read" => self.resources_read(params),
            other => Err(RpcError::new(
                code::METHOD_NOT_FOUND,
                format!("method not found: {other}"),
            )),
        }
    }

    /// 版本协商：客户端报的我们能做就照它，否则回我们最新的。
    fn initialize(&mut self, params: &Value) -> Value {
        let requested = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or_default();
        self.protocol = PROTOCOL_VERSIONS
            .iter()
            .find(|v| **v == requested)
            .copied()
            .unwrap_or(DEFAULT_PROTOCOL);
        self.initialized = true;
        json!({
            "protocolVersion": self.protocol,
            "capabilities": {
                "tools": {"listChanged": false},
                "resources": {"subscribe": false, "listChanged": false},
            },
            "serverInfo": {
                "name": "wxlike",
                "title": "wxlike 命令面",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "instructions": format!(
                "先 im_list_commands 发现能力，再按工具名调用。共 {} 条命令；\
                 写操作需要启动时授予 --allow-write，破坏性操作另需 --allow-destructive。",
                catalog::CATALOG.len()
            ),
        })
    }

    /// 每个命令一个工具 + 三个元工具。顺序稳定（catalog 顺序），便于快照。
    fn tools_list(&self) -> Value {
        let mut tools: Vec<Value> = META_TOOLS.iter().map(|name| meta_tool(name)).collect();
        tools.extend(catalog::CATALOG.iter().map(catalog::tool_definition));
        json!({ "tools": tools })
    }

    fn tools_call(&self, params: &Value) -> Result<Value, RpcError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::new(code::INVALID_PARAMS, "tools/call 缺 name"))?;
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));

        if META_TOOLS.contains(&name) {
            return Ok(self.meta_call(name, &args));
        }
        let spec = catalog::CATALOG
            .iter()
            .find(|s| s.tool_name() == name)
            .ok_or_else(|| {
                RpcError::new(
                    code::INVALID_PARAMS,
                    format!("unknown tool {name:?}（先 tools/list）"),
                )
            })?;
        // 未知/缺失/类型不符一律协议级拒绝：这不是"业务失败"，是调用方拼错了。
        catalog::validate_args(spec, &args).map_err(arg_error)?;
        // 来源可信度**不依赖是否配了 scope**：聊天正文里的指令永远不能触发写。
        if let Err(e) = wxlike_cmd::auth::guard_provenance(spec, provenance_of(params)) {
            return Ok(tool_err(&e.to_string()));
        }
        // 会话与 scope：先于执行，且拒绝是**工具错误**（调用方能读懂并改 token）
        if let Some(gate) = &self.gate {
            if let Err(e) = gate.check(spec, &args, provenance_of(params)) {
                return Ok(tool_err(&e.to_string()));
            }
        }
        Ok(run(&self.exec, spec, &args))
    }

    fn meta_call(&self, name: &str, args: &Value) -> Value {
        match name {
            "im_list_commands" => {
                let q = args.get("query").and_then(Value::as_str).unwrap_or("");
                tool_ok(&catalog_list(q))
            }
            "im_inspect" => inspect(&self.exec),
            // 兼容旧客户端：保留统一入口，但仍然按 catalog 校验参数
            _ => execute_one(&self.exec, args),
        }
    }

    fn resources_list(&self) -> Value {
        json!({"resources": [
            {
                "uri": "wxlike://catalog",
                "name": "动作目录",
                "description": "全部命令的 id / 能力 / 参数 / 平台 / 确认要求",
                "mimeType": "application/json",
            },
            {
                "uri": "wxlike://policy",
                "name": "当前生效策略",
                "description": "会话白名单与三个能力开关的判定结果（给人看的表格）",
                "mimeType": "text/plain",
            }
        ]})
    }

    fn resources_read(&self, params: &Value) -> Result<Value, RpcError> {
        let uri = params
            .get("uri")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::new(code::INVALID_PARAMS, "resources/read 缺 uri"))?;
        let (text, mime) = match uri {
            "wxlike://catalog" => (
                serde_json::to_string_pretty(&catalog_list("")).unwrap_or_default(),
                "application/json",
            ),
            "wxlike://policy" => (wxlike_cmd::policy::render(&self.policy), "text/plain"),
            other => match other.strip_prefix(catalog::ACTION_URI_PREFIX) {
                Some(id) => {
                    let spec = catalog::get(id).ok_or_else(|| {
                        RpcError::new(code::INVALID_PARAMS, format!("unknown action {id:?}"))
                    })?;
                    (
                        serde_json::to_string_pretty(&catalog::schema_of(spec)).unwrap_or_default(),
                        "application/json",
                    )
                }
                None => {
                    return Err(RpcError::new(
                        code::INVALID_PARAMS,
                        format!("unknown resource {other:?}"),
                    ))
                }
            },
        };
        Ok(json!({"contents": [{"uri": uri, "mimeType": mime, "text": text}]}))
    }
}

/// 参数来源：调用方可以在 `_meta` 里声明这批参数是从不可信内容抽出来的。
///
/// 默认 `Trusted` 是刻意的：这是**调用方自述**，不是安全边界；真正的边界是
/// 服务端再鉴权 + 确认令牌（G15-04）。但主动声明"这来自聊天正文"的调用方
/// 会立刻被拒，而不是等到出事。
fn provenance_of(params: &Value) -> Provenance {
    let declared = params
        .get("_meta")
        .and_then(|m| m.get("wxlike/provenance"))
        .and_then(Value::as_str);
    match declared {
        Some(v) if v.starts_with("untrusted") => Provenance::Untrusted("untrusted content"),
        _ => Provenance::Trusted,
    }
}

fn arg_error(e: ArgError) -> RpcError {
    RpcError::new(code::INVALID_PARAMS, e.to_string())
}

fn error_frame(id: Value, e: &RpcError) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": e.code, "message": e.message}})
}

/// 执行一条命令并包成 MCP 的工具结果。
///
/// 业务失败用 `isError: true`（agent 能读到原因并纠正），**不是**协议错误。
/// `structuredContent` 给严格客户端，`content[0].text` 给只读文本的客户端。
fn run(exec: &Executor, spec: &CommandSpec, args: &Value) -> Value {
    match exec.run(spec.id, args) {
        Ok(v) => tool_ok(&v),
        Err(e) => tool_err(&e),
    }
}

fn execute_one(exec: &Executor, args: &Value) -> Value {
    let Some(cmd) = args.get("command").and_then(Value::as_str) else {
        return tool_err("im_execute 需要 command 参数");
    };
    let params = args.get("params").cloned().unwrap_or_else(|| json!({}));
    let Some(spec) = catalog::get(cmd) else {
        return tool_err(&format!("unknown command {cmd:?}（先 im_list_commands）"));
    };
    if let Err(e) = catalog::validate_args(spec, &params) {
        return tool_err(&e.to_string());
    }
    run(exec, spec, &params)
}

fn inspect(exec: &Executor) -> Value {
    let mut out = serde_json::Map::new();
    for id in ["me.info", "conv.list"] {
        match exec.run(id, &json!({})) {
            Ok(v) => {
                out.insert(id.to_string(), v);
            }
            Err(e) => return tool_err(&format!("{id}: {e}")),
        }
    }
    tool_ok(&Value::Object(out))
}

fn catalog_list(query: &str) -> Value {
    let items: Vec<Value> = catalog::search(query)
        .into_iter()
        .map(|s| {
            json!({
                "id": s.id,
                "action_id": s.action_id(),
                "tool": s.tool_name(),
                "summary": s.summary,
                "params": s.params,
                "capability": s.capability.as_str(),
                "scope": s.scope.as_str(),
                "confirmation": if s.confirmation.is_required() { "required" } else { "none" },
                "idempotency": if s.idempotency.is_required() { "required" } else { "none" },
                "platforms": s.platforms.names(),
                "host": s.host.as_str(),
            })
        })
        .collect();
    json!({ "count": items.len(), "commands": items })
}

fn meta_tool(name: &str) -> Value {
    match name {
        "im_list_commands" => list_commands_tool(),
        "im_inspect" => inspect_tool(),
        _ => execute_tool(),
    }
}

fn list_commands_tool() -> Value {
    json!({
            "name": "im_list_commands",
            "description": "列出可用的 IM 命令（id / 工具名 / 参数 / 能力 / scope）。先用它发现能力，再按工具名调用。",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {"query": {"type": "string", "description": "按 id 或说明过滤，空=全部"}}
            },
            "annotations": {"readOnlyHint": true, "destructiveHint": false}
    })
}

fn inspect_tool() -> Value {
    json!({
            "name": "im_inspect",
            "description": "不用截图就能自查：我是谁、我有哪些会话、各自未读多少。",
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
            "annotations": {"readOnlyHint": true, "destructiveHint": false}
    })
}

fn execute_tool() -> Value {
    json!({
            "name": "im_execute",
            "description": "【兼容入口，建议改用按命令生成的工具】执行一条 IM 命令；参数同样按 catalog 校验。",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["command"],
                "properties": {
                    "command": {"type": "string", "description": "im_list_commands 给出的 id"},
                    "params": {"type": "object", "description": "该命令的参数对象"}
                }
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": true}
    })
}

fn tool_ok(value: &Value) -> Value {
    json!({
        "content": [{"type": "text", "text": serde_json::to_string_pretty(value).unwrap_or_default()}],
        "structuredContent": value,
        "isError": false
    })
}

fn tool_err(message: &str) -> Value {
    json!({
        "content": [{"type": "text", "text": message}],
        "isError": true
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxlike_client::Http;

    fn server(policy: Policy) -> Server {
        let exec = Executor::new(Http::new("http://127.0.0.1:1"), policy.clone());
        Server::new(exec, policy)
    }

    fn call(srv: &mut Server, line: &str) -> Value {
        let reply = srv.handle_line(line).expect("response");
        serde_json::from_str(&reply).expect("json")
    }

    /// 握过手才能用业务方法：这是 MCP 生命周期，不是可选项。
    fn ready() -> Server {
        let mut srv = server(Policy::none());
        let v = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
        );
        assert_eq!(v["result"]["protocolVersion"], "2025-06-18");
        srv
    }

    #[test]
    fn initialize_negotiates_known_version_and_falls_back_otherwise() {
        let mut srv = server(Policy::none());
        let v = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
        );
        assert_eq!(v["result"]["protocolVersion"], DEFAULT_PROTOCOL);
        assert_eq!(v["result"]["serverInfo"]["name"], "wxlike");
        assert!(v["result"]["capabilities"]["tools"].is_object());
        assert!(v["result"]["capabilities"]["resources"].is_object());

        let v = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2019-01-01"}}"#,
        );
        assert_eq!(
            v["result"]["protocolVersion"], DEFAULT_PROTOCOL,
            "不认识的版本回最新"
        );
    }

    #[test]
    fn business_methods_require_initialize() {
        let mut srv = server(Policy::none());
        let v = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        );
        assert_eq!(v["error"]["code"], code::NOT_INITIALIZED);
        // ping 不需要握手
        let v = call(&mut srv, r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#);
        assert_eq!(v["result"], json!({}));
    }

    #[test]
    fn tools_list_exposes_every_command_plus_meta_tools() {
        let mut srv = ready();
        let v = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#,
        );
        let tools = v["result"]["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), META_TOOLS.len() + catalog::CATALOG.len());
        for spec in catalog::CATALOG {
            let tool = tools
                .iter()
                .find(|t| t["name"] == spec.tool_name())
                .unwrap_or_else(|| panic!("缺工具 {}", spec.tool_name()));
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
            assert_eq!(tool["_meta"]["wxlike/actionId"], spec.id);
        }
    }

    #[test]
    fn list_commands_can_filter() {
        let mut srv = ready();
        let v = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"im_list_commands","arguments":{"query":"unread"}}}"#,
        );
        let text = v["result"]["content"][0]["text"].as_str().expect("text");
        let parsed: Value = serde_json::from_str(text).expect("json");
        assert_eq!(parsed["count"], 1);
        assert_eq!(parsed["commands"][0]["id"], "conv.unread");
        assert_eq!(parsed["commands"][0]["tool"], "conv-unread");
    }

    /// 拒绝必须是 `isError: true` 的**工具结果**，而不是 JSON-RPC 错误：
    /// agent 要能读到原因并纠正。
    #[test]
    fn denied_command_is_a_tool_error_not_a_protocol_error() {
        let mut srv = ready();
        let v = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"conv-unread","arguments":{"conv":"d-a~b"}}}"#,
        );
        assert!(v.get("error").is_none(), "不该是协议错误: {v}");
        assert_eq!(v["result"]["isError"], true);
        let text = v["result"]["content"][0]["text"].as_str().expect("text");
        assert!(text.contains("not allowed"), "{text}");
    }

    /// 参数拼错是**协议级**错误：调用方连 schema 都没遵守，不该重试业务。
    #[test]
    fn unknown_argument_is_invalid_params() {
        let mut srv = ready();
        let v = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"conv-unread","arguments":{"conv":"g-1","cnv":"typo"}}}"#,
        );
        assert_eq!(v["error"]["code"], code::INVALID_PARAMS);
        assert!(v["error"]["message"].as_str().expect("msg").contains("cnv"));

        let v = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"msg-send","arguments":{"conv":"g-1","text":"hi"}}}"#,
        );
        assert_eq!(
            v["error"]["code"],
            code::INVALID_PARAMS,
            "缺幂等键要拒: {v}"
        );
    }

    /// 未知工具名同样拒绝，且提示去 `tools/list`。
    #[test]
    fn unknown_tool_lists_the_right_next_step() {
        let mut srv = ready();
        let v = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"msg-send-for-ai","arguments":{}}}"#,
        );
        assert_eq!(v["error"]["code"], code::INVALID_PARAMS);
        assert!(v["error"]["message"]
            .as_str()
            .expect("msg")
            .contains("tools/list"));
    }

    #[test]
    fn resources_expose_catalog_policy_and_per_action_schema() {
        let mut srv = ready();
        let v = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":9,"method":"resources/list"}"#,
        );
        let uris: Vec<&str> = v["result"]["resources"]
            .as_array()
            .expect("resources")
            .iter()
            .map(|r| r["uri"].as_str().expect("uri"))
            .collect();
        assert_eq!(uris, ["wxlike://catalog", "wxlike://policy"]);

        for uri in [
            "wxlike://catalog",
            "wxlike://policy",
            "wxlike://actions/msg.send",
        ] {
            let frame = json!({
                "jsonrpc": "2.0", "id": 10, "method": "resources/read",
                "params": {"uri": uri}
            });
            let v = srv.handle(&frame).expect("response");
            assert!(v.get("error").is_none(), "{uri}: {v}");
            assert!(!v["result"]["contents"][0]["text"]
                .as_str()
                .expect("text")
                .is_empty());
        }

        let v = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":11,"method":"resources/read","params":{"uri":"wxlike://actions/nope"}}"#,
        );
        assert_eq!(v["error"]["code"], code::INVALID_PARAMS);
    }

    #[test]
    fn unknown_method_and_broken_json_keep_their_codes() {
        let mut srv = ready();
        let v = call(&mut srv, r#"{"jsonrpc":"2.0","id":12,"method":"nope"}"#);
        assert_eq!(v["error"]["code"], code::METHOD_NOT_FOUND);

        let v = call(&mut srv, "{not json");
        assert_eq!(v["error"]["code"], code::PARSE_ERROR);
        assert!(v["id"].is_null());

        let v = call(&mut srv, r#"{"jsonrpc":"2.0","id":13}"#);
        assert_eq!(v["error"]["code"], code::INVALID_REQUEST);
    }

    #[test]
    fn notifications_get_no_reply() {
        let mut srv = ready();
        assert!(srv
            .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none());
    }
}
