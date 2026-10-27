//! G01-02：新 UI（apps/desktop-web，契约 v1）的宿主桥。
//!
//! 前端只发 `ActionRequest`（snake_case JSON）、监听 `core://event`；
//! 这里把 [`IMPLEMENTED_ACTIONS`] 列出的 action_id 映射到 [`AppState`] 的既有业务方法
//! （HTTP 控制面 + QUIC hub + 本地 SQLite 镜像库），**不引入第二套业务
//! 逻辑**——旧 UI 与新 UI 走同一个后端，行为不会分叉。
//!
//! 数据形状以 `apps/desktop-web/src/shell/models.ts` 与 fixture 适配器为
//! 参照（fixture 是新 UI 开发时的参照实现）；本文件里的映射函数负责把
//! 服务端/本地库的事件形状（render.rs 的 payload）转换成 UI 的
//! ChatMessage / Conversation / Contact。
//!
//! 事件：`emit_core` 以 EventEnvelope 形状发 `core://event`，同时把每条信封
//! 记进有界环形日志；收包泵（app_state::start_recv_pump）每条入站消息都会调
//! [`forward_inbound`] 同步转发一份。`core_subscribe` 带的游标即据此补发
//! （见 [`subscribe`] 的诚实边界：同进程能补，跨重启只报 gap）。
//!
//! 未实现的 action 返回 `NOT_IMPLEMENTED` 并注明原因——按"最新 UI 所有
//! 入口都能完成真实操作"的验收口径，宁可明确说没有，也不伪造成功。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use tauri::Manager as _;

use super::{AppState, Http, LocalStore};

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
    /// 服务端认得这个人，但这件事他不许做（403）。与 401 分开是契约要求的：
    /// 401 要重新登录，403 重新登录也没用（`core-contract.ts` 的 `ErrorCode`
    /// 本来就有 `FORBIDDEN` 这一档，桥一直没用过它）。
    Forbidden(String),
    Validation(String),
    Conflict(String),
    Confirmation(String),
    /// 429：限流。与 `OFFLINE` 同为可重试，但原因不同（服务端在挡，不是链路断）。
    RateLimited(String),
    NotImpl(String),
    Dependency(String),
    Offline(String),
    Other(String),
}

impl BridgeError {
    fn code(&self) -> &'static str {
        match self {
            BridgeError::Unauth(_) => "UNAUTHENTICATED",
            BridgeError::Forbidden(_) => "FORBIDDEN",
            BridgeError::Validation(_) => "VALIDATION",
            BridgeError::Conflict(_) => "CONFLICT",
            BridgeError::Confirmation(_) => "REQUIRES_CONFIRMATION",
            BridgeError::RateLimited(_) => "RATE_LIMITED",
            BridgeError::NotImpl(_) => "NOT_IMPLEMENTED",
            BridgeError::Dependency(_) => "DEPENDENCY_UNAVAILABLE",
            BridgeError::Offline(_) => "OFFLINE",
            BridgeError::Other(_) => "INTERNAL",
        }
    }
    fn message(&self) -> &str {
        match self {
            BridgeError::Unauth(m)
            | BridgeError::Forbidden(m)
            | BridgeError::Validation(m)
            | BridgeError::Conflict(m)
            | BridgeError::Confirmation(m)
            | BridgeError::RateLimited(m)
            | BridgeError::NotImpl(m)
            | BridgeError::Dependency(m)
            | BridgeError::Offline(m)
            | BridgeError::Other(m) => m,
        }
    }
    fn retryable(&self) -> bool {
        matches!(self, BridgeError::Offline(_) | BridgeError::RateLimited(_))
    }
}

