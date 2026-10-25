//! P3 投递策略（合同不变式 9 / §3.2 拉黑 / §3.3 禁言的投递面落地）。
//!
//! hub 在内容帧入站后、持久化与扇出前咨询此策略；裁决权在调用方注入的
//! 实现（server 用 `Platform` 的关系/禁言表作答，测试用静态表）。三条裁决
//! 全部为**帧级丢弃**：被拦的帧不落库、不暂存、不扇出——被拉黑者的消息连
//! 痕迹都不留（与 WP-03 伪造撤回同一处理哲学）。
//!
//! 豁免范围（刻意收窄）：
//! * **控制帧**（回执/撤回）不咨询策略——撤回自己已发出的消息不受后续拉黑
//!   影响，回执路由同理；
//! * **系统帧**（`Notice` body，好友/群事件）不咨询——管理与关系事件必须
//!   到达（否则拉黑后连"取消拉黑"都收不到对方的任何信号）。
//!
//! 失败语义：实现方在裁决查询出错时**放行**（fail-open）并自行记日志——
//! 拦截增强不能变成可用性单点。

/// 发送闸门允许的最长退避。超过它的 `Sleep` 不再等待，按 `Reject` 丢弃。
///
/// 退避发生在该连接的读循环里：无上限的 sleep 等于把连接任务交给策略方
/// 无限挂起（慢速 DoS）。1 秒够吸收一次突发，又不会让一条连接停摆太久。
pub const MAX_SEND_GATE_SLEEP_MS: u64 = 1_000;

/// P4 发送闸门结果（风控 + 内容审核）。
///
/// 与 `blocked`/`muted` 的区别：那两条是**关系裁决**（谁可以给谁发），
/// 这条是**行为裁决**（这个人此刻还能不能发、这条内容能不能投递）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendGate {
    Allow,
    /// 降速：帧丢弃，调用方应提示"发送过快"。
    Throttle {
        retry_after_ms: u64,
    },
    /// 软限流（Synapse `should_sleep` 同型）：等 `delay_ms` 后继续投递。
    ///
    /// 超过 [`MAX_SEND_GATE_SLEEP_MS`] 的请求由 hub 按拒绝处理。
    Sleep {
        delay_ms: u64,
    },
    /// 拒绝：硬违规或封禁。
    Reject {
        reason: String,
    },
}

#[async_trait::async_trait]
pub trait DeliveryPolicy: Send + Sync + 'static {
    /// 收件人 `to` 已拉黑发送者 `from`：该帧对此收件人不存在。
    async fn blocked(&self, from: &str, to: &str) -> bool;

    /// `user` 在 `group` 当前被禁言（全员或单人、未到解除时刻）。
    async fn muted(&self, group: &str, user: &str) -> bool;

    /// `from` 可直发 `to`（好友，或对方接受过其陌生人请求——不变式 9）。
    async fn direct_allowed(&self, from: &str, to: &str) -> bool;

    /// P4 发送闸门：风控限流 + 内容审核（ADR-20 前两层）。
    ///
    /// 默认放行——保持 P3 的既有实现与测试不动；只有注入真实平台的部署
    /// 才覆盖它。`Throttle`/`Reject` 时 hub 丢弃该帧（不落库、不暂存、
    /// 不扇出）；`Sleep` 时先退避再走正常投递。
    async fn send_gate(&self, _from: &str, _conv: &str, _text: &str) -> SendGate {
        SendGate::Allow
    }

    /// 发送闸门在限流指标里的名字（Synapse `rate_limiter_name` 标签）。
    ///
    /// 多个限流器共用一个 hub 时靠它区分序列；注入真实风控的部署可以
    /// 覆盖成 `risk_send` 之类，默认值代表"策略层统一闸门"。
    fn rate_limiter_name(&self) -> &'static str {
        "send_gate"
    }

    /// AR-6（第四轮审计 #30）：秘密会话的消息 TTL 兜底。
    ///
    /// 返回该会话启用的消息存活时长（毫秒）；`None` = 非秘密会话 / 未启用
    /// / 查询失败（fail-open，与 `blocked` 同语义）。hub 在**补发离线帧**
    /// 时咨询它：`sent_at_ms + ttl < now` 的内容帧直接 ack 丢弃——秘密
    /// 消息"到期即不可再取"从只覆盖 HTTP 检索/搜索面，扩展到 QUIC/WS
    /// 两条实时链路的最后一跳（issue #15 的"端到端闭环"缺口）。
    /// 回执/撤回/系统帧豁免：撤回与回执恰恰要在过期后仍能到达。
    async fn secret_ttl_ms(&self, _conv_id: &str) -> Option<u64> {
        None
    }
}

