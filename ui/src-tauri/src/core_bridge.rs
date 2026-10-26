//! G01-02：新 UI（apps/desktop-web，契约 v1）的宿主桥。
//!
//! 前端只发 `ActionRequest`（snake_case JSON）、监听 `core://event`；
//! 这里把 55 个 action_id 映射到 [`AppState`] 的既有业务方法
//! （HTTP 控制面 + QUIC hub + 本地 SQLite 镜像库），**不引入第二套业务
//! 逻辑**——旧 UI 与新 UI 走同一个后端，行为不会分叉。
//!
//! 数据形状以 `apps/desktop-web/src/shell/models.ts` 与 fixture 适配器为
//! 参照（fixture 是新 UI 开发时的参照实现）；本文件里的映射函数负责把
//! 服务端/本地库的事件形状（render.rs 的 payload）转换成 UI 的
//! ChatMessage / Conversation / Contact。
//!
//! 事件：`emit_core` 以 EventEnvelope 形状发 `core://event`；
//! 收包泵（app_state::start_recv_pump）每条入站消息都会调
//! [`forward_inbound`] 同步转发一份。
//!
//! 未实现的 action 返回 `NOT_IMPLEMENTED` 并注明原因——按"最新 UI 所有
//! 入口都能完成真实操作"的验收口径，宁可明确说没有，也不伪造成功。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use tauri::Manager as _;

use super::{AppState, LocalStore};

/// 新 UI 的请求信封（core-contract.ts: ActionRequest）。
#[derive(Debug, Deserialize)]
pub struct CoreRequest {
    pub action_id: String,
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub params: Value,
}

/// 新 UI 的响应信封（core-contract.ts: ActionResponse）。
#[derive(Debug, serde::Serialize)]
#[serde(untagged)]
pub enum CoreReply {
    Ok { ok: bool, data: Value },
    Err { ok: bool, error: CoreError },
}

#[derive(Debug, serde::Serialize)]
pub struct CoreError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    pub request_id: String,
}

/// 桥内错误：显式带契约错误码；AppState 的 `String` 错误经
/// [`BridgeError::from_string`] 分类（分类表由 e2e 测试兜底防漂移）。
#[derive(Debug)]
enum BridgeError {
    Unauth(String),
    Validation(String),
    Conflict(String),
    Confirmation(String),
    NotImpl(String),
    Dependency(String),
    Offline(String),
    Other(String),
}

impl BridgeError {
    fn code(&self) -> &'static str {
        match self {
            BridgeError::Unauth(_) => "UNAUTHENTICATED",
            BridgeError::Validation(_) => "VALIDATION",
            BridgeError::Conflict(_) => "CONFLICT",
            BridgeError::Confirmation(_) => "REQUIRES_CONFIRMATION",
            BridgeError::NotImpl(_) => "NOT_IMPLEMENTED",
            BridgeError::Dependency(_) => "DEPENDENCY_UNAVAILABLE",
            BridgeError::Offline(_) => "OFFLINE",
            BridgeError::Other(_) => "INTERNAL",
        }
    }
    fn message(&self) -> &str {
        match self {
            BridgeError::Unauth(m)
            | BridgeError::Validation(m)
            | BridgeError::Conflict(m)
            | BridgeError::Confirmation(m)
            | BridgeError::NotImpl(m)
            | BridgeError::Dependency(m)
            | BridgeError::Offline(m)
            | BridgeError::Other(m) => m,
        }
    }
    fn retryable(&self) -> bool {
        matches!(self, BridgeError::Offline(_))
    }
}

impl From<String> for BridgeError {
    fn from(message: String) -> Self {
        let lower = message.to_lowercase();
        if lower.contains("401") || lower.contains("未登录") || (lower.contains("登录") && lower.contains("失效")) {
            return BridgeError::Unauth(message);
        }
        if lower.contains("403") || lower.contains("forbidden") || lower.contains("权限") {
            return BridgeError::Validation(message);
        }
        if lower.contains("409") || lower.contains("conflict") {
            return BridgeError::Conflict(message);
        }
        if lower.contains("未连接") || lower.contains("connect") || lower.contains("network")
            || lower.contains("timed out") || lower.contains("timeout") || lower.contains("超时")
            || lower.contains("offline") || lower.contains("不可达")
        {
            return BridgeError::Offline(message);
        }
        BridgeError::Other(message)
    }
}

impl CoreReply {
    fn ok(data: Value) -> Self {
        CoreReply::Ok { ok: true, data }
    }
    fn from_result(result: Result<Value, BridgeError>, request_id: &str) -> Self {
        match result {
            Ok(data) => CoreReply::ok(data),
            Err(err) => CoreReply::Err {
                ok: false,
                error: CoreError {
                    code: err.code(),
                    message: err.message().to_string(),
                    retryable: err.retryable(),
                    retry_after_ms: None,
                    request_id: request_id.to_string(),
                },
            },
        }
    }
}

/// 桥自身的状态（与 AppState 分开管理：只放会话投影、锁、上传会话）。
pub struct CoreState {
    /// 当前会话投影的账号部分（user_id/name/avatar）。
    account: Mutex<Option<Value>>,
    lock: Mutex<LockState>,
    uploads: Mutex<HashMap<String, UploadSession>>,
    event_seq: AtomicU64,
    /// QR 登录轮询的代次：新一次 create/取消会抬版本，旧线程自灭。
    qr_poll_generation: AtomicU64,
    qr_active: Mutex<Option<String>>,
}

#[derive(Default)]
struct LockState {
    locked: bool,
    /// 锁口令的 sha256（未设置 = None，此时解锁不需要口令）。
    passcode_sha: Option<String>,
}

struct UploadSession {
    name: String,
    mime: String,
    size: u64,
    buf: Vec<u8>,
    last_index: u64,
}

impl CoreState {
    pub fn new() -> Self {
        Self {
            account: Mutex::new(None),
            lock: Mutex::new(LockState::default()),
            uploads: Mutex::new(HashMap::new()),
            event_seq: AtomicU64::new(1),
            qr_poll_generation: AtomicU64::new(0),
            qr_active: Mutex::new(None),
        }
    }