impl From<String> for BridgeError {
    fn from(message: String) -> Self {
        let lower = message.to_lowercase();
        if lower.contains("401")
            || lower.contains("未登录")
            || (lower.contains("登录") && lower.contains("失效"))
        {
            return BridgeError::Unauth(message);
        }
        if lower.contains("403") || lower.contains("forbidden") || lower.contains("权限") {
            // #293：403 是"这事不许做"，不是"参数填错了"。旧表把它折进 VALIDATION，
            // UI 于是会把权限问题当校验错误显示（重新登录/改参数都不管用）。
            return BridgeError::Forbidden(message);
        }
        if lower.contains("409") || lower.contains("conflict") {
            return BridgeError::Conflict(message);
        }
        if lower.contains("未连接")
            || lower.contains("connect")
            || lower.contains("network")
            || lower.contains("timed out")
            || lower.contains("timeout")
            || lower.contains("超时")
            || lower.contains("offline")
            || lower.contains("不可达")
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

/// 桥自身的状态（与 AppState 分开管理：只放会话投影、锁、上传会话、事件日志）。
pub struct CoreState {
    /// 当前会话投影的账号部分（user_id/name/avatar）。
    account: Mutex<Option<Value>>,
    lock: Mutex<LockState>,
    uploads: Mutex<HashMap<String, UploadSession>>,
    event_seq: AtomicU64,
    /// 本进程事件流的标识：游标只在同一 epoch 内可比（见 [`subscribe`]）。
    epoch: String,
    /// `emit_core` 发过的信封日志 + 补发去重表（见 [`subscribe`]）。
    events: Mutex<EventJournal>,
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

/// 有界事件环形日志（`core_subscribe` 的补发来源）。
///
/// 只在内存：本地 SQLite 存的是消息/会话投影，不是事件流，跨进程重启无历史可
/// 言——所以 [`subscribe`] 对"游标来自上一个进程"如实报 gap，让调用方重新拉列表。
#[derive(Default)]
struct EventJournal {
    /// `(seq, 信封)`，按 seq 升序；超过 [`JOURNAL_CAP`] 时挤掉最旧的。
    entries: VecDeque<(u64, Value)>,
    /// 已被挤掉的最高 seq：补发窗口若落在这之下就说明中间有洞。
    dropped_through: u64,
    /// 去重表：`游标 -> 该游标已经补发到的最高 seq`。同一游标反复
    /// 卸载重订不会二次投递（#296 要求的 "not double-deliver"）。
    resumed: HashMap<String, u64>,
}

impl CoreState {
    pub fn new() -> Self {
        Self {
            account: Mutex::new(None),
            lock: Mutex::new(LockState::default()),
            uploads: Mutex::new(HashMap::new()),
            event_seq: AtomicU64::new(1),
            epoch: new_epoch(),
            events: Mutex::new(EventJournal::default()),
            qr_poll_generation: AtomicU64::new(0),
            qr_active: Mutex::new(None),
        }
    }

    fn account(&self) -> Option<Value> {
        self.account
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn lock_state(&self) -> MutexGuard<'_, LockState> {
        self.lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn journal(&self) -> MutexGuard<'_, EventJournal> {
        self.events.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 已发事件的最高序号（= 下一次补发的右边界）。
    fn head(&self) -> u64 {
        self.event_seq.load(Ordering::Relaxed).saturating_sub(1)
    }

    /// 记一条已发信封（超容量挤掉最旧的，并记下被挤掉的序号以便如实报 gap）。
    fn record(&self, seq: u64, envelope: Value) {
        let mut journal = self.journal();
        journal.entries.push_back((seq, envelope));
        while journal.entries.len() > JOURNAL_CAP {
            if let Some((dropped, _)) = journal.entries.pop_front() {
                journal.dropped_through = journal.dropped_through.max(dropped);
            }
        }
    }

    /// 游标字符串（契约里的 `EventCursor.cursor` 形状：`"<epoch>:<seq>"`）。
    fn cursor_at(&self, seq: u64) -> String {
        format!("{}:{seq}", self.epoch)
    }
}

/// 每次 boot 唯一的进程流标识（pid + 启动毫秒 + 本进程内计数）。
///
/// 它同时出现在 `event_id` 与游标里：`event_id` 因此全局唯一，重启后新事件的
/// id 不会与上一个进程发过的相撞——调用方按 id 幂等合并才有保证（#296 的
/// "不重复投递"半边）。
fn new_epoch() -> String {
    static BOOT: AtomicU64 = AtomicU64::new(0);
    let n = BOOT.fetch_add(1, Ordering::Relaxed);
    format!(
        "{:x}-{:x}-{:x}",
        std::process::id(),
        wxlike_core::types::now_ms() as u64,
        n
    )
}

impl Default for CoreState {
    fn default() -> Self {
        Self::new()
    }
}

/// 同步范围的"全部"档位（act_sync_range_set 的合法值之一）。
const SYNC_RANGE_MONTH_DAYS: i64 = 30;
const SYNC_RANGE_ALL_DAYS: i64 = 90;
/// QR 票据状态轮询间隔（毫秒）。
const QR_POLL_INTERVAL_MS: u64 = 1500;
/// 搜索命中标题/摘要的截断宽度（字符）。
const SEARCH_TITLE_CHARS: usize = 30;
const SEARCH_SNIPPET_CHARS: usize = 50;
/// 列表类 action 的默认页大小与页上限。
const DEFAULT_PAGE_SIZE: u64 = 30;
const MAX_PAGE_SIZE: u64 = 100;
/// 事件补发日志容量（条）。桌面桥的事件都是"投影增量"，512 条足够覆盖一次
/// webview 刷新/断线重连期间的漏事件；再老的就只能报 gap 让调用方重拉。
const JOURNAL_CAP: usize = 512;
/// [`JOURNAL_CAP`] 的十倍：去重表按游标键增长，超过这个数就清空（防御性上限）。
const RESUME_DEDUPE_CAP: usize = 64;
/// 本桥的事件流 id（只有一条；契约 `EventCursor.stream_id` 用它）。
pub const EVENT_STREAM_ID: &str = "core";

/// 这些 action_id 有真实实现（capabilities 如实上报）。
///
/// 不变式（由 `tests/catalog_drift.rs` 逐条将源码对账，漂移即红）：
/// 路由表里出现的 action == `IMPLEMENTED_ACTIONS ∪ STUBBED_ACTIONS`，
/// 且两者互斥。也就是说：
/// * 只在 `dispatch_inner`/子路由里挂了钩子却没登记在本表 → UI 无法发现（#296）；
/// * 登记在本表却没路由/处理器只会回 `NOT_IMPLEMENTED` → capabilities 说谎。
///
/// 新增"已路由但服务端能力缺失、如实拒绝"的 action 请登记到
/// [`STUBBED_ACTIONS`]，不要塞进本表。
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
    "relations.group.kick",
    "relations.group.settings.set",
    "wallet.balance.get",
    "wallet.ledger.page",
    "wallet.transfer",
    "wallet.redpacket.create",
    "wallet.redpacket.get",
    "wallet.redpacket.claim",
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

/// 已在路由表登记、但处理器如实返回 `NOT_IMPLEMENTED` 的 action（服务端能力缺失，
/// 拒绝伪造成功）。它们**不得**出现在 [`IMPLEMENTED_ACTIONS`] 里；登记在此是为了
/// 让"路由 == 目录 ∪ 本表"这条不变式可被机器检查（见 `tests/catalog_drift.rs`）。
///
/// #296 之前 `relations.group.kick` 属于本表：它的旧注释声称"服务端暂无群成员
/// 移除路由"，实际 `crates/platform/src/groups.rs` 挂了
/// `DELETE /api/groups/{group_id}/members/{user_id}`（CLI 的 `group.kick` 用的
/// 就是它），所以已改为真实实现并移入 [`IMPLEMENTED_ACTIONS`]。
///
/// #293 的钱包切片里只有这一条是真的做不到：`crates/platform/src/wallet.rs`
/// 只挂了 `POST /api/wallet/redpacket`、`GET /api/wallet/redpacket/{id}`、
/// `POST /api/wallet/redpacket/{id}/claim`、`GET /api/wallet/balance`、
/// `GET /api/wallet/ledger`、`POST /api/wallet/transfer`——**没有**红包列表路由。
/// 没有可读来源就不许拿"我发出去的红包 ∩ 本地消息镜像"糊一份列表出来：
/// 那会漏掉别人在群里发的包，还会把 UI 的"领取"按钮点进死路。等服务端补
/// `GET /api/wallet/redpacket`（见 #293 交付说明）时，这条搬到
/// [`IMPLEMENTED_ACTIONS`] 并接上 [`authed_json`]。
pub const STUBBED_ACTIONS: &[&str] = &["wallet.redpacket.list"];

// ---------------------------------------------------------------------------
// 事件
// ---------------------------------------------------------------------------

/// 以新契约的 EventEnvelope 形状发 `core://event`，并把信封登记进补发日志。
///
/// 信封里带 `cursor`（`EventCursor` 形状）：调用方每收到一条就把它记下来，
/// 重订阅时原样带回，[`subscribe`] 据此补发漏掉的事件。
fn emit_core<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    event_type: &str,
    aggregate_id: &str,
    payload: Value,
) {
    use tauri::Emitter as _;
    let core = app.state::<CoreState>();
    let seq = core.event_seq.fetch_add(1, Ordering::Relaxed);
    let now = wxlike_core::types::now_ms();
    let envelope = json!({
        // epoch 参与 id：序号会在重启后从 1 重新数，只有 epoch 能保证 id 全局唯一。
        "event_id": format!("ev-{}-{seq}", core.epoch),
        "aggregate_id": aggregate_id,
        "aggregate_version": "1",
        "event_type": event_type,
        "server_time_ms": iso_ms(now as i64),
        "cursor": { "stream_id": EVENT_STREAM_ID, "cursor": core.cursor_at(seq) },
        "payload": payload,
    });
    core.record(seq, envelope.clone());
    let _ = app.emit("core://event", envelope);
}

/// 游标解析结果。
enum Resume {
    /// 没带游标 = 冷订阅：只收此后的事件，不补发。
    Fresh,
    /// 带了一个当前不认识的形状（非本流 id、解析不出序号）。
    Invalid,
    /// 正常游标：`{epoch, seq}`，epoch 可能来自上一个进程。
    At {
        epoch: String,
        seq: u64,
        token: String,
    },
}

/// 游标只带了一个裸序号（`3` / `"3"`）：没有 epoch 可依，按"当前这条流"解释。
fn at_head(core: &CoreState, seq: u64) -> Resume {
    Resume::At {
        epoch: core.epoch.clone(),
        seq,
        token: core.cursor_at(seq),
    }
}

/// 裸序号写法：十进制字符串才认（负数、小数、非数字都是 None）。
fn parse_seq(text: &str) -> Option<u64> {
    text.parse::<u64>().ok()
}

/// JSON 值里的裸序号：数字或十进制字符串。
fn bare_seq(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str().and_then(parse_seq))
}

/// `"<epoch>:<seq>"`；切不出 `:` 就当裸序号（`"3"`）。
fn parse_text_cursor(core: &CoreState, text: &str) -> Resume {
    // epoch 里有 '-' 但没有 ':'，所以按最后一个 ':' 切。
    let Some((epoch, seq)) = text.rsplit_once(':') else {
        return parse_seq(text).map_or(Resume::Invalid, |seq| at_head(core, seq));
    };
    let Some(seq) = parse_seq(seq) else {
        return Resume::Invalid;
    };
    Resume::At {
        epoch: epoch.to_string(),
        seq,
        token: text.to_string(),
    }
}

/// 对象写法：契约的 `EventCursor`（`{stream_id, cursor}`）或 `{epoch, seq}`。
fn parse_map_cursor(core: &CoreState, map: &serde_json::Map<String, Value>) -> Resume {
    // 不是本桥那条流的游标，补不了也不能假装补。
    if map
        .get("stream_id")
        .and_then(Value::as_str)
        .is_some_and(|stream| stream != EVENT_STREAM_ID)
    {
        return Resume::Invalid;
    }
    if let Some(inner) = map.get("cursor") {
        return parse_cursor(core, inner);
    }
    let Some(seq) = map.get("seq").and_then(bare_seq) else {
        return Resume::Invalid;
    };
    let epoch = map
        .get("epoch")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| core.epoch.clone());
    Resume::At {
        token: format!("{epoch}:{seq}"),
        epoch,
        seq,
    }
}

/// 解析调用方带回的游标。兼容这些写法（都不带也合法）：
/// `null` / `3` / `"3"` / `"<epoch>:<seq>"` /
/// `{ "stream_id": "core", "cursor": "<epoch>:<seq>" }`（契约 `EventCursor`）/
/// `{ "epoch": ..., "seq": ... }`。
fn parse_cursor(core: &CoreState, raw: &Value) -> Resume {
    match raw {
        // 没带游标 = 冷订阅。
        Value::Null => Resume::Fresh,
        Value::Number(_) => bare_seq(raw).map_or(Resume::Invalid, |seq| at_head(core, seq)),
        Value::String(text) => parse_text_cursor(core, text),
        Value::Object(map) => parse_map_cursor(core, map),
        _ => Resume::Invalid,
    }
}

/// `core_subscribe` 的实现：按游标补发（resume）。
///
/// 契约：入参是调用方上次记下的游标（可为 None，兼容写法见 [`parse_cursor`]），
/// 返回订阅回执 JSON。补发是**真的重新 `emit` 一遍 `core://event`**，所以现有
/// 前端一行都不用改（它的 `applyEvent` 按 id 幂等合并，重复投影会收敛）。
///
/// 为什么不是"全量重放"——#296 的诚实边界，写在这里：
/// * 事件只活在 [`EventJournal`] 这块内存环形日志里。本地 SQLite 存的是
///   消息/会话这些**投影**，不是事件流；进程重启后日志归零，没有历史事件可补。
///   硬凑"重放"只能把投影 diff 成事件，那是第二套业务逻辑，正是本文件开头禁止的事。
/// * 所以跨重启如实报 `gap: true, reason: "stream_reset"` 并附上要重拉的 action
///   列表，由调用方走 `messaging.conversations.list` / `messaging.messages.page`
///   全量刷新（也正是冷启动本来就走的路径）。同进程内日志被挤掉的窗口同理报
///   `journal_truncated`，能补的那段照样补。
/// * 不丢：同进程内的卸载重订（webview 刷新、断线重连）漏掉的事件全部补回。
/// * 不重：补发只取 `seq > 游标`，且同一游标再订阅只补"上次补发之后"的新事件
///   （[`EventJournal::resumed`] 去重表）；`event_id` 带 epoch，重启后也不会与
///   上一个进程发过的撞号。
pub fn subscribe<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    core: &CoreState,
    cursor: Option<Value>,
) -> Value {
    let head = core.head();
    let incoming = cursor.unwrap_or(Value::Null);

    // 冷订阅：没有游标就无从判断漏了什么，把当前 head 交给调用方当基线。
    let replay = match parse_cursor(core, &incoming) {
        Resume::Fresh => Replay::nothing(head),
        Resume::Invalid => Replay::missing(head, "invalid_cursor"),
        Resume::At { epoch, seq, token } => resume_at(app, core, &epoch, seq, &token, head),
    };

    subscribe_reply(core, head, replay)
}

/// 一次续订的补发结果（喂给 [`subscribe_reply`]）。
#[derive(Default)]
struct Replay {
    /// 补不出的原因；None = 没有洞。
    gap: Option<&'static str>,
    /// 这次真的重发了几条。
    count: u64,
    /// 补发窗口的左右边界（没补发时 = 基线 head）。
    from: u64,
    to: u64,
}

impl Replay {
    /// 什么都不补发（冷订阅，或认不出/用不了的游标）。
    fn nothing(head: u64) -> Self {
        Self {
            gap: None,
            count: 0,
            from: head,
            to: head,
        }
    }

    /// 承认有洞：一条也补不出，但要告诉调用方从哪里重拉。
    fn missing(head: u64, reason: &'static str) -> Self {
        Self {
            gap: Some(reason),
            ..Self::nothing(head)
        }
    }
}

/// 游标合法（同一条流、没超前）时的补发。
///
/// 缺口用 `&'static str` 而不是 bool——调用方要按原因决定重拉哪几份投影。
fn resume_at<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    core: &CoreState,
    epoch: &str,
    seq: u64,
    token: &str,
    head: u64,
) -> Replay {
    use tauri::Emitter as _;
    if epoch != core.epoch {
        // 游标来自上一个进程：环形日志已随之消失，只能让调用方重拉。
        return Replay::missing(head, "stream_reset");
    }
    if seq > head {
        // 调用方声称收到过还没发出去的事件：状态不可信，重拉。
        return Replay::missing(head, "cursor_ahead");
    }
    let (pending, base, truncated) = take_replay(core, seq, token, head);
    let mut replay = Replay {
        gap: truncated,
        count: pending.len() as u64,
        from: pending.first().map(|(s, _)| *s).unwrap_or(base),
        to: head,
    };
    // 锁已在 [`take_replay`] 返回前释放：emit 会同步回调监听器，监听器里可能再次订阅。
    for (event_seq, envelope) in &pending {
        let _ = app.emit("core://event", envelope.clone());
        replay.to = *event_seq;
    }
    record_resumed(core, token.to_string(), base.max(replay.to));
    replay
}

