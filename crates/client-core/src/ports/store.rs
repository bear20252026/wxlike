//! 本地存储端口：客户端唯一的"真相"。
//!
//! UI 不许自己再存一份消息（"UI 自己持第二份真相"是本轮返工最多的原因：
//! 核心更新了、UI 没收到、刷新一下又好了，问题只在杀进程后出现）。
//!
//! 四条语义必须由实现保证，核心按它们写逻辑：
//!
//! 1. **写 outbox 是原子的**：要么"这条待发送记录 + 它的幂等键"一起进盘，
//!    要么都没进。半条记录 = 用户点过发送但重启后什么都没有。
//! 2. **`append_event` 幂等**：同一 `event_id` 重复投递只落一条，
//!    返回值告诉你"是不是新的"（重连重放会重复投递，这是常态不是异常）。
//! 3. **游标与事件同事务推进**：先落事件再推游标；崩在中间时宁可重放，
//!    不可跳过（跳过 = 永久丢消息）。
//! 4. **配额与私有模式是正常错误**：`QuotaExceeded` / `Unavailable` 必须
//!    返回错误而不是 panic 或静默丢弃——浏览器私有模式下 IndexedDB 打不开，
//!    用户该看到"当前模式不能离线保存"，而不是白屏。

use wxlike_types::{ActionId, Cursor, EventEnvelope, IdempotencyKey};

use super::MaybeSend;

/// 一条待发送记录。
///
/// 需要序列化是因为 Web 端把它整条存进 IndexedDB（原生端拆成列存 SQLite，
/// 两种形态由各自实现决定，端口只认结构）。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OutboxRecord {
    /// 幂等键：重试时不变，服务端据此派生消息 id。
    pub idempotency_key: IdempotencyKey,
    /// 要执行的命令（`msg.send` / `msg.forward` …）。
    pub action_id: ActionId,
    /// 命令参数（原样存，重试时原样发）。
    pub params: serde_json::Value,
    /// 已经试过几次（含首次）。
    pub attempts: u32,
    /// 下次可以尝试的时间（unix 毫秒）。退避窗口内不会被取出。
    pub next_attempt_at_ms: i64,
    /// 入队时间，用于稳定排序与"最老待发"展示。
    pub created_at_ms: i64,
    /// 上次失败原因（给用户看的那一句）。
    pub last_error: Option<String>,
}

impl OutboxRecord {
    /// 新建一条（首次尝试）。
    #[must_use]
    pub fn new(
        idempotency_key: IdempotencyKey,
        action_id: ActionId,
        params: serde_json::Value,
        now_ms: i64,
    ) -> Self {
        Self {
            idempotency_key,
            action_id,
            params,
            attempts: 0,
            next_attempt_at_ms: now_ms,
            created_at_ms: now_ms,
            last_error: None,
        }
    }
}

/// 本地存储错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// 被别的写事务占着（浏览器多 tab、SQLite busy）。
    Busy,
    /// 配额用尽（IndexedDB / 磁盘满）。
    QuotaExceeded,
    /// 不可用（私有模式、被用户禁用、库文件损坏到打不开）。
    Unavailable(String),
    /// 数据损坏（能打开但读不出来）。
    Corrupt(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Busy => f.write_str("本地库正忙"),
            StoreError::QuotaExceeded => f.write_str("本地存储空间不足"),
            StoreError::Unavailable(detail) => write!(f, "本地存储不可用：{detail}"),
            StoreError::Corrupt(detail) => write!(f, "本地数据损坏：{detail}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl StoreError {
    /// 重试能不能好。
    ///
    /// `Busy` 值得重试（等另一个 tab 写完）；`QuotaExceeded` 重试也没用，
    /// 得先清理；`Corrupt` 必须走重建流程，不能靠重试。
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, StoreError::Busy)
    }
}

/// 本地存储端口。
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait LocalStore: MaybeSend {
    /// 原子入队。同一幂等键重复入队必须**覆盖而不新增**。
    ///
    /// # Errors
    /// 见 [`StoreError`]。
    async fn put_outbox(&self, record: &OutboxRecord) -> Result<(), StoreError>;

    /// 取出到点的待发记录（`next_attempt_at_ms <= now`），按 `created_at_ms` 升序。
    ///
    /// # Errors
    /// 见 [`StoreError`]。
    async fn due_outbox(&self, now_ms: i64, limit: u32) -> Result<Vec<OutboxRecord>, StoreError>;

    /// 标记已成功（删除记录）。**必须在服务端确认之后**调用。
    ///
    /// # Errors
    /// 见 [`StoreError`]。
    async fn mark_outbox_done(&self, key: &IdempotencyKey) -> Result<(), StoreError>;

    /// 记一次失败：尝试次数 +1、退避到 `next_attempt_at_ms`、留下错误。
    ///
    /// # Errors
    /// 见 [`StoreError`]。
    async fn defer_outbox(
        &self,
        key: &IdempotencyKey,
        next_attempt_at_ms: i64,
        error: &str,
    ) -> Result<(), StoreError>;

    /// 追加事件。同一 `event_id` 已存在时返回 `false`（不算错误）。
    ///
    /// # Errors
    /// 见 [`StoreError`]。
    async fn append_event(&self, event: &EventEnvelope) -> Result<bool, StoreError>;

    /// 读游标之后的事件（`None` = 从头）。
    ///
    /// # Errors
    /// 见 [`StoreError`]。
    async fn events_after(
        &self,
        cursor: Option<&Cursor>,
        limit: u32,
    ) -> Result<Vec<EventEnvelope>, StoreError>;

    /// 推进游标。只许前进：实现必须拒绝回退（回退 = 重放已确认的事件）。
    ///
    /// # Errors
    /// 见 [`StoreError`]。
    async fn put_cursor(&self, cursor: &Cursor) -> Result<(), StoreError>;

    /// 读游标。
    ///
    /// # Errors
    /// 见 [`StoreError`]。
    async fn cursor(&self) -> Result<Option<Cursor>, StoreError>;

    /// 写渲染缓存。`sensitive = true` 时实现必须加密后再落盘
    /// （Web 端用 WebCrypto，原生端用设备密钥）。
    ///
    /// # Errors
    /// 见 [`StoreError`]。
    async fn put_cache(&self, key: &str, value: &[u8], sensitive: bool) -> Result<(), StoreError>;

    /// 读渲染缓存。
    ///
    /// # Errors
    /// 见 [`StoreError`]。
    async fn get_cache(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError>;

    /// 清空（退出登录）。幂等。
    ///
    /// # Errors
    /// 见 [`StoreError`]。
    async fn purge(&self) -> Result<(), StoreError>;
}
