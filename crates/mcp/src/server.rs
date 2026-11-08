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
//! **但这条链在 MCP 上不是"问过人了"**：这里没有人在场，计划与令牌都回给
//! 同一个 agent，它自己能走完两步。它的价值是"参数摘要必须再核一遍"
//! （换人/换金额要重新确认、跨动作不能挪用），不是人的批准。
//! 真正要"只有人能做"的动作标 `Confirmation::HumanOnly`：本进程是
//! [`Channel::Agent`]，这类命令在 [`wxlike_cmd::auth::guard_channel`]
//! 处直接拒，**不签发也不兑换令牌**，错误消息让人去有人守着的界面里做。
//!
//! #320 之后，JSON-RPC 的分派与 MCP 的数据模型都由官方 **rmcp** 承担
//! （根 `Cargo.toml` 钉 3.8.0，只开 `server` / `transport-io`）：本文件不再自己
//! 数 `method` 字符串、不再自己拼 `{"jsonrpc":"2.0",...}` 错误帧、也不再自己
//! 解释 `initialize` 的版本协商。剩下的职责只有一层适配：
//! * 把 catalog 生成的工具表交给 rmcp 的 `Tool` 模型；
//! * 把 rmcp 解析出来的请求转给 `wxlike_client::execute::Executor`；
//! * 守住 wxlike 特有而 rmcp 不管的口径：未握手拒 `-32002`、一次性确认令牌、
//!   作业账、参数/响应上限、`_meta` 里的 contract-v1 信封。
//!
//! 帧的边界仍然由 [`crate::transport`] 一帧一帧地喂（`Server::handle_line`），
//! 所以**没有**引入第二个 MCP 框架，也**没有**因为"rmcp 自带 transport"就
//! 开出远程监听：rmcp 的 streamable-http / SSE feature 一个都没开，
//! `Transport::parse` 继续拒绝 http/sse（见 G15-02）。

use std::borrow::Cow;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ClientJsonRpcMessage,
    CompleteRequestMethod, CompleteRequestParams, CompleteResult, CustomRequest, CustomResult,
    ErrorCode, ErrorData as McpError, GetPromptRequestMethod, GetPromptRequestParams,
    GetPromptResponse, Implementation, InitializeRequestParams, InitializeResult,
    ListPromptsRequestMethod, ListPromptsResult, ListResourceTemplatesRequestMethod,
    ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
    ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
    RequestId, RequestMetaObject, ResourcesCapability, ServerCapabilities, ServerConfig,
    ServerJsonRpcMessage, Tool, ToolsCapability,
};
use rmcp::service::{serve_directly, RequestContext};
use rmcp::transport::Transport as RmcpTransport;
use rmcp::{RoleServer, ServerHandler};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use wxlike_types::{ActionResult, DomainError};

use wxlike_client::execute::Executor;
use wxlike_cmd::auth::{ActorContext, AuthGate, Channel, Provenance};
use wxlike_cmd::availability;
use wxlike_cmd::catalog::{self, ArgError, CommandSpec};
use wxlike_cmd::confirmation::ConfirmationStore;
use wxlike_cmd::contract;
use wxlike_cmd::jobs::{JobRegistry, LONG_RUNNING_ACTION};
use wxlike_cmd::policy::Policy;

/// 我们实现的协议版本，**从新到旧**。客户端报的版本在这个表里就按它回。
pub const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// 默认协商版本（客户端没报或报了个我们不认识的，就回这个）。
pub const DEFAULT_PROTOCOL: &str = PROTOCOL_VERSIONS[0];

/// 元工具：不执行具体业务，只做发现、自查与作业账。
const META_TOOLS: [&str; 4] = ["im_list_commands", "im_execute", "im_inspect", "im_jobs"];

/// 单次调用参数上限（64 KiB）。再大就不是"参数"，是把文件塞进 JSON。
const MAX_ARGS_BYTES: usize = 64 * 1024;

/// 工具文本响应上限（256 KiB）。超出就截断并**明确说明截断了**，
/// 免得调用方以为自己拿到了完整结果。
const MAX_TEXT_BYTES: usize = 256 * 1024;

/// 本服务实现的方法。rmcp 认得的方法名远不止这些（`tasks/*`、`prompts/*`…），
/// 已知方法落到 `on_custom_request` 就说明参数不符合 MCP 模型（`-32602`），
/// 未知方法才是 `-32601`——这张表就是两者的分界。
const SERVED_METHODS: [&str; 5] = [
    "ping",
    "tools/list",
    "tools/call",
    "resources/list",
    "resources/read",
];

/// 没有配 `--actor/--session-id` 时的本地身份。
///
/// 它不是信任边界（服务端仍会再验），只是为了让确认令牌**永远绑定到某个
/// 主体与会话**——"没有身份"不能成为跳过绑定的理由。
const LOCAL_ACTOR: &str = "local-agent";
const LOCAL_SESSION: &str = "local-session";

/// 本进程的通道身份。**写死，不是命令行开关**：一个能被调用方（或它的模型）
/// 改成 `Human` 的闸门等于没有闸门。将来若加远程 transport 或定时任务入口，
/// 各自在自己的进程里同样写死，不共用这个常量。
const CHANNEL: Channel = Channel::Agent;

/// JSON-RPC / MCP 错误码。
pub mod code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    /// rmcp 的循环已经收不到下一帧（会话结束）。
    pub const INTERNAL_ERROR: i64 = -32603;
    /// 还没 `initialize` 就调业务方法。
    pub const NOT_INITIALIZED: i64 = -32002;
}

/// 一个协议层错误。帧级错误（解析失败、会话结束）与业务侧拒绝都用它表达，
/// 出边界时统一换成 rmcp 的 `ErrorData`。
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

    fn to_mcp(&self) -> McpError {
        McpError::new(
            ErrorCode(self.code as i32),
            self.message.as_str().to_owned(),
            None,
        )
    }
}

/// 出边界时，协议层错误就是 rmcp 的 `ErrorData`：错误码与文案原样带过去，
/// 帧本身由 rmcp 序列化（这一层不再手拼 `{"jsonrpc":...}`）。
impl From<RpcError> for McpError {
    fn from(e: RpcError) -> Self {
        e.to_mcp()
    }
}