/// 这次续订该补发哪些事件：返回 `(待补发, 补发基线 seq, 缺口原因)`。
///
/// 锁在返回前释放，`subscribe` 才能在持锁之外做 emit。
fn take_replay(
    core: &CoreState,
    seq: u64,
    token: &str,
    head: u64,
) -> (Vec<(u64, Value)>, u64, Option<&'static str>) {
    let journal = core.journal();
    let gap = if seq < journal.dropped_through {
        Some("journal_truncated")
    } else {
        None
    };
    // 去重：这个游标此前已补发到 resumed[token]，只补那之后的新事件。
    let base = seq.max(journal.resumed.get(token).copied().unwrap_or(seq));
    let pending = journal
        .entries
        .iter()
        .filter(|(s, _)| *s > base && *s <= head)
        .cloned()
        .collect();
    drop(journal);
    (pending, base, gap)
}

/// 记下这次补发到的水位；表太长就整表清空（它是去重加速，不是真相源）。
fn record_resumed(core: &CoreState, token: String, upto: u64) {
    let mut journal = core.journal();
    if journal.resumed.len() >= RESUME_DEDUPE_CAP {
        journal.resumed.clear();
    }
    journal.resumed.insert(token, upto);
}

/// 续订回执。`replay.gap` 为真时带上要重拉的投影清单，绝不让调用方以为补全了。
fn subscribe_reply(core: &CoreState, head: u64, replay: Replay) -> Value {
    let available_from = core
        .journal()
        .entries
        .front()
        .map(|(s, _)| s.saturating_sub(1))
        .unwrap_or(head);
    let gap = replay.gap;
    json!({
        "subscribed": true,
        "stream_id": EVENT_STREAM_ID,
        // 调用方把它原样带回 core_subscribe 即可续订。
        "cursor": { "stream_id": EVENT_STREAM_ID, "cursor": core.cursor_at(head) },
        "head": head,
        "replayed": replay.count,
        "replayed_from": replay.from,
        "replayed_to": replay.to,
        "gap": gap.is_some(),
        "reason": gap,
        // gap 时调用方要重拉的投影（不补事件，改拉全量）。
        "refetch": if gap.is_some() {
            json!(["messaging.conversations.list", "messaging.messages.page", "relations.list", "preferences.get"])
        } else {
            json!([])
        },
        "journal_capacity": JOURNAL_CAP,
        "journal_available_from": available_from,
    })
}

/// 收包泵的转发钩子：一条入站消息 → `message.created` + `conversation.upsert`。
/// `me` 用于判定方向；None 时全部按入站处理（控制面不可用时的兜底）。
///
/// 对 crate 外可见：补发/游标测试要驱动**真实生产者**（收包泵就是调它），
/// 而不是自己拼一份事件形状。
pub fn forward_inbound<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    payload: &Value,
    me: Option<&str>,
) {
    let conv_id = payload
        .get("conv_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
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
    if conv_id.starts_with("d-") {
        "direct"
    } else {
        "group"
    }
}

/// 直聊会话的对端（conv 形如 `d-{a}~{b}`，另一端是自己时取空）。
fn peer_of(conv_id: &str, me: &str) -> Option<String> {
    let rest = conv_id.strip_prefix("d-")?;
    rest.split('~').find(|p| *p != me).map(str::to_string)
}

fn preview_of(payload: &Value) -> String {
    match payload
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "text" | "e2ee" => payload
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(40)
            .collect(),
        "image" => "[图片]".into(),
        "msg.voice" => "[语音]".into(),
        "msg.file" => "[文件]".into(),
        "msg.sticker" => "[表情]".into(),
        "msg.video" => "[视频]".into(),
        "wallet.redpacket" => "[红包]".into(),
        "system" | "notice" => payload
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(40)
            .collect(),
        other => format!("[{other}]"),
    }
}

/// 本地库/服务端历史的行（render.rs payload 形状）→ UI 的 ChatMessage。
fn row_to_message(row: &Value, me: &str) -> Value {
    let sender = row
        .get("sender")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let direction = if sender == me { "out" } else { "in" };
    let kind = row.get("kind").and_then(Value::as_str).unwrap_or("text");
    let body = match kind {
        "text" | "e2ee" => {
            json!({ "kind": "text", "text": row.get("text").and_then(Value::as_str).unwrap_or("") })
        }
        "image" => {
            json!({ "kind": "image", "media_id": row.get("media_id").and_then(Value::as_str).unwrap_or("") })
        }
        "system" | "notice" | "recall" => {
            json!({ "kind": "system", "text": row.get("text").and_then(Value::as_str).unwrap_or("") })
        }
        custom => custom_body(custom, &row.get("payload").cloned().unwrap_or(json!({}))),
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
/// 自定义消息体（msg.voice/file/sticker/video 等）→ UI 的 MessageBody。
fn custom_body(custom: &str, p: &Value) -> Value {
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
        _ => json!({ "kind": "system", "text": "[ unsupported message ]" }),
    }
}

/// iso_ms 的 civil-date 算法常量（Howard Hinnant `date_algorithms`）。
const DAYS_TO_CIVIL_EPOCH: i64 = 719_468;
const ERA_DAYS: i64 = 146_097;
const ERA_YEAR_DAYS: i64 = 1_460;
const CENTURY_DAYS: i64 = 36_524;
const ERA_CENTURY_SHIFT: i64 = 146_096;
const YEARS_PER_ERA: i64 = 400;
const MONTHS_PER_ERA_DENOM: i64 = 153;

fn iso_ms(ms: i64) -> String {
    // 没有 chrono：手工换算（UTC）。够用——只用于展示与排序。
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (h, m, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    // 1970-01-01 起的 civil date（Howard Hinnant 的算法，无依赖版）
    let z = days + DAYS_TO_CIVIL_EPOCH;
    let era = z.div_euclid(ERA_DAYS);
    let doe = z.rem_euclid(ERA_DAYS);
    let yoe = (doe - doe / ERA_YEAR_DAYS + doe / CENTURY_DAYS - doe / ERA_CENTURY_SHIFT) / 365;
    let y = yoe + era * YEARS_PER_ERA;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / MONTHS_PER_ERA_DENOM;
    let d = doy - (MONTHS_PER_ERA_DENOM * mp + 2) / 5 + 1;
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
fn open_session<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
) -> Result<Value, BridgeError> {
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
    connect_hub_best_effort(app, state, &data_dir);
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

/// hub 连接（尽力而为）：`/endpoints` 给出 QUIC 端口，TOFU 固定证书后
/// 连接并起双泵；连不上不阻断登录——消息进 outbox，重连后由泵补发。
///
/// 死锁备忘：`state.http()` 的临时 MutexGuard 不能活在 if-let 条件里，
/// 否则后面 `auth_token()` 再锁同一把锁就是自己跟自己死锁（审计 R5 实锤）。
fn connect_hub_best_effort<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    data_dir: &std::path::Path,
) {
    let endpoints_quic = state
        .http()
        .expect_json("GET", "/endpoints", None)
        .ok()
        .and_then(|v| v["quic"].as_str().map(str::to_string));
    let Some(quic) = endpoints_quic else {
        return;
    };
    let pins = data_dir.join("tofu-pins.json");
    if state
        .connect_hub(&quic, &state.auth_token().unwrap_or_default(), &pins)
        .is_ok()
    {
        state.start_outbox_pump(app.clone());
        // 收包泵自带代次守卫：重复登录/重连不会叠出双泵。
        let _ = state.start_recv_pump(app.clone());
    }
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
// dispatch：全部已登记 action 的总入口
// ---------------------------------------------------------------------------

/// 域路由入口的统一签名（四个参数就是 `act_*` 的那四个）。
type Route<R> =
    fn(&tauri::AppHandle<R>, &AppState, &CoreState, &CoreRequest) -> Result<Value, BridgeError>;

pub fn dispatch<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: CoreRequest,
) -> CoreReply {
    CoreReply::from_result(dispatch_inner(app, state, core, &req), &req.request_id)
}

/// 全部已登记 action 的总路由：按**域前缀**选子路由，这里不列举 action。
///
/// action 名单的唯一来源是各 `*_router` 的 match 臂。在总入口再抄一份正是 #296
/// 的漂移来源（`relations.group.kick` 同时挂在两处，能力目录里却没有它），
/// 所以 `tests/catalog_drift.rs` 把"总入口列举 action"直接判红。
fn dispatch_inner<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let Some(route) = route_for::<R>(&req.action_id) else {
        return Err(unrouted(req));
    };
    route(app, state, core, req)
}

/// 域前缀表：action 的第一段（identity 取到第二段）→ 子路由。
///
/// 表里只有前缀，没有完整 action_id。`tests/catalog_drift.rs` 按这份表逐条验证
/// "子路由的每条臂都能从这里走到、且走到的是它自己"，所以挂错域/漏一个域都会红。
fn route_for<R: tauri::Runtime>(action_id: &str) -> Option<Route<R>> {
    let mut segments = action_id.split('.');
    let head = segments.next().unwrap_or_default();
    let family = segments.next().unwrap_or_default();
    Some(match (head, family) {
        ("identity", "qr_login") => qr_router::<R>,
        ("identity", _) => session_router::<R>,
        ("security", _) => session_router::<R>,
        ("sync", _) => session_router::<R>,
        ("messaging", _) => messaging_router::<R>,
        ("media", _) => media_router::<R>,
        ("relations", _) => relations_router::<R>,
        ("wallet", _) => wallet_router::<R>,
        ("search", _) => search_router::<R>,
        ("preferences", _) => settings_router::<R>,
        ("secrets", _) => settings_router::<R>,
        ("storage", _) => settings_router::<R>,
        ("backup", _) => settings_router::<R>,
        ("app", _) => info_router::<R>,
        _ => return None,
    })
}

/// 两条"走不到处理器"的路共用同一句实话：总入口认不出这个域，
/// 或域里没有这条臂。错误文案只有一份，不会两边各写一套再各自漂移。
fn unrouted(req: &CoreRequest) -> BridgeError {
    BridgeError::NotImpl(format!(
        "core 桥未实现 {}（action 需先在 cmd catalog 登记）",
        req.action_id
    ))
}

fn session_router<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    match req.action_id.as_str() {
        "identity.session.projection" => act_identity_session_projection(app, state, core, req),
        "identity.session.login" => act_identity_session_login(app, state, core, req),
        "identity.session.register" => act_identity_session_register(app, state, core, req),
        "identity.session.logout" => act_identity_session_logout(app, state, core, req),
        "identity.session.restore" => act_identity_session_restore(app, state, core, req),
        "security.lock" => act_security_lock(app, state, core, req),
        "security.unlock" => act_security_unlock(app, state, core, req),
        "sync.range.get" => act_sync_range_get(app, state, core, req),
        "sync.range.set" => act_sync_range_set(app, state, core, req),
        _ => Err(unrouted(req)),
    }
}

fn qr_router<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    match req.action_id.as_str() {
        "identity.qr_login.create" => act_identity_qr_login_create(app, state, core, req),
        "identity.qr_login.cancel" => act_identity_qr_login_cancel(app, state, core, req),
        "identity.qr_login.switch_account" => {
            act_identity_qr_login_switch_account(app, state, core, req)
        }
        _ => Err(unrouted(req)),
    }
}

