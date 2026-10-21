//! 重试与退避：**只有这一份实现**。
//!
//! 现在至少三个地方需要它——客户端发送队列（`outbox`）、转码作业队列
//! （`wxlike_video::jobs`）、以及以后任何"失败了等一会儿再试"的东西。
//!
//! 三份实现的危害不是"多写几行"，是**参数会悄悄漂移**：一处把起始延迟改成 2s、
//! 另一处还是 1s，用户的体感是"有的东西重试快有的慢"，而没有任何测试会发现。
//! 上一轮并行开发已经出现过一次——`video/jobs.rs` 的注释直接写着
//! "与 outbox 的 `BASE_DELAY_MS` 一致"，**注释就是重复的证据**。
//!
//! 两个容易写错的地方，这里一次说清：
//!
//! 1. **限流不算失败**（`throttled`）。被本地漏斗挡下的重发不该消耗尝试次数，
//!    否则用户会看到"发送失败"，而实际上再等一秒就好。这个语义在
//!    `outbox::SendFailure` 上，不在本模块——本模块只管"第 N 次失败等多久"。
//! 2. **溢出**。`1s << 20` 会回绕成 0（release 下），表现是"立刻重试"，
//!    比不重试更糟。所以位移量必须封顶，乘法用 `saturating_mul`。

/// 一条退避策略：起始延迟、上限、最多尝试几次。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    /// 首次失败后等多久。
    pub base_ms: u64,
    /// 单次等待的上限。再长用户就该看到失败，而不是无限转圈。
    pub max_ms: u64,
    /// 最多尝试几次（**含首次**）。达到就判死，等人工重试。
    pub max_attempts: u32,
}

impl Backoff {
    /// 默认策略：1s 起、30s 封顶、最多 5 次（含首次）。
    ///
    /// 与 Mars 的 `DEF_TASK_RETRY_INTERNAL`（1s）对齐；上限 30s 是"用户还能忍"的经验值。
    pub const DEFAULT: Self = Self {
        base_ms: 1_000,
        max_ms: 30_000,
        max_attempts: 5,
    };

    /// 第 `attempts` 次失败之后等多久（1/2/4/8/16/30s 封顶）。
    ///
    /// `attempts` 是**已经失败的次数**：首次失败传 1，得到 `base_ms`。
    /// 传 0 也返回 `base_ms`（调用方不该传 0，但传了也不该拿到 0 延迟）。
    pub fn delay_ms(&self, attempts: u32) -> u64 {
        let shift = attempts.saturating_sub(1).min(6);
        self.base_ms.saturating_mul(1u64 << shift).min(self.max_ms)
    }

    /// 还能不能再试。`attempts` 已达上限就判死。
    pub fn exhausted(&self, attempts: u32) -> bool {
        attempts >= self.max_attempts
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_double_then_stop_at_the_cap() {
        let b = Backoff::DEFAULT;
        assert_eq!(b.delay_ms(1), 1_000);
        assert_eq!(b.delay_ms(2), 2_000);
        assert_eq!(b.delay_ms(3), 4_000);
        assert_eq!(b.delay_ms(4), 8_000);
        assert_eq!(b.delay_ms(5), 16_000);
        assert_eq!(b.delay_ms(6), 30_000, "16s 之后到上限");
        assert_eq!(b.delay_ms(99), 30_000, "再多次也不溢出");
        assert_eq!(b.delay_ms(u32::MAX), 30_000, "极值也不回绕");
    }

    #[test]
    fn attempt_zero_still_waits_something() {
        // 调用方传 0 是 bug，但"等 0 毫秒"会变成忙等，比等 1 秒更糟。
        assert_eq!(Backoff::DEFAULT.delay_ms(0), 1_000);
    }

    #[test]
    fn exhaustion_is_inclusive_of_the_first_try() {
        let b = Backoff::DEFAULT;
        assert!(!b.exhausted(4));
        assert!(b.exhausted(5), "5 次含首次：第 5 次失败后不再自动重试");
        assert!(b.exhausted(6));
    }

    #[test]
    fn custom_policy_is_honoured() {
        let b = Backoff {
            base_ms: 250,
            max_ms: 1_000,
            max_attempts: 3,
        };
        assert_eq!(b.delay_ms(1), 250);
        assert_eq!(b.delay_ms(2), 500);
        assert_eq!(b.delay_ms(3), 1_000);
        assert_eq!(b.delay_ms(4), 1_000);
        assert!(b.exhausted(3));
    }
}
