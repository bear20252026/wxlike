//! 绑定层的数据形状（Dart 侧直接渲染的东西）。
//!
//! 每个结构体都是**Rust 已经算完**的结果：`mine` 已经比过发送者是不是我、
//! `unread` 直接来自服务端信箱、`pending` 是本会话队列里还没发出去条数。
//! Dart 拿不到"需要再判断一次"的原始字段。
//!
//! 数字一律用 `i64`（含时间戳与 seq）：UniFFI 的 `u64` 在 Dart 侧会变成
//! `BigInt`，把 JS 风格的数值问题带进 UI；`i64` 对应 Dart 的 `int`。

/// 一次会话凭据。token 本身不出现在这里——移动端不需要看它，
/// 存它的是 Rust（`crates/ffi/src/cache.rs`），发请求时由 Rust 带上。
#[derive(uniffi::Record, Clone, Debug)]
pub struct SessionInfo {
    pub user_id: String,
    pub server: String,
    pub signed_in: bool,
}

/// 会话列表的一行。`kind` ∈ `direct` / `group`。
#[derive(uniffi::Record, Clone, Debug)]
pub struct ConversationRow {
    pub conv: String,
    pub kind: String,
    pub title: String,
    pub members: i64,
    /// 服务端信箱算出来的未读数；查询失败时为 -1（UI 显示"未读未知"，
    /// 不能显示 0——那是"已读完"的假绿）。
    pub unread: i64,
    /// 本会话队列里待发送/失败的条数。
    pub pending: i64,
}

/// 一页会话列表。
///
/// 包一层记录而不是直接返回数组，有两个原因：一是 `MessagePage` 已经带的
/// `from_cache` 在这里同样需要（列表读的是本地缓存时，界面不能说"这是最新的"）；
/// 二是绑定层的现实——`uniffi-bindgen-dart` 生成"顶层返回数组"的方法时会留下
/// 一个 `UnsupportedError` 占位（RustCallStatus out-arg 那条路径还没实现），
/// 记录返回则正常。数组装进记录里就没这个问题。
#[derive(uniffi::Record, Clone, Debug)]
pub struct ConversationList {
    pub conversations: Vec<ConversationRow>,
    pub from_cache: bool,
}

/// 一条可渲染的消息。
#[derive(uniffi::Record, Clone, Debug)]
pub struct ChatMessage {
    pub conv: String,
    pub seq: i64,
    pub msg_id: String,
    pub sender: String,
    /// 服务端 `kind`：`text` / `image` / `system` / `recall` / 自定义类型。
    pub kind: String,
    /// `kind` 为 `text` / `system` / `recall` 时的正文；其余为空串。
    pub text: String,
    /// `kind` 为 `image` 等带媒体时的 media_id；没有则空串。
    pub media_id: String,
    pub sent_at_ms: i64,
    /// 作者撤回过（本地库里的占位行）。
    pub withdrawn: bool,
    /// 发送者是不是当前登录用户。判断在 Rust：三端不能各比一次。
    pub mine: bool,
}

/// 一页历史。`from_cache` 为真表示这一页来自本地库（当前是离线兜底）。
#[derive(uniffi::Record, Clone, Debug)]
pub struct MessagePage {
    pub messages: Vec<ChatMessage>,
    pub from_cache: bool,
    /// 往前翻的游标；没有更多时为 -1。
    pub next_before_seq: i64,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct UnreadState {
    pub conv: String,
    /// 属主类型：`direct` / `group` / `channel`（`crates/message` 的三类）。
    pub owner: String,
    pub head_seq: i64,
    pub read_seq: i64,
    pub unread: i64,
}

/// 队列里一条消息的状态。
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboxState {
    /// 还没发出去，或在退避重试中
    Pending,
    Sent,
    /// 不再自动重试（服务端明确拒绝，或尝试次数用尽）
    Failed,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct OutboxEntry {
    pub task_id: String,
    pub conv: String,
    pub kind: String,
    pub text: String,
    pub state: OutboxState,
    /// 已经失败过几次
    pub attempts: i64,
    /// 下一次自动重试的毫秒时间戳；`Failed` 时为 0
    pub next_retry_at: i64,
    pub last_error: String,
}

/// `outbox_status()` 的返回：全局计数 + 需要渲染的行。
#[derive(uniffi::Record, Clone, Debug)]
pub struct OutboxSummary {
    pub pending: i64,
    pub failed: i64,
    pub entries: Vec<OutboxEntry>,
}

/// `flush_outbox()` 一轮的结果（对应 `wxlike_core::outbox::FlushReport`）。
#[derive(uniffi::Record, Clone, Debug)]
pub struct FlushReport {
    pub sent: i64,
    /// 失败但还会重试
    pub retrying: i64,
    /// 失败且不再重试
    pub failed: i64,
}

/// `send_text()` 的结果。
///
/// 注意它**不一定抛错**：网络发不出去时消息已经进了队列，这才是真相
/// （`state = Pending` + `error` 说明为什么）。会抛的只有两种：
/// 本地库写不进去，或 `Policy` 在入队之前就把这条命令拒了。
#[derive(uniffi::Record, Clone, Debug)]
pub struct SendOutcome {
    pub task_id: String,
    pub state: OutboxState,
    /// 服务端分配的 msg_id；没发出去时为空串。
    pub msg_id: String,
    /// 失败原因；成功时为空串。
    pub error: String,
}

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveState {
    /// 还没有 token
    SignedOut,
    /// 有 token，但连不上服务端
    Offline,
    Online,
}

/// 连接与队列的整体状态，给顶部的"离线/待发送"指示器用。
#[derive(uniffi::Record, Clone, Debug)]
pub struct LiveInfo {
    pub state: LiveState,
    pub user_id: String,
    pub server: String,
    pub pending: i64,
    /// 后台补发线程最近一次的错误；空串表示一切正常。
    ///
    /// 为什么要有这个字段：补发线程没有返回值可看，出错时如果什么都不说，
    /// 就是本项目明令禁止的"看着能用、其实什么都没发生"。
    pub pump_error: String,
}
