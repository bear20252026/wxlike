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

// ---------------------------------------------------------------------------
// 抖动样本：真随机，且 wasm 上可用
//
// 曾经这里返回常量 `JITTER_RANGE / 2`：多端同时掉线 → 同时到点 → 同时重发，
// 退避的"摊开"作用完全失效（雷群）。改用 xorshift64*：
//
// * **不引依赖**：`rand` 会把 `getrandom` 拖进 wasm32（正是本轮修的构建坑），
//   client-core 有意零运行时依赖；
// * **种子 = 时钟 + 单调计数 + 上一状态**：同一毫秒内的多次调用也不同，
//   重启后也不同（时钟参与种子）；
// * 状态是原子量：原生多线程安全，wasm 单线程零开销。
// ---------------------------------------------------------------------------

/// 抖动状态。0 是 xorshift 的不动点，初始化/存储时保证非零。
static JITTER_STATE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// 种子里的单调计数。
static JITTER_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// xorshift64* 的乘数（SplitMix 同源常数）。
const GOLDEN: u64 = 0x2545_F491_4F6C_DD1D;
/// 非零兜底种子。
const SEED_FLOOR: u64 = 0x9E37_79B9_7F4A_7C15;
/// xorshift64\* 的三个移位常数（Marsaglia 的 64 位三移位组合：12/25/27）。
const XORSHIFT_SHIFT_A: u32 = 12;
/// 见 [`XORSHIFT_SHIFT_A`]。
const XORSHIFT_SHIFT_B: u32 = 25;
/// 见 [`XORSHIFT_SHIFT_A`]。
const XORSHIFT_SHIFT_C: u32 = 27;

/// 取一个抖动样本，取值 `0..JITTER_RANGE`。
///
/// 不承诺密码学随机（退避抖动不需要），承诺的是：**相邻调用不同值**
/// （雷群不收敛）、值域有界（不会推出 0 或负延迟之外的东西）。
#[must_use]
pub fn jitter_sample() -> u32 {
    use std::sync::atomic::Ordering;
    let counter = JITTER_COUNT.fetch_add(1, Ordering::Relaxed);
    let clock = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);
    let previous = JITTER_STATE.load(Ordering::Relaxed);
    // 三路混合：计数保证同毫秒不同、时钟保证重启不同、上一状态保证序列相关
    let mut state = clock ^ counter.wrapping_mul(GOLDEN) ^ previous.rotate_left(1);
    if state == 0 {
        state = SEED_FLOOR;
    }
    // xorshift64*：缺任一步都会退化成短周期
    state ^= state >> XORSHIFT_SHIFT_A;
    state ^= state << XORSHIFT_SHIFT_B;
    state ^= state >> XORSHIFT_SHIFT_C;
    JITTER_STATE.store(state, Ordering::Relaxed);
    let mixed = state.wrapping_mul(GOLDEN);
    (mixed % u64::from(JITTER_RANGE)) as u32
}

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

    /// 抖动必须真的"抖"：连续样本不能是常量（雷群），且落在声明区间内。
    #[test]
    fn jitter_samples_are_not_a_constant_and_stay_in_range() {
        let mut distinct = std::collections::HashSet::new();
        for _ in 0..64 {
            let sample = jitter_sample();
            assert!(sample < JITTER_RANGE, "样本越界：{sample}");
            distinct.insert(sample);
        }
        assert!(
            distinct.len() > 8,
            "64 个样本只有 {} 个不同值——抖动退化成常量，雷群重发会回来",
            distinct.len()
        );
    }

    /// 同一毫秒内的连续调用也必须不同（时钟同值时靠计数器拉开）。
    #[test]
    fn jitter_differs_within_the_same_instant() {
        let first = jitter_sample();
        let second = jitter_sample();
        let third = jitter_sample();
        assert!(
            first != second || second != third,
            "同一毫秒内的样本全等：({first},{second},{third})"
        );
    }
}
