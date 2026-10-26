//! 取消信号：贯穿"界面点取消 → 传输层中断 → 结果丢弃"整条链路。
//!
//! 一个取消语义要成立，得同时满足四件事，缺一件就会变成"取消了但没用"：
//!
//! 1. **可被观测**：传输层在发请求前、收响应后各查一次（[`Cancellation::check`]）；
//! 2. **可传递**：`Clone` 出去的所有副本指向同一个标志（`Arc<AtomicBool>`）；
//! 3. **可唤醒**：等待中的异步操作（空闲的事件源、长轮询）要在 `cancel()`
//!    的**当次调用**里被叫醒，而不是等它自己的下一次轮询——那是
//!    [`Wake`] 的职责：`cancel()` 既置标志也 `notify()`；
//! 4. **不误伤**：取消是"调用方不要了"，不是失败——所以它有自己的错误码
//!    `cancelled`，不进重试队列（否则用户点了取消，队列还在后台重试 5 次）。
//!
//! [`Wake`] 与 [`race`] 不依赖 tokio（client-core 的 src 侧零运行时依赖，
//! wasm 上也没有多线程），用 `std` 的原子量 + Waker 列表手写。

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use wxlike_types::DomainError;

/// 取消信号。默认未取消，`Clone` 共享同一个标志与唤醒通道。
pub struct Cancellation(Arc<Inner>);

struct Inner {
    flag: AtomicBool,
    wake: Wake,
}

impl Cancellation {
    /// 新的（未取消）标志。
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(Inner {
            flag: AtomicBool::new(false),
            wake: Wake::new(),
        }))
    }

    /// 取消。幂等：重复调用没有副作用。**会唤醒**所有挂在
    /// [`Cancellation::wake`] 上的等待者。
    pub fn cancel(&self) {
        self.0.flag.store(true, Ordering::SeqCst);
        self.0.wake.notify();
    }

    /// 是否已取消。
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.flag.load(Ordering::SeqCst)
    }

    /// 已取消则返回 `cancelled` 错误，否则 `Ok(())`。
    ///
    /// # Errors
    /// 已取消时返回 [`DomainError::Cancelled`]。
    pub fn check(&self) -> Result<(), DomainError> {
        if self.is_cancelled() {
            return Err(DomainError::cancelled());
        }
        Ok(())
    }

    /// 唤醒通道：把它与任意 Future 一起 [`race`]，`cancel()` 一调即返回。
    #[must_use]
    pub fn wake(&self) -> &Wake {
        &self.0.wake
    }
}

impl Default for Cancellation {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for Cancellation {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl std::fmt::Debug for Cancellation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cancellation")
            .field("cancelled", &self.is_cancelled())
            .finish_non_exhaustive()
    }
}

/// 一次性唤醒通道：`notify()` 之后所有 `notified()` 等待立即完成。
///
/// 手写而不是用 `tokio::sync::Notify`：client-core 不依赖任何运行时
/// （tokio 只在 dev-dependencies 里），wasm32 上也没有别的选择。
/// 语义上它是一次性的——`notify` 之后**所有现在与将来的** `notified()`
/// 都立刻完成，恰好就是"取消"要的行为（取消之后不该再有人睡过去）。
pub struct Wake {
    notified: AtomicBool,
    wakers: Mutex<Vec<Waker>>,
}

impl Wake {
    /// 新的（未通知）通道。
    #[must_use]
    pub fn new() -> Self {
        Self {
            notified: AtomicBool::new(false),
            wakers: Mutex::new(Vec::new()),
        }
    }

    /// 唤醒所有等待者；之后的等待立即完成。幂等。
    pub fn notify(&self) {
        self.notified.store(true, Ordering::Release);
        let mut wakers = self.wakers.lock().unwrap_or_else(|e| e.into_inner());
        let pending = std::mem::take(&mut *wakers);
        drop(wakers);
        for waker in pending {
            waker.wake();
        }
    }

    /// 等待通知。已通知（含通知多次）时立即完成。
    pub fn notified(&self) -> Notified<'_> {
        Notified { source: self }
    }
}

