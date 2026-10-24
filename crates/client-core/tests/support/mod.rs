//! 端口的契约测试替身（in-memory）。
//!
//! 放在 `tests/support/` 而不是 `src/`：它不是生产代码，不该进产物。

// 测试替身里不是每个方法都被用到：按需取用是它的本分。
#![allow(dead_code)]

//!
//! **用途只有两个**，别拿它过集成验收：
//!
//! 1. 证明端口契约本身成立（"同幂等键入队两次只留一条"这类不变量）；
//! 2. 上游卡还没合入时并行开发，让核心状态机能先跑起来。
//!
//! 最终验收必须用真实实现（`crates/client-native` 的 SQLite、
//! `crates/client-web` 的 IndexedDB）。区别在于：这些替身**不模拟**事务、
//! 配额、跨进程锁——真出问题的地方恰好都在那儿。

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use wxlike_types::{
    ActionId, Cursor, CursorKind, Deadline, DomainError, EventEnvelope, IdempotencyKey, Seq,
    SubscribeRequest,
};

use wxlike_client_core::ports::{
    Clock, EventSource, HttpMethod, InstanceLock, Lifecycle, LocalStore, Notification,
    NotificationError, NotificationSink, OutboxRecord, Platform, PlatformCaps, PlatformError,
    Release, Secret, SecretError, Secrets, StoreError, Transport, TransportError, TransportRequest,
    TransportResponse,
};

/// 内存版本地库。
#[derive(Default)]
pub struct MemStore {
    state: Mutex<MemState>,
}

#[derive(Default)]
struct MemState {
    outbox: BTreeMap<String, OutboxRecord>,
    events: Vec<EventEnvelope>,
    cursor: Option<Cursor>,
    cache: BTreeMap<String, (Vec<u8>, bool)>,
    /// 模拟配额用尽 / 私有模式。
    fail_with: Option<StoreError>,
}

impl MemStore {
    /// 空库。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 让后续所有写操作失败（模拟配额用尽 / 私有模式）。
    pub fn fail_next_writes(&self, error: StoreError) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.fail_with = Some(error);
    }

    /// 当前 outbox 里有多少条。
    #[must_use]
    pub fn outbox_len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .outbox
            .len()
    }

    /// 事件条数。
    #[must_use]
    pub fn event_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .events
            .len()
    }

    /// 缓存里的原始字节（测试加密是否生效）。
    #[must_use]
    pub fn raw_cache(&self, key: &str) -> Option<(Vec<u8>, bool)> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cache
            .get(key)
            .cloned()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl LocalStore for MemStore {
    async fn put_outbox(&self, record: &OutboxRecord) -> Result<(), StoreError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(err) = state.fail_with.clone() {
            return Err(err);
        }
        state
            .outbox
            .insert(record.idempotency_key.to_string(), record.clone());
        Ok(())
    }

    async fn due_outbox(&self, now_ms: i64, limit: u32) -> Result<Vec<OutboxRecord>, StoreError> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<OutboxRecord> = state
            .outbox
            .values()
            .filter(|r| r.next_attempt_at_ms <= now_ms)
            .cloned()
            .collect();
        rows.sort_by_key(|r| r.created_at_ms);
        rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(rows)
    }

    async fn mark_outbox_done(&self, key: &IdempotencyKey) -> Result<(), StoreError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(err) = state.fail_with.clone() {
            return Err(err);
        }
        state.outbox.remove(key.as_str());
        Ok(())
    }

    async fn defer_outbox(
        &self,
        key: &IdempotencyKey,
        next_attempt_at_ms: i64,
        error: &str,
    ) -> Result<(), StoreError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(err) = state.fail_with.clone() {
            return Err(err);
        }
        if let Some(row) = state.outbox.get_mut(key.as_str()) {
            row.attempts += 1;
            row.next_attempt_at_ms = next_attempt_at_ms;
            row.last_error = Some(error.to_string());
        }
        Ok(())
    }

    async fn append_event(&self, event: &EventEnvelope) -> Result<bool, StoreError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(err) = state.fail_with.clone() {
            return Err(err);
        }
        if state.events.iter().any(|e| e.event_id == event.event_id) {
            return Ok(false);
        }
        state.events.push(event.clone());
        Ok(true)
    }

    async fn events_after(
        &self,
        cursor: Option<&Cursor>,
        limit: u32,
    ) -> Result<Vec<EventEnvelope>, StoreError> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let after = cursor
            .and_then(|c| c.decode().ok())
            .map_or(Seq::ZERO, |v| v.seq);
        let mut rows: Vec<EventEnvelope> = state
            .events
            .iter()
            .filter(|e| e.seq.get() > after.get())
            .cloned()
            .collect();
        rows.sort_by_key(|e| e.seq.get());
        rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(rows)
    }

    async fn put_cursor(&self, cursor: &Cursor) -> Result<(), StoreError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(err) = state.fail_with.clone() {
            return Err(err);
        }
        let incoming = cursor
            .decode()
            .map_err(|e| StoreError::Corrupt(e.to_string()))?;
        if let Some(existing) = state.cursor.as_ref().and_then(|c| c.decode().ok()) {
            if incoming.seq < existing.seq {
                return Err(StoreError::Corrupt("游标不许回退".to_string()));
            }
        }
        state.cursor = Some(cursor.clone());
        Ok(())
    }

    async fn cursor(&self) -> Result<Option<Cursor>, StoreError> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Ok(state.cursor.clone())
    }

    async fn put_cache(&self, key: &str, value: &[u8], sensitive: bool) -> Result<(), StoreError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(err) = state.fail_with.clone() {
            return Err(err);
        }
        state
            .cache
            .insert(key.to_string(), (value.to_vec(), sensitive));
        Ok(())
    }

    async fn get_cache(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Ok(state.cache.get(key).map(|(bytes, _)| bytes.clone()))
    }

    async fn purge(&self) -> Result<(), StoreError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.outbox.clear();
        state.events.clear();
        state.cache.clear();
        state.cursor = None;
        Ok(())
    }
}