fn messaging_router<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    match req.action_id.as_str() {
        "messaging.conversations.list" => act_messaging_conversations_list(app, state, core, req),
        "messaging.messages.page" => act_messaging_messages_page(app, state, core, req),
        "messaging.send" => act_messaging_send(app, state, core, req),
        "messaging.retry" => act_messaging_retry(app, state, core, req),
        "messaging.recall" => act_messaging_recall(app, state, core, req),
        "messaging.forward" => act_messaging_forward(app, state, core, req),
        "messaging.read" => act_messaging_read(app, state, core, req),
        "messaging.conversation.pin" => act_messaging_conversation_pin(app, state, core, req),
        "messaging.conversation.mute" => act_messaging_conversation_pin(app, state, core, req),
        "messaging.conversation.read_all" => {
            act_messaging_conversation_read_all(app, state, core, req)
        }
        "messaging.conversation.ensure" => act_messaging_conversation_ensure(app, state, core, req),
        "messaging.draft.get" => act_messaging_draft_get(app, state, core, req),
        "messaging.draft.save" => act_messaging_draft_save(app, state, core, req),
        "messaging.draft.clear" => act_messaging_draft_clear(app, state, core, req),
        _ => Err(unrouted(req)),
    }
}

fn media_router<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    match req.action_id.as_str() {
        "media.upload.begin" => act_media_upload_begin(app, state, core, req),
        "media.upload.part" => act_media_upload_part(app, state, core, req),
        "media.upload.complete" => act_media_upload_complete(app, state, core, req),
        "media.upload.cancel" => act_media_upload_cancel(app, state, core, req),
        _ => Err(unrouted(req)),
    }
}

fn relations_router<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    match req.action_id.as_str() {
        "relations.list" => act_relations_list(app, state, core, req),
        "relations.requests.incoming" => act_relations_requests_incoming(app, state, core, req),
        "relations.request.send" => act_relations_request_send(app, state, core, req),
        "relations.request.accept" => act_relations_request_accept(app, state, core, req),
        "relations.request.decline" => act_relations_request_accept(app, state, core, req),
        "relations.block" => act_relations_block(app, state, core, req),
        "relations.note.set" => act_relations_note_set(app, state, core, req),
        "relations.tag.set" => act_relations_tag_set(app, state, core, req),
        "relations.star.set" => act_relations_tag_set(app, state, core, req),
        "relations.profile" => act_relations_profile(app, state, core, req),
        "relations.common_groups" => act_relations_common_groups(app, state, core, req),
        "relations.group.members" => act_relations_group_members(app, state, core, req),
        "relations.group.kick" => act_relations_group_kick(app, state, core, req),
        "relations.group.settings.set" => act_relations_group_settings_set(app, state, core, req),
        _ => Err(unrouted(req)),
    }
}

/// 钱包域（#293）：钱的所有读写都从这里过，UI 侧不再自己发 HTTP。
///
/// 每条臂都走 [`authed_json`]——它是桥里唯一允许直连平台的出口，Bearer 由
/// `wxlike-client` 的 `Http` 统一挂（凭据只在 Rust 侧，没有任何 action 会把
/// token 回给 webview，见 `tests/bridge_wallet_auth.rs`）。
fn wallet_router<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    match req.action_id.as_str() {
        "wallet.balance.get" => act_wallet_balance(app, state, core, req),
        "wallet.ledger.page" => act_wallet_ledger_page(app, state, core, req),
        "wallet.transfer" => act_wallet_transfer(app, state, core, req),
        "wallet.redpacket.create" => act_wallet_redpacket_create(app, state, core, req),
        "wallet.redpacket.get" => act_wallet_redpacket_get(app, state, core, req),
        "wallet.redpacket.claim" => act_wallet_redpacket_claim(app, state, core, req),
        "wallet.redpacket.list" => act_wallet_redpacket_list(app, state, core, req),
        _ => Err(unrouted(req)),
    }
}

fn search_router<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    match req.action_id.as_str() {
        "search.query" => act_search_query(app, state, core, req),
        _ => Err(unrouted(req)),
    }
}

fn settings_router<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    match req.action_id.as_str() {
        "preferences.get" => act_preferences_get(app, state, core, req),
        "preferences.set" => act_preferences_set(app, state, core, req),
        "secrets.status" => act_secrets_status(app, state, core, req),
        "secrets.lock" => act_secrets_lock(app, state, core, req),
        "secrets.migrate" => act_secrets_migrate(app, state, core, req),
        "storage.status" => act_storage_status(app, state, core, req),
        "storage.cache.clear" => act_storage_cache_clear(app, state, core, req),
        "backup.create" => act_backup_create(app, state, core, req),
        "backup.verify" => act_backup_verify(app, state, core, req),
        "backup.import" => act_backup_import(app, state, core, req),
        _ => Err(unrouted(req)),
    }
}

fn info_router<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    match req.action_id.as_str() {
        "app.info" => act_app_info(app, state, core, req),
        _ => Err(unrouted(req)),
    }
}

fn act_identity_session_projection<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    session_projection(state, core)
}

fn act_identity_session_login<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let username = str_param("username");
    let password = str_param("password");
    if username.trim().is_empty() || password.is_empty() {
        return Err(BridgeError::Validation("账号或口令为空".into()));
    }
    state.login(&username, &password)?;
    open_session(app, state, core)
}

fn act_identity_session_register<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let username = str_param("username");
    let password = str_param("password");
    let display_name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if username.trim().is_empty() || password.len() < 6 {
        return Err(BridgeError::Validation(
            "账号不能为空，口令至少 6 位".into(),
        ));
    }
    state.register(&username, &password)?;
    if !display_name.is_empty() {
        let _ = state.update_profile(Some(&display_name), None, None);
    }
    open_session(app, state, core)
}

fn act_identity_session_logout<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let _ = state.logout(); // 服务端撤销失败也照常清本地（不把人锁在旧会话里）
    state.http().token = None;
    core.account
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    let _ = with_local(state, |s| {
        s.kv_delete("auth_token")?;
        s.kv_delete("account")
    });
    let projection = json!({ "phase": "signed_out" });
    emit_core(app, "session.state", "session", projection.clone());
    Ok(json!({ "revoked": true }))
}

fn act_identity_session_restore<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    ensure_local(app, state)?;
    let token = with_local(state, |s| Ok(s.kv_get("auth_token")))
        .ok()
        .flatten();
    match token {
        Some(token) if !token.is_empty() => {
            state.set_auth_token(&token);
            open_session(app, state, core)
        }
        _ => Err(BridgeError::Unauth("没有可恢复的会话".into())),
    }
}

fn act_security_lock<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    _state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let account = core.account();
    if account.is_none() {
        return Err(BridgeError::Unauth("尚未登录".into()));
    }
    core.lock_state().locked = true;
    let projection = json!({ "phase": "locked", "account": account, "lock_reason": "manual" });
    emit_core(app, "session.state", "session", projection.clone());
    Ok(projection)
}

fn act_security_unlock<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    _state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
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

fn act_identity_qr_login_create<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    qr_create(app, state, core)
}

fn act_identity_qr_login_cancel<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    _state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    stop_qr_poll(core);
    Ok(json!({ "cancelled": true }))
}

fn act_identity_qr_login_switch_account<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    stop_qr_poll(core);
    state.http().token = None;
    core.account
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    let _ = with_local(state, |s| s.kv_delete("auth_token"));
    emit_core(
        app,
        "session.state",
        "session",
        json!({ "phase": "signed_out" }),
    );
    Ok(json!({ "revoked": true }))
}

fn act_sync_range_get<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
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

fn act_sync_range_set<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    if core.account().is_none() {
        return Err(BridgeError::Unauth("尚未登录".into()));
    }
    let days = params.get("days").and_then(Value::as_i64).unwrap_or(-1);
    if ![0, 7, SYNC_RANGE_MONTH_DAYS, SYNC_RANGE_ALL_DAYS].contains(&days) {
        return Err(BridgeError::Validation("同步范围不合法".into()));
    }
    let mut prefs = preferences_read(state);
    prefs["sync_range_days"] = json!(days);
    preferences_write(app, state, prefs)
}

fn act_messaging_conversations_list<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    conversations_list(state, params)
}

fn act_messaging_messages_page<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    messages_page(state, params)
}

fn act_messaging_send<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    message_send(app, state, params)
}

fn act_messaging_retry<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let msg_id = str_param("msg_id");
    if msg_id.is_empty() {
        return Err(BridgeError::Validation("消息不存在".into()));
    }
    let ob = state
        .outbox()
        .ok_or_else(|| BridgeError::Other("本地库未打开".into()))?;
    ob.requeue(&msg_id, wxlike_core::types::now_ms() as i64)
        .map_err(|e| BridgeError::Other(e.to_string()))?;
    Ok(json!({ "msg_id": msg_id, "state": "LOCAL_QUEUED" }))
}

fn act_messaging_recall<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
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

fn act_messaging_forward<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let msg_ids: Vec<String> = params
        .get("msg_ids")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let targets: Vec<String> = params
        .get("target_conv_ids")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
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

fn act_messaging_read<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let conv_id = str_param("conv_id");
    state.http().mark_conv_read(&conv_id)?;
    emit_core(
        app,
        "conversation.upsert",
        &conv_id,
        json!({ "conv_id": conv_id, "unread": 0 }),
    );
    emit_core(app, "unread.updated", "me", json!({ "total": 0 }));
    Ok(json!({ "unread": 0 }))
}

fn act_messaging_conversation_pin<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let conv_id = str_param("conv_id");
    let value = params
        .get("value")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let key = format!("conv_flags:{conv_id}");
    let mut flags = with_local(state, |s| {
        Ok(s.kv_get(&key)
            .and_then(|v| serde_json::from_str::<Value>(&v).ok())
            .unwrap_or(json!({})))
    })?;
    let field = if req.action_id.ends_with("pin") {
        "pinned"
    } else {
        "muted"
    };
    flags[field] = json!(value);
    with_local(state, |s| s.kv_put(&key, &flags.to_string()))?;
    let mut conv = conversation_summary(state, &conv_id)?;
    // 投影补齐另一侧标志：kv 里没写过的字段必须是 bool 而不是 null，
    // 否则契约里的 Conversation.pinned/muted（boolean）会被写成 null
    // （conversation-list.tsx 读 conv.pinned 时就是看这个字段）。
    conv["pinned"] = json!(flags["pinned"].as_bool().unwrap_or(false));
    conv["muted"] = json!(flags["muted"].as_bool().unwrap_or(false));
    emit_core(app, "conversation.upsert", &conv_id, conv.clone());
    Ok(conv)
}