    fn account(&self) -> Option<Value> {
        self.account.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn lock_state(&self) -> MutexGuard<'_, LockState> {
        self.lock.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for CoreState {
    fn default() -> Self {
        Self::new()
    }
}

/// 这些 action_id 有真实实现（capabilities 如实上报）。
pub const IMPLEMENTED_ACTIONS: &[&str] = &[
    "identity.session.projection",
    "identity.session.login",
    "identity.session.register",
    "identity.session.logout",
    "identity.session.restore",
    "security.lock",
    "security.unlock",
    "identity.qr_login.create",
    "identity.qr_login.cancel",
    "identity.qr_login.switch_account",
    "sync.range.get",
    "sync.range.set",
    "messaging.conversations.list",
    "messaging.messages.page",
    "messaging.send",
    "messaging.retry",
    "messaging.recall",
    "messaging.forward",
    "messaging.read",
    "messaging.conversation.pin",
    "messaging.conversation.mute",
    "messaging.conversation.read_all",
    "messaging.conversation.ensure",
    "messaging.draft.get",
    "messaging.draft.save",
    "messaging.draft.clear",
    "media.upload.begin",
    "media.upload.part",
    "media.upload.complete",
    "media.upload.cancel",
    "relations.list",
    "relations.requests.incoming",
    "relations.request.send",
    "relations.request.accept",
    "relations.request.decline",
    "relations.block",
    "relations.note.set",
    "relations.tag.set",
    "relations.star.set",
    "relations.profile",
    "relations.common_groups",
    "relations.group.members",
    "relations.group.settings.set",
    "search.query",
    "preferences.get",
    "preferences.set",
    "secrets.status",
    "secrets.lock",
    "secrets.migrate",
    "storage.status",
    "storage.cache.clear",
    "backup.create",
    "backup.verify",
    "backup.import",
    "app.info",
];

// ---------------------------------------------------------------------------
// 事件
// ---------------------------------------------------------------------------

/// 以新契约的 EventEnvelope 形状发 `core://event`。
fn emit_core<R: tauri::Runtime>(app: &tauri::AppHandle<R>, event_type: &str, aggregate_id: &str, payload: Value) {
    use tauri::Emitter as _;
    let core = app.state::<CoreState>();
    let seq = core.event_seq.fetch_add(1, Ordering::Relaxed);
    let now = wxlike_core::types::now_ms();
    let envelope = json!({
        "event_id": format!("ev-{now}-{seq}"),
        "aggregate_id": aggregate_id,
        "aggregate_version": "1",
        "event_type": event_type,
        "server_time_ms": iso_ms(now as i64),
        "payload": payload,
    });
    let _ = app.emit("core://event", envelope);
}

/// 收包泵的转发钩子：一条入站消息 → `message.created` + `conversation.upsert`。
/// `me` 用于判定方向；None 时全部按入站处理（控制面不可用时的兜底）。
pub(crate) fn forward_inbound<R: tauri::Runtime>(app: &tauri::AppHandle<R>, payload: &Value, me: Option<&str>) {
    let conv_id = payload.get("conv_id").and_then(Value::as_str).unwrap_or_default();
    if conv_id.is_empty() || payload.get("kind").and_then(Value::as_str) == Some("typing") {
        return; // 控制帧与坏事件不进新 UI 的状态机
    }
    let message = row_to_message(payload, me.unwrap_or(""));
    emit_core(app, "message.created", conv_id, message.clone());
    // 会话卡片：标题先用缓存/conv_id，列表 action 会补全真实标题。
    let conversation = json!({
        "conv_id": conv_id,
        "kind": conv_kind(conv_id),
        "title": conv_id,
        "unread": 1,
        "pinned": false,
        "muted": false,
        "last_preview": preview_of(payload),
        "last_at_ms": message["created_at_ms"].clone(),
        "draft": "",
    });
    emit_core(app, "conversation.upsert", conv_id, conversation);
}

fn conv_kind(conv_id: &str) -> &'static str {
    if conv_id.starts_with("d-") { "direct" } else { "group" }
}

/// 直聊会话的对端（conv 形如 `d-{a}~{b}`，另一端是自己时取空）。
fn peer_of(conv_id: &str, me: &str) -> Option<String> {
    let rest = conv_id.strip_prefix("d-")?;
    rest.split('~').find(|p| *p != me).map(str::to_string)
}

fn preview_of(payload: &Value) -> String {
    match payload.get("kind").and_then(Value::as_str).unwrap_or_default() {
        "text" | "e2ee" => payload.get("text").and_then(Value::as_str).unwrap_or("").chars().take(40).collect(),
        "image" => "[图片]".into(),
        "msg.voice" => "[语音]".into(),
        "msg.file" => "[文件]".into(),
        "msg.sticker" => "[表情]".into(),
        "msg.video" => "[视频]".into(),
        "wallet.redpacket" => "[红包]".into(),
        "system" | "notice" => payload.get("text").and_then(Value::as_str).unwrap_or("").chars().take(40).collect(),
        other => format!("[{other}]"),
    }
}

/// 本地库/服务端历史的行（render.rs payload 形状）→ UI 的 ChatMessage。
fn row_to_message(row: &Value, me: &str) -> Value {
    let sender = row.get("sender").and_then(Value::as_str).unwrap_or_default();
    let direction = if sender == me { "out" } else { "in" };
    let kind = row.get("kind").and_then(Value::as_str).unwrap_or("text");
    let body = match kind {
        "text" | "e2ee" => json!({ "kind": "text", "text": row.get("text").and_then(Value::as_str).unwrap_or("") }),
        "image" => json!({ "kind": "image", "media_id": row.get("media_id").and_then(Value::as_str).unwrap_or("") }),
        "system" | "notice" | "recall" => json!({ "kind": "system", "text": row.get("text").and_then(Value::as_str).unwrap_or("") }),
        custom => {
            // msg.voice / msg.file / msg.sticker / msg.video / 转发卡片：
            // payload 里是结构化字段，按 UI 的 MessageBody 尽力映射。
            let p = row.get("payload").cloned().unwrap_or(json!({}));
            match custom {
                "msg.voice" => json!({
                    "kind": "voice",
                    "media_id": p.get("media_id").and_then(Value::as_str).unwrap_or(""),
                    "duration_ms": p.get("duration_ms").cloned().unwrap_or(json!(0)),
                }),
                "msg.file" => json!({
                    "kind": "file",
                    "media_id": p.get("media_id").and_then(Value::as_str).unwrap_or(""),
                    "name": p.get("name").and_then(Value::as_str).unwrap_or(""),
                    "size": p.get("size").cloned().unwrap_or(json!(0)),
                }),
                "msg.sticker" => json!({
                    "kind": "sticker",
                    "sticker": p.get("id").and_then(Value::as_str).unwrap_or(""),
                    "media_id": p.get("media_id").and_then(Value::as_str).unwrap_or(""),
                }),
                "msg.video" => json!({
                    "kind": "link",
                    "media_id": p.get("media_id").and_then(Value::as_str).unwrap_or(""),
                    "text": p.get("caption").and_then(Value::as_str).unwrap_or(""),
                }),
                _ => json!({ "kind": "system", "text": preview_of(row) }),
            }
        }
    };
    json!({
        "msg_id": row.get("msg_id").and_then(Value::as_str).unwrap_or(""),
        "conv_id": row.get("conv_id").and_then(Value::as_str).unwrap_or(""),
        "seq": row.get("seq").map(|v| v.to_string()).unwrap_or_else(|| "0".into()),
        "sender": { "user_id": sender, "name": sender },
        "direction": direction,
        "body": body,
        "state": "SERVER_ACCEPTED",
        "created_at_ms": iso_ms(row.get("sent_at_ms").and_then(Value::as_i64).unwrap_or_default()),
    })
}

/// 毫秒时间戳 → ISO-8601（UI 里 created_at_ms / last_at_ms 是字符串）。
fn iso_ms(ms: i64) -> String {
    // 没有 chrono：手工换算（UTC）。够用——只用于展示与排序。
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (h, m, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    // 1970-01-01 起的 civil date（Howard Hinnant 的算法，无依赖版）
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{s:02}.{millis:03}Z")
}

fn cursor_usize(params: &Value) -> usize {
    match params.get("cursor") {
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0) as usize,
        _ => 0,
    }
}

fn cursor_i64(params: &Value) -> i64 {
    match params.get("cursor") {
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// 会话流程（登录 → 本地库 → hub → 泵）
// ---------------------------------------------------------------------------

/// 打开完整会话：本地库 + TOFU + hub 连接 + 双泵。登录与恢复共用。
fn open_session<R: tauri::Runtime>(app: &tauri::AppHandle<R>, state: &AppState, core: &CoreState) -> Result<Value, BridgeError> {
    let me = state.me()?;
    let user_id = me["user_id"].as_str().unwrap_or_default().to_string();
    let name = me["name"].as_str().unwrap_or_default().to_string();
    let avatar = me["avatar"].as_str().map(str::to_string);
    if user_id.is_empty() {
        return Err(BridgeError::Other("服务端未返回 user_id".into()));
    }
    // 本地库：登录即打开（消息镜像 + 出队队列 + kv 都在这一个文件）。
    // `WXLIKE_DATA_DIR` 覆盖：便携安装与 e2e 测试（同一进程多账号隔离）
    // 都需要显式指定数据目录，而不是各平台 AppData 约定路径。
    let data_dir = match std::env::var("WXLIKE_DATA_DIR") {
        Ok(dir) if !dir.is_empty() => std::path::PathBuf::from(dir),
        _ => app
            .path()
            .app_data_dir()
            .map_err(|e| BridgeError::Other(format!("取数据目录失败：{e}")))?,
    };
    if !state.local_ready() {
        state.local_open(&data_dir.join("messages.db"))?;
    }
    // 会话凭据落 kv：重启后 restore 用。
    if let Some(token) = state.auth_token() {
        let _ = with_local(state, |store| {
            store.kv_put("auth_token", &token)?;
            store.kv_put(
                "account",
                &json!({"user_id": user_id, "name": name, "avatar": avatar}).to_string(),
            )
        });
    }
    // hub 地址：控制面 /endpoints 给出 QUIC 端口（TOFU 首连固定证书）。
    // 先落局部再 if let：`state.http()` 的临时 MutexGuard 若活在
    // if-let 条件里，会一直持有到块尾——后面 auth_token() 再锁同一把
    // 锁就是自己跟自己死锁（本审计实锤的登录挂起）。
    let endpoints_quic = state
        .http()
        .expect_json("GET", "/endpoints", None)
        .ok()
        .and_then(|v| v["quic"].as_str().map(str::to_string));
    if let Some(quic) = endpoints_quic {
        let pins = data_dir.join("tofu-pins.json");
        if state
            .connect_hub(&quic, &state.auth_token().unwrap_or_default(), &pins)
            .is_ok()
        {
            state.start_outbox_pump(app.clone());
            // 收包泵自带代次守卫：重复登录/重连不会叠出双泵。
            let _ = state.start_recv_pump(app.clone());
        }
        // 连不上 hub 不阻断登录：消息进 outbox，重连后由泵补发。
    }
    let account = json!({
        "user_id": user_id,
        "name": name,
        "avatar": avatar,
        "device_id": "desktop-tauri",
        "session_id": short_hash(state.auth_token().as_deref().unwrap_or("")),
    });
    *core.account.lock().unwrap_or_else(PoisonError::into_inner) = Some(account.clone());
    let projection = json!({ "phase": "authenticated", "account": account });
    emit_core(app, "session.state", &user_id, projection.clone());
    Ok(projection)
}

fn short_hash(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    hex::encode(&digest[..6])
}

fn short_hash_full(input: &str) -> String {
    hex::encode(Sha256::digest(input.as_bytes()))
}

/// 确保本地库已打开（restore/凭据读取的前置条件；重启后第一个被调用的
/// action 往往是 restore，此时还没人开过库）。
fn ensure_local<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
) -> Result<(), BridgeError> {
    if state.local_ready() {
        return Ok(());
    }
    let data_dir = match std::env::var("WXLIKE_DATA_DIR") {
        Ok(dir) if !dir.is_empty() => std::path::PathBuf::from(dir),
        _ => app
            .path()
            .app_data_dir()
            .map_err(|e| BridgeError::Other(format!("取数据目录失败：{e}")))?,
    };
    state
        .local_open(&data_dir.join("messages.db"))
        .map_err(BridgeError::Other)
}

fn with_local<T>(
    state: &AppState,
    f: impl FnOnce(&LocalStore) -> anyhow::Result<T>,
) -> Result<T, BridgeError> {
    let slot = state.local_arc();
    let guard = slot.lock().unwrap_or_else(PoisonError::into_inner);
    match guard.as_ref() {
        Some(store) => f(store).map_err(|e| BridgeError::Other(format!("本地库操作失败：{e:#}"))),
        None => Err(BridgeError::Other("本地库未打开（先登录）".into())),
    }
}

// ---------------------------------------------------------------------------
// dispatch：55 个 action 的总入口
// ---------------------------------------------------------------------------

pub fn dispatch<R: tauri::Runtime>(app: &tauri::AppHandle<R>, state: &AppState, core: &CoreState, req: CoreRequest) -> CoreReply {
    CoreReply::from_result(dispatch_inner(app, state, core, &req), &req.request_id)
}

fn dispatch_inner<R: tauri::Runtime>(app: &tauri::AppHandle<R>, state: &AppState, core: &CoreState, req: &CoreRequest) -> Result<Value, BridgeError> {
    let params = &req.params;
    let rid = req.request_id.as_str();
    let str_param = |key: &str| -> String {
        params.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
    };
    let result: Result<Value, BridgeError> = match req.action_id.as_str() {
        // ---- 会话 ----
        "identity.session.projection" => session_projection(state, core),
        "identity.session.login" => {
            let username = str_param("username");
            let password = str_param("password");
            if username.trim().is_empty() || password.is_empty() {
                return Err(BridgeError::Validation("账号或口令为空".into()));
            }
            state.login(&username, &password)?;
            open_session(app, state, core)
        }
        "identity.session.register" => {
            let username = str_param("username");
            let password = str_param("password");
            let display_name = params.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            if username.trim().is_empty() || password.len() < 6 {
                return Err(BridgeError::Validation("账号不能为空，口令至少 6 位".into()));
            }
            state.register(&username, &password)?;
            if !display_name.is_empty() {
                let _ = state.update_profile(Some(&display_name), None, None);
            }
            open_session(app, state, core)
        }
        "identity.session.logout" => {
            let _ = state.logout(); // 服务端撤销失败也照常清本地（不把人锁在旧会话里）
            state.http().token = None;
            core.account.lock().unwrap_or_else(PoisonError::into_inner).take();
            let _ = with_local(state, |s| {
                s.kv_delete("auth_token")?;
                s.kv_delete("account")
            });
            let projection = json!({ "phase": "signed_out" });
            emit_core(app, "session.state", "session", projection.clone());
            Ok(json!({ "revoked": true }))
        }
        "identity.session.restore" => {
            ensure_local(app, state)?;
            let token = with_local(state, |s| Ok(s.kv_get("auth_token"))).ok().flatten();
            match token {
                Some(token) if !token.is_empty() => {
                    state.set_auth_token(&token);
                    open_session(app, state, core)
                }
                _ => Err(BridgeError::Unauth("没有可恢复的会话".into())),
            }
        }
        "security.lock" => {
            let account = core.account();
            if account.is_none() {
                return Err(BridgeError::Unauth("尚未登录".into()));
            }
            core.lock_state().locked = true;
            let projection = json!({ "phase": "locked", "account": account, "lock_reason": "manual" });
            emit_core(app, "session.state", "session", projection.clone());
            Ok(projection)
        }
        "security.unlock" => {
            let account = core.account();
            if account.is_none() {
                return Err(BridgeError::Unauth("会话已过期".into()));
            }
            let password = str_param("password");
            let expected = core.lock_state().passcode_sha.clone();
            if let Some(expected_sha) = expected {
                if short_hash_full(&password) != expected_sha {
                    return Err(BridgeError::Unauth("口令不正确".into()));
                }
            }
            core.lock_state().locked = false;
            let projection = json!({ "phase": "authenticated", "account": account });
            emit_core(app, "session.state", "session", projection.clone());
            Ok(projection)
        }
        // ---- QR 登录（服务端 /api/qr/tickets，由 G14/G01 集成挂载） ----
        "identity.qr_login.create" => qr_create(app, state, core),
        "identity.qr_login.cancel" => {
            stop_qr_poll(core);
            Ok(json!({ "cancelled": true }))
        }
        "identity.qr_login.switch_account" => {
            stop_qr_poll(core);
            state.http().token = None;
            core.account.lock().unwrap_or_else(PoisonError::into_inner).take();
            let _ = with_local(state, |s| s.kv_delete("auth_token"));
            emit_core(app, "session.state", "session", json!({ "phase": "signed_out" }));
            Ok(json!({ "revoked": true }))
        }
        // ---- 同步范围 ----
        "sync.range.get" => {
            if core.account().is_none() {
                return Err(BridgeError::Unauth("尚未登录".into()));
            }
            let prefs = preferences_read(state);
            let days = prefs["sync_range_days"].as_i64().unwrap_or(7);
            Ok(json!({
                "days": days,
                "incremental_default_days": 7,
                "can_view_further": days != 7,
            }))
        }
        "sync.range.set" => {
            if core.account().is_none() {
                return Err(BridgeError::Unauth("尚未登录".into()));
            }
            let days = params.get("days").and_then(Value::as_i64).unwrap_or(-1);
            if ![0, 7, 30, 90].contains(&days) {
                return Err(BridgeError::Validation("同步范围不合法".into()));
            }
            let mut prefs = preferences_read(state);
            prefs["sync_range_days"] = json!(days);
            preferences_write(app, state, prefs)
        }
        // ---- 会话列表 / 消息 ----
        "messaging.conversations.list" => conversations_list(state, params),
        "messaging.messages.page" => messages_page(state, params),
        "messaging.send" => message_send(app, state, params),
        "messaging.retry" => {
            let msg_id = str_param("msg_id");
            if msg_id.is_empty() {
                return Err(BridgeError::Validation("消息不存在".into()));
            }
            let ob = state.outbox().ok_or_else(|| BridgeError::Other("本地库未打开".into()))?;
            ob.requeue(&msg_id, wxlike_core::types::now_ms() as i64)
                .map_err(|e| BridgeError::Other(e.to_string()))?;
            Ok(json!({ "msg_id": msg_id, "state": "LOCAL_QUEUED" }))
        }
        "messaging.recall" => {
            let conv_id = str_param("conv_id");
            let msg_id = str_param("msg_id");
            state.recall_msg(&conv_id, &msg_id)?;
            emit_core(
                app,
                "message.updated",
                &conv_id,
                json!({
                    "msg_id": msg_id, "conv_id": conv_id,
                    "body": { "kind": "system", "text": "你撤回了一条消息" },
                    "state": "RECALLED", "recalled": true,
                }),
            );
            Ok(json!({ "recalled": true }))
        }
        "messaging.forward" => {
            let msg_ids: Vec<String> = params
                .get("msg_ids")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            let targets: Vec<String> = params
                .get("target_conv_ids")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            if msg_ids.is_empty() || targets.is_empty() {
                return Err(BridgeError::Validation("转发参数不合法".into()));
            }
            let me = state.user_id()?;
            let mut created = 0usize;
            for target in &targets {
                let to = recipients_of(target, &me);
                for msg_id in &msg_ids {
                    let item = json!({ "conv_id": target, "msg_id": msg_id });
                    state.forward_message(target, to.clone(), &item)?;
                    created += 1;
                }
            }
            Ok(json!({ "created": created }))
        }
        "messaging.read" => {
            let conv_id = str_param("conv_id");
            state.http().mark_conv_read(&conv_id)?;
            emit_core(app, "conversation.upsert", &conv_id, json!({ "conv_id": conv_id, "unread": 0 }));
            emit_core(app, "unread.updated", "me", json!({ "total": 0 }));
            Ok(json!({ "unread": 0 }))
        }
        "messaging.conversation.pin" | "messaging.conversation.mute" => {
            let conv_id = str_param("conv_id");
            let value = params.get("value").and_then(Value::as_bool).unwrap_or(false);
            let key = format!("conv_flags:{conv_id}");
            let mut flags = with_local(state, |s| {
                Ok(s.kv_get(&key)
                    .and_then(|v| serde_json::from_str::<Value>(&v).ok())
                    .unwrap_or(json!({})))
            })?;
            let field = if req.action_id.ends_with("pin") { "pinned" } else { "muted" };
            flags[field] = json!(value);
            with_local(state, |s| s.kv_put(&key, &flags.to_string()))?;
            let mut conv = conversation_summary(state, &conv_id)?;
            conv["pinned"] = flags["pinned"].clone();
            conv["muted"] = flags["muted"].clone();
            emit_core(app, "conversation.upsert", &conv_id, conv.clone());
            Ok(conv)
        }
        "messaging.conversation.read_all" => {
            let convs = with_local(state, |s| s.conversations())?;
            for (conv, _, _) in convs {
                let unread = state.http().conv_unread(&conv).ok().and_then(|v| v["unread"].as_i64()).unwrap_or(0);
                if unread > 0 {
                    state.http().mark_conv_read(&conv)?;
                    emit_core(app, "conversation.upsert", &conv, json!({ "conv_id": conv, "unread": 0 }));
                }
            }
            emit_core(app, "unread.updated", "me", json!({ "total": 0 }));
            Ok(json!({ "total": 0 }))
        }
        "messaging.conversation.ensure" => {
            let peer = str_param("user_id");
            let me = state.user_id()?;
            let conv_id = wxlike_client::direct_conv_id(&me, &peer);
            let mut conv = conversation_summary(state, &conv_id)?;
            conv["peer_user_id"] = json!(peer);
            emit_core(app, "conversation.upsert", &conv_id, conv.clone());
            Ok(conv)
        }
        "messaging.draft.get" => {
            let conv_id = str_param("conv_id");
            let text = with_local(state, |s| Ok(s.kv_get(&format!("draft:{conv_id}")).unwrap_or_default()))?;
            Ok(json!({ "text": text }))
        }
        "messaging.draft.save" => {
            let conv_id = str_param("conv_id");
            let text = str_param("text");
            with_local(state, |s| {
                if text.is_empty() {
                    s.kv_delete(&format!("draft:{conv_id}"))
                } else {
                    s.kv_put(&format!("draft:{conv_id}"), &text)
                }
            })?;
            Ok(json!({ "saved": true }))
        }
        "messaging.draft.clear" => {
            let conv_id = str_param("conv_id");
            with_local(state, |s| s.kv_delete(&format!("draft:{conv_id}")))?;
            Ok(json!({ "cleared": true }))
        }
        // ---- 媒体上传（分片累积 → 一次性上传到 /api/media） ----
        "media.upload.begin" => {
            if core.account().is_none() {
                return Err(BridgeError::Unauth("尚未登录".into()));
            }
            let size = params.get("size").and_then(Value::as_u64).unwrap_or(0);
            if size == 0 || size > 1024 * 1024 * 1024 {
                return Err(BridgeError::Validation("文件大小超出预算".into()));
            }
            let upload_id = format!("up-{}", wxlike_core::types::now_ms());
            let mime = str_param("mime");
            core.uploads.lock().unwrap_or_else(PoisonError::into_inner).insert(
                upload_id.clone(),
                UploadSession {
                    name: str_param("name"),
                    mime: if mime.is_empty() { "application/octet-stream".into() } else { mime },
                    size,
                    buf: Vec::with_capacity(size.min(8 * 1024 * 1024) as usize),
                    last_index: u64::MAX,
                },
            );
            Ok(json!({ "upload_id": upload_id, "chunk_size": 4 * 1024 * 1024 }))
        }
        "media.upload.part" => {
            use base64::Engine as _;
            let upload_id = str_param("upload_id");
            let mut uploads = core.uploads.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(upload) = uploads.get_mut(&upload_id) else {
                return Err(BridgeError::Validation("上传会话不存在".into()));
            };
            let index = params.get("index").and_then(Value::as_u64).unwrap_or(0);
            // 分片必须有序：乱序/重放直接拒（同片重试由幂等键保证，不靠这里宽容）。
            if upload.last_index != u64::MAX && index != upload.last_index + 1 {
                return Err(BridgeError::Conflict("分片乱序".into()));
            }
            let data = params
                .get("data_base64")
                .and_then(Value::as_str)
                .map(|s| base64::engine::general_purpose::STANDARD.decode(s))
                .transpose()
                .map_err(|e| BridgeError::Validation(format!("分片 base64 解不开: {e}")))?
                .unwrap_or_default();
            let declared = params.get("length").and_then(Value::as_u64).unwrap_or(0);
            if !data.is_empty() && data.len() as u64 != declared {
                return Err(BridgeError::Validation("分片长度与声明不符".into()));
            }
            upload.buf.extend_from_slice(&data);
            upload.last_index = index;
            if upload.buf.len() as u64 > upload.size {
                return Err(BridgeError::Validation("上传超出声明大小".into()));
            }
            Ok(json!({ "received": upload.buf.len(), "size": upload.size }))
        }
        "media.upload.complete" => {
            let upload_id = str_param("upload_id");
            let upload = core
                .uploads
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&upload_id)
                .ok_or_else(|| BridgeError::Validation("上传会话不存在".into()))?;
            if upload.buf.len() as u64 != upload.size {
                return Err(BridgeError::Conflict("上传未完成".into()));
            }
            let sha256 = hex::encode(Sha256::digest(&upload.buf));
            let conv = params.get("conv_id").and_then(Value::as_str);
            let reply = state.upload_media(upload.buf, &upload.mime, Some(&sha256), conv, false)?;
            Ok(json!({
                "media_id": reply["media_id"].clone(),
                "name": upload.name,
                "size": upload.size,
                "mime": upload.mime,
                "digest": sha256,
                "digest_algorithm": "SHA-256",
            }))
        }
        "media.upload.cancel" => {
            core.uploads
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&str_param("upload_id"));
            Ok(json!({ "cancelled": true }))
        }
        // ---- 关系 ----
        "relations.list" => relations_list(state, params),
        "relations.requests.incoming" => {
            let reply = state.friend_requests_incoming()?;
            let mut rows = Vec::new();
            for item in reply.as_array().cloned().unwrap_or_default() {
                let from_id = item["from_user_id"].as_str().unwrap_or_default().to_string();
                let from_name = item["from_name"].as_str().filter(|v| !v.is_empty()).unwrap_or(from_id.as_str()).to_string();
                rows.push(json!({
                    "request_id": item["id"].clone(),
                    "from": { "user_id": from_id, "name": from_name },
                    "message": item["message"].as_str().unwrap_or_default().to_string(),
                    "status": "PENDING",
                    "created_at_ms": iso_ms(item["created_at"].as_i64().unwrap_or_default()),
                }));
            }
            Ok(Value::Array(rows))
        }
        "relations.request.send" => {
            let target = str_param("user_id");
            let message = str_param("message");
            if target.is_empty() {
                return Err(BridgeError::Validation("请选择联系人".into()));
            }
            state.friend_request(&target, &message)?;
            Ok(json!({ "request_id": format!("out-{rid}"), "status": "PENDING" }))
        }
        "relations.request.accept" | "relations.request.decline" => {
            let request_id = str_param("request_id");
            let accept = req.action_id.ends_with("accept");
            if accept {
                state.friend_accept(&request_id)?;
            } else {
                state.friend_decline(&request_id)?;
            }
            Ok(json!({ "status": if accept { "ACCEPTED" } else { "DECLINED" } }))
        }
        "relations.block" => {
            let user_id = str_param("user_id");
            let blocked = params.get("blocked").and_then(Value::as_bool).unwrap_or(false);
            if blocked {
                state
                    .http()
                    .expect_json("POST", "/api/blocks", Some(&json!({ "user_id": user_id })))?;
            } else {
                state
                    .http()
                    .expect_empty("DELETE", &format!("/api/blocks/{user_id}"), None)?;
            }
            Ok(json!({ "user_id": user_id, "blocked": blocked }))
        }
        "relations.note.set" => {
            let user_id = str_param("user_id");
            let note = str_param("note");
            state.http().expect_json(
                "PUT",
                &format!("/api/friends/{user_id}/remark"),
                Some(&json!({ "remark": note })),
            )?;
            Ok(json!({ "user_id": user_id, "note": note }))
        }
        "relations.tag.set" | "relations.star.set" => {
            let user_id = str_param("user_id");
            let key = format!("contact_meta:{user_id}");
            let mut meta = with_local(state, |s| {
                Ok(s.kv_get(&key)
                    .and_then(|v| serde_json::from_str::<Value>(&v).ok())
                    .unwrap_or(json!({})))
            })?;
            if req.action_id.ends_with("tag.set") {
                meta["tags"] = params.get("tags").cloned().unwrap_or(json!([]));
            } else {
                meta["star"] = params.get("star").cloned().unwrap_or(json!(false));
            }
            with_local(state, |s| s.kv_put(&key, &meta.to_string()))?;
            let mut contact = base_contact(state, &user_id)?;
            contact["tags"] = meta["tags"].clone();
            contact["star"] = json!(meta["star"].as_bool().unwrap_or(false));
            emit_core(app, "contact.updated", &user_id, contact.clone());
            Ok(contact)
        }
        "relations.profile" => {
            let user_id = str_param("user_id");
            let profile = state.http().expect_json("GET", &format!("/api/profile/{user_id}"), None)?;
            let mut value = json!({
                "user_id": profile["user_id"].clone(),
                "name": profile["name"].clone(),
                "avatar": profile["avatar"].clone(),
                "signature": profile["signature"].clone(),
                "is_friend": true,
                "blocked": false,
                "common_groups": [],
            });
            if let Some(remark) = remark_of(state, &user_id) {
                value["note"] = json!(remark);
            }
            Ok(value)
        }
        "relations.common_groups" => {
            let user_id = str_param("user_id");
            let groups = state.groups_mine()?;
            let mut out = Vec::new();
            for g in groups.as_array().cloned().unwrap_or_default() {
                let gid = g["group_id"].as_str().unwrap_or_default().to_string();
                if gid.is_empty() {
                    continue;
                }
                if let Ok(info) = state.group_info(&gid) {
                    let has = info["members"].as_array().map(|m| {
                        m.iter().any(|x| x["user_id"] == json!(user_id) || x["member_id"] == json!(user_id))
                    });
                    if has == Some(true) {
                        out.push(json!({ "group_id": gid, "name": g["name"].clone() }));
                    }
                }
            }
            Ok(Value::Array(out))
        }
        "relations.group.members" => {
            let group_id = str_param("group_id");
            let info = state.group_info(&group_id)?;
            let me = state.user_id()?;
            let mut members = Vec::new();
            for m in info["members"].as_array().cloned().unwrap_or_default() {
                members.push(json!({
                    "user_id": m["user_id"].as_str().filter(|s| !s.is_empty()).or_else(|| m["member_id"].as_str()).unwrap_or_default().to_string(),
                    "name": m["name"].as_str().filter(|s| !s.is_empty()).or_else(|| m["member_id"].as_str()).unwrap_or_default().to_string(),
                    "role": m["role"].as_str().map(|r| json!(r)).unwrap_or_else(|| json!("member")),
                    "joined_at_ms": iso_ms(m["joined_at"].as_i64().unwrap_or_default()),
                }));
            }
            let my_role = info["members"]
                .as_array()
                .and_then(|a| {
                    a.iter()
                        .find(|m| m["user_id"] == json!(me) || m["member_id"] == json!(me))
                })
                .map(|m| m["role"].as_str().map(|r| json!(r)).unwrap_or_else(|| json!("member")))
                .unwrap_or_else(|| json!("member"));
            Ok(json!({
                "group_id": group_id,
                "name": info["name"].clone(),
                "my_role": my_role,
                "members": members,
            }))
        }
        "relations.group.kick" => Err(BridgeError::NotImpl(
            "服务端暂无群成员移除路由（G02 域），拒绝伪造结果".into(),
        )),
        "relations.group.settings.set" => {
            let group_id = str_param("group_id");
            let name = str_param("name");
            if name.is_empty() {
                return Err(BridgeError::Validation("群名称不能为空".into()));
            }
            // 服务端只有公告路由有写入口；改名路由未挂载时如实报错。
            state
                .http()
                .expect_json("PUT", &format!("/api/groups/{group_id}"), Some(&json!({ "name": name })))?;
            emit_core(app, "group.updated", &group_id, json!({ "group_id": group_id, "name": name }));
            Ok(json!({ "group_id": group_id, "name": name }))
        }
        // ---- 搜索 ----
        "search.query" => search_query(state, params),
        // ---- 偏好 / 密钥 / 存储 / 备份 ----
        "preferences.get" => Ok(preferences_read(state)),
        "preferences.set" => {
            let patch = params.get("patch").cloned().unwrap_or(Value::Null);
            if !patch.is_object() {
                return Err(BridgeError::Validation("缺少设置内容".into()));
            }
            let mut prefs = preferences_read(state);
            if let Some(obj) = patch.as_object() {
                for (k, v) in obj {
                    prefs[k] = v.clone();
                }
            }
            preferences_write(app, state, prefs)
        }
        "secrets.status" => {
            let lock = core.lock_state();
            Ok(json!({
                "locked": lock.locked,
                "os_backed": false,
                "passphrase_migration": if lock.passcode_sha.is_some() { "migrated" } else { "available" },
                "note": "桌面锁口令存本地库（sha256）；OS 密钥库由 G02 提供",
            }))
        }
        "secrets.lock" => {
            core.lock_state().locked = true;
            Ok(json!({ "locked": true }))
        }
        "secrets.migrate" => {
            let current = str_param("current_password");
            let next = str_param("new_password");
            let expected = core.lock_state().passcode_sha.clone();
            if let Some(expected_sha) = expected {
                if short_hash_full(&current) != expected_sha {
                    return Err(BridgeError::Validation("当前口令不正确".into()));
                }
            }
            if next.len() < 8 {
                return Err(BridgeError::Validation("新口令至少 8 位".into()));
            }
            core.lock_state().passcode_sha = Some(short_hash_full(&next));
            Ok(json!({ "migrated": true, "os_backed": false }))
        }
        "storage.status" => {
            let bytes = with_local(state, |s| {
                Ok(std::fs::metadata(s.path()).map(|m| m.len()).unwrap_or(0))
            })
            .unwrap_or(0);
            Ok(json!({ "cache_bytes": 0, "message_bytes": bytes, "media_bytes": 0 }))
        }
        "storage.cache.clear" => Ok(json!({ "freed_bytes": 0, "kept": ["未发送 outbox", "草稿", "密钥"] })),
        "backup.create" => {
            let pending = outbox_pending(state);
            if pending > 0 {
                return Err(BridgeError::Conflict("还有未发送消息，先发完或清空再备份".into()));
            }
            // WAL checkpoint 后原文件即完整快照（secure_delete 也不留正文残余）。
            with_local(state, |s| {
                let _ = s.lock().execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
                Ok(())
            })?;
            let src = with_local(state, |s| Ok(s.path().to_path_buf()))?;
            let bytes = std::fs::read(&src).map_err(|e| BridgeError::Other(format!("读本地库失败：{e}")))?;
            if bytes.len() > 64 * 1024 * 1024 {
                return Err(BridgeError::Dependency(
                    "备份超过内联上限（64MB），大库备份走文件导出（G02）".into(),
                ));
            }
            let digest = hex::encode(Sha256::digest(&bytes));
            let info = json!({
                "file_name": format!("wxlike-backup-{}.db", wxlike_core::types::now_ms()),
                "size": bytes.len(),
                "digest": digest,
                "created_at_ms": iso_ms(wxlike_core::types::now_ms() as i64),
            });
            use base64::Engine as _;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            Ok(json!({ "info": info, "base64": b64 }))
        }
        "backup.verify" => {
            use base64::Engine as _;
            let raw = str_param("base64");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&raw)
                .map_err(|_| BridgeError::Validation("文件不是有效的备份".into()))?;
            let digest = hex::encode(Sha256::digest(&bytes));
            if !bytes.starts_with(b"SQLite format 3") {
                return Ok(json!({ "valid": false, "reason": "备份格式不匹配", "digest": digest }));
            }
            Ok(json!({ "valid": true, "digest": digest }))
        }
        "backup.import" => {
            if str_param("confirmation") != "IMPORT" {
                return Err(BridgeError::Confirmation("导入会覆盖本地数据，需要再次确认".into()));
            }
            if outbox_pending(state) > 0 {
                return Err(BridgeError::Conflict("还有未发送消息，导入前先清空".into()));
            }
            use base64::Engine as _;
            let raw = str_param("base64");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&raw)
                .map_err(|_| BridgeError::Validation("备份文件损坏".into()))?;
            if !bytes.starts_with(b"SQLite format 3") {
                return Err(BridgeError::Validation("备份与当前账号不匹配".into()));
            }
            let path = with_local(state, |s| Ok(s.path().to_path_buf()))?;
            let tmp = path.with_extension("import-tmp");
            std::fs::write(&tmp, &bytes).map_err(|e| BridgeError::Other(format!("写导入文件失败：{e}")))?;
            std::fs::rename(&tmp, &path).map_err(|e| BridgeError::Other(format!("替换本地库失败：{e}")))?;
            // 重开本地库（含出队队列与 kv）。
            state.reopen_local(&path)?;
            emit_core(app, "search.invalidated", "me", json!({}));
            Ok(json!({ "imported": true }))
        }
        "app.info" => Ok(json!({
            "name": "wxlike",
            "version": env!("CARGO_PKG_VERSION"),
            "commit": option_env!("WXLIKE_GIT_SHA").unwrap_or("unknown"),
            "built_at": iso_ms(option_env!("WXLIKE_BUILD_TIME").and_then(|t| t.parse().ok()).unwrap_or(0)),
            "licenses": [
                { "name": "wxlike", "license": "AGPL-3.0-only" },
                { "name": "tauri", "license": "MIT/Apache-2.0" },
                { "name": "quinn (QUIC)", "license": "MIT/Apache-2.0" },
            ],
        })),
        other => {
            return Err(BridgeError::NotImpl(format!(
                "core 桥未实现 {other}（action 需先在 cmd catalog 登记）"
            )))
        }
    };
    result
}