// ---------------------------------------------------------------------------
// G03-04 要求 3：frame / 连接 / 订阅 / 用户四级速率上限。
// ---------------------------------------------------------------------------

/// Hard ceilings a hub applies to one connection and one user.
///
/// Every field is enforced somewhere (see `mq_hub`): an unread counter would be
/// a config file, not a limit. Defaults are deliberately generous so an
/// existing deployment does not start shedding traffic on upgrade; the ceiling
/// that matters is "bounded", not "this exact number".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Largest frame the hub will even look at. `framing::MAX_FRAME` is the
    /// transport ceiling; this is the per-deployment one (a hub that only serves
    /// text has no business accepting 8 MiB frames).
    pub max_frame_bytes: usize,
    /// Live connections one user may hold on one hub (multi-device fan-out).
    pub max_conns_per_user: usize,
    /// Scopes one connection may subscribe to.
    pub max_subscriptions_per_conn: usize,
    /// Content frames a user may push per minute (token bucket, not a fixed
    /// window: the boundary must not be spammable).
    pub max_msgs_per_user_per_min: u64,
    /// Frames written to one connection that are still awaiting a stored ack.
    pub max_inflight_per_conn: usize,
}

/// Defaults are generous on purpose: tightening them is a deployment decision,
/// and an upgrade must not start shedding live traffic by surprise.
const DEFAULT_CONNS_PER_USER: usize = 8;
const DEFAULT_SUBSCRIPTIONS_PER_CONN: usize = 512;
const DEFAULT_MSGS_PER_USER_PER_MIN: u64 = 600;

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_frame_bytes: crate::framing::MAX_FRAME as usize,
            max_conns_per_user: DEFAULT_CONNS_PER_USER,
            max_subscriptions_per_conn: DEFAULT_SUBSCRIPTIONS_PER_CONN,
            max_msgs_per_user_per_min: DEFAULT_MSGS_PER_USER_PER_MIN,
            max_inflight_per_conn: crate::delivery::MAX_INFLIGHT_PER_CONNECTION,
        }
    }
}

/// Integer token bucket: `capacity` burst, refilled at `per_min` per 60 s.
///
/// The remainder of the division is carried in `TokenBucket::carry` so a slow
/// trickle is not rounded down to zero forever.
#[derive(Clone, Copy, Debug)]
pub struct TokenBucket {
    capacity: u64,
    per_min: u64,
    tokens: u64,
    last_ms: u64,
    carry: u64,
}

const MS_PER_MIN: u128 = 60_000;

impl TokenBucket {
    pub fn new(capacity: u64, per_min: u64, now_ms: u64) -> Self {
        Self {
            capacity: capacity.max(1),
            per_min,
            tokens: capacity.max(1),
            last_ms: now_ms,
            carry: 0,
        }
    }

    fn refill(&mut self, now_ms: u64) {
        let elapsed = now_ms.saturating_sub(self.last_ms) as u128;
        if elapsed == 0 {
            return;
        }
        let scaled = elapsed * self.per_min as u128 + self.carry as u128;
        self.tokens = (self.tokens + (scaled / MS_PER_MIN) as u64).min(self.capacity);
        self.carry = (scaled % MS_PER_MIN) as u64;
        self.last_ms = now_ms;
    }

    /// Take `cost` tokens; false = over the limit, caller drops the frame.
    pub fn spend(&mut self, now_ms: u64, cost: u64) -> bool {
        self.refill(now_ms);
        if self.tokens >= cost {
            self.tokens -= cost;
            return true;
        }
        false
    }

    /// Milliseconds until `cost` tokens could be granted (0 when allowed now).
    pub fn retry_after_ms(&mut self, now_ms: u64, cost: u64) -> u64 {
        self.refill(now_ms);
        let missing = cost.saturating_sub(self.tokens);
        if missing == 0 {
            return 0;
        }
        if self.per_min == 0 {
            // a budget of zero can never be met: report a full window instead of
            // pretending the caller may retry immediately
            return MS_PER_MIN as u64;
        }
        let numerator = missing as u128 * MS_PER_MIN;
        let denominator = self.per_min as u128;
        numerator.div_ceil(denominator).max(1) as u64
    }

    pub fn available(&mut self, now_ms: u64) -> u64 {
        self.refill(now_ms);
        self.tokens
    }
}

/// Beyond this attempt count the doubling would overflow before the cap bites.
const MAX_BACKOFF_SHIFT: u32 = 20;