fn act_messaging_conversation_read_all<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let convs = with_local(state, |s| s.conversations())?;
    for (conv, _, _) in convs {
        let unread = state
            .http()
            .conv_unread(&conv)
            .ok()
            .and_then(|v| v["unread"].as_i64())
            .unwrap_or(0);
        if unread > 0 {
            state.http().mark_conv_read(&conv)?;
            emit_core(
                app,
                "conversation.upsert",
                &conv,
                json!({ "conv_id": conv, "unread": 0 }),
            );
        }
    }
    emit_core(app, "unread.updated", "me", json!({ "total": 0 }));
    Ok(json!({ "total": 0 }))
}

fn act_messaging_conversation_ensure<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let peer = str_param("user_id");
    let me = state.user_id()?;
    let conv_id = wxlike_client::direct_conv_id(&me, &peer);
    let mut conv = conversation_summary(state, &conv_id)?;
    conv["peer_user_id"] = json!(peer);
    emit_core(app, "conversation.upsert", &conv_id, conv.clone());
    Ok(conv)
}

fn act_messaging_draft_get<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let conv_id = str_param("conv_id");
    let text = with_local(state, |s| {
        Ok(s.kv_get(&format!("draft:{conv_id}")).unwrap_or_default())
    })?;
    Ok(json!({ "text": text }))
}

fn act_messaging_draft_save<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
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

fn act_messaging_draft_clear<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let conv_id = str_param("conv_id");
    with_local(state, |s| s.kv_delete(&format!("draft:{conv_id}")))?;
    Ok(json!({ "cleared": true }))
}

fn act_media_upload_begin<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    _state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    if core.account().is_none() {
        return Err(BridgeError::Unauth("尚未登录".into()));
    }
    let size = params.get("size").and_then(Value::as_u64).unwrap_or(0);
    if size == 0 || size > 1024 * 1024 * 1024 {
        return Err(BridgeError::Validation("文件大小超出预算".into()));
    }
    let upload_id = format!("up-{}", wxlike_core::types::now_ms());
    let mime = str_param("mime");
    core.uploads
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(
            upload_id.clone(),
            UploadSession {
                name: str_param("name"),
                mime: if mime.is_empty() {
                    "application/octet-stream".into()
                } else {
                    mime
                },
                size,
                buf: Vec::with_capacity(size.min(8 * 1024 * 1024) as usize),
                last_index: u64::MAX,
            },
        );
    Ok(json!({ "upload_id": upload_id, "chunk_size": 4 * 1024 * 1024 }))
}

fn act_media_upload_part<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    _state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
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

fn act_media_upload_complete<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
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

fn act_media_upload_cancel<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    _state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    core.uploads
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&str_param("upload_id"));
    Ok(json!({ "cancelled": true }))
}

fn act_relations_list<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    relations_list(state, params)
}

fn act_relations_requests_incoming<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let reply = state.friend_requests_incoming()?;
    let mut rows = Vec::new();
    for item in reply.as_array().cloned().unwrap_or_default() {
        // 服务端字段是 `from_uid`（crates/platform/src/friends.rs 的
        // `RequestRow`，旧 UI 的 `ui_commands.rs:135` 也读这个）。旧代码读
        // `from_user_id` → 恒缺省，于是每条申请的 `from.user_id`/`from.name`
        // 都是空串，新 UI 的"新好友"页看不出是谁在申请（#296 附带发现）。
        let from_id = item["from_uid"]
            .as_str()
            .or_else(|| item["from_user_id"].as_str())
            .unwrap_or_default()
            .to_string();
        // 显示名与直聊标题同一套：kv 缓存 + profile 兜底，取不到就用 id。
        let from_name = if from_id.is_empty() {
            String::new()
        } else {
            cached_or_fetch(state, &format!("peer_name:{from_id}"), || {
                state
                    .http()
                    .expect_json("GET", &format!("/api/profile/{from_id}"), None)
                    .ok()
                    .and_then(|p| p["name"].as_str().map(str::to_string))
                    .unwrap_or_else(|| from_id.clone())
            })
        };
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

fn act_relations_request_send<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let target = str_param("user_id");
    let message = str_param("message");
    if target.is_empty() {
        return Err(BridgeError::Validation("请选择联系人".into()));
    }
    state.friend_request(&target, &message)?;
    Ok(json!({ "request_id": format!("out-{}", req.request_id), "status": "PENDING" }))
}

fn act_relations_request_accept<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let request_id = str_param("request_id");
    let accept = req.action_id.ends_with("accept");
    if accept {
        state.friend_accept(&request_id)?;
    } else {
        state.friend_decline(&request_id)?;
    }
    Ok(json!({ "status": if accept { "ACCEPTED" } else { "DECLINED" } }))
}

fn act_relations_block<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let user_id = str_param("user_id");
    let blocked = params
        .get("blocked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
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

fn act_relations_note_set<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let user_id = str_param("user_id");
    let note = str_param("note");
    state.http().expect_json(
        "PUT",
        &format!("/api/friends/{user_id}/remark"),
        Some(&json!({ "remark": note })),
    )?;
    Ok(json!({ "user_id": user_id, "note": note }))
}

fn act_relations_tag_set<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
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
    // 投影只有一处真相：base_contact 读的就是刚写进去的同一个 kv 键，
    // 且对 tags/star 都做了类型兜底。旧代码在此又用
    // `contact["tags"] = meta["tags"].clone()` 覆写一遍，于是
    // `relations.star.set` 会把没设过标签的联系人的 tags 写成 **null**
    // （契约是 string[]，contacts-page.tsx 直接 contact.tags.length → 崩）。
    // #296 删掉这层覆写：分支只改自己的字段，投影交给 base_contact。
    let contact = base_contact(state, &user_id)?;
    emit_core(app, "contact.updated", &user_id, contact.clone());
    Ok(contact)
}

fn act_relations_profile<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let user_id = str_param("user_id");
    let profile = state
        .http()
        .expect_json("GET", &format!("/api/profile/{user_id}"), None)?;
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

fn act_relations_common_groups<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let user_id = str_param("user_id");
    let groups = state.groups_mine()?;
    let mut out = Vec::new();
    for g in groups.as_array().cloned().unwrap_or_default() {
        let gid = g["group_id"].as_str().unwrap_or_default().to_string();
        let in_group = state
            .group_info(&gid)
            .ok()
            .and_then(|info| info["members"].as_array().cloned())
            .map(|members| {
                members
                    .iter()
                    .any(|x| x["user_id"] == json!(user_id) || x["member_id"] == json!(user_id))
            });
        if in_group == Some(true) {
            out.push(json!({ "group_id": gid, "name": g["name"].clone() }));
        }
    }
    Ok(Value::Array(out))
}

fn act_relations_group_members<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
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
        .map(|m| {
            m["role"]
                .as_str()
                .map(|r| json!(r))
                .unwrap_or_else(|| json!("member"))
        })
        .unwrap_or_else(|| json!("member"));
    Ok(json!({
        "group_id": group_id,
        "name": info["name"].clone(),
        "my_role": my_role,
        "members": members,
    }))
}

fn act_relations_group_kick<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = core;
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let group_id = str_param("group_id");
    let user_id = str_param("user_id");
    // 空 id 会拼出 `/api/groups//members/` 这种畸形路径，先拦掉。
    if group_id.is_empty() || user_id.is_empty() {
        return Err(BridgeError::Validation(
            "group_id 与 user_id 都不能为空".into(),
        ));
    }
    // 真路由：crates/platform/src/groups.rs 的
    // `.route("/api/groups/{group_id}/members/{user_id}", delete(remove_member))`
    // ——owner-only、拒绝移除群主，与 CLI `group.kick`
    // （crates/client/src/execute/handlers.rs:208）走同一条，不另起一套业务逻辑。
    // 该端点成功时 204：必须 expect_empty（into_json 会把空 body 当解码错误）。
    state.http().expect_empty(
        "DELETE",
        &format!("/api/groups/{group_id}/members/{user_id}"),
        None,
    )?;
    // UI 的 kick() 自己会 group.reload()；这条事件是给"没点踢人但开着群页"的
    // 其它投影用的（store.tsx 认 group.members.updated → 增量刷新）。
    emit_core(
        app,
        "group.members.updated",
        &group_id,
        json!({ "group_id": group_id, "removed": user_id }),
    );
    Ok(json!({ "group_id": group_id, "removed": user_id }))
}

fn act_relations_group_settings_set<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let group_id = str_param("group_id");
    let name = str_param("name");
    if name.is_empty() {
        return Err(BridgeError::Validation("群名称不能为空".into()));
    }
    // 服务端只有公告路由有写入口；改名路由未挂载时如实报错。
    state.http().expect_json(
        "PUT",
        &format!("/api/groups/{group_id}"),
        Some(&json!({ "name": name })),
    )?;
    emit_core(
        app,
        "group.updated",
        &group_id,
        json!({ "group_id": group_id, "name": name }),
    );
    Ok(json!({ "group_id": group_id, "name": name }))
}

fn act_search_query<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    search_query(state, params)
}

fn act_preferences_get<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    Ok(preferences_read(state))
}

fn act_preferences_set<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
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

fn act_secrets_status<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    _state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let lock = core.lock_state();
    Ok(json!({
        "locked": lock.locked,
        "os_backed": false,
        "passphrase_migration": if lock.passcode_sha.is_some() { "migrated" } else { "available" },
        "note": "桌面锁口令存本地库（sha256）；OS 密钥库由 G02 提供",
    }))
}

fn act_secrets_lock<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    _state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    core.lock_state().locked = true;
    Ok(json!({ "locked": true }))
}

fn act_secrets_migrate<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    _state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
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

fn act_storage_status<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let bytes = with_local(state, |s| {
        Ok(std::fs::metadata(s.path()).map(|m| m.len()).unwrap_or(0))
    })
    .unwrap_or(0);
    Ok(json!({ "cache_bytes": 0, "message_bytes": bytes, "media_bytes": 0 }))
}

fn act_storage_cache_clear<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    _state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    Ok(json!({ "freed_bytes": 0, "kept": ["未发送 outbox", "草稿", "密钥"] }))
}

fn act_backup_create<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let pending = outbox_pending(state);
    if pending > 0 {
        return Err(BridgeError::Conflict(
            "还有未发送消息，先发完或清空再备份".into(),
        ));
    }
    // WAL checkpoint 后原文件即完整快照（secure_delete 也不留正文残余）。
    with_local(state, |s| {
        let _ = s.lock().execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        Ok(())
    })?;
    let src = with_local(state, |s| Ok(s.path().to_path_buf()))?;
    let bytes =
        std::fs::read(&src).map_err(|e| BridgeError::Other(format!("读本地库失败：{e}")))?;
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

fn act_backup_verify<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    _state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
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