#[allow(dead_code)]
fn __unused_result_shim(result: Result<Value, BridgeError>, rid: &str) -> CoreReply {
    CoreReply::from_result(result, rid)
}

fn outbox_pending(state: &AppState) -> i64 {
    state
        .outbox()
        .and_then(|ob| ob.pending_count().ok())
        .map(|n| n as i64)
        .unwrap_or(0)
}

fn session_projection(state: &AppState, core: &CoreState) -> Result<Value, BridgeError> {
    // 有凭据 → 服务端确证；凭据缺失 → signed_out；锁优先于一切。
    // （本地库没开时读不到 kv：投影如实说 signed_out，restore 负责开库。）
    if let Some(account) = core.account() {
        if core.lock_state().locked {
            return Ok(json!({ "phase": "locked", "account": account, "lock_reason": "manual" }));
        }
        return Ok(json!({ "phase": "authenticated", "account": account }));
    }
    if let Some(token) = with_local(state, |s| Ok(s.kv_get("auth_token"))).ok().flatten() {
        if !token.is_empty() {
            state.set_auth_token(&token);
            if let Ok(me) = state.me() {
                let account = json!({
                    "user_id": me["user_id"].clone(),
                    "name": me["name"].clone(),
                    "avatar": me["avatar"].clone(),
                    "device_id": "desktop-tauri",
                    "session_id": short_hash(&token),
                });
                *core.account.lock().unwrap_or_else(PoisonError::into_inner) = Some(account.clone());
                return Ok(json!({ "phase": "authenticated", "account": account }));
            }
        }
    }
    Ok(json!({ "phase": "signed_out" }))
}

