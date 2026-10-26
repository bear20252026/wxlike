//! 会话：一个进程/页面里**只有一个**客户端核心。
//!
//! 三条不变量（每条都有对应的验收用例）：
//!
//! 1. **单实例**：`open()` 抢平台锁，抢不到就返回 `AlreadyRunning`。
//!    两个核心同时消费一个游标 = 消息重复、outbox 重复发送。
//! 2. **先落盘再回话**：写命令先进本地 outbox（原子提交），成功之后才向
//!    调用方回 `queued`。所以"界面显示已发送 → 杀进程"这条路径上消息不会丢。
//! 3. **可退出**：`shutdown()` / `Drop` 取消一切在飞的订阅与补发，锁随之释放。

use std::sync::Mutex;

use serde_json::json;
use wxlike_types::{
    ActionId, ActionOutcome, ActionRequest, ActionResult, DomainError, EventEnvelope,
    IdempotencyKey, SubscribeRequest,
};

use crate::cancel::Cancellation;
use crate::ports::{InstanceLock, LocalStore, PlatformError, Ports, StoreError};
use crate::runtime::backoff::{self, Backoff};
use crate::runtime::router;
use crate::runtime::subscription::EventSubscription;

/// 会话打开失败。
#[derive(Debug)]
pub enum SessionError {
    /// 已有另一个实例持锁。
    AlreadyRunning,
    /// 平台不支持单实例锁（例如浏览器私有模式）。
    LockUnsupported(String),
    /// 本地库不可用（配额、私有模式、损坏）。
    Store(StoreError),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::AlreadyRunning => f.write_str("已有另一个客户端实例在运行"),
            SessionError::LockUnsupported(why) => write!(f, "无法保证单实例：{why}"),
            SessionError::Store(err) => write!(f, "本地存储不可用：{err}"),
        }
    }
}

impl std::error::Error for SessionError {}

/// 一轮补发的结果。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PumpReport {
    /// 取出来试了几条。
    pub attempted: u32,
    /// 成功（已出队）。
    pub sent: u32,
    /// 失败但还会重试（已按退避推迟）。
    pub deferred: u32,
    /// 永久失败或超过重试上限（已出队，等用户手动重试）。
    pub failed: u32,
}

impl PumpReport {
    /// 这一轮有没有改变任何状态（没有就不必刷新界面）。
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.attempted == 0
    }
}

/// 客户端核心会话。
pub struct Session {
    ports: Ports,
    /// 单实例锁；`None` 表示已释放。
    lock: Mutex<Option<InstanceLock>>,
    cancel: Cancellation,
    backoff: Backoff,
}

impl Session {
    /// 打开会话：抢单实例锁，读一次本地游标。
    ///
    /// # Errors
    /// 抢不到锁、平台不支持、本地库不可用。
    pub async fn open(ports: Ports) -> Result<Self, SessionError> {
        let lock = ports
            .platform
            .acquire_instance_lock()
            .await
            .map_err(|err| match err {
                PlatformError::AlreadyRunning => SessionError::AlreadyRunning,
                PlatformError::Unsupported(why) => SessionError::LockUnsupported(why),
            })?;
        // 本地库连不上就别装作能离线：这时候打开会话，用户发出去的东西
        // 只会留在内存里，杀进程就没了。
        ports.store.cursor().await.map_err(SessionError::Store)?;
        Ok(Self {
            ports,
            lock: Mutex::new(Some(lock)),
            cancel: Cancellation::new(),
            backoff: Backoff::DEFAULT,
        })
    }

    /// 换一套退避参数（测试用）。
    #[must_use]
    pub fn with_backoff(mut self, backoff: Backoff) -> Self {
        self.backoff = backoff;
        self
    }

    /// 取消信号（订阅、补发都挂在它上面）。
    #[must_use]
    pub fn cancellation(&self) -> Cancellation {
        self.cancel.clone()
    }

    /// 端口（平台实现要拿它做别的事时用）。
    #[must_use]
    pub fn ports(&self) -> &Ports {
        &self.ports
    }

    /// 当前时间（u64 毫秒）。负值（时钟被调回 1970 之前）一律按 0 处理。
    fn now_ms(&self) -> u64 {
        u64::try_from(self.ports.clock.now_ms()).unwrap_or(0)
    }

    /// 执行一条命令。
    pub async fn dispatch(&self, request: ActionRequest) -> ActionResult {
        let action = request.action_id.clone();
        match self.dispatch_inner(&request).await {
            Ok(outcome) => ActionResult {
                action_id: action,
                outcome,
                cursor: None,
                completed_at_ms: self.now_ms(),
            },
            Err(error) => ActionResult::failed(action, error, self.now_ms()),
        }
    }

    async fn dispatch_inner(&self, request: &ActionRequest) -> Result<ActionOutcome, DomainError> {
        self.cancel.check()?;
        self.check_deadline(request)?;
        let spec = catalog_spec(&request.action_id)?;
        let is_write = !matches!(spec.capability, wxlike_cmd::Capability::Read);
        request.check_idempotency(is_write)?;
        if is_write {
            self.submit_write(request).await
        } else {
            self.send_now(request).await
        }
    }