fn act_backup_import<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    if str_param("confirmation") != "IMPORT" {
        return Err(BridgeError::Confirmation(
            "导入会覆盖本地数据，需要再次确认".into(),
        ));
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

fn act_app_info<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    _state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    Ok(json!({
        "name": "wxlike",
        "version": env!("CARGO_PKG_VERSION"),
        "commit": option_env!("WXLIKE_GIT_SHA").unwrap_or("unknown"),
        "built_at": iso_ms(option_env!("WXLIKE_BUILD_TIME").and_then(|t| t.parse().ok()).unwrap_or(0)),
        "licenses": [
            { "name": "wxlike", "license": "AGPL-3.0-only" },
            { "name": "tauri", "license": "MIT/Apache-2.0" },
            { "name": "quinn (QUIC)", "license": "MIT/Apache-2.0" },
        ],
    }))
}

// ---------------------------------------------------------------------------
// 钱包（#293）：认证传输 + 域处理器
//
// 这七条 action 是"核心是唯一会话归属地"的第一块竖切：React 侧的钱包/商务页
// 过去直接 `fetch(baseUrl() + "/api/wallet/…")`，而 `setBaseUrl`/`saveSession`
// 没有任何调用点，所以那些请求**根本没带凭据**（#293 的缺陷本体）。改由桥代持
// 会话之后，webview 不再发 HTTP，token 也不出 Rust。
// ---------------------------------------------------------------------------

/// 账本一页的默认条数与上限（与服务端 `LEDGER_DEFAULT_LIMIT` / `clamp(1, 200)`
/// 同口径；这里重复一次是为了把超限的请求先削平，别让服务端报 400）。
const LEDGER_DEFAULT_LIMIT: i64 = 50;
const LEDGER_MAX_LIMIT: i64 = 200;
/// 进 URL 路径的 id 允许的最长长度（服务端生成的 id 远小于此，超限即畸形请求）。
const PATH_ID_MAX: usize = 128;
/// 钱包币种：服务端只有虚拟零钱账户，账本金额一律**整数分**。
const WALLET_CURRENCY: &str = "CNY";

/// 会话闸门：**没登录就一个字节都不发**（#293 的核心要求）。
///
/// 拦在发请求之前，是为了让"未登录"稳定报 `UNAUTHENTICATED`，而不是拿一个不带
/// 凭据的请求去换服务端的 401——断网时那条路还会退化成 `OFFLINE`，UI 更加猜不到
/// 是会话没了。也绝不许"没登录就返回空账本"：那是把没有会话伪装成没有数据。
fn require_session(state: &AppState) -> Result<(), BridgeError> {
    if state.auth_token().is_some() {
        return Ok(());
    }
    Err(BridgeError::Unauth(
        "尚未登录：钱包数据只经认证的 Rust 核心会话读取".into(),
    ))
}

/// 桥内**唯一**允许直连平台的出口（`wallet` 域用它；其余域逐步收拢到这里）。
///
/// * 凭据：base URL 与 Bearer 都在 [`AppState`] 的那一份 `Http` 里，由
///   `wxlike-client` 统一挂头。token 的值不经过这个函数，也不会出现在任何
///   action 的返回值里（`tests/bridge_wallet_auth.rs` 逐条验）；
/// * 幂等：`idem` 有值就转成 `Idempotency-Key`（重试只落一笔），没值就**不发**
///   这个头——绝不替调用方编一个键，那会让服务端把两笔不同的操作认成同一笔；
/// * 失败：非 2xx 一律按状态码如实报错（见 [`status_error`]），不折成
///   `{"data": null}` 那种"看着像成功"的空载荷。
fn authed_json(
    state: &AppState,
    method: &str,
    path: &str,
    body: Option<&Value>,
    idem: Option<&str>,
) -> Result<Value, BridgeError> {
    require_session(state)?;
    let read = matches!(method, "GET" | "DELETE");
    if !read && !matches!(method, "POST" | "PUT") {
        return Err(BridgeError::Other(format!("core 桥不支持的方法：{method}")));
    }
    if read && body.is_some() {
        return Err(BridgeError::Validation("读方法不该带 body".into()));
    }
    let (status, value) = call_platform(state, method, path, body, idem)?;
    if status.is_success() {
        return Ok(value);
    }
    Err(status_error(status, value, path))
}

/// 把请求发出去并把响应体读成 JSON。传输层失败（连不上/超时/DNS）折成
/// `OFFLINE`；HTTP 状态码原样带回去，由 [`status_error`] 分流——那里不再
/// 靠"错误文案里有没有 401 这几个字"猜状态。
fn call_platform(
    state: &AppState,
    method: &str,
    path: &str,
    body: Option<&Value>,
    idem: Option<&str>,
) -> Result<(http::StatusCode, Value), BridgeError> {
    let guard = state.http();
    let got = if matches!(method, "GET" | "DELETE") {
        guard.req(method, path).call()
    } else {
        platform_write(&guard, method, path, body, idem)
    };
    let mut resp =
        got.map_err(|err| BridgeError::Offline(format!("{method} {path}: 传输失败：{err}")))?;
    let status = resp.status();
    Ok((status, reply_json(&mut resp)))
}

/// 写请求（POST/PUT）：带幂等键（有的话），body 缺省时发空体——
/// 服务端的 `…/claim` 就不看 body，但 POST 无体才是合法形状（GET 才有 typestate 区分）。
fn platform_write(
    client: &Http,
    method: &str,
    path: &str,
    body: Option<&Value>,
    idem: Option<&str>,
) -> Result<http::Response<ureq::Body>, ureq::Error> {
    let req = client.req_write(method, path);
    let req = match idem {
        Some(key) => req.header("Idempotency-Key", key),
        None => req,
    };
    match body {
        Some(v) => req.send_json(v),
        None => req.send_empty(),
    }
}

/// 响应体 → JSON：空体给 `{}`（2xx 无体的端点），非 JSON 的体包成
/// `{"error": 原文}`。两者都**不**等于"服务端什么都没说"——字段提取那一步
/// 照样会因为缺字段而报错，不会静默变成一份空数据。
fn reply_json(resp: &mut http::Response<ureq::Body>) -> Value {
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    if text.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str(&text).unwrap_or_else(|_| json!({ "error": text }))
}

/// 状态码 → 契约错误码：**这份表是桥里的唯一出处**。
///
/// 401 与 403 必须是错误信封而不是 `{"data":null}`：前者要重新登录，后者重新登录
/// 也没用（`core-contract.ts` 的 `ErrorCode` 本来就有 `FORBIDDEN`，桥此前没用过）。
/// 404 归 `VALIDATION`——契约里没有 `NOT_FOUND` 这一档，服务端对无权读的红包也
/// 一律回 404（不确认存在性），把它说成"内部错误"才是真误导。
fn status_error(status: http::StatusCode, value: Value, path: &str) -> BridgeError {
    let reason = value
        .get("error")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("服务端拒绝了该请求");
    let message = format!("{reason}（{path} → {status}）");
    match status.as_u16() {
        401 => BridgeError::Unauth(message),
        403 => BridgeError::Forbidden(message),
        404 => BridgeError::Validation(message),
        409 => BridgeError::Conflict(message),
        429 => BridgeError::RateLimited(message),
        _ if status.is_server_error() => BridgeError::Dependency(message),
        _ => BridgeError::Other(message),
    }
}

/// 服务端响应缺字段 = 契约破了，如实报错。
///
/// 这里刻意不用 `value["x"].as_i64().unwrap_or_default()`：那会把"没给余额"
/// 上报成 `balance_minor: 0`，UI 显示的就是一份**看起来真实**的 0 元余额。
fn malformed(field: &str) -> BridgeError {
    BridgeError::Other(format!(
        "服务端响应缺少字段 {field}（拒绝把它兜成 0/空串再上报）"
    ))
}

/// 调用方少给了必填参数 = 校验错（不是 INTERNAL：服务端没做错任何事）。
fn missing_param(field: &str) -> BridgeError {
    BridgeError::Validation(format!("缺少参数 {field}"))
}

fn field_i64(value: &Value, field: &str) -> Option<i64> {
    value.get(field).and_then(Value::as_i64)
}

fn field_str(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_string)
}

// —— 以下 `need_*` 读的是**服务端响应**：缺字段一律 [`malformed`]。——
fn need_i64(value: &Value, field: &str) -> Result<i64, BridgeError> {
    field_i64(value, field).ok_or_else(|| malformed(field))
}

fn need_str(value: &Value, field: &str) -> Result<String, BridgeError> {
    field_str(value, field).ok_or_else(|| malformed(field))
}

fn need_bool(value: &Value, field: &str) -> Result<bool, BridgeError> {
    value
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| malformed(field))
}

fn need_array(value: &Value, field: &str) -> Result<Vec<Value>, BridgeError> {
    value
        .get(field)
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| malformed(field))
}

/// `cursor` 允许是 `null`（服务端还没有账本头），但字段必须存在。
fn need_opt_i64(value: &Value, field: &str) -> Result<Option<i64>, BridgeError> {
    let raw = value.get(field).ok_or_else(|| malformed(field))?;
    if raw.is_null() {
        return Ok(None);
    }
    raw.as_i64().map(Some).ok_or_else(|| malformed(field))
}

// —— 以下 `param_*` 读的是**调用方请求**：缺/错一律 VALIDATION，且都在发请求之前。——
fn param_i64(value: &Value, field: &str) -> Result<i64, BridgeError> {
    field_i64(value, field).ok_or_else(|| missing_param(field))
}

fn param_str(value: &Value, field: &str) -> Result<String, BridgeError> {
    let raw = field_str(value, field).ok_or_else(|| missing_param(field))?;
    if raw.trim().is_empty() {
        return Err(missing_param(field));
    }
    Ok(raw)
}

/// 必填的正整数金额/个数（单位：分、条）。缺、非整数、≤0 一律先拦掉再发请求。
fn param_amount(value: &Value, field: &str) -> Result<i64, BridgeError> {
    let amount = param_i64(value, field)?;
    if amount > 0 {
        return Ok(amount);
    }
    Err(BridgeError::Validation(format!(
        "{field} 必须是正整数（单位：分/条）"
    )))
}

/// 可选字符串参数：没给或 `null` → 空串；给了但不是字符串 → 校验错
/// （不许 `"memo": 123` 悄悄变成一条空备注的转账）。
fn opt_str(value: &Value, field: &str) -> Result<String, BridgeError> {
    match value.get(field).filter(|v| !v.is_null()) {
        Some(v) => v
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| BridgeError::Validation(format!("{field} 必须是字符串"))),
        None => Ok(String::new()),
    }
}