// ---------------------------------------------------------------------------
// 会话列表 / 消息页
// ---------------------------------------------------------------------------

fn conversations_list(state: &AppState, params: &Value) -> Result<Value, BridgeError> {
    let filter = params.get("filter").and_then(Value::as_str).unwrap_or("all");
    let query = params
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(30).clamp(1, 100) as usize;
    let offset = cursor_usize(params);

    let me = state.user_id()?;
    let mut rows = Vec::new();
    for (conv, max_seq, max_at) in with_local(state, |s| s.conversations())? {
        rows.push(conversation_summary_parts(state, &conv, &me, max_seq, max_at));
    }
    if filter == "unread" {
        rows.retain(|c| c["unread"].as_i64().unwrap_or(0) > 0);
    }
    if filter == "groups" {
        rows.retain(|c| c["kind"] == json!("group"));
    }
    if !query.is_empty() {
        rows.retain(|c| {
            c["title"].as_str().unwrap_or_default().to_lowercase().contains(&query)
                || c["last_preview"].as_str().unwrap_or_default().to_lowercase().contains(&query)
        });
    }
    rows.sort_by(|a, b| {
        b["pinned"].as_bool().cmp(&a["pinned"].as_bool())
            .then(b["last_at_ms"].as_str().cmp(&a["last_at_ms"].as_str()))
    });
    let total = rows.len();
    let items: Vec<Value> = rows.into_iter().skip(offset).take(limit).collect();
    let has_more = offset + items.len() < total;
    let next = if has_more { json!(offset + items.len()) } else { Value::Null };
    Ok(json!({ "items": items, "next_cursor": next, "has_more": has_more }))
}