    fn check_deadline(&self, request: &ActionRequest) -> Result<(), DomainError> {
        let Some(deadline) = request.deadline else {
            return Ok(());
        };
        if deadline.expired(self.ports.clock.now_ms()) {
            return Err(DomainError::timeout("请求已超过截止时间"));
        }
        Ok(())
    }

    /// 写命令：**先落盘，再尝试发送**。
    async fn submit_write(&self, request: &ActionRequest) -> Result<ActionOutcome, DomainError> {
        let key = request
            .idempotency_key
            .clone()
            .ok_or_else(|| DomainError::invalid_field("idempotency_key", "写命令必须带幂等键"))?;
        let now = self.ports.clock.now_ms();
        let record = crate::ports::OutboxRecord::new(
            key.clone(),
            request.action_id.clone(),
            request.params.clone(),
            now,
        );
        self.ports
            .store
            .put_outbox(&record)
            .await
            .map_err(store_error)?;
        match self.send_now(request).await {
            Ok(outcome) => {
                self.forget(&key).await;
                Ok(outcome)
            }
            Err(error) if error.is_retryable() => Ok(ActionOutcome::Ok {
                data: json!({
                    "queued": true,
                    "idempotency_key": key.as_str(),
                    "retry_reason": error.code(),
                }),
            }),
            Err(error) => {
                // 永久失败（403 / 参数错）：留在队列里只会反复失败，出队并如实报错
                self.forget(&key).await;
                Err(error)
            }
        }
    }

    async fn forget(&self, key: &IdempotencyKey) {
        // 出队失败不该盖住真正的结果：下一次 pump 还会看到它，服务端幂等会兜住
        let _ = self.ports.store.mark_outbox_done(key).await;
    }

    /// 发一次请求（读写共用）。
    ///
    /// 单端点命令走 [`router::route`]；复合命令（`conv.list` / `conv.ack`）
    /// 在核心内拼多个请求（见 [`router::composite`]），不再永远返回
    /// "不能直接路由"。
    async fn send_now(&self, request: &ActionRequest) -> Result<ActionOutcome, DomainError> {
        let value = if router::is_composite(&request.action_id) {
            router::composite(
                self.ports.transport.as_ref(),
                &request.action_id,
                &request.params,
                request.deadline,
            )
            .await?
        } else {
            let mut transport_request = router::route(&request.action_id, &request.params)?;
            transport_request.deadline = request.deadline;
            let response = self
                .ports
                .transport
                .send(transport_request)
                .await
                .map_err(|err| err.into_domain())?;
            response.json()?
        };
        Ok(if wxlike_types::is_empty_payload(&value) {
            ActionOutcome::Empty
        } else {
            ActionOutcome::Ok { data: value }
        })
    }

    /// 补发一轮：取到点的记录，逐条重试。
    ///
    /// # Errors
    /// 只有"读队列"本身失败才返回错误；单条失败记进 [`PumpReport`]。
    pub async fn pump(&self, limit: u32) -> Result<PumpReport, DomainError> {
        self.cancel.check()?;
        let now = self.ports.clock.now_ms();
        let due = self
            .ports
            .store
            .due_outbox(now, limit)
            .await
            .map_err(store_error)?;
        let mut report = PumpReport {
            attempted: due.len() as u32,
            ..PumpReport::default()
        };
        for record in due {
            if self.cancel.is_cancelled() {
                return Err(DomainError::cancelled());
            }
            match self.pump_one(&record, now).await {
                Step::Sent => report.sent += 1,
                Step::Deferred => report.deferred += 1,
                Step::Failed => report.failed += 1,
            }
        }
        Ok(report)
    }

    async fn pump_one(&self, record: &crate::ports::OutboxRecord, now: i64) -> Step {
        if !self.backoff.may_retry(record.attempts) {
            // 超过上限：推到最远处，等用户手动重试（不是静默丢弃）
            let park_at = now.saturating_add(i64::from(u32::MAX));
            let _ = self
                .ports
                .store
                .defer_outbox(&record.idempotency_key, park_at, "已达最大重试次数")
                .await;
            return Step::Failed;
        }
        let request = ActionRequest::new(
            record.action_id.clone(),
            record.params.clone(),
            trace_for(&record.idempotency_key),
        )
        .with_idempotency(record.idempotency_key.clone());
        match self.send_now(&request).await {
            Ok(_) => {
                let _ = self
                    .ports
                    .store
                    .mark_outbox_done(&record.idempotency_key)
                    .await;
                Step::Sent
            }
            Err(error) if error.is_retryable() => {
                let delay = self
                    .backoff
                    .delay_ms(record.attempts, backoff::jitter_sample());
                let next_at = now.saturating_add(i64::try_from(delay).unwrap_or(i64::MAX));
                let _ = self
                    .ports
                    .store
                    .defer_outbox(&record.idempotency_key, next_at, error.message())
                    .await;
                Step::Deferred
            }
            Err(_permanent) => {
                // 永久失败：出队，等用户看到失败提示后手动重试
                let _ = self
                    .ports
                    .store
                    .mark_outbox_done(&record.idempotency_key)
                    .await;
                Step::Failed
            }
        }
    }

