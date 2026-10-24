//! 取消信号：贯穿"界面点取消 → 传输层中断 → 结果丢弃"整条链路。
//!
//! 一个取消语义要成立，得同时满足三件事，缺一件就会变成"取消了但没用"：
//!
//! 1. **可被观测**：传输层在发请求前、收响应后各查一次（[`Cancellation::check`]）；
//! 2. **可传递**：`Clone` 出去的所有副本指向同一个标志（`Arc<AtomicBool>`）；
//! 3. **不误伤**：取消是"调用方不要了"，不是失败——所以它有自己的错误码
//!    `cancelled`，不进重试队列（否则用户点了取消，队列还在后台重试 5 次）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use wxlike_types::DomainError;

/// 取消标志。默认未取消，`Clone` 共享同一个标志。
#[derive(Clone, Default, Debug)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    /// 新的（未取消）标志。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 取消。幂等：重复调用没有副作用。
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// 是否已取消。
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
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
}
