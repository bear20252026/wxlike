//! 事件订阅：**先落盘、再交付**。
//!
//! 顺序不能反。反过来（先交给 UI、再落盘）时，用户看到的最后几条消息在
//! 刷新之后会消失——因为刷新是从本地库重放的，而那几条还没进库。
//!
//! 三条规则：
//!
//! 1. `next()` 拿到事件先 `append_event`（幂等），落盘成功才推进游标，
//!    然后才交给调用方；
//! 2. 同一 `event_id` 重投只交付一次（重连重放是常态，不是异常）；
//! 3. `cancel()` 之后 `next()` 立刻返回 `cancelled`，不再碰网络与存储。

use wxlike_types::{Cursor, CursorKind, DomainError, EventEnvelope, Seq, SubscribeRequest};

use crate::cancel::{Cancellation, RaceOutcome};
use crate::ports::{EventSource, Ports, StoreError};

/// 一条事件订阅。
pub struct EventSubscription {
    source: Box<dyn EventSource>,
    ports: Ports,
    request: SubscribeRequest,
    cancel: Cancellation,
}

impl EventSubscription {
    /// 由 `Session::subscribe` 构造。
    #[must_use]
    pub fn new(
        source: Box<dyn EventSource>,
        ports: Ports,
        request: SubscribeRequest,
        cancel: Cancellation,
    ) -> Self {
        Self {
            source,
            ports,
            request,
            cancel,
        }
    }

    /// 下一条要交付给调用方的事件。
    ///
    /// 返回 `None` 表示流正常结束（服务端要求换连接），调用方应重新订阅。
    ///
    /// # Errors
    /// 被取消、传输失败、本地落盘失败。
    pub async fn next(&mut self) -> Result<Option<EventEnvelope>, DomainError> {
        loop {
            self.cancel.check()?;
            let Some(event) = self.pull().await? else {
                return Ok(None);
            };
            let fresh = self.persist(&event).await?;
            if fresh && self.request.accepts(&event) {
                return Ok(Some(event));
            }
            // 重复投递、或调用方不关心这一类：继续拉，不打扰调用方
        }
    }

    async fn pull(&mut self) -> Result<Option<EventEnvelope>, DomainError> {
        // 与取消通道赛跑：`cancel()` 一调，`source.next()` 的 Future 被
        // 丢弃——WebSocket 的轮询循环 / 长轮询的 sleep 都随它一起结束，
        // `next()` 立刻以 `cancelled` 返回，而不是等源自己吐下一条。
        // （`self.source` 可变借用与 `self.cancel` 共享借用不相交。）
        let cancelled = self.cancel.wake().notified();
        match crate::cancel::race(self.source.next(), cancelled).await {
            RaceOutcome::Left(result) => result.map_err(|err| err.into_domain()),
            RaceOutcome::Right(()) => Err(DomainError::cancelled()),
        }
    }

    /// 落盘 + 推进游标。返回"是不是新事件"。
    async fn persist(&self, event: &EventEnvelope) -> Result<bool, DomainError> {
        let fresh = self
            .ports
            .store
            .append_event(event)
            .await
            .map_err(store_error)?;
        if !fresh {
            return Ok(false);
        }
        let cursor = Cursor::at(CursorKind::Event, Seq::new(event.seq.get()));
        self.ports
            .store
            .put_cursor(&cursor)
            .await
            .map_err(store_error)?;
        Ok(true)
    }

    /// 取消订阅。幂等，可在另一个任务里调用。
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// 是否已取消。
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// 主动关闭底层连接。
    pub async fn close(&mut self) {
        self.cancel.cancel();
        self.source.close().await;
    }
}

fn store_error(error: StoreError) -> DomainError {
    match error {
        StoreError::QuotaExceeded => DomainError::unavailable("本地存储空间不足，事件无法落盘"),
        StoreError::Busy => DomainError::unavailable("本地存储正忙"),
        StoreError::Unavailable(why) => DomainError::unavailable(format!("本地存储不可用：{why}")),
        StoreError::Corrupt(why) => DomainError::internal(format!("本地数据损坏：{why}")),
    }
}