    /// 打开事件订阅。
    ///
    /// # Errors
    /// 建连失败、或本地库读不出游标。
    pub async fn subscribe(
        &self,
        request: SubscribeRequest,
    ) -> Result<EventSubscription, DomainError> {
        self.cancel.check()?;
        let source = self
            .ports
            .transport
            .subscribe(request.clone())
            .await
            .map_err(|err| err.into_domain())?;
        Ok(EventSubscription::new(
            source,
            self.ports.clone(),
            request,
            self.cancel.clone(),
        ))
    }

    /// 手动重试一条已被搁置（重试上限用尽）的待发记录。
    ///
    /// 重试预算**重新从 0 起算**：用户点"重试"是一次新决定，不该第一次
    /// 手动重试就被"已达上限"再顶回去。返回 `false` 表示队列里没有这条
    /// （可能已发出，或键写错了）。
    ///
    /// # Errors
    /// 本地库读写失败。
    pub async fn retry_outbox(&self, key: &str) -> Result<bool, DomainError> {
        let now = self.ports.clock.now_ms();
        // i64::MAX 当"现在"：把包括被搁置（next_attempt_at 被推到极远）在内
        // 的全部记录取出来找这条键。
        let all = self
            .ports
            .store
            .due_outbox(i64::MAX, u32::MAX)
            .await
            .map_err(store_error)?;
        let Some(mut record) = all
            .into_iter()
            .find(|record| record.idempotency_key.as_str() == key)
        else {
            return Ok(false);
        };
        record.attempts = 0;
        record.next_attempt_at_ms = now;
        record.last_error = None;
        self.ports
            .store
            .put_outbox(&record)
            .await
            .map_err(store_error)?;
        Ok(true)
    }

    /// 本地重放（冷启动 / 刷新页面）：从游标之后把已落盘的事件交出来。
    ///
    /// # Errors
    /// 本地库读失败。
    pub async fn replay(&self, limit: u32) -> Result<Vec<EventEnvelope>, DomainError> {
        let cursor = self.ports.store.cursor().await.map_err(store_error)?;
        self.ports
            .store
            .events_after(cursor.as_ref(), limit)
            .await
            .map_err(store_error)
    }

    /// 退出：取消在飞任务、释放单实例锁。幂等。
    pub fn shutdown(&self) {
        self.cancel.cancel();
        let mut lock = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mut held) = lock.take() {
            held.unlock();
        }
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("cancelled", &self.cancel.is_cancelled())
            .field("backoff", &self.backoff)
            .finish_non_exhaustive()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// 单条记录的处理结果。
enum Step {
    Sent,
    Deferred,
    Failed,
}

/// 从 catalog 取命令定义。
fn catalog_spec(action: &ActionId) -> Result<&'static wxlike_cmd::CommandSpec, DomainError> {
    wxlike_cmd::CATALOG
        .iter()
        .find(|spec| spec.id == action.as_str())
        .ok_or_else(|| DomainError::invalid(format!("未登记的命令：{}", action.as_str())))
}

// 空结果判定收口到契约层：`wxlike_types::is_empty_payload` 是
// `crates/cmd` 与这里共用的唯一口径（两处各写一份时口径必然漂移）。

/// 本地存储错误 → 领域错误。**只在这一处**映射。
fn store_error(error: StoreError) -> DomainError {
    match error {
        StoreError::QuotaExceeded => {
            DomainError::unavailable("本地存储空间不足，无法保存待发送消息")
        }
        StoreError::Busy => DomainError::unavailable("本地存储正忙，请稍后重试"),
        StoreError::Unavailable(why) => DomainError::unavailable(format!("本地存储不可用：{why}")),
        StoreError::Corrupt(why) => DomainError::internal(format!("本地数据损坏：{why}")),
    }
}

/// 补发请求的追踪 id：由幂等键派生，重试之间保持一致（日志能串起来）。
fn trace_for(key: &IdempotencyKey) -> wxlike_types::TraceId {
    wxlike_types::TraceId::derive(&format!("retry-{}", key.as_str()))
}

/// 供 `router` 之外的地方判断"这条命令是不是写"。
#[must_use]
pub fn is_write_action(action: &ActionId) -> bool {
    catalog_spec(action)
        .map(|spec| !matches!(spec.capability, wxlike_cmd::Capability::Read))
        .unwrap_or(false)
}

/// 本地队列里还剩多少条（诊断/角标用）。
///
/// # Errors
/// 本地库读失败。
pub async fn pending_count(store: &dyn LocalStore, now_ms: i64) -> Result<usize, DomainError> {
    let rows = store
        .due_outbox(now_ms, u32::MAX)
        .await
        .map_err(store_error)?;
    Ok(rows.len())
}