/// 服务器状态：一条 rmcp 会话 + 与它共享的执行状态。
pub struct Server {
    /// 与 rmcp 服务面共享的可变状态（锁在同步处理里取，跨 `.await` 不持有）。
    core: Arc<Mutex<Core>>,
    /// 塞进 rmcp 会话循环的帧。
    to_rmcp: mpsc::UnboundedSender<ClientJsonRpcMessage>,
    /// rmcp 会话循环产出的帧。
    from_rmcp: mpsc::UnboundedReceiver<ServerJsonRpcMessage>,
    /// rmcp 是异步的，而这一层的边界是同步的"一帧进一帧出"：
    /// 用一个当前线程运行时把两者接起来，不改变调用方的形态。
    rt: Option<tokio::runtime::Runtime>,
}

impl Server {
    pub fn new(exec: Executor, policy: Policy) -> Self {
        Self::start(exec, policy, None)
    }

    /// 挂上会话/scope 闸门。**没挂不等于没权限**：服务端仍会再验一次，
    /// 但本地就不会提前拦——所以远程 transport 必须挂。
    ///
    /// 改的是与 rmcp 服务面共享的状态；此刻会话循环还没收到任何帧。
    pub fn with_gate(self, gate: Option<AuthGate>) -> Self {
        self.lock().gate = gate;
        self
    }