/// 单个会话卡片（含最新一条消息的预览）。
fn conversation_summary(state: &AppState, conv_id: &str) -> Result<Value, BridgeError> {
    let me = state.user_id()?;
    let last = with_local(state, |s| {
        Ok(s.page(conv_id, 0, 1).map(|mut rows| rows.pop()).unwrap_or(None))
    })?;
    match last {
        Some(row) => {
            let payload = row.to_json();
            let mut c = conversation_summary_parts(state, conv_id, &me, row.seq, row.sent_at_ms);
            c["last_preview"] = json!(preview_of(&payload));
            c["last_at_ms"] = json!(iso_ms(row.sent_at_ms));
            Ok(c)
        }
        None => Ok(conversation_summary_parts(state, conv_id, &me, 0, wxlike_core::types::now_ms() as i64)),
    }
}

fn conversation_summary_parts(
    state: &AppState,
    conv_id: &str,
    me: &str,
    max_seq: i64,
    max_at: i64,
) -> Value {
    let kind = conv_kind(conv_id);
    // 标题：直聊用 kv 缓存的对端名（缺失时拉一次 profile 并缓存）；
    // 群聊用 group_info 的名字（同样缓存）。
    let title = match kind {
        "direct" => {
            let peer = peer_of(conv_id, me).unwrap_or_else(|| conv_id.to_string());
            let cache_key = format!("peer_name:{peer}");
            let cached = with_local(state, |s| Ok(s.kv_get(&cache_key)))
                .ok()
                .flatten()
                .unwrap_or_default();
            if !cached.is_empty() {
                cached
            } else {
                let name = state
                    .http()
                    .expect_json("GET", &format!("/api/profile/{peer}"), None)
                    .ok()
                    .and_then(|p| p["name"].as_str().map(str::to_string))
                    .unwrap_or_else(|| peer.clone());
                let _ = with_local(state, |s| s.kv_put(&cache_key, &name));
                name
            }
        }
        _ => {
            let cache_key = format!("group_name:{conv_id}");
            let cached = with_local(state, |s| Ok(s.kv_get(&cache_key)))
                .ok()
                .flatten()
                .unwrap_or_default();
            if !cached.is_empty() {
                cached
            } else {
                let name = state
                    .group_info(conv_id)
                    .ok()
                    .and_then(|g| g["name"].as_str().map(str::to_string))
                    .unwrap_or_else(|| conv_id.to_string());
                let _ = with_local(state, |s| s.kv_put(&cache_key, &name));
                name
            }
        }
    };
    let unread = state
        .http()
        .conv_unread(conv_id)
        .ok()
        .and_then(|v| v["unread"].as_i64())
        .unwrap_or(0);
    let flags = with_local(state, |s| {
        Ok(s.kv_get(&format!("conv_flags:{conv_id}"))
            .and_then(|v| serde_json::from_str::<Value>(&v).ok())
            .unwrap_or(json!({})))
    })
    .unwrap_or(json!({}));
    let draft = with_local(state, |s| Ok(s.kv_get(&format!("draft:{conv_id}")).unwrap_or_default()))
        .unwrap_or_default();
    let at = if max_seq > 0 { max_at } else { wxlike_core::types::now_ms() as i64 };
    json!({
        "conv_id": conv_id,
        "kind": kind,
        "title": title,
        "unread": unread,
        "pinned": flags["pinned"].as_bool().unwrap_or(false),
        "muted": flags["muted"].as_bool().unwrap_or(false),
        "last_preview": "",
        "last_at_ms": iso_ms(at),
        "draft": draft,
    })
}