/// 进 URL 路径的 id：只允许安全字符。空串、`/`、`?`、`#`、空格、`..` 都会拼出
/// 畸形路径（`/api/wallet/redpacket//claim`）甚至改写请求目标，一律先拒。
/// 与 `relations.group.kick` 的空 id 拦截同口径，这里挡得更全（钱包 id 直接进路径）。
fn param_path_id(value: &Value, field: &str) -> Result<String, BridgeError> {
    let id = param_str(value, field)?;
    let safe = id.len() <= PATH_ID_MAX
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~'));
    if safe {
        return Ok(id);
    }
    Err(BridgeError::Validation(format!(
        "{field} 不是合法的 id（长度 ≤{PATH_ID_MAX}，只允许字母数字与 -_.~）"
    )))
}

fn act_wallet_balance<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let reply = authed_json(state, "GET", "/api/wallet/balance", None, None)?;
    Ok(json!({
        // 单位：分。服务端字段叫 `balance`，UI 模型叫 `balance_minor`（整数分）。
        "balance_minor": need_i64(&reply, "balance")?,
        "currency": WALLET_CURRENCY,
    }))
}

/// 账本一行。字段逐个必需。
///
/// `amount_minor` 是服务端给的**金额大小（整数分，恒为正）**，方向在 `kind` 里
/// （`transfer_out`/`transfer_in`/`redpacket_out`/`redpacket_in`/
/// `redpacket_refund`/`fund_in`，见 `crates/platform/src/wallet.rs` 的 `mod kind`）。
/// 桥**不**按 kind 给它补负号：那是在客户端另立一份服务端账本口径（`refund` 算进
/// 还是算出只有服务端知道），两边各猜一套迟早分叉。`created_at_ms` 按桥的惯例
/// 转成 ISO-8601（见 `shell/models.ts`）。
fn ledger_entry(row: &Value) -> Result<Value, BridgeError> {
    Ok(json!({
        "entry_id": need_i64(row, "id")?,
        "kind": need_str(row, "kind")?,
        "amount_minor": need_i64(row, "amount")?,
        "counterparty": need_str(row, "counterparty")?,
        "ref_id": need_str(row, "ref_id")?,
        "memo": need_str(row, "memo")?,
        "created_at_ms": iso_ms(need_i64(row, "created_at")?),
    }))
}

fn act_wallet_ledger_page<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    // `since`/`limit` 是**请求**侧的默认值（分页游标缺省从头、页大小缺省 50），
    // 与"把缺字段兜成默认值上报"是两回事：这里没有任何数据被编出来。
    let since = cursor_i64(&req.params).max(0);
    let limit = req
        .params
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(LEDGER_DEFAULT_LIMIT)
        .clamp(1, LEDGER_MAX_LIMIT);
    let path = format!("/api/wallet/ledger?since={since}&limit={limit}");
    let reply = authed_json(state, "GET", &path, None, None)?;
    let rows = need_array(&reply, "entries")?;
    let items = rows
        .iter()
        .map(ledger_entry)
        .collect::<Result<Vec<Value>, BridgeError>>()?;
    Ok(json!({
        "items": items,
        "next_cursor": need_opt_i64(&reply, "cursor")?,
        "has_more": need_bool(&reply, "has_more")?,
    }))
}

fn act_wallet_transfer<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let params = &req.params;
    let to_uid = param_path_id(params, "to_uid")?;
    let amount = param_amount(params, "amount_minor")?;
    let memo = opt_str(params, "memo")?;
    let body = json!({ "to_uid": to_uid, "amount": amount, "memo": memo });
    let reply = authed_json(
        state,
        "POST",
        "/api/wallet/transfer",
        Some(&body),
        req.idempotency_key.as_deref(),
    )?;
    Ok(json!({ "transfer_id": need_str(&reply, "transfer_id")? }))
}

fn act_wallet_redpacket_create<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let params = &req.params;
    let amount = param_amount(params, "amount_minor")?;
    let count = param_amount(params, "count")?;
    let memo = opt_str(params, "memo")?;
    // 会话可选：没给（或给了空串）就发 null，服务端按红包自身入账。
    let conv = params
        .get("conv_id")
        .and_then(Value::as_str)
        .filter(|c| !c.trim().is_empty());
    let body = json!({
        "amount": amount,
        "count": count,
        "memo": memo,
        "conv_id": conv,
    });
    let reply = authed_json(
        state,
        "POST",
        "/api/wallet/redpacket",
        Some(&body),
        req.idempotency_key.as_deref(),
    )?;
    Ok(json!({ "redpacket_id": need_str(&reply, "redpacket_id")? }))
}

/// 红包本体 → UI 形状。`remaining` 是**算出来的**（count − claimed），
/// 两个加数都来自服务端且都必需，缺任何一个就报错，不许猜 0。
fn redpacket_record(reply: &Value) -> Result<Value, BridgeError> {
    let count = need_i64(reply, "count")?;
    let claimed = need_i64(reply, "claimed")?;
    Ok(json!({
        "redpacket_id": need_str(reply, "id")?,
        "sender_id": need_str(reply, "from_uid")?,
        "conv_id": need_str(reply, "conv_id")?,
        "total_minor": need_i64(reply, "total")?,
        "count": count,
        "claimed": claimed,
        "remaining": count - claimed,
        "claimed_total_minor": need_i64(reply, "claimed_total")?,
        "memo": need_str(reply, "memo")?,
        "status": need_str(reply, "status")?,
        "created_at_ms": iso_ms(need_i64(reply, "created_at")?),
        "expire_at_ms": iso_ms(need_i64(reply, "expire_at")?),
    }))
}

fn act_wallet_redpacket_get<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = param_path_id(&req.params, "redpacket_id")?;
    let reply = authed_json(
        state,
        "GET",
        &format!("/api/wallet/redpacket/{id}"),
        None,
        None,
    )?;
    redpacket_record(&reply)
}

fn act_wallet_redpacket_claim<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = param_path_id(&req.params, "redpacket_id")?;
    let path = format!("/api/wallet/redpacket/{id}/claim");
    let reply = authed_json(state, "POST", &path, None, req.idempotency_key.as_deref())?;
    Ok(json!({
        "redpacket_id": id,
        "amount_minor": need_i64(&reply, "amount")?,
    }))
}

/// 红包列表：**服务端没有这条路由**，所以这里只有如实拒绝。
///
/// `crates/platform/src/wallet.rs` 挂的是 `POST /api/wallet/redpacket`、
/// `GET /api/wallet/redpacket/{id}`、`POST /api/wallet/redpacket/{id}/claim`、
/// `GET /api/wallet/balance`、`GET /api/wallet/ledger`、`POST /api/wallet/transfer`。
/// 没有列表就没有可靠来源：拿本地消息镜像拼一份"我发过的包"会漏掉别人在群里发的、
/// 还会让"领取"按钮点进一条不存在的路。登记在 [`STUBBED_ACTIONS`]（capabilities
/// 不吹它），等服务端补 `GET /api/wallet/redpacket` 后接 [`authed_json`] 落实。
fn act_wallet_redpacket_list<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    _state: &AppState,
    _core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    Err(BridgeError::NotImpl(
        "服务端没有红包列表路由（GET /api/wallet/redpacket 未挂载）：见 core_bridge 的 STUBBED_ACTIONS".into(),
    ))
}

fn message_hits(state: &AppState, query: &str) -> Result<Vec<Value>, BridgeError> {
    let reply = state.search_messages(query, 20)?;
    let rows = reply["results"].as_array().cloned().unwrap_or_default();
    let mut hits = Vec::new();
    for row in rows {
        let text = row.get("text").and_then(Value::as_str).unwrap_or_default();
        hits.push(json!({
            "id": row.get("msg_id").cloned().unwrap_or_else(|| json!("")),
            "kind": "content",
            "title": text.chars().take(SEARCH_TITLE_CHARS).collect::<String>(),
            "subtitle": row.get("conv_id").and_then(Value::as_str).unwrap_or_default(),
            "conv_id": row.get("conv_id").cloned().unwrap_or(Value::Null),
            "snippet": text.chars().skip(SEARCH_TITLE_CHARS).take(SEARCH_SNIPPET_CHARS).collect::<String>(),
        }));
    }
    Ok(hits)
}

fn contact_hits(state: &AppState, query: &str) -> Result<Vec<Value>, BridgeError> {
    let items = relations_list(state, &json!({ "query": query }))?["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let hits = items
        .iter()
        .map(|contact| {
            json!({
                "id": contact["user_id"].clone(),
                "kind": "contact",
                "title": contact["name"].clone(),
                "subtitle": contact["user_id"].clone(),
                "user_id": contact["user_id"].clone(),
            })
        })
        .collect();
    Ok(hits)
}

fn group_hits(state: &AppState, query: &str) -> Result<Vec<Value>, BridgeError> {
    let groups = state.groups_mine()?;
    let needle = query.to_lowercase();
    let mut hits = Vec::new();
    for g in groups.as_array().cloned().unwrap_or_default() {
        let name = g["name"].as_str().unwrap_or_default();
        if !name.to_lowercase().contains(&needle) {
            continue;
        }
        hits.push(json!({
            "id": g["group_id"].clone(),
            "kind": "group",
            "title": name,
            "subtitle": format!("{} 人", g["member_count"].as_i64().unwrap_or(0)),
            "group_id": g["group_id"].clone(),
        }));
    }
    Ok(hits)
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
        let phase = if core.lock_state().locked {
            "locked"
        } else {
            "authenticated"
        };
        let mut v = json!({ "phase": phase, "account": account });
        if phase == "locked" {
            v["lock_reason"] = json!("manual");
        }
        return Ok(v);
    }
    let token = with_local(state, |s| Ok(s.kv_get("auth_token")))
        .ok()
        .flatten()
        .unwrap_or_default();
    if token.is_empty() {
        return Ok(json!({ "phase": "signed_out" }));
    }
    state.set_auth_token(&token);
    let Ok(me) = state.me() else {
        return Ok(json!({ "phase": "signed_out" }));
    };
    let account = json!({
        "user_id": me["user_id"].clone(),
        "name": me["name"].clone(),
        "avatar": me["avatar"].clone(),
        "device_id": "desktop-tauri",
        "session_id": short_hash(&token),
    });
    *core.account.lock().unwrap_or_else(PoisonError::into_inner) = Some(account.clone());
    Ok(json!({ "phase": "authenticated", "account": account }))
}

// ---------------------------------------------------------------------------
// 会话列表 / 消息页
// ---------------------------------------------------------------------------