    /// 当前会话协商到的协议版本（未握手时是默认口径）。
    pub fn protocol(&self) -> &'static str {
        self.lock().protocol
    }

    /// `tools/list` 的裸 JSON（诊断/夹具用，不需要先 `initialize`）。
    pub fn tools_dump(&self) -> String {
        let tools = tool_definitions();
        serde_json::to_string_pretty(&json!({ "tools": tools })).unwrap_or_default()
    }

    /// 处理一帧（可能是一行 JSON）。返回 `None` = 通知，不该回包。
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let raw: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(err) => {
                let e = RpcError::new(code::PARSE_ERROR, format!("parse error: {err}"));
                return Some(error_frame(None, &e));
            }
        };
        // rmcp 的消息模型负责判定这是请求、通知还是别的；读不进 MCP 的帧
        // 一律 INVALID_REQUEST（-32600），不静默当成通知吞掉。
        let msg = match serde_json::from_value::<ClientJsonRpcMessage>(raw.clone()) {
            Ok(m) => m,
            Err(err) => {
                let e = RpcError::new(code::INVALID_REQUEST, format!("invalid request: {err}"));
                return Some(error_frame(frame_id(&raw), &e));
            }
        };
        match msg {
            ClientJsonRpcMessage::Request(_) | ClientJsonRpcMessage::Notification(_) => {
                self.pump(msg)
            }
            // 本服务不向客户端发起请求，所以客户端的 response/error 无处投递。
            _ => {
                let e = RpcError::new(
                    code::INVALID_REQUEST,
                    "invalid request: 这里是服务端，不收 response/error 帧",
                );
                Some(error_frame(frame_id(&raw), &e))
            }
        }
    }

    /// 处理一帧已解析的 JSON。通知（无 `id`）一律返回 `None`。
    pub fn handle(&mut self, msg: &Value) -> Option<Value> {
        let line = serde_json::to_string(msg).ok()?;
        let reply = self.handle_line(&line)?;
        let id = msg.get("id").cloned();
        // 帧是我们自己序列化的，解不出来＝实现坏了。回一帧协议错误，
        // 不 panic：panic 会把整条 stdio 连接砸断，连累后面好好的请求。
        Some(
            serde_json::from_str::<Value>(&reply)
                .unwrap_or_else(|_| json!({"jsonrpc":"2.0","id":id,"error":{"code":code::INTERNAL_ERROR,"message":"会话循环产出了无法解析的帧"}})),
        )
    }

    fn start(exec: Executor, policy: Policy, gate: Option<AuthGate>) -> Self {
        let core = Arc::new(Mutex::new(Core::new(exec, policy, gate)));
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let rt = single_thread_runtime();
        let handler = Handler {
            core: Arc::clone(&core),
        };
        match rt {
            Some(ref rt) => {
                rt.spawn(async move {
                    let session = serve_directly(
                        handler,
                        FrameTransport {
                            incoming: in_rx,
                            outgoing: out_tx,
                        },
                        None,
                    );
                    let _ = session.waiting().await;
                });
            }
            // 连当前线程运行时都建不出来（拿不到线程/epoll）也不再 expect panic：
            // 直接把入站接收端丢掉，之后每一帧都在 pump 里拿到明确的 INTERNAL_ERROR
            // 回包——"一帧进一帧出"的契约不因启动失败而破。
            None => {
                drop(in_rx);
                drop(out_tx);
            }
        }
        Server {
            core,
            to_rmcp: in_tx,
            from_rmcp: out_rx,
            rt,
        }
    }

    /// 一帧进、一帧出：请求等它的回包，通知不回包。
    ///
    /// 通知不"等它被处理完"：rmcp 的循环读帧是**串行**的，同一会话里后面的
    /// 帧一定排在它之后，所以既不会挤掉它，也不会让请求拿到错位的回包。
    fn pump(&mut self, msg: ClientJsonRpcMessage) -> Option<String> {
        let is_request = matches!(msg, ClientJsonRpcMessage::Request(_));
        if self.to_rmcp.send(msg).is_err() {
            let e = RpcError::new(code::INTERNAL_ERROR, "协议会话已结束");
            return Some(error_frame(None, &e));
        }
        if !is_request {
            return None;
        }
        let Some(rt) = self.rt.as_ref() else {
            let e = RpcError::new(
                code::INTERNAL_ERROR,
                "协议核心未启动（建不出 tokio 运行时）",
            );
            return Some(error_frame(None, &e));
        };
        let rx = &mut self.from_rmcp;
        rt.block_on(terminal_reply(rx))
            .map(|frame| serde_json::to_string(&frame).unwrap_or_default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Core> {
        // 与 platform/db.rs 同一处理：毒锁只在 SQL/处理里 panic 过时出现，
        // 连接与状态都还在，恢复 guard 比把进程砸了更好。
        self.core.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// 单线程运行时：帧是同步推进的，同一时刻只有一帧在会话里。
fn single_thread_runtime() -> Option<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()
}

/// 会话循环可能先产出非终帧（通知）；只把 response/error 当作这一帧的回包。
async fn terminal_reply(
    rx: &mut mpsc::UnboundedReceiver<ServerJsonRpcMessage>,
) -> Option<ServerJsonRpcMessage> {
    loop {
        let frame = rx.recv().await?;
        if matches!(
            frame,
            ServerJsonRpcMessage::Response(_) | ServerJsonRpcMessage::Error(_)
        ) {
            return Some(frame);
        }
    }
}

/// 帧里原来的 `id`，取不到就按 `null` 回。
fn frame_id(raw: &Value) -> Option<RequestId> {
    raw.get("id")
        .and_then(|id| serde_json::from_value::<RequestId>(id.clone()).ok())
}

/// 帧级错误：由 rmcp 的 `JsonRpcMessage` 负责序列化，这里不再手拼 JSON。
fn error_frame(id: Option<RequestId>, e: &RpcError) -> String {
    let frame = ServerJsonRpcMessage::error(e.to_mcp(), id);
    serde_json::to_string(&frame).unwrap_or_default()
}

/// 一帧进、一帧出的 transport：`Server` 把帧塞进来，rmcp 的会话循环把回包递出去。
///
/// 刻意**不用** rmcp 自带的 transport：stdio 的读写留在 `transport.rs`（那样
/// "有人把日志 println 到 stdout"仍然只有真进程能发现），而远程 transport
/// 需要 G15-02 的会话授权，feature 层面也一个都没开。
struct FrameTransport {
    incoming: mpsc::UnboundedReceiver<ClientJsonRpcMessage>,
    outgoing: mpsc::UnboundedSender<ServerJsonRpcMessage>,
}

impl RmcpTransport<RoleServer> for FrameTransport {
    type Error = mpsc::error::SendError<ServerJsonRpcMessage>;

    fn send(
        &mut self,
        item: ServerJsonRpcMessage,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let tx = self.outgoing.clone();
        async move { tx.send(item) }
    }

    async fn receive(&mut self) -> Option<ClientJsonRpcMessage> {
        self.incoming.recv().await
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.incoming.close();
        Ok(())
    }
}

/// 执行面需要的状态。业务动作一律转 `Executor`，这里只做协议侧的记账。
struct Core {
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
    /// 握手状态：MCP 生命周期要求先 `initialize`，rmcp 不管这一条（它假设
    /// 会话由握手建立），所以口径留在这一层。
    initialized: bool,
    /// 协商到的协议版本，**只用于记账与诊断**：出帧的能力表不随版本变，
    /// 三个版本走的是同一套方法。
    protocol: &'static str,
}

impl Core {
    fn new(exec: Executor, policy: Policy, gate: Option<AuthGate>) -> Self {
        Self {
            exec,
            policy,
            gate,
            confirm: ConfirmationStore::new(),
            jobs: JobRegistry::new(),
            seq: 0,
            initialized: false,
            protocol: DEFAULT_PROTOCOL,
        }
    }

    fn tools_call(
        &mut self,
        name: &str,
        args: &Value,
        meta: &RequestMetaObject,
    ) -> Result<Value, RpcError> {
        if META_TOOLS.contains(&name) {
            return Ok(self.meta_call(name, args, meta));
        }
        let spec = self.resolve_tool(name)?;
        if args.to_string().len() > MAX_ARGS_BYTES {
            return Err(RpcError::new(
                code::INVALID_PARAMS,
                format!("arguments too large (limit {MAX_ARGS_BYTES} bytes)"),
            ));
        }
        // 未知/缺失/类型不符一律协议级拒绝：这不是"业务失败"，是调用方拼错了。
        catalog::validate_args(spec, args).map_err(arg_error)?;
        if let Some(denial) = self.deny(spec, args, meta) {
            return Ok(refusal(spec, &denial));
        }
        let env = match envelope_of(meta) {
            Ok(env) => env,
            Err(e) => return Ok(tool_failed(spec, &e)),
        };
        Ok(self.execute_checked(spec, args, &env))
    }

    /// 执行前的判定，**只有这一处实现**：全部委托给
    /// [`wxlike_cmd::availability`]，它内部按执行顺序调用与以前相同的三个原语
    /// （通道 → 参数来源 → scope → 本地策略）。
    ///
    /// 为什么不再在这里直接调 `guard_*`：`im_execute` 这条兼容路径以前**绕过了
    /// 全部三道判定**（`meta_call` 在 `tools_call` 的守卫之前就 return 了），
    /// 一次 `{"command":"group.kick"}` 就能带着不可信参数走到底。判定收敛到一个
    /// 函数之后，两条入口共用同一份，`listed_availability_is_the_same_refusal`
    /// 与 `meta_execute_cannot_bypass_the_guards` 一边测一致、一边测绕过。
    fn deny(
        &self,
        spec: &CommandSpec,
        params: &Value,
        meta: &RequestMetaObject,
    ) -> Option<availability::Denial> {
        let ctx = availability::Context {
            channel: CHANNEL,
            provenance: provenance_of(meta),
            gate: self.gate.as_ref(),
            policy: &self.policy,
        };
        availability::why_not(spec, params, &ctx)
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
    fn execute_checked(
        &mut self,
        spec: &'static CommandSpec,
        args: &Value,
        env: &contract::Envelope,
    ) -> Value {
        if spec.confirmation.is_required() {
            match self.confirm_step(spec, args) {
                Ok(Some(plan_only)) => return plan_only,
                Ok(None) => {}
                Err(e) => return tool_failed(spec, &contract::classify(&e.to_string())),
            }
        }
        self.launch(spec, args, env)
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

    /// 真正执行：契约校验 → 执行 → ActionResult → 作业账 → 追踪 id。
    fn launch(&mut self, spec: &CommandSpec, args: &Value, env: &contract::Envelope) -> Value {
        // 写命令缺幂等键 / deadline 已过：本地就拒，**不发请求**
        if let Err(e) = contract::build_request(spec, args, env) {
            return tool_failed(spec, &e);
        }
        if let Err(e) = contract::check_deadline(env, contract::now_ms_i64()) {
            return tool_failed(spec, &e);
        }
        self.seq += 1;
        let operation_id = format!("op-{}-{}", self.seq, spec.id);
        let mut frame = match self.exec.run(spec.id, args) {
            Ok(data) => {
                let result = contract::ok_result(spec, data, contract::now_ms());
                self.record_job(spec, &operation_id, &result);
                tool_result(&result)
            }
            Err(message) => tool_failed(spec, &contract::classify(&message)),
        };
        frame["_meta"] = json!({
            "wxlike/requestId": format!("req-{}", self.seq),
            "wxlike/operationId": operation_id,
            "wxlike/traceId": env.trace_id.as_str(),
        });
        frame
    }

    /// 身份上下文：配了闸门用闸门的，否则用本地身份（仍然绑定，不豁免）。
    fn actor_ctx(&self) -> ActorContext {
        match &self.gate {
            Some(gate) => gate.context().clone(),
            None => wxlike_cmd::auth::context_from_parts(LOCAL_ACTOR, LOCAL_SESSION, "conv:*"),
        }
    }

    /// 可用性判定的现场：本进程的通道、当前闸门与策略。
    /// 列表侧用**可信来源**：这里回答的是"这条命令在此进程里能不能做"，
    /// 某一次调用的参数是不是从聊天正文里抠出来的，由 `tools_call` 那关管。
    fn availability_ctx(&self) -> availability::Context<'_> {
        availability::Context {
            channel: CHANNEL,
            provenance: Provenance::Trusted,
            gate: self.gate.as_ref(),
            policy: &self.policy,
        }
    }

    /// 只有长任务进作业账：一次就结束的写命令记进去只会让账变噪音。
    fn record_job(&mut self, spec: &CommandSpec, operation_id: &str, result: &ActionResult) {
        if spec.id != LONG_RUNNING_ACTION {
            return;
        }
        let job = self.jobs.submit(spec.id, operation_id, now());
        let output = contract::result_json(result);
        let _ = self.jobs.finish(&job.id, output, now());
    }

    fn meta_call(&mut self, name: &str, args: &Value, meta: &RequestMetaObject) -> Value {
        match name {
            "im_list_commands" => {
                let q = args.get("query").and_then(Value::as_str).unwrap_or("");
                tool_ok(&catalog_list(q, &self.availability_ctx()))
            }
            "im_inspect" => inspect(&self.exec),
            "im_jobs" => self.jobs_call(args),
            // 兼容旧客户端的统一执行入口。它不再是判定之外的一条路：
            // 解析、校验参数、过同一份 `deny`，与 `tools/call` 完全同源。
            _ => self.meta_execute(args, meta),
        }
    }

    /// `im_execute` 的一整条路：解析 → 判定 → 执行。
    /// 单独成函数而不是嵌在 `match` 里，是为了让"判定在中间"这件事在缩进上
    /// 也看得出来（同时压住质量闸门的嵌套层数指标）。
    fn meta_execute(&self, args: &Value, meta: &RequestMetaObject) -> Value {
        let (spec, params) = match self.resolve_execute_request(args) {
            Err(frame) => return frame,
            Ok(pair) => pair,
        };
        if let Some(denial) = self.deny(spec, &params, meta) {
            return refusal(spec, &denial);
        }
        execute_one(&self.exec, spec, &params)
    }

    /// `im_execute` 的入参解析：把 `{"command","params"}` 变成 `(spec, params)`。
    /// 返回 `Err(Value)` 时那个帧就是最终响应（形状与收敛前一致）。
    fn resolve_execute_request(
        &self,
        args: &Value,
    ) -> Result<(&'static CommandSpec, Value), Value> {
        let Some(cmd) = args.get("command").and_then(Value::as_str) else {
            return Err(tool_err("im_execute 需要 command 参数"));
        };
        let Some(spec) = catalog::get(cmd) else {
            return Err(tool_err(&format!(
                "unknown command {cmd:?}（先 im_list_commands）"
            )));
        };
        let params = args.get("params").cloned().unwrap_or_else(|| json!({}));
        if let Err(e) = catalog::validate_args(spec, &params) {
            return Err(tool_err(&e.to_string()));
        }
        Ok((spec, params))
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

    fn resources_read(&self, uri: &str) -> Result<Value, RpcError> {
        let (text, mime) = match uri {
            "wxlike://catalog" => (
                serde_json::to_string_pretty(&catalog_list("", &self.availability_ctx()))
                    .unwrap_or_default(),
                "application/json",
            ),
            "wxlike://policy" => (wxlike_cmd::policy::render(&self.policy), "text/plain"),
            "wxlike://coverage" => (
                serde_json::to_string_pretty(&wxlike_cmd::domains::report()).unwrap_or_default(),
                "application/json",
            ),
            other => {
                let Some(id) = other.strip_prefix(catalog::ACTION_URI_PREFIX) else {
                    return Err(RpcError::new(
                        code::INVALID_PARAMS,
                        format!("unknown resource {other:?}"),
                    ));
                };
                let spec = catalog::get(id).ok_or_else(|| {
                    RpcError::new(code::INVALID_PARAMS, format!("unknown action {id:?}"))
                })?;
                (
                    serde_json::to_string_pretty(&catalog::schema_of(spec)).unwrap_or_default(),
                    "application/json",
                )
            }
        };
        Ok(json!({"contents": [{"uri": uri, "mimeType": mime, "text": text}]}))
    }
}

/// rmcp 的服务面：把 [`Core`] 接进 MCP 的分派。
struct Handler {
    core: Arc<Mutex<Core>>,
}

impl Handler {
    fn with<T>(&self, f: impl FnOnce(&mut Core) -> T) -> T {
        let mut core = self.core.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut core)
    }

    /// 未握手的业务调用一律拒：MCP 生命周期不是可选项。
    fn ready(&self) -> Result<(), McpError> {
        if self.with(|core| core.initialized) {
            return Ok(());
        }
        Err(RpcError::new(
            code::NOT_INITIALIZED,
            "server not initialized: send initialize first",
        )
        .to_mcp())
    }

    /// 记下协商结果：握手状态 + 实际谈成的版本。
    fn mark_ready(&self, negotiated: &ProtocolVersion) {
        // rmcp 只会谈成 `supported_protocol_versions` 里的那个表，落不进去
        // 就按默认口径记（不猜第三个值）。
        let recorded = PROTOCOL_VERSIONS
            .iter()
            .find(|v| **v == negotiated.as_str())
            .copied()
            .unwrap_or(DEFAULT_PROTOCOL);
        self.with(|core| {
            core.initialized = true;
            core.protocol = recorded;
        });
    }
}

impl ServerHandler for Handler {
    fn get_info(&self) -> ServerConfig {
        let mut tools = ToolsCapability::default();
        tools.list_changed = Some(false);
        let mut resources = ResourcesCapability::default();
        resources.subscribe = Some(false);
        resources.list_changed = Some(false);
        let mut capabilities = ServerCapabilities::default();
        capabilities.tools = Some(tools);
        capabilities.resources = Some(resources);
        let mut server_info = Implementation::new("wxlike", env!("CARGO_PKG_VERSION"));
        server_info.title = Some("wxlike 命令面".to_string());
        let mut info = ServerConfig::new(capabilities)
            .with_server_info(server_info)
            .with_instructions(format!(
                "先 im_list_commands 发现能力，再按工具名调用。共 {} 条命令；\
                 写操作需要启动时授予 --allow-write，破坏性操作另需 --allow-destructive。",
                catalog::CATALOG.len()
            ));
        // 协商的默认口径 = 我们支持的最新版本；客户端报了不认识的版本就回它。
        info.protocol_version = ProtocolVersion::V_2025_06_18;
        info
    }

    /// 只声明这三个版本；报了别的（含更新的）一律回退到最新的那个，不报错。
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Owned(
            PROTOCOL_VERSIONS
                .iter()
                .map(|v| version_of(v))
                .collect::<Vec<ProtocolVersion>>(),
        )
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        let info = self.negotiate_initialize(&request)?;
        self.mark_ready(&info.protocol_version);
        Ok(info)
    }

    /// 每个命令一个工具 + 四个元工具。顺序稳定（元工具在前、catalog 顺序），便于快照。
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        self.ready()?;
        let tools = tool_definitions()
            .into_iter()
            .map(|t| {
                serde_json::from_value::<Tool>(t).map_err(|e| {
                    McpError::internal_error(format!("工具定义进不了 MCP 模型: {e}"), None)
                })
            })
            .collect::<Result<Vec<Tool>, _>>()?;
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        self.ready()?;
        let args = Value::Object(request.arguments.clone().unwrap_or_default());
        // `_meta` 由 rmcp 收在请求上下文里（不在 params 上），这里原样交给契约层。
        let frame = self.with(|core| core.tools_call(&request.name, &args, &context.meta))?;
        tool_frame(frame)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        self.ready()?;
        let frame = self.with(|core| core.resources_list());
        from_model(frame)
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        self.ready()?;
        let frame = self.with(|core| core.resources_read(&request.uri))?;
        // 内容与顺序都由这层的帧决定，rmcp 只负责把它装进它自己的结果类型。
        Ok(from_model::<ReadResourceResult>(frame)?.into())
    }

    /// 未实现的能力显式回 `-32601`，不用 rmcp 的默认空结果冒充"实现了但刚好为空"。
    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Err(McpError::method_not_found::<ListPromptsRequestMethod>())
    }

    async fn get_prompt(
        &self,
        _request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, McpError> {
        Err(McpError::method_not_found::<GetPromptRequestMethod>())
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Err(McpError::method_not_found::<
            ListResourceTemplatesRequestMethod,
        >())
    }

    async fn complete(
        &self,
        _request: CompleteRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, McpError> {
        Err(McpError::method_not_found::<CompleteRequestMethod>())
    }

    /// rmcp 把它认得的方法分完之后，剩下的都到这里：
    /// `initialize` 允许是不完整的一帧（旧客户端不带 clientInfo / capabilities），
    /// 其余已知方法是参数不合法，未知方法是 `-32601`。
    async fn on_custom_request(
        &self,
        request: CustomRequest,
        _context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, McpError> {
        let method = request.method.as_str();
        if method == "initialize" {
            let params = lenient_initialize(request.params.as_ref());
            let info = self.negotiate_initialize(&params)?;
            self.mark_ready(&info.protocol_version);
            return serde_json::to_value(info)
                .map(CustomResult)
                .map_err(|e| McpError::internal_error(format!("握手结果异常: {e}"), None));
        }
        if SERVED_METHODS.contains(&method) {
            return Err(McpError::invalid_params(
                format!("{method} 的参数不符合 MCP 模型"),
                None,
            ));
        }
        self.ready()?;
        Err(McpError::new(
            ErrorCode::METHOD_NOT_FOUND,
            format!("method not found: {method}"),
            None,
        ))
    }
}

/// 协议版本字符串 → rmcp 的类型。未知字符串也要能表示，所以走 serde；
/// 万一进不去就按默认口径，不让一帧握手把整个会话带崩。
fn version_of(text: &str) -> ProtocolVersion {
    serde_json::from_value(Value::String(text.to_string())).unwrap_or(ProtocolVersion::V_2025_06_18)
}

/// 不完整但仍合法的 `initialize`：缺失的字段按我们的默认口径补，
/// 报了不认识的 `protocolVersion` 也照样回退到最新版本。
fn lenient_initialize(raw: Option<&Value>) -> InitializeRequestParams {
    let mut params = InitializeRequestParams::default();
    if let Some(text) = raw
        .and_then(|v| v.get("protocolVersion"))
        .and_then(Value::as_str)
    {
        params.protocol_version = version_of(text);
    }
    params
}

/// 工具帧（`content` / `structuredContent` / `isError` / `_meta`）进 MCP 类型。
fn tool_frame(frame: Value) -> Result<CallToolResponse, McpError> {
    let result: CallToolResult = from_mcp(frame)?;
    Ok(CallToolResponse::Complete(result))
}

/// 序列化形态的帧 → rmcp 的模型类型；进不去就是本层拼错了，不是客户端的错。
fn from_model<T>(frame: Value) -> Result<T, McpError>
where
    T: serde::de::DeserializeOwned,
{
    from_mcp(frame)
}

fn from_mcp<T>(frame: Value) -> Result<T, McpError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(frame)
        .map_err(|e| McpError::internal_error(format!("响应帧不符合 MCP 模型: {e}"), None))
}

/// Unix 秒。确认令牌与作业账都用同一把时钟，测试里换成显式时间。
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 从 `_meta` 取契约信封：`wxlike/traceId`、`wxlike/entryId`、`wxlike/deadlineMs`。
///
/// 三个都是可选的；给了非法值就报领域错误，**不静默忽略**——静默忽略会让
/// "我明明传了 trace" 变成排查时找不到的幽灵。
fn envelope_of(meta: &RequestMetaObject) -> Result<contract::Envelope, DomainError> {
    let get_str = |key: &str| meta.0 .0.get(key).and_then(Value::as_str);
    let deadline = meta.0 .0.get("wxlike/deadlineMs").and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    });
    contract::Envelope::from_raw(
        get_str("wxlike/traceId"),
        get_str("wxlike/entryId"),
        deadline,
    )
}