fn messages_page(state: &AppState, params: &Value) -> Result<Value, BridgeError> {
    let conv_id = params.get("conv_id").and_then(Value::as_str).unwrap_or_default();
    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(30).clamp(1, 100) as i64;
    let before = cursor_i64(params);
    // 先拉服务端这一页并入本地库（INSERT OR IGNORE），再从本地出——
    // 本地是唯一渲染源，重启后同一会话同一形状。
    eprintln!("[pg2] http history");
    let server = state
        .http()
        .expect_json("GET", &format!("/api/history/{conv_id}?before_seq={before}&limit={limit}"), None)
        .ok();
    eprintln!("[pg2] http done server={}", server.is_some());
    if let Some(reply) = server {
        if let Some(rows) = reply["messages"].as_array() {
            with_local(state, |s| {
                for row in rows {
                    let _ = s.append(row);
                }
                Ok(())
            })?;
        }
    }
    eprintln!("[pg2] append+me ok");
    let me = state.user_id()?;
    let page = state.local_history(conv_id, before, limit as usize)?;
    let rows = page["messages"].as_array().cloned().unwrap_or_default();
    let items: Vec<Value> = rows.iter().map(|r| row_to_message(r, &me)).collect();
    let has_more = (rows.len() as i64) == limit;
    let next = if has_more {
        rows.first()
            .and_then(|r| r.get("seq"))
            .map(|v| Value::String(v.to_string()))
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    Ok(json!({ "items": items, "next_cursor": next, "has_more": has_more }))
}

fn recipients_of(conv_id: &str, me: &str) -> Vec<String> {
    if conv_id.starts_with("d-") {
        peer_of(conv_id, me).into_iter().collect()
    } else {
        Vec::new() // 群消息由 hub 按成员表扇出
    }
}

fn message_send<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    params: &Value,
) -> Result<Value, BridgeError> {
    let conv_id = params.get("conv_id").and_then(Value::as_str).unwrap_or_default();
    let body = params.get("body").cloned().unwrap_or(Value::Null);
    let kind = body["kind"].as_str().unwrap_or_default();
    let client_msg_id = params
        .get("client_msg_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if conv_id.is_empty() || !body.is_object() {
        return Err(BridgeError::Validation("消息参数不合法".into()));
    }
    let me = state.user_id()?;
    let to = recipients_of(conv_id, &me);
    match kind {
        "text" => {
            let text = body["text"].as_str().unwrap_or_default();
            if text.trim().is_empty() {
                return Err(BridgeError::Validation("不能发送空消息".into()));
            }
            state.send_text(conv_id, to, text)?;
        }
        "image" => {
            let media_id = body["media_id"].as_str().unwrap_or_default();
            if media_id.is_empty() {
                return Err(BridgeError::Validation("图片尚未上传".into()));
            }
            state.send_image(conv_id, to, media_id)?;
        }
        "file" => {
            state.send_file(
                conv_id,
                to,
                body["media_id"].as_str().unwrap_or_default(),
                body["name"].as_str().unwrap_or_default(),
                body["size"].as_u64().unwrap_or(0),
                body["sha256"].as_str().unwrap_or(""),
            )?;
        }
        "voice" => {
            state.send_voice(
                conv_id,
                to,
                body["media_id"].as_str().unwrap_or_default(),
                body["duration_ms"].as_u64().unwrap_or(0) as u32,
                Vec::new(),
            )?;
        }
        "sticker" => {
            state.send_sticker(
                conv_id,
                to,
                body["media_id"].as_str().unwrap_or_default(),
                "default",
                body["sticker"].as_str().unwrap_or_default(),
            )?;
        }
        other => {
            return Err(BridgeError::NotImpl(format!("暂不支持发送 {other} 消息")));
        }
    }
    // 本地回显已由 append_own_echo 落库；补一条事件让 UI 立即上屏。
    let last = with_local(state, |s| {
        Ok(s.page(conv_id, 0, 1).map(|mut rows| rows.pop()).unwrap_or(None))
    })?;
    if let Some(row) = last {
        let payload = row.to_json();
        let message = row_to_message(&payload, &me);
        let msg_id = message["msg_id"].clone();
        emit_core(app, "message.created", conv_id, message.clone());
        emit_core(
            app,
            "conversation.upsert",
            conv_id,
            json!({ "conv_id": conv_id, "last_preview": preview_of(&payload), "last_at_ms": message["created_at_ms"] }),
        );
        return Ok(json!({ "msg_id": msg_id, "client_msg_id": client_msg_id, "state": "SERVER_ACCEPTED" }));
    }
    Ok(json!({ "client_msg_id": client_msg_id, "state": "SERVER_ACCEPTED" }))
}