/// Reconnect backoff with **full jitter** (AWS architecture guidance): a hub
/// restart releases thousands of clients, and an identical schedule from all of
/// them is a second outage. `fraction` is injected so the sequence is testable.
///
/// `attempt` is 1-based. `base_ms << attempt` is the un-jittered ceiling, clamped
/// to `cap_ms`; the result is `fraction * ceiling`.
pub fn jittered_backoff(attempt: u32, base_ms: u64, cap_ms: u64, fraction: f64) -> u64 {
    let shift = attempt.saturating_sub(1).min(MAX_BACKOFF_SHIFT);
    let ceiling = base_ms
        .saturating_mul(1u64.checked_shl(shift).unwrap_or(u64::MAX))
        .min(cap_ms);
    let f = fraction.clamp(0.0, 1.0);
    (ceiling as f64 * f).round() as u64
}

#[cfg(test)]
mod limits_tests {
    use super::*;

    #[test]
    fn bucket_allows_a_burst_then_refills_exactly() {
        let mut b = TokenBucket::new(3, 60, 0);
        for _ in 0..3 {
            assert!(b.spend(0, 1), "burst of 3 is allowed");
        }
        assert!(!b.spend(0, 1), "the fourth in the same instant is not");
        // 60/min = one token per second, no rounding loss
        assert!(b.spend(1_000, 1));
        assert!(!b.spend(1_500, 1), "half a second is not a whole token");
        assert!(b.spend(2_000, 1));
        assert_eq!(b.available(2_000), 0);
    }

    #[test]
    fn bucket_never_grows_past_capacity_and_a_zero_rate_never_refills() {
        let mut b = TokenBucket::new(2, 60, 0);
        assert!(b.spend(10 * 60_000, 1));
        assert_eq!(
            b.available(10 * 60_000),
            1,
            "a long idle does not bank tokens"
        );
        let mut stopped = TokenBucket::new(1, 0, 0);
        assert!(stopped.spend(0, 1));
        assert!(
            !stopped.spend(10 * 60_000, 1),
            "a budget of zero stays shut"
        );
        assert_eq!(stopped.retry_after_ms(0, 1), 60_000, "and says so honestly");
    }

    #[test]
    fn retry_after_is_the_time_to_the_next_grant() {
        let mut b = TokenBucket::new(1, 120, 0);
        assert!(b.spend(0, 1));
        assert_eq!(b.retry_after_ms(0, 1), 500, "120/min = one every 500 ms");
        assert_eq!(b.retry_after_ms(500, 1), 0, "now it is affordable");
    }

    #[test]
    fn backoff_grows_then_caps_and_jitters_within_the_window() {
        // no jitter (fraction 1.0): pure exponential up to the cap
        assert_eq!(jittered_backoff(1, 200, 60_000, 1.0), 200);
        assert_eq!(jittered_backoff(2, 200, 60_000, 1.0), 400);
        assert_eq!(jittered_backoff(4, 200, 60_000, 1.0), 1_600);
        assert_eq!(jittered_backoff(40, 200, 60_000, 1.0), 60_000, "capped");
        // full jitter: the same slot spreads across the whole window
        assert_eq!(jittered_backoff(4, 200, 60_000, 0.0), 0);
        assert_eq!(jittered_backoff(4, 200, 60_000, 0.5), 800);
        // out-of-range fractions are clamped, not trusted
        assert_eq!(jittered_backoff(1, 200, 60_000, 9.0), 200);
        assert_eq!(jittered_backoff(1, 200, 60_000, f64::NAN), 0);
    }

    #[test]
    fn a_reconnect_storm_spreads_across_the_window() {
        // 1_000 clients on the same attempt must not all wake at one instant:
        // an identical schedule after a hub restart is a second outage.
        let mut buckets = std::collections::HashMap::new();
        for i in 0..1_000 {
            let f = ((i as f64) * 0.618_034).fract();
            let ms = jittered_backoff(9, 200, 60_000, f);
            assert!(ms <= 51_200, "never longer than the un-jittered ceiling");
            *buckets.entry(ms / 1_000).or_insert(0usize) += 1;
        }
        assert!(
            buckets.len() > 20,
            "wakeups landed in only {} distinct seconds",
            buckets.len()
        );
        // no single second holds most of the storm
        let busiest = *buckets.values().max().unwrap_or(&0);
        assert!(busiest < 100, "one second absorbed {busiest} wakeups");
    }

    #[test]
    fn defaults_are_generous_but_never_unbounded() {
        let l = Limits::default();
        assert!(l.max_frame_bytes > 0 && l.max_conns_per_user > 0);
        assert!(l.max_subscriptions_per_conn > 0 && l.max_msgs_per_user_per_min > 0);
        assert!(
            l.max_inflight_per_conn >= 16,
            "a window smaller than a page would thrash"
        );
    }
}