impl Default for Wake {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Wake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wake")
            .field("notified", &self.notified.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

/// [`Wake::notified`] 返回的 Future（借用 [`Wake`]，天然 Unpin）。
pub struct Notified<'a> {
    source: &'a Wake,
}

impl Future for Notified<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.source.notified.load(Ordering::Acquire) {
            return Poll::Ready(());
        }
        // 注册 waker（去重：同一 future 重复 poll 时替换而非堆积）
        let mut wakers = self
            .source
            .wakers
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = wakers.iter_mut().find(|waker| waker.will_wake(cx.waker())) {
            *slot = cx.waker().clone();
        } else {
            wakers.push(cx.waker().clone());
        }
        drop(wakers);
        // 注册后再查一次：notify 与注册之间的窗口不能漏
        if self.source.notified.load(Ordering::Acquire) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

/// 谁先完成。`Left` / `Right` 对应输入顺序。
pub enum RaceOutcome<A, B> {
    /// 第一个 Future 先完成。
    Left(A),
    /// 第二个 Future 先完成（第一个被丢弃，不再推进）。
    Right(B),
}

/// 两个 Future 赛跑：先完成者胜出，另一个被**丢弃**（其 Drop 即取消）。
///
/// 只支持 `Unpin` 的 Future——`EventSource::next` 返回装箱 Future
/// （async_trait），[`Notified`] 天然 Unpin，两边都满足；这个约束让我们
/// 不需要 pin-project、也不需要 `unsafe`（本 crate `forbid(unsafe_code)`）。
pub fn race<A, B>(left: A, right: B) -> Race<A, B>
where
    A: Future + Unpin,
    B: Future + Unpin,
{
    Race { left, right }
}

/// [`race`] 返回的 Future。
pub struct Race<A, B> {
    left: A,
    right: B,
}

impl<A, B> Future for Race<A, B>
where
    A: Future + Unpin,
    B: Future + Unpin,
{
    type Output = RaceOutcome<A::Output, B::Output>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if let Poll::Ready(value) = Pin::new(&mut this.left).poll(cx) {
            return Poll::Ready(RaceOutcome::Left(value));
        }
        if let Poll::Ready(value) = Pin::new(&mut this.right).poll(cx) {
            return Poll::Ready(RaceOutcome::Right(value));
        }
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_one_flag() {
        let a = Cancellation::new();
        let b = a.clone();
        assert!(!b.is_cancelled());
        a.cancel();
        assert!(b.is_cancelled(), "副本必须看到取消");
        assert_eq!(b.check().expect_err("cancelled").code(), "cancelled");
    }

    #[test]
    fn cancel_is_idempotent() {
        let flag = Cancellation::new();
        flag.cancel();
        flag.cancel();
        assert!(flag.is_cancelled());
    }

    /// 取消必须**立刻**唤醒挂在 wake 上的等待者：等下一次轮询不算数。
    #[tokio::test]
    async fn cancel_wakes_pending_waiters_immediately() {
        let flag = Cancellation::new();
        let waiter = flag.wake().notified();
        tokio::pin!(waiter);
        // 未取消时挂起
        tokio::select! {
            _ = &mut waiter => panic!("未通知就完成了"),
            _ = tokio::task::yield_now() => {}
        }
        flag.cancel();
        // 现在 notified 必须 already ready（不是等超时才返回）
        tokio::time::timeout(
            std::time::Duration::from_millis(1_000),
            flag.wake().notified(),
        )
        .await
        .expect("cancel 之后 notified 必须立刻完成");
    }

    /// race：右（取消通道）先完成时，左边的 Future 被丢弃。
    #[tokio::test]
    async fn race_returns_the_first_finisher() {
        let flag = Cancellation::new();
        let never = std::future::pending::<()>();
        let outcome = race(never, flag.wake().notified());
        tokio::pin!(outcome);
        // 没有取消时整体挂起
        tokio::select! {
            _ = &mut outcome => panic!("没取消就出结果了"),
            _ = tokio::task::yield_now() => {}
        }
        flag.cancel();
        match outcome.await {
            RaceOutcome::Right(()) => {}
            RaceOutcome::Left(()) => panic!("never future 不该先完成"),
        }
    }
}
