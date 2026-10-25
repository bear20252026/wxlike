//! 退避策略：**全仓库只有这一份**。
//!
//! 三条要求，每一条都来自踩过的坑：
//!
//! 1. **指数退避**：1s → 2s → 4s → 8s → 16s（上限 30s）。固定间隔在服务端
//!    抖动时会变成雪崩，线性退避在手机断网恢复时又太慢。
//! 2. **必须有抖动**：多端同时掉线会同时重连、同时重发。抖动把重试摊开。
//!    用"等量抖动"（一半固定 + 一半随机），既不退化成固定间隔，也不会
//!    出现"退避到 0"的极端值。
//! 3. **有上限次数**：无限重试等于永远在耗电，而且用户看不到"发送失败"。

/// 抖动样本的取值范围（含 0，不含上限）。
pub const JITTER_RANGE: u32 = 1_000;

/// 退避参数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    /// 首次重试的等待（毫秒）。
    pub base_ms: u64,
    /// 单次等待的上限（毫秒）。
    pub max_ms: u64,
    /// 最多尝试几次（含首次）。
    pub max_attempts: u32,
}

impl Backoff {
    /// 默认：1s 起、30s 封顶、最多 5 次。
    ///
    /// 5 次的总窗口约 1+2+4+8+16 = 31 秒，正好覆盖"地铁进隧道"这类短暂断网；
    /// 再长用户就会认为"卡住了"，不如早点显示失败让用户点重试。
    pub const DEFAULT: Self = Self {
        base_ms: 1_000,
        max_ms: 30_000,
        max_attempts: 5,
    };

    /// 第 `attempt` 次失败后该等多久。`attempt` 从 0 开始（首次失败 = 0）。
    ///
    /// `sample` 是抖动样本，取值 `0..JITTER_RANGE`。**由调用方提供**而不是
    /// 内部取随机数：这样测试可以钉住具体值，不用去猜"随机之后是多少"。
    #[must_use]
    pub fn delay_ms(&self, attempt: u32, sample: u32) -> u64 {
        let raw = self.raw_delay_ms(attempt);
        // 等量抖动：一半固定，一半按 sample 摊开
        let half = raw / 2;
        let spread = raw - half;
        let factor = u64::from(sample % JITTER_RANGE);
        half + spread * factor / u64::from(JITTER_RANGE)
    }

    /// 未加抖动的退避值（诊断与测试用）。
    #[must_use]
    pub fn raw_delay_ms(&self, attempt: u32) -> u64 {
        // 移位上限：再大的 attempt 也已经封顶，避免溢出
        let shift = attempt.min(SHIFT_CAP);
        let scaled = self.base_ms.saturating_mul(1u64 << shift);
        scaled.min(self.max_ms)
    }

    /// 还有没有尝试机会（`attempts` 是已经失败的次数）。
    #[must_use]
    pub fn may_retry(&self, attempts: u32) -> bool {
        attempts < self.max_attempts
    }
}

/// 移位的上限：`1 << 20` 已经远超任何 `max_ms`。
const SHIFT_CAP: u32 = 20;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grows_exponentially_then_caps() {
        let backoff = Backoff::DEFAULT;
        assert_eq!(backoff.raw_delay_ms(0), 1_000);
        assert_eq!(backoff.raw_delay_ms(1), 2_000);
        assert_eq!(backoff.raw_delay_ms(2), 4_000);
        assert_eq!(backoff.raw_delay_ms(3), 8_000);
        assert_eq!(backoff.raw_delay_ms(4), 16_000);
        assert_eq!(backoff.raw_delay_ms(5), 30_000, "封顶");
        assert_eq!(backoff.raw_delay_ms(99), 30_000, "不许溢出");
    }

    #[test]
    fn jitter_stays_within_equal_jitter_bounds() {
        let backoff = Backoff::DEFAULT;
        let raw = backoff.raw_delay_ms(2); // 4000
        assert_eq!(backoff.delay_ms(2, 0), raw / 2, "样本 0 = 只有下半段");
        // 样本接近上限时几乎等于原值，但不会**超过**它
        let top = backoff.delay_ms(2, JITTER_RANGE - 1);
        assert!(top <= raw, "{top} 不该超过 {raw}");
        assert!(top >= raw - raw / 500, "{top} 应当接近 {raw}");
        for sample in [0, 1, 250, 500, 750, 999] {
            let delay = backoff.delay_ms(2, sample);
            assert!(delay >= raw / 2, "不该低于一半");
            assert!(delay <= raw, "不该超过原值");
        }
    }

    #[test]
    fn jitter_actually_spreads_retries() {
        let backoff = Backoff::DEFAULT;
        let a = backoff.delay_ms(3, 100);
        let b = backoff.delay_ms(3, 900);
        assert!(b > a, "不同样本必须得到不同的等待");
    }

    #[test]
    fn retry_budget_is_finite() {
        let backoff = Backoff::DEFAULT;
        assert!(backoff.may_retry(0));
        assert!(backoff.may_retry(4));
        assert!(!backoff.may_retry(5), "第 5 次失败后不再重试");
    }
}