/// 参数来源：调用方可以在 `_meta` 里声明这批参数是从不可信内容抽出来的。
///
/// 默认 `Trusted` 是刻意的：这是**调用方自述**，不是安全边界；真正的边界是
/// 服务端再鉴权 + 确认令牌（G15-04）。但主动声明"这来自聊天正文"的调用方
/// 会立刻被拒，而不是等到出事。
fn provenance_of(meta: &RequestMetaObject) -> Provenance {
    let declared = meta.0 .0.get("wxlike/provenance").and_then(Value::as_str);
    match declared {
        Some(v) if v.starts_with("untrusted") => Provenance::Untrusted("untrusted content"),
        _ => Provenance::Trusted,
    }
}

fn arg_error(e: ArgError) -> RpcError {
    RpcError::new(code::INVALID_PARAMS, e.to_string())
}

/// `im_execute` 的执行段：判定已经在外层做过，这里只剩"跑 + 包结果"。
fn execute_one(exec: &Executor, spec: &CommandSpec, params: &Value) -> Value {
    match exec.run(spec.id, params) {
        Ok(data) => tool_result(&contract::ok_result(spec, data, contract::now_ms())),
        Err(message) => tool_failed(spec, &contract::classify(&message)),
    }
}

/// 一条拒绝的**外形**映射，与三道判定收敛前逐字一致：
///
/// * 参数来源不可信、本地策略（写/破坏性没开、会话不在白名单）这两类，原来
///   就是从 `guard_provenance` / `Executor.run` 里以 contract-v1 失败结果出去
///   的，业务面看得见 `status`，所以继续走 `tool_failed`；
/// * 通道与 scope 原来就是工具错误（改配置或换 token 即可重试），继续 `tool_err`。
///
/// 收敛的是**判断**，不是响应形状——形状一变就会把 contract-v1 的既有断言
/// 一起卷进来，那种改法该单独走一次基线变更。
fn refusal(spec: &CommandSpec, denial: &availability::Denial) -> Value {
    const RESULT_SHAPED: [&str; 6] = [
        "untrusted_content",
        "read_only",
        "write_disabled",
        "destructive_disabled",
        "conv_not_allowed",
        "missing_conv",
    ];
    if RESULT_SHAPED.contains(&denial.code) {
        return tool_failed(spec, &contract::classify(&denial.message));
    }
    tool_err(&denial.message)
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

fn catalog_list(query: &str, ctx: &availability::Context<'_>) -> Value {
    let items: Vec<Value> = catalog::search(query)
        .into_iter()
        .map(|s| {
            // 可用性由 `wxlike_cmd::availability` 判定，与执行路径同一个函数、
            // 与 `wxlike policy` 同一组探针参数。这里不加任何本地推断。
            let denial = availability::why_not(s, &availability::probe_params(s), ctx);
            json!({
                "id": s.id,
                "action_id": s.action_id(),
                "tool": s.tool_name(),
                "summary": s.summary,
                "params": s.params,
                "capability": s.capability.as_str(),
                "scope": s.scope.as_str(),
                "confirmation": s.confirmation.as_str(),
                "idempotency": if s.idempotency.is_required() { "required" } else { "none" },
                "platforms": s.platforms.names(),
                "host": s.host.as_str(),
                "available": denial.is_none(),
                "why_not": denial.map(|d| json!({ "code": d.code, "message": d.message })),
            })
        })
        .collect();
    let available = items
        .iter()
        .filter(|i| i["available"] == Value::Bool(true))
        .count();
    json!({ "count": items.len(), "available": available, "commands": items })
}

/// 工具面的**唯一出处**：四个元工具在前，然后按 catalog 顺序一条命令一个工具。
/// `tools_dump`（诊断）与 rmcp 的 `tools/list` 都从这里取，两条通道不会分叉。
fn tool_definitions() -> Vec<Value> {
    let mut tools: Vec<Value> = META_TOOLS.iter().map(|name| meta_tool(name)).collect();
    tools.extend(catalog::CATALOG.iter().map(catalog::tool_definition));
    tools
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
    let text = serde_json::to_string_pretty(value).unwrap_or_default();
    json!({
        "content": [{"type": "text", "text": clamp_text(text)}],
        "structuredContent": value,
        "isError": false
    })
}

/// 业务结果：`structuredContent` 是 contract-v1 的 `ActionResult`
/// （`action_id` + `status` + `data|error` + `completed_at_ms`），
/// `isError` 与 `status` 同源——不会出现"UI 说失败、MCP 说成功"。
fn tool_result(result: &ActionResult) -> Value {
    let json = contract::result_json(result);
    let text = match &result.outcome {
        wxlike_types::ActionOutcome::Ok { data } => {
            serde_json::to_string_pretty(data).unwrap_or_default()
        }
        wxlike_types::ActionOutcome::Empty => "（空结果）".to_string(),
        wxlike_types::ActionOutcome::Failed { error } => error.message().to_string(),
    };
    json!({
        "content": [{"type": "text", "text": clamp_text(text)}],
        "structuredContent": json,
        "isError": !result.is_ok()
    })
}

/// 失败结果：同样带 `ActionResult`，agent 能直接读 `error.code` 决定下一步。
fn tool_failed(spec: &CommandSpec, error: &DomainError) -> Value {
    tool_result(&contract::failed_result(
        spec,
        error.clone(),
        contract::now_ms(),
    ))
}

/// 超长文本截断到上限，并留下可机器判断的标记。
fn clamp_text(text: String) -> String {
    if text.len() <= MAX_TEXT_BYTES {
        return text;
    }
    let mut cut = MAX_TEXT_BYTES;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n…[truncated at {MAX_TEXT_BYTES} bytes]", &text[..cut])
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

        // 谈成的版本要落到会话状态里，不是只写进那一帧
        let mut srv = server(Policy::none());
        call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":3,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
        );
        assert_eq!(srv.protocol(), "2024-11-05");
    }

    /// 完整的一帧 initialize（带 capabilities 与 clientInfo）走 rmcp 的**类型化**
    /// 路径，不是补默认值的兼容分支。两条都要有覆盖，否则真客户端的路等于没测。
    #[test]
    fn fully_specified_initialize_goes_through_the_typed_path() {
        let mut srv = server(Policy::none());
        let v = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{"roots":{"listChanged":true}},"clientInfo":{"name":"real-client","version":"1.0"}}}"#,
        );
        assert_eq!(v["result"]["protocolVersion"], "2025-03-26", "{v}");
        assert_eq!(srv.protocol(), "2025-03-26", "类型化路径也要记版本");
        assert_eq!(v["result"]["serverInfo"]["title"], "wxlike 命令面");
        assert!(v["result"]["instructions"]
            .as_str()
            .expect("instructions")
            .contains("50 条命令"));
        // 握手之后业务方法立即可用
        let v = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        );
        assert_eq!(v["result"]["tools"].as_array().expect("tools").len(), 54);
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

    /// 大参数与大响应都有上限：不设限等于给"用 JSON 塞文件"开门。
    #[test]
    fn oversized_arguments_and_responses_are_bounded() {
        let mut srv = ready();
        let huge = "x".repeat(MAX_ARGS_BYTES);
        let frame = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "msg-send", "arguments": {
                "conv": "g-1", "text": huge, "idempotency_key": "k"
            }}
        });
        let v = srv.handle(&frame).expect("response");
        assert_eq!(v["error"]["code"], code::INVALID_PARAMS, "{v}");
        assert!(v["error"]["message"]
            .as_str()
            .expect("msg")
            .contains("too large"));

        // 截断标记可机器判断
        let long = "y".repeat(MAX_TEXT_BYTES + 10);
        let text = clamp_text(long);
        assert!(text.ends_with("bytes]"), "{}", &text[text.len() - 40..]);
        assert!(text.len() <= MAX_TEXT_BYTES + 64);
        // 短文本原样返回
        assert_eq!(clamp_text("ok".to_string()), "ok");
    }

    #[test]
    fn notifications_get_no_reply() {
        let mut srv = ready();
        assert!(srv
            .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none());
    }

    /// 能力没 advertise 的方法不能用"空结果"冒充实现。
    #[test]
    fn unadvertised_capabilities_stay_method_not_found() {
        let mut srv = ready();
        for (id, method) in [
            (20, "prompts/list"),
            (21, "prompts/get"),
            (22, "resources/templates/list"),
            (23, "completion/complete"),
        ] {
            let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": {}});
            let v = srv.handle(&frame).expect("response");
            assert_eq!(v["error"]["code"], code::METHOD_NOT_FOUND, "{method}: {v}");
        }
    }

    /// 本服务不向客户端发请求，所以客户端的 response 帧无处投递——
    /// 显式拒绝，不能当通知吞掉。
    #[test]
    fn client_response_frames_are_rejected_not_swallowed() {
        let mut srv = ready();
        let v = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":24,"result":{"anything":true}}"#,
        );
        assert_eq!(v["error"]["code"], code::INVALID_REQUEST, "{v}");
    }

    /// 指定策略的握手会话。
    fn ready_with(policy: Policy) -> Server {
        let mut srv = server(policy);
        let v = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
        );
        assert_eq!(v["result"]["protocolVersion"], "2025-06-18");
        srv
    }

    /// 不带 query 的全量列表。
    fn list_all(srv: &mut Server) -> Value {
        call(
            srv,
            r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"im_list_commands","arguments":{}}}"#,
        )
    }

    /// 握手 + 挂闸门的会话。scope 收窄时用，专测"判定是不是真的在执行前"。
    fn gated(policy: Policy, scope: &str) -> Server {
        let exec = Executor::new(Http::new("http://127.0.0.1:1"), policy.clone());
        let gate = AuthGate::from_parts(
            "u-alice",
            "s-1",
            scope,
            Some("s-1"),
            Vec::new(),
            Channel::Agent,
        )
        .expect("gate");
        let mut srv = Server::new(exec, policy).with_gate(Some(gate));
        let v = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
        );
        assert_eq!(v["result"]["protocolVersion"], "2025-06-18");
        srv
    }

    /// **兼容入口不是后门**。`im_execute` 以前在 `tools_call` 的三道判定之外
    /// 直接落到执行器：同样一条 `msg.send`，用 `msg-send` 工具会被来源与 scope
    /// 拦下，包进 `im_execute` 就畅通无阻。现在两条路共用 `Core::deny`，
    /// 这里逐条测那两道拦得住的判定。
    #[test]
    fn meta_execute_cannot_bypass_the_guards() {
        // 写权限开着、会话 g-1 在白名单里，所以拦住它的只可能是判定本身
        // （空 `allow_conv` 是"一个都不许"，不能拿来当"不设限"用）
        let write = Policy::new(["g-1"], true, false);
        let mut srv = gated(write, "conv:g-1");
        // 1) 不可信来源：正文里的指令不许触发写
        let v = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"im_execute","arguments":{"command":"msg.send","params":{"conv":"g-1","text":"hi","idempotency_key":"k"}},"_meta":{"wxlike/provenance":"untrusted: message body"}}}"#,
        );
        assert_eq!(v["result"]["isError"], true, "{v}");
        let text = v["result"]["content"][0]["text"].as_str().expect("text");
        assert!(text.contains("untrusted content"), "{text}");
        // 2) scope：会话不在授权范围内，必须和工具面一样拒
        let v = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"im_execute","arguments":{"command":"msg.send","params":{"conv":"g-2","text":"hi","idempotency_key":"k"}}}}"#,
        );
        assert_eq!(v["result"]["isError"], true, "{v}");
        let text = v["result"]["content"][0]["text"].as_str().expect("text");
        assert!(text.contains("conv:g-2"), "应点名缺哪个 scope：{text}");
        // 3) 三道判定都过了，才会轮到执行本身（这里必然是连不上桩服务端的失败，
        //    绝不是判定类拒绝——判定类文案一次都不该出现）
        let v = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"im_execute","arguments":{"command":"msg.send","params":{"conv":"g-1","text":"hi","idempotency_key":"k"}}}}"#,
        );
        let text = v["result"]["content"][0]["text"].as_str().expect("text");
        for phrase in [
            "untrusted",
            "is not allowed",
            "not granted",
            "missing scope",
            "read-only",
        ] {
            assert!(
                !text.contains(phrase),
                "过了判定还报判定类拒绝（{phrase:?}）：{text}"
            );
        }
    }

    /// `im_list_commands` 报的"不能做"必须与真调用的拒绝**同源同文**。
    /// 分成两套判定的后果是模型反复撞同一堵墙，而人看不出为什么——
    /// wordcraft 就漏了这一半（headless 的 `list_commands` 干脆不输出可用性）。
    #[test]
    fn listed_availability_is_the_same_refusal_as_the_call() {
        let mut srv = ready(); // Policy::none()：写权限没开
        let listed = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"im_list_commands","arguments":{"query":"msg.send"}}}"#,
        );
        let items = listed["result"]["structuredContent"]["commands"]
            .as_array()
            .expect("commands");
        assert_eq!(items.len(), 1, "{listed}");
        let item = &items[0];
        assert_eq!(item["id"], "msg.send");
        assert_eq!(item["available"], false, "写权限没开就该报不可用");
        assert_eq!(item["why_not"]["code"], "write_disabled", "{item}");
        let listed_text = item["why_not"]["message"].as_str().expect("message");

        // 同一参数的真调用，拒绝文案必须逐字相同（同一函数、同一 Display）
        let v = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"im_execute","arguments":{"command":"msg.send","params":{"conv":"g-1","text":"hi","idempotency_key":"k"}}}}"#,
        );
        assert_eq!(v["result"]["isError"], true, "{v}");
        assert_eq!(
            v["result"]["content"][0]["text"].as_str().expect("text"),
            listed_text,
            "列表与调用给出两句不同的拒绝，就是两份真相"
        );
    }

    /// `available` 必须**跟着策略变**：只读时报出的可用数正好是只读那批，
    /// 开写之后全部可用。两个方向都测，否则这个字段可能一直是 `false`
    /// （或一直 `true`），照样"看起来有报"。
    #[test]
    fn listing_availability_moves_with_the_policy_in_both_directions() {
        let reads = catalog::CATALOG
            .iter()
            .filter(|s| s.capability == wxlike_cmd::Capability::Read)
            .count();
        assert!(
            reads > 0 && reads < catalog::CATALOG.len(),
            "catalog 形状变了"
        );

        // 会话全放行，只留"写"这一个开关——否则读也会被白名单拦，测不出这一档
        let mut srv = ready_with(Policy::new(["*"], false, false));
        let content = &list_all(&mut srv)["result"]["structuredContent"];
        let items = content["commands"].as_array().expect("commands");
        assert_eq!(items.len(), catalog::CATALOG.len(), "不带 query 是全集");
        assert_eq!(
            content["available"].as_u64().expect("count") as usize,
            reads,
            "只读策略下可用的应当就是只读那批：{content}"
        );
        for item in items.iter().filter(|i| i["available"] == false) {
            // 只读策略拦的是"要写的两类"：write 缺写权限、destructive 缺破坏性
            let cap = item["capability"].as_str().expect("capability");
            let expected = match cap {
                "write" => "write_disabled",
                "destructive" => "destructive_disabled",
                "read" => panic!("只读策略不该拦只读命令：{item}"),
                other => panic!("未知能力 {other:?}：{item}"),
            };
            assert_eq!(item["why_not"]["code"], expected, "{item}");
            let flag = if expected == "write_disabled" {
                "--allow-write"
            } else {
                "--allow-destructive"
            };
            assert!(
                item["why_not"]["message"].as_str().unwrap().contains(flag),
                "要说清怎么放开：{item}"
            );
        }

        // 会话通配全开 + 写与破坏性都开：一条都不该被拦
        let mut srv = ready_with(Policy::new(["*"], true, true));
        let content = &list_all(&mut srv)["result"]["structuredContent"];
        assert_eq!(
            content["available"].as_u64().expect("count") as usize,
            catalog::CATALOG.len(),
            "全放开还报不可用，就是判定过保守：{content}"
        );
    }

    /// 会话白名单没覆盖到探针会话时，只读命令也会标不可用——
    /// 这一条测的是"列表不许把做不到的说成做得到"。
    #[test]
    fn listing_reports_the_conv_allowlist_gap_even_for_reads() {
        let mut srv = ready_with(Policy::new(["g-1"], true, true));
        let listed = call(
            &mut srv,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"im_list_commands","arguments":{"query":"msg.forward"}}}"#,
        );
        let item = &listed["result"]["structuredContent"]["commands"][0];
        assert_eq!(item["id"], "msg.forward", "{listed}");
        // 探针参数落在白名单外 ⇒ 列表按"这个形状现在不能做"报，不粉饰
        assert_eq!(item["why_not"]["code"], "conv_not_allowed", "{item}");
    }
}
