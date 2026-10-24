//! MCP 协议核心：**一帧请求进、一帧响应出**，不碰 stdin/stdout。
//!
//! 拆出来是为了可测：本地 stdio 与（将来的）远程 transport 都只是"搬运字节"，
//! 协议语义只有这一份实现。G15-05 的契约 fixture 直接喂这里。
//!
//! 覆盖的方法：
//! * `initialize` / `ping`
//! * `tools/list` / `tools/call`（每个命令一个工具，schema 由 catalog 生成）
//! * `resources/list` / `resources/read`（catalog、覆盖表、策略、单条动作 schema）
//!
//! 危险动作走 `prepare → confirmation_token → execute`（G15-04）：
//! 第一次调用只回计划与令牌，**不执行**；带令牌再调才真正执行，且令牌一次性。
//!
//! 刻意不做的事：
//! * **不实现第二套 MCP 框架**——协议层是手写的 JSON-RPC 分派，官方 `rmcp`
//!   的接入申请写在 `integration-requests/G15/G15-01.yaml`（根依赖与锁归 G00）；
//! * **不把执行搬进来**——真正干活的是 `wxlike_client::execute::Executor`。

use serde_json::{json, Value};

use wxlike_client::execute::Executor;
use wxlike_cmd::auth::{ActorContext, AuthGate, Provenance};
use wxlike_cmd::catalog::{self, ArgError, CommandSpec};
use wxlike_cmd::confirmation::ConfirmationStore;
use wxlike_cmd::jobs::{JobRegistry, LONG_RUNNING_ACTION};
use wxlike_cmd::policy::Policy;

/// 我们实现的协议版本，**从新到旧**。客户端报的版本在这个表里就按它回。
pub const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// 默认协商版本（客户端没报或报了个我们不认识的，就回这个）。
pub const DEFAULT_PROTOCOL: &str = PROTOCOL_VERSIONS[0];

/// 元工具：不执行具体业务，只做发现、自查与作业账。
const META_TOOLS: [&str; 4] = ["im_list_commands", "im_execute", "im_inspect", "im_jobs"];

/// 没有配 `--actor/--session-id` 时的本地身份。
///
/// 它不是信任边界（服务端仍会再验），只是为了让确认令牌**永远绑定到某个
/// 主体与会话**——"没有身份"不能成为跳过绑定的理由。
const LOCAL_ACTOR: &str = "local-agent";
const LOCAL_SESSION: &str = "local-session";

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
    /// 一次性确认令牌（G15-04）。进程内即可：跨进程就是换了会话。
    confirm: ConfirmationStore,
    /// 长任务账（G15-04）：可查、可取消、输出脱敏。
    jobs: JobRegistry,
    /// 请求序号：每个 `tools/call` 一个 request_id / operation_id，便于审计追踪。
    seq: u64,
    protocol: &'static str,
    initialized: bool,
}