fn conversations_list(state: &AppState, params: &Value) -> Result<Value, BridgeError> {
    let filter = params
        .get("filter")
        .and_then(Value::as_str)
        .unwrap_or("all");
    let query = params
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE) as usize;
    let offset = cursor_usize(params);

    let me = state.user_id()?;
    let mut rows = Vec::new();
    for (conv, max_seq, max_at) in with_local(state, |s| s.conversations())? {
        rows.push(conversation_summary_parts(
            state, &conv, &me, max_seq, max_at,
        ));
    }
    if filter == "unread" {
        rows.retain(|c| c["unread"].as_i64().unwrap_or(0) > 0);
    }
    if filter == "groups" {
        rows.retain(|c| c["kind"] == json!("group"));
    }
    if !query.is_empty() {
        rows.retain(|c| conversation_matches_query(c, &query));
    }
    rows.sort_by(|a, b| {
        b["pinned"]
            .as_bool()
            .cmp(&a["pinned"].as_bool())
            .then(b["last_at_ms"].as_str().cmp(&a["last_at_ms"].as_str()))
    });
    let total = rows.len();
    let items: Vec<Value> = rows.into_iter().skip(offset).take(limit).collect();
    let has_more = offset + items.len() < total;
    let next = if has_more {
        json!(offset + items.len())
    } else {
        Value::Null
    };
    Ok(json!({ "items": items, "next_cursor": next, "has_more": has_more }))
}

/// 会话卡片的标题或最后一条预览里（已小写）有没有这段查询词。
fn conversation_matches_query(conversation: &Value, query: &str) -> bool {
    ["title", "last_preview"].iter().any(|field| {
        conversation[*field]
            .as_str()
            .unwrap_or_default()
            .to_lowercase()
            .contains(query)
    })
}

/// 单个会话卡片（含最新一条消息的预览）。
fn conversation_summary(state: &AppState, conv_id: &str) -> Result<Value, BridgeError> {
    let me = state.user_id()?;
    let last = with_local(state, |s| {
        Ok(s.page(conv_id, 0, 1)
            .map(|mut rows| rows.pop())
            .unwrap_or(None))
    })?;
    match last {
        Some(row) => {
            let payload = row.to_json();
            let mut c = conversation_summary_parts(state, conv_id, &me, row.seq, row.sent_at_ms);
            c["last_preview"] = json!(preview_of(&payload));
            c["last_at_ms"] = json!(iso_ms(row.sent_at_ms));
            Ok(c)
        }
        None => Ok(conversation_summary_parts(
            state,
            conv_id,
            &me,
            0,
            wxlike_core::types::now_ms() as i64,
        )),
    }
}

/// 会话标题：直聊用 kv 缓存的对端名（缺失时拉一次 profile 并缓存）；
/// 群聊用 group_info 的名字（同样缓存）。
fn conv_title(state: &AppState, conv_id: &str, me: &str, kind: &str) -> String {
    match kind {
        "direct" => {
            let peer = peer_of(conv_id, me).unwrap_or_else(|| conv_id.to_string());
            cached_or_fetch(state, &format!("peer_name:{peer}"), || {
                state
                    .http()
                    .expect_json("GET", &format!("/api/profile/{peer}"), None)
                    .ok()
                    .and_then(|p| p["name"].as_str().map(str::to_string))
                    .unwrap_or_else(|| peer.clone())
            })
        }
        _ => cached_or_fetch(state, &format!("group_name:{conv_id}"), || {
            state
                .group_info(conv_id)
                .ok()
                .and_then(|g| g["name"].as_str().map(str::to_string))
                .unwrap_or_else(|| conv_id.to_string())
        }),
    }
}

/// kv 缓存读取 + 未命中时取值并回写。
fn cached_or_fetch(state: &AppState, cache_key: &str, fetch: impl FnOnce() -> String) -> String {
    let cached = with_local(state, |s| Ok(s.kv_get(cache_key)))
        .ok()
        .flatten()
        .unwrap_or_default();
    if !cached.is_empty() {
        return cached;
    }
    let name = fetch();
    let _ = with_local(state, |s| s.kv_put(cache_key, &name));
    name
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
    let title = conv_title(state, conv_id, me, kind);
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
    let draft = with_local(state, |s| {
        Ok(s.kv_get(&format!("draft:{conv_id}")).unwrap_or_default())
    })
    .unwrap_or_default();
    let at = if max_seq > 0 {
        max_at
    } else {
        wxlike_core::types::now_ms() as i64
    };
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

/// 服务端这一页并入本地库（INSERT OR IGNORE）：本地是唯一渲染源。
fn merge_server_history(state: &AppState, conv_id: &str, before: i64, limit: i64) {
    let Ok(reply) = state.http().expect_json(
        "GET",
        &format!("/api/history/{conv_id}?before_seq={before}&limit={limit}"),
        None,
    ) else {
        return;
    };
    let Some(rows) = reply["messages"].as_array() else {
        return;
    };
    let _ = with_local(state, |s| {
        for row in rows {
            let _ = s.append(row);
        }
        Ok(())
    });
}

fn messages_page(state: &AppState, params: &Value) -> Result<Value, BridgeError> {
    let conv_id = params
        .get("conv_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE) as i64;
    let before = cursor_i64(params);
    merge_server_history(state, conv_id, before, limit);
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
    let conv_id = params
        .get("conv_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let body = params.get("body").cloned().unwrap_or(Value::Null);
    let client_msg_id = params
        .get("client_msg_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if conv_id.is_empty() || !body.is_object() {
        return Err(BridgeError::Validation("消息参数不合法".into()));
    }
    let me = state.user_id()?;
    send_body_by_kind(state, conv_id, &me, &body)?;
    notify_sent_message(app, state, conv_id, &me, &client_msg_id)
}

/// 按 body.kind 分派到对应的 AppState 发送方法。
fn send_body_by_kind(
    state: &AppState,
    conv_id: &str,
    me: &str,
    body: &Value,
) -> Result<(), BridgeError> {
    let to = recipients_of(conv_id, me);
    let kind = body["kind"].as_str().unwrap_or_default();
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
    Ok(())
}

/// 发送落库后的本地回显：最新一条推给 UI 立即上屏。
fn notify_sent_message<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    conv_id: &str,
    me: &str,
    client_msg_id: &str,
) -> Result<Value, BridgeError> {
    let last = with_local(state, |s| {
        Ok(s.page(conv_id, 0, 1)
            .map(|mut rows| rows.pop())
            .unwrap_or(None))
    })?;
    let Some(row) = last else {
        return Ok(json!({ "client_msg_id": client_msg_id, "state": "SERVER_ACCEPTED" }));
    };
    let payload = row.to_json();
    let message = row_to_message(&payload, me);
    let msg_id = message["msg_id"].clone();
    emit_core(app, "message.created", conv_id, message.clone());
    emit_core(
        app,
        "conversation.upsert",
        conv_id,
        json!({ "conv_id": conv_id, "last_preview": preview_of(&payload), "last_at_ms": message["created_at_ms"] }),
    );
    Ok(json!({ "msg_id": msg_id, "client_msg_id": client_msg_id, "state": "SERVER_ACCEPTED" }))
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
        .and_then(|r| {
            r["remark"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
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
    let tag_filter = params
        .get("tag")
        .and_then(Value::as_str)
        .unwrap_or_default();
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
            c["name"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase()
                .contains(&query)
                || c["user_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&query)
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
    let query = params
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let category = params
        .get("category")
        .and_then(Value::as_str)
        .unwrap_or("all");
    if query.trim().is_empty() {
        return Ok(json!({ "category": category, "hits": [] }));
    }
    let mut hits = Vec::new();
    // 消息全文（服务端 /api/search/messages）——全部/内容/会话分类。
    if matches!(category, "all" | "content" | "chats") {
        hits.extend(message_hits(state, query)?);
    }
    // 联系人与群（本地已有的关系面）。
    if matches!(category, "all" | "contacts") {
        hits.extend(contact_hits(state, query)?);
    }
    if matches!(category, "all" | "groups") {
        hits.extend(group_hits(state, query)?);
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

fn preferences_write<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    prefs: Value,
) -> Result<Value, BridgeError> {
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
    *core
        .qr_active
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = None;
}

fn qr_create<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
) -> Result<Value, BridgeError> {
    // 已有未结束的票据：直接复用（fixture 同语义）。
    if let Some(active) = core
        .qr_active
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
    {
        // 同上：http 守卫先落局部，别活在 if-let 条件里。
        let existing = state
            .http()
            .expect_json("GET", &format!("/api/qr/tickets/{active}"), None);
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
        return Err(BridgeError::NotImpl(
            "服务端 QR 票据响应缺少 ticket_id".into(),
        ));
    }
    let challenge = qr_challenge(&ticket, "CREATED", &reply);
    // 轮询线程：票据状态变化 → qr.state 事件（代次守卫：新 create/取消让旧线程退场）。
    stop_qr_poll(core);
    let generation = core.qr_poll_generation.load(Ordering::SeqCst);
    *core
        .qr_active
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(ticket.clone());
    spawn_qr_poll(app.clone(), state, &ticket, generation, challenge.clone());
    Ok(challenge)
}
/// 轮询线程：票据状态变化 → `qr.state` 事件（代次守卫：新 create/取消让旧线程退场）。
fn spawn_qr_poll<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: &AppState,
    ticket: &str,
    generation: u64,
    challenge: Value,
) {
    let ticket_for_poll = ticket.to_string();
    std::thread::spawn(move || {
        // 走 emit_core（而不是自己拼一份信封）：补发日志必须是事件流的完整真相，
        // 第二条平行发射手路就是 #296 那类"目录/实现漂移"的温床。
        emit_core(&app, "qr.state", &ticket_for_poll, challenge);
        poll_qr_ticket(&app, &ticket_for_poll, generation);
    });
    // state 参数为对称签名保留（轮询线程内经 app.state() 取用）。
    let _ = state;
}

fn poll_qr_ticket<R: tauri::Runtime>(app: &tauri::AppHandle<R>, ticket: &str, generation: u64) {
    let mut last = String::new();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(QR_POLL_INTERVAL_MS));
        if app
            .state::<CoreState>()
            .qr_poll_generation
            .load(Ordering::SeqCst)
            != generation
        {
            break;
        }
        let state = app.state::<AppState>();
        let Ok(reply) = state
            .http()
            .expect_json("GET", &format!("/api/qr/tickets/{ticket}"), None)
        else {
            continue;
        };
        let now = reply["state"].as_str().unwrap_or_default().to_string();
        if now.is_empty() || now == last {
            continue;
        }
        last = now.clone();
        let payload = qr_challenge(ticket, &now, &reply);
        emit_core(app, "qr.state", ticket, payload);
        if matches!(now.as_str(), "APPROVED" | "EXPIRED" | "REJECTED") {
            break;
        }
    }
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
        assert_eq!(
            BridgeError::from("401 unauthorized".to_string()).code(),
            "UNAUTHENTICATED"
        );
        assert_eq!(
            BridgeError::from("未连接到服务器".to_string()).code(),
            "OFFLINE"
        );
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