/// 内存版时钟：`sleep` 直接把"现在"往前拨，测试不用真等。
pub struct MemClock {
    now_ms: Mutex<i64>,
    sleeps: Mutex<Vec<u64>>,
}

impl MemClock {
    /// 从某个时刻开始。
    #[must_use]
    pub fn at(now_ms: i64) -> Self {
        Self {
            now_ms: Mutex::new(now_ms),
            sleeps: Mutex::new(Vec::new()),
        }
    }

    /// 手动推进时间。
    pub fn advance(&self, ms: u64) {
        let mut now = self.now_ms.lock().unwrap_or_else(|e| e.into_inner());
        *now += i64::try_from(ms).unwrap_or(i64::MAX);
    }

    /// 记录过的睡眠时长。
    #[must_use]
    pub fn sleeps(&self) -> Vec<u64> {
        self.sleeps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Clock for MemClock {
    fn now_ms(&self) -> i64 {
        *self.now_ms.lock().unwrap_or_else(|e| e.into_inner())
    }

    async fn sleep(&self, ms: u64) {
        self.sleeps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(ms);
        self.advance(ms);
    }
}

/// 内存版事件源：把预置的事件按序吐出来，吐完返回 `None`（流结束）。
pub struct MemEventSource {
    queue: VecDeque<EventEnvelope>,
    closed: bool,
}

impl MemEventSource {
    /// 用一批事件构造。
    #[must_use]
    pub fn new(events: Vec<EventEnvelope>) -> Self {
        Self {
            queue: events.into(),
            closed: false,
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl EventSource for MemEventSource {
    async fn next(&mut self) -> Result<Option<EventEnvelope>, TransportError> {
        if self.closed {
            return Ok(None);
        }
        Ok(self.queue.pop_front())
    }

    async fn close(&mut self) {
        self.closed = true;
        self.queue.clear();
    }
}

/// 内存版传输：按脚本返回响应，记录发出去的请求。
#[derive(Default)]
pub struct MemTransport {
    script: Mutex<VecDeque<Result<TransportResponse, TransportError>>>,
    sent: Mutex<Vec<TransportRequest>>,
    events: Mutex<Vec<EventEnvelope>>,
}

impl MemTransport {
    /// 空脚本（所有请求都会返回 503）。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 排队一条响应。
    pub fn push_json(&self, status: u16, body: serde_json::Value) {
        let text = body.to_string();
        self.script
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(Ok(TransportResponse {
                status,
                body: text.into_bytes(),
            }));
    }

    /// 排队一个传输错误。
    pub fn push_error(&self, error: TransportError) {
        self.script
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(Err(error));
    }

    /// 已经发出去的请求。
    #[must_use]
    pub fn sent(&self) -> Vec<TransportRequest> {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 发出去几条。
    #[must_use]
    pub fn sent_len(&self) -> usize {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// 预置一条事件：`subscribe` 时按序交给订阅方。
    pub fn push_event(&self, event: EventEnvelope) {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(event);
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Transport for MemTransport {
    async fn send(&self, request: TransportRequest) -> Result<TransportResponse, TransportError> {
        self.sent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(request);
        let next = self
            .script
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front();
        next.unwrap_or(Ok(TransportResponse {
            status: 503,
            body: b"{\"error\":\"no script\"}".to_vec(),
        }))
    }

    async fn subscribe(
        &self,
        _request: SubscribeRequest,
    ) -> Result<Box<dyn EventSource>, TransportError> {
        let events = self
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        Ok(Box::new(MemEventSource::new(events)))
    }
}

/// 内存版密钥。
#[derive(Default)]
pub struct MemSecrets {
    items: Mutex<BTreeMap<String, Vec<u8>>>,
}

impl MemSecrets {
    /// 空。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Secrets for MemSecrets {
    async fn get(&self, key: &str) -> Result<Option<Secret>, SecretError> {
        let items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        Ok(items.get(key).map(|bytes| Secret::new(bytes.clone())))
    }

    async fn put(&self, key: &str, value: Secret) -> Result<(), SecretError> {
        self.items
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_string(), value.expose().to_vec());
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), SecretError> {
        self.items
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
        Ok(())
    }
}

/// 锁的释放动作：把标志置回 false。
///
/// 真实现里这是关文件句柄 / 释放 Web Lock；替身里就是一个 bool。
/// **`Drop` 必须真的释放**——`InstanceLock` 靠 `Drop` 解锁，替身不释放的话
/// "退出后能重新进入"这条永远测不出来。
struct MemRelease(std::sync::Arc<Mutex<bool>>);

impl Release for MemRelease {
    fn release(&mut self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = false;
    }
}

/// 内存版平台。
pub struct MemPlatform {
    caps: PlatformCaps,
    lock_held: std::sync::Arc<Mutex<bool>>,
    lifecycle: Mutex<VecDeque<Lifecycle>>,
}

impl MemPlatform {
    /// 用给定能力构造。
    #[must_use]
    pub fn new(caps: PlatformCaps) -> Self {
        Self {
            caps,
            lock_held: std::sync::Arc::new(Mutex::new(false)),
            lifecycle: Mutex::new(VecDeque::new()),
        }
    }

    /// 排一条生命周期事件。
    pub fn push_lifecycle(&self, event: Lifecycle) {
        self.lifecycle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(event);
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Platform for MemPlatform {
    fn caps(&self) -> PlatformCaps {
        self.caps
    }

    async fn acquire_instance_lock(&self) -> Result<InstanceLock, PlatformError> {
        let mut held = self.lock_held.lock().unwrap_or_else(|e| e.into_inner());
        if *held {
            return Err(PlatformError::AlreadyRunning);
        }
        *held = true;
        Ok(InstanceLock::new(Box::new(MemRelease(
            self.lock_held.clone(),
        ))))
    }

    async fn next_lifecycle(&self) -> Option<Lifecycle> {
        self.lifecycle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
    }
}

/// 内存版通知。
#[derive(Default)]
pub struct MemNotifications {
    foreground: Mutex<bool>,
    sent: Mutex<Vec<Notification>>,
}

impl MemNotifications {
    /// 默认前台。
    #[must_use]
    pub fn new() -> Self {
        Self {
            foreground: Mutex::new(true),
            sent: Mutex::new(Vec::new()),
        }
    }

    /// 设置前台/后台。
    pub fn set_foreground(&self, foreground: bool) {
        *self.foreground.lock().unwrap_or_else(|e| e.into_inner()) = foreground;
    }

    /// 已发通知。
    #[must_use]
    pub fn sent(&self) -> Vec<Notification> {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl NotificationSink for MemNotifications {
    fn is_foreground(&self) -> bool {
        *self.foreground.lock().unwrap_or_else(|e| e.into_inner())
    }

    async fn notify(&self, notification: Notification) -> Result<(), NotificationError> {
        if self.is_foreground() {
            return Ok(());
        }
        self.sent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(notification);
        Ok(())
    }
}

/// 造一条最小的命令请求（测试里反复要用）。
#[must_use]
pub fn action(id: &str) -> ActionId {
    ActionId::parse(id).expect("测试用 action id 必须合法")
}

/// 造一个幂等键。
#[must_use]
pub fn idem(key: &str) -> IdempotencyKey {
    IdempotencyKey::parse(key).expect("测试用幂等键必须合法")
}

/// 造一个事件游标。
#[must_use]
pub fn event_cursor(seq: u64) -> Cursor {
    Cursor::at(CursorKind::Event, Seq::new(seq))
}

/// 造一个未来截止时间。
#[must_use]
pub fn deadline_in(now_ms: i64, budget_ms: u64) -> Deadline {
    Deadline::after(now_ms, budget_ms)
}

/// 把 `DomainError` 转成便于断言的字符串（测试里常用）。
#[must_use]
pub fn code_of(error: &DomainError) -> &'static str {
    error.code()
}

/// 把 `HttpMethod` 转成便于断言的字符串。
#[must_use]
pub fn method_of(method: HttpMethod) -> &'static str {
    method.as_str()
}