impl Server {
    pub fn new(exec: Executor, policy: Policy) -> Self {
        Self {
            exec,
            policy,
            gate: None,
            confirm: ConfirmationStore::new(),
            jobs: JobRegistry::new(),
            seq: 0,
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

    fn tools_call(&mut self, params: &Value) -> Result<Value, RpcError> {
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
        let spec = self.resolve_tool(name)?;
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
        Ok(self.execute_checked(spec, &args))
    }

    fn resolve_tool(&self, name: &str) -> Result<&'static CommandSpec, RpcError> {
        catalog::CATALOG
            .iter()
            .find(|s| s.tool_name() == name)
            .ok_or_else(|| {
                RpcError::new(
                    code::INVALID_PARAMS,
                    format!("unknown tool {name:?}（先 tools/list）"),
                )
            })
    }

    /// 过完协议与授权检查之后：先过确认，再执行，最后记作业账与追踪 id。
    fn execute_checked(&mut self, spec: &'static CommandSpec, args: &Value) -> Value {
        if spec.confirmation.is_required() {
            match self.confirm_step(spec, args) {
                Ok(Some(plan_only)) => return plan_only,
                Ok(None) => {}
                Err(e) => return tool_err(&e.to_string()),
            }
        }
        self.launch(spec, args)
    }

    /// 确认这一步：没令牌就出计划，令牌不对就拒（**都不执行**）。
    fn confirm_step(
        &mut self,
        spec: &'static CommandSpec,
        args: &Value,
    ) -> Result<Option<Value>, wxlike_cmd::confirmation::ConfirmError> {
        let ctx = self.actor_ctx();
        let token = args
            .get("confirmation_token")
            .and_then(Value::as_str)
            .filter(|t| !t.trim().is_empty());
        let Some(raw) = token else {
            let prepared = self.confirm.prepare(&ctx, spec, args, now())?;
            return Ok(Some(tool_ok(&prepared.to_json())));
        };
        self.confirm.redeem(&ctx, spec, args, raw, now())?;
        Ok(None)
    }

    /// 真正执行 + 作业账 + request/operation id。
    fn launch(&mut self, spec: &CommandSpec, args: &Value) -> Value {
        self.seq += 1;
        let operation_id = format!("op-{}-{}", self.seq, spec.id);
        let mut result = run(&self.exec, spec, args);
        if result["isError"] != json!(false) {
            return result;
        }
        self.record_job(spec, &operation_id, &result);
        result["_meta"] = json!({
            "wxlike/requestId": format!("req-{}", self.seq),
            "wxlike/operationId": operation_id,
        });
        result
    }

    /// 身份上下文：配了闸门用闸门的，否则用本地身份（仍然绑定，不豁免）。
    fn actor_ctx(&self) -> ActorContext {
        match &self.gate {
            Some(gate) => gate.context().clone(),
            None => wxlike_cmd::auth::context_from_parts(LOCAL_ACTOR, LOCAL_SESSION, "conv:*"),
        }
    }

    /// 只有长任务进作业账：一次就结束的写命令记进去只会让账变噪音。
    fn record_job(&mut self, spec: &CommandSpec, operation_id: &str, result: &Value) {
        if spec.id != LONG_RUNNING_ACTION {
            return;
        }
        let job = self.jobs.submit(spec.id, operation_id, now());
        let output = result["structuredContent"].clone();
        if output.is_null() {
            return;
        }
        let _ = self.jobs.finish(&job.id, output, now());
    }

    fn meta_call(&mut self, name: &str, args: &Value) -> Value {
        match name {
            "im_list_commands" => {
                let q = args.get("query").and_then(Value::as_str).unwrap_or("");
                tool_ok(&catalog_list(q))
            }
            "im_inspect" => inspect(&self.exec),
            "im_jobs" => self.jobs_call(args),
            // 兼容旧客户端：保留统一入口，但仍然按 catalog 校验参数
            _ => execute_one(&self.exec, args),
        }
    }

    /// 作业账：`list` / `get` / `cancel`。取消只改本地账，远端是否停住如实回报。
    fn jobs_call(&mut self, args: &Value) -> Value {
        let action = args.get("action").and_then(Value::as_str).unwrap_or("list");
        let job_id = args.get("job_id").and_then(Value::as_str);
        match (action, job_id) {
            ("list", _) => self.jobs_list(),
            ("get", Some(id)) => self.jobs_get(id),
            ("cancel", Some(id)) => self.jobs_cancel(id),
            ("get" | "cancel", None) => tool_err("im_jobs 的 get/cancel 需要 job_id"),
            (other, _) => tool_err(&format!(
                "unknown im_jobs action {other:?}（list/get/cancel）"
            )),
        }
    }

    fn jobs_list(&self) -> Value {
        let items: Vec<Value> = self.jobs.list().iter().map(|j| j.to_json()).collect();
        tool_ok(&json!({"count": items.len(), "jobs": items}))
    }

    fn jobs_get(&self, id: &str) -> Value {
        match self.jobs.get(id) {
            Some(job) => tool_ok(&job.to_json()),
            None => tool_err(&format!("unknown job {id:?}")),
        }
    }

    fn jobs_cancel(&mut self, id: &str) -> Value {
        match self.jobs.cancel(id, now()) {
            Ok(outcome) => tool_ok(&outcome.to_json()),
            Err(e) => tool_err(&e.to_string()),
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
                "uri": "wxlike://coverage",
                "name": "业务覆盖报告",
                "description": "业务域 → 入口 → 状态（自动 / 宿主能力 / 未做，未做带负责组与 issue）",
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
            "wxlike://coverage" => (
                serde_json::to_string_pretty(&wxlike_cmd::domains::report()).unwrap_or_default(),
                "application/json",
            ),
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

/// Unix 秒。确认令牌与作业账都用同一把时钟，测试里换成显式时间。
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
        "im_jobs" => jobs_tool(),
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

fn jobs_tool() -> Value {
    let action = json!({
        "type": "string",
        "enum": ["list", "get", "cancel"],
        "description": "默认 list"
    });
    let job_id = json!({"type": "string", "description": "get/cancel 必填"});
    let mut properties = serde_json::Map::new();
    properties.insert("action".into(), action);
    properties.insert("job_id".into(), job_id);
    json!({
        "name": "im_jobs",
        "description": "长任务账：list 看全部、get 看单个、cancel 请求取消（远端是否停住会如实回报）。",
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": properties
        },
        "annotations": {"readOnlyHint": false, "destructiveHint": false}
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
        assert_eq!(
            uris,
            ["wxlike://catalog", "wxlike://coverage", "wxlike://policy"]
        );

        for uri in [
            "wxlike://catalog",
            "wxlike://coverage",
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

    /// 危险动作：第一次只给计划与令牌；带令牌才执行；令牌一次性。
    #[test]
    fn dangerous_action_needs_confirmation_and_the_token_is_single_use() {
        let policy = Policy::new(["g-1"], true, true);
        let exec = Executor::new(Http::new("http://127.0.0.1:1"), policy.clone());
        let mut srv = Server::new(exec, policy);
        call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":0,"method":"initialize"}"#,
        );

        let ask = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"group-kick","arguments":{"conv":"g-1","user_id":"u2"}}}"#,
        );
        assert_eq!(ask["result"]["isError"], false, "第一次不该报错：{ask}");
        let plan = &ask["result"]["structuredContent"];
        assert_eq!(plan["confirmation_required"], true);
        let token = plan["token"].as_str().expect("token").to_string();
        assert!(token.starts_with("wxc-"), "{token}");
        assert_eq!(plan["plan"]["action_id"], "group.kick");

        // 带令牌 → 真的执行（这里网络不可达，所以是 connect 错误，而不是确认错误）
        let executed = call(
            &mut srv,
            &format!(
                r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"group-kick","arguments":{{"conv":"g-1","user_id":"u2","confirmation_token":"{token}"}}}}}}"#
            ),
        );
        let text = executed["result"]["content"][0]["text"]
            .as_str()
            .expect("text");
        assert!(!text.contains("confirmation"), "不该再要求确认：{text}");

        // 重放同一个令牌 → 拒
        let replay = call(
            &mut srv,
            &format!(
                r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"group-kick","arguments":{{"conv":"g-1","user_id":"u2","confirmation_token":"{token}"}}}}}}"#
            ),
        );
        let text = replay["result"]["content"][0]["text"]
            .as_str()
            .expect("text");
        assert!(text.contains("already used"), "重放要拒：{text}");
    }

    /// 换了成员（等价于换收款人/换金额）就必须重新确认。
    #[test]
    fn confirmation_does_not_survive_a_parameter_change() {
        let policy = Policy::new(["g-1"], true, true);
        let exec = Executor::new(Http::new("http://127.0.0.1:1"), policy.clone());
        let mut srv = Server::new(exec, policy);
        call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":0,"method":"initialize"}"#,
        );

        let ask = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"group-kick","arguments":{"conv":"g-1","user_id":"u-bob"}}}"#,
        );
        let token = ask["result"]["structuredContent"]["token"]
            .as_str()
            .expect("token")
            .to_string();
        let changed = call(
            &mut srv,
            &format!(
                r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"group-kick","arguments":{{"conv":"g-1","user_id":"u-evil","confirmation_token":"{token}"}}}}}}"#
            ),
        );
        let text = changed["result"]["content"][0]["text"]
            .as_str()
            .expect("text");
        assert!(text.contains("parameters changed"), "{text}");
    }

    /// 作业账：空表、未知作业、缺 job_id 都要有明确回复。
    #[test]
    fn jobs_meta_tool_answers_list_get_and_cancel() {
        let mut srv = ready();
        let list = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"im_jobs","arguments":{"action":"list"}}}"#,
        );
        let text = list["result"]["content"][0]["text"].as_str().expect("text");
        let parsed: Value = serde_json::from_str(text).expect("json");
        assert_eq!(parsed["count"], 0);

        let cancel = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"im_jobs","arguments":{"action":"cancel","job_id":"job-nope"}}}"#,
        );
        assert_eq!(cancel["result"]["isError"], true);
        let text = cancel["result"]["content"][0]["text"]
            .as_str()
            .expect("text");
        assert!(text.contains("unknown job"), "{text}");

        let missing = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"im_jobs","arguments":{"action":"get"}}}"#,
        );
        assert_eq!(missing["result"]["isError"], true);
    }

    #[test]
    fn notifications_get_no_reply() {
        let mut srv = ready();
        assert!(srv
            .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none());
    }
}
