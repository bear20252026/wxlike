// support 碎片 1/6：桥状态与能力目录（CoreState/BridgeError/CoreReply、
// 常量、IMPLEMENTED_ACTIONS/STUBBED_ACTIONS）。由 support.inc 按序 include。

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
    "call.state",
    "call.start",
    "call.join",
    "call.leave",
    "call.mute",
    "call.mute_all",
    "call.lock",
    "call.record",
    "call.hand",
    "call.kick",
    "call.signal.post",
    "call.signal.take",
    "video.meta",
    "video.stats",
    "video.view",
    "video.feed",
    "live.state",
    "live.start",
    "live.stop",
    "live.join",
    "live.leave",
    "live.danmaku",
    "moment.publish",
    "moment.feed",
    "moment.detail",
    "moment.remove",
    "moment.like",
    "moment.unlike",
    "moment.comment",
    "moment.comment.remove",
    "report.create",
    "favorite.list",
    "favorite.one",
    "favorite.remove",
    "favorite.add",
    "favorite.update",
    "sticker.packs",
    "moment.media_url",
    "file.list",
    "file.rename",
    "file.share",
    "file.remove",
    "discover.services",
    "discover.quote",
    "discover.order_create",
    "discover.order_get",
    "discover.orders",
    "discover.order_refund",
    "mini.list",
    "mini.releases",
    "mini.grants",
    "mini.grant",
    "mini.revoke",
    "mini.uninstall",
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