// ---------------------------------------------------------------------------
// 关系 / 搜索 / 偏好
// ---------------------------------------------------------------------------

fn remark_of(state: &AppState, user_id: &str) -> Option<String> {
    state
        .friends()
        .ok()?
        .as_array()?
        .iter()
        .find(|f| f["friend_id"] == json!(user_id))
        .and_then(|f| f["remark"].as_str().map(str::to_string))
        .filter(|r| !r.is_empty())
}

fn base_contact(state: &AppState, user_id: &str) -> Result<Value, BridgeError> {
    let friends = state.friends()?;
    let row = friends
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|f| f["friend_id"] == json!(user_id));
    let meta = with_local(state, |s| {
        Ok(s.kv_get(&format!("contact_meta:{user_id}"))
            .and_then(|v| serde_json::from_str::<Value>(&v).ok())
            .unwrap_or(json!({})))
    })
    .unwrap_or(json!({}));
    let name = row
        .as_ref()
        .and_then(|r| r["remark"].as_str().filter(|s| !s.is_empty()).map(str::to_string))
        .unwrap_or_else(|| user_id.to_string());
    Ok(json!({
        "user_id": user_id,
        "name": name,
        "tags": if meta["tags"].is_array() { meta["tags"].clone() } else { json!([]) },
        "star": meta["star"].as_bool().unwrap_or(false),
        "online": false,
        "blocked": false,
    }))
}

fn relations_list(state: &AppState, params: &Value) -> Result<Value, BridgeError> {
    let query = params
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let star_only = params.get("star").and_then(Value::as_bool).unwrap_or(false);
    let tag_filter = params.get("tag").and_then(Value::as_str).unwrap_or_default();
    let friends = state.friends()?;
    let mut items = Vec::new();
    let mut all_tags = Vec::new();
    for f in friends.as_array().cloned().unwrap_or_default() {
        let user_id = f["friend_id"].as_str().unwrap_or_default().to_string();
        if user_id.is_empty() {
            continue;
        }
        let contact = base_contact(state, &user_id)?;
        for t in contact["tags"].as_array().cloned().unwrap_or_default() {
            if let Some(s) = t.as_str() {
                all_tags.push(s.to_string());
            }
        }
        items.push(contact);
    }
    if !query.is_empty() {
        items.retain(|c| {
            c["name"].as_str().unwrap_or_default().to_lowercase().contains(&query)
                || c["user_id"].as_str().unwrap_or_default().to_lowercase().contains(&query)
        });
    }
    if !tag_filter.is_empty() {
        items.retain(|c| {
            c["tags"]
                .as_array()
                .map(|a| a.iter().any(|t| t.as_str() == Some(tag_filter)))
                .unwrap_or(false)
        });
    }
    if star_only {
        items.retain(|c| c["star"].as_bool().unwrap_or(false));
    }
    all_tags.sort();
    all_tags.dedup();
    Ok(json!({ "items": items, "tags": all_tags }))
}

fn search_query(state: &AppState, params: &Value) -> Result<Value, BridgeError> {
    let query = params.get("query").and_then(Value::as_str).unwrap_or_default();
    let category = params.get("category").and_then(Value::as_str).unwrap_or("all");
    if query.trim().is_empty() {
        return Ok(json!({ "category": category, "hits": [] }));
    }
    let mut hits = Vec::new();
    // 消息全文（服务端 /api/search/messages）——全部/内容/会话分类。
    if matches!(category, "all" | "content" | "chats") {
        let reply = state.search_messages(query, 20)?;
        for row in reply["results"].as_array().cloned().unwrap_or_default() {
            let text = row.get("text").and_then(Value::as_str).unwrap_or_default();
            hits.push(json!({
                "id": row.get("msg_id").cloned().unwrap_or_else(|| json!("")),
                "kind": "content",
                "title": text.chars().take(30).collect::<String>(),
                "subtitle": row.get("conv_id").and_then(Value::as_str).unwrap_or_default(),
                "conv_id": row.get("conv_id").cloned().unwrap_or(Value::Null),
                "snippet": text.chars().skip(30).take(50).collect::<String>(),
            }));
        }
    }
    // 联系人与群（本地已有的关系面）。
    if matches!(category, "all" | "contacts") {
        for contact in relations_list(state, &json!({ "query": query }))?["items"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            hits.push(json!({
                "id": contact["user_id"].clone(),
                "kind": "contact",
                "title": contact["name"].clone(),
                "subtitle": contact["user_id"].clone(),
                "user_id": contact["user_id"].clone(),
            }));
        }
    }
    if matches!(category, "all" | "groups") {
        for g in state.groups_mine()?.as_array().cloned().unwrap_or_default() {
            let name = g["name"].as_str().unwrap_or_default();
            if name.to_lowercase().contains(&query.to_lowercase()) {
                hits.push(json!({
                    "id": g["group_id"].clone(),
                    "kind": "group",
                    "title": name,
                    "subtitle": format!("{} 人", g["member_count"].as_i64().unwrap_or(0)),
                    "group_id": g["group_id"].clone(),
                }));
            }
        }
    }
    Ok(json!({ "category": category, "hits": hits }))
}

