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
// 可判定的撤销（#293 台账 1/2/3 条共用）
// ---------------------------------------------------------------------------

/// 必填字符串参数：缺失或空白**一律报错**。
///
/// 台账第 3 条的成因就是 `params.get(key).and_then(Value::as_str)
/// .unwrap_or_default()`：参数没给时拿到空串，`remove("")` 必然落空，
/// 返回值却照样是 `cancelled: true`。缺参数是调用方的错，要如实拒绝，
/// 不许造一个"看着像参数"的值往下传。
fn required_str(params: &Value, key: &str) -> Result<String, BridgeError> {
    let raw = params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| BridgeError::Validation(format!("缺少参数 {key}")))?;
    let value = raw.trim();
    if value.is_empty() {
        return Err(BridgeError::Validation(format!("参数 {key} 不能为空")));
    }
    Ok(value.to_string())
}

/// 拿一张凭据真打一次需要鉴权的端点：`true` = 服务端仍然认它。
///
/// 这是"撤销到底生效没有"的唯一判据：`/api/logout` 回 2xx 只说明服务端收到了
/// 请求，不说明那行会话真的删了。用独立的 `Http` 探，避免动当前会话的凭据槽。
fn session_is_live(state: &AppState, token: &str) -> bool {
    let mut probe = Http::new(state.http_base());
    probe.token = Some(token.to_string());
    probe.expect_json("GET", "/api/me", None).is_ok()
}

/// 服务端登出 + 回查确证：只有"服务端确实不再认这张凭据"才算成功。
///
/// 撤销没生效时**不清本地**：把一张仍然生效的凭据从本地抹掉，等于让人
/// 连重试撤销的机会都没有（只剩"以为退出了"的假象）。
///
/// 这里走 `raw_json` 而不是 `AppState::logout()`：后者在 2xx 时顺手把本地
/// token 清了，而"2xx"恰恰是本项目要怀疑的那一步（收了请求 ≠ 撤了会话）。
/// 清本地由 [`sign_out_confirmed`] 在确证之后统一做。
fn revoke_session_on_server(state: &AppState, token: &str) -> Result<(), BridgeError> {
    if let Err(err) = state.raw_json("POST", "/api/logout", None) {
        // 401 = 服务端早就不认这张票了，等同已撤销（否则人会被锁在退不出的会话里）。
        if !err.contains("(401)") {
            return Err(BridgeError::from(format!("服务端登出失败：{err}")));
        }
    }
    if session_is_live(state, token) {
        return Err(BridgeError::Dependency(
            "服务端登出没有生效：旧凭据仍能调 /api/me，本地会话保持不变".into(),
        ));
    }
    Ok(())
}

/// 当前会话凭据 + 服务端登出确证；没有凭据时如实报错（没什么可撤销）。
fn sign_out_confirmed<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
) -> Result<(), BridgeError> {
    match state.auth_token() {
        Some(token) => revoke_session_on_server(state, &token)?,
        None => {
            return Err(BridgeError::Unauth(
                "当前没有已登录的会话，没有凭据可撤销".into(),
            ))
        }
    }
    state.http().token = None;
    core.account
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    // kv 里那份凭据也要抹掉，否则 restore 会把这张已撤销的票再捡回来。
    let _ = with_local(state, |s| {
        s.kv_delete("auth_token")?;
        s.kv_delete("account")
    });
    emit_core(
        app,
        "session.state",
        "session",
        json!({ "phase": "signed_out" }),
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// dispatch：全部已登记 action 的总入口
// ---------------------------------------------------------------------------

/// 域路由入口的统一签名（四个参数就是 `act_*` 的那四个）。
type Route<R> =
    fn(&tauri::AppHandle<R>, &AppState, &CoreState, &CoreRequest) -> Result<Value, BridgeError>;

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