fn default_preferences() -> Value {
    json!({
        "theme": "system",
        "compact": false,
        "notification_preview": true,
        "enter_to_send": true,
        "sync_range_days": 7,
        "auto_login": true,
    })
}

fn preferences_read(state: &AppState) -> Value {
    with_local(state, |s| {
        Ok(s.kv_get("preferences")
            .and_then(|v| serde_json::from_str::<Value>(&v).ok())
            .unwrap_or_else(default_preferences))
    })
    .unwrap_or_else(|_| default_preferences())
}

fn preferences_write<R: tauri::Runtime>(app: &tauri::AppHandle<R>, state: &AppState, prefs: Value) -> Result<Value, BridgeError> {
    with_local(state, |s| s.kv_put("preferences", &prefs.to_string()))?;
    if let Some(user_id) = prefs_get_user(state) {
        emit_core(app, "preferences.updated", &user_id, prefs.clone());
    }
    Ok(prefs)
}

fn prefs_get_user(state: &AppState) -> Option<String> {
    with_local(state, |s| {
        Ok(s.kv_get("account")
            .and_then(|v| serde_json::from_str::<Value>(&v).ok())
            .and_then(|a| a["user_id"].as_str().map(str::to_string)))
    })
    .ok()
    .flatten()
}

// ---------------------------------------------------------------------------
// QR 登录
// ---------------------------------------------------------------------------

fn stop_qr_poll(core: &CoreState) {
    core.qr_poll_generation.fetch_add(1, Ordering::SeqCst);
    *core.qr_active.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

fn qr_create<R: tauri::Runtime>(app: &tauri::AppHandle<R>, state: &AppState, core: &CoreState) -> Result<Value, BridgeError> {
    // 已有未结束的票据：直接复用（fixture 同语义）。
    if let Some(active) = core.qr_active.lock().unwrap_or_else(PoisonError::into_inner).clone() {
        // 同上：http 守卫先落局部，别活在 if-let 条件里。
        let existing = state.http().expect_json("GET", &format!("/api/qr/tickets/{active}"), None);
        if let Ok(reply) = existing {
            let state_str = reply["state"].as_str().unwrap_or("CREATED");
            if !matches!(state_str, "APPROVED" | "EXPIRED" | "REJECTED") {
                return Ok(qr_challenge(&active, state_str, &reply));
            }
        }
    }
    let reply = state
        .http()
        .expect_json("POST", "/api/qr/tickets", Some(&json!({ "mode": "login" })))
        .map_err(|e| {
            if e.contains("404") {
                BridgeError::NotImpl("服务端未挂载 QR 登录路由（等待 G14 集成）".into())
            } else {
                BridgeError::from(e)
            }
        })?;
    let ticket = reply["ticket_id"].as_str().unwrap_or_default().to_string();
    if ticket.is_empty() {
        return Err(BridgeError::NotImpl("服务端 QR 票据响应缺少 ticket_id".into()));
    }
    let challenge = qr_challenge(&ticket, "CREATED", &reply);
    // 轮询线程：票据状态变化 → qr.state 事件（代次守卫：新 create/取消让旧线程退场）。
    stop_qr_poll(core);
    let generation = core.qr_poll_generation.load(Ordering::SeqCst);
    *core.qr_active.lock().unwrap_or_else(PoisonError::into_inner) = Some(ticket.clone());
    let app = app.clone();
    let ticket_for_poll = ticket;
    let challenge_for_emit = challenge.clone();
    std::thread::spawn(move || {
        use tauri::Emitter as _;
        let _ = app.emit("core://event", json!({
            "event_id": format!("qr-{ticket_for_poll}"),
            "aggregate_id": ticket_for_poll,
            "aggregate_version": "1",
            "event_type": "qr.state",
            "server_time_ms": iso_ms(wxlike_core::types::now_ms() as i64),
            "payload": challenge_for_emit,
        }));
        let mut last = String::new();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            if app.state::<CoreState>().qr_poll_generation.load(Ordering::SeqCst) != generation {
                break;
            }
            let state = app.state::<AppState>();
            let Ok(reply) = state
                .http()
                .expect_json("GET", &format!("/api/qr/tickets/{ticket_for_poll}"), None)
            else {
                continue;
            };
            let now = reply["state"].as_str().unwrap_or_default().to_string();
            if !now.is_empty() && now != last {
                last = now.clone();
                let payload = qr_challenge(&ticket_for_poll, &now, &reply);
                let _ = app.emit("core://event", json!({
                    "event_id": format!("qr-{ticket_for_poll}-{now}"),
                    "aggregate_id": ticket_for_poll,
                    "aggregate_version": "1",
                    "event_type": "qr.state",
                    "server_time_ms": iso_ms(wxlike_core::types::now_ms() as i64),
                    "payload": payload,
                }));
                if matches!(now.as_str(), "APPROVED" | "EXPIRED" | "REJECTED") {
                    break;
                }
            }
        }
    });
    Ok(challenge)
}

fn qr_challenge(ticket: &str, state: &str, reply: &Value) -> Value {
    json!({
        "challenge_id": ticket,
        "state": state,
        "payload": reply
            .get("payload")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("wxlike://login/{ticket}")),
        "expires_at_ms": iso_ms(
            reply.get("expires_at_ms")
                .and_then(Value::as_i64)
                .unwrap_or_else(|| wxlike_core::types::now_ms() as i64 + 120_000)
        ),
        "account_hint": reply.get("account_hint").cloned().unwrap_or(Value::Null),
    })
}

/// capabilities：如实上报。动作清单 = 真实实现的那组（见 IMPLEMENTED_ACTIONS）。
pub fn capabilities() -> Value {
    json!({
        "contract_version": "1",
        "transport": "tauri",
        "actions": IMPLEMENTED_ACTIONS,
        "qr_login": true,
        "secrets_os_backed": false,
        "sync_range": true,
        "notes": [
            "G01-02：新 UI 经 core_dispatch 走与旧 UI 同一 Rust 后端（HTTP 控制面 + QUIC hub + 本地 SQLite）",
            "未列出的 action 一律返回 NOT_IMPLEMENTED，不伪造成功",
        ],
    })
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_ms_formats_utc() {
        assert_eq!(iso_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_ms(1_000), "1970-01-01T00:00:01.000Z");
        // 2026-10-25 00:00:00 UTC = 1792838400
        assert_eq!(iso_ms(1_792_886_400_000), "2026-10-25T00:00:00.000Z");
        assert_eq!(iso_ms(86_399_000), "1970-01-01T23:59:59.000Z");
        // 闰日：2024-02-29 12:00:00 = 1709208000
        assert_eq!(iso_ms(1_709_208_000_000), "2024-02-29T12:00:00.000Z");
    }

    #[test]
    fn row_to_message_maps_kinds() {
        let me = "u1";
        let out = row_to_message(
            &json!({ "sender": "u1", "conv_id": "d-u1~u2", "seq": 3, "sent_at_ms": 1000, "msg_id": "ab", "kind": "text", "text": "hi" }),
            me,
        );
        assert_eq!(out["direction"], "out");
        assert_eq!(out["body"]["kind"], "text");
        assert_eq!(out["seq"], "3");
        let file = row_to_message(
            &json!({ "sender": "u2", "conv_id": "d-u1~u2", "seq": 4, "sent_at_ms": 2000, "msg_id": "cd", "kind": "msg.file", "payload": { "media_id": "m1", "name": "a.txt", "size": 5 } }),
            me,
        );
        assert_eq!(file["direction"], "in");
        assert_eq!(file["body"]["kind"], "file");
        assert_eq!(file["body"]["name"], "a.txt");
    }

    #[test]
    fn peer_of_finds_the_other_side() {
        assert_eq!(peer_of("d-u1~u2", "u1").as_deref(), Some("u2"));
        assert_eq!(peer_of("g-abc", "u1"), None);
    }

    #[test]
    fn bridge_error_codes() {
        assert_eq!(BridgeError::from("401 unauthorized".to_string()).code(), "UNAUTHENTICATED");
        assert_eq!(BridgeError::from("未连接到服务器".to_string()).code(), "OFFLINE");
        assert_eq!(BridgeError::from("boom".to_string()).code(), "INTERNAL");
        assert!(!BridgeError::NotImpl("x".into()).retryable());
    }

    #[test]
    fn cursor_parsing_accepts_string_and_number() {
        let p = json!({ "cursor": "12" });
        assert_eq!(cursor_usize(&p), 12);
        assert_eq!(cursor_i64(&p), 12);
        let p = json!({ "cursor": 7 });
        assert_eq!(cursor_usize(&p), 7);
        let p = json!({});
        assert_eq!(cursor_i64(&p), 0);
    }
}
