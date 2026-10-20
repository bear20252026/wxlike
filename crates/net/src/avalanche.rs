//! 客户端防雪崩：频率漏斗 + 流量漏斗，且**只在非活跃态生效**。
//!
//! 参照 `refs/mars/mars/stn/src/anti_avalanche.{h,cc}` + `frequency_limit.cc` +
//! `flow_limit.cc`（腾讯 Mars，MIT）。它要挡的东西很具体：弱网下客户端自己
//! 把自己淹死——重试风暴、轮询风暴，每一条都"合理"，加起来把流量打光、
//! 把连接彻底堵死。
//!
//! # 两个漏斗
//!
//! * **频率漏斗**：同一个键在滑动窗口内的次数上限（`freq_max` / `freq_window_ms`）。
//! * **流量漏斗**：漏桶。字节数按 `flow_rate_bps` 匀速排空，桶容量 `flow_burst_bytes`。
//!   一次性超过桶容量直接拒——大包在弱网下会占满整条连接。
//!
//! # 与 Mars 的三处偏离
//!
//! 1. **键由调用方给**（命令 id / 会话 id），不是 `adler32(请求体)`。
//!    Mars 按请求体哈希分组，好处是"完全相同的重试"落一个桶，坏处是
//!    "同一条命令不同参数"完全不受约束。我们按语义分组，调用方知道得更清楚。
//! 2. **窗口是真滑动窗口**，不是"计数 + 每小时清理"。Mars 是计数到 105 才拦、
//!    10 分钟内的记录才清，实际效果是"10 分钟内同请求体 105 次"。我们用
//!    `VecDeque` 存时间戳，语义可预测、可测。
//! 3. **活跃态完全旁路，且不消耗额度**。Mars 的 `Active()` 也是直接返回 true，
//!    但它的计数在旁路期间仍会走别的分支；我们明确：旁路期间**不记账**，
//!    否则用户连续点 20 下之后再自动重试就被限了——那不是用户想表达的意思。
//!
//! # 为什么"活跃态"必须旁路
//!
//! 用户手点发送，就是明确表达"我现在要发"。限流是给**机器行为**（重试、轮询、
//! agent 循环）准备的，不是给手指准备的。把两者混在一起，用户会觉得"这软件卡"。

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};

/// 一个漏斗的配置。
#[derive(Debug, Clone, Copy)]
pub struct AvalancheConfig {
    /// 频率窗口长度
    pub freq_window_ms: u64,
    /// 窗口内同一键允许的次数
    pub freq_max: u32,
    /// 漏桶排空速率（字节/秒）
    pub flow_rate_bps: u64,
    /// 漏桶容量（字节）。单次超过它的包直接拒。
    pub flow_burst_bytes: u64,
}

impl AvalancheConfig {
    /// 控制面（命令面）：一轮 agent 循环里同一条命令最多 60 次/分钟，
    /// 流量 128 KB/s、突发 512 KB。控制面的包都很小，这个额度很宽松。
    pub const fn control_plane() -> Self {
        Self {
            freq_window_ms: 60_000,
            freq_max: 60,
            flow_rate_bps: 128 * 1024,
            flow_burst_bytes: 512 * 1024,
        }
    }

    /// 消息面（QUIC 发送 / outbox 重发）：同一会话 30 条/分钟，
    /// 流量 32 KB/s、突发 256 KB。弱网下 32 KB/s 已经够发文字与缩略图。
    pub const fn messages() -> Self {
        Self {
            freq_window_ms: 60_000,
            freq_max: 30,
            flow_rate_bps: 32 * 1024,
            flow_burst_bytes: 256 * 1024,
        }
    }
}

/// 被挡下的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// 同一个键在窗口内次数超限
    Frequency,
    /// 漏桶满了
    Flow,
}

impl DenyReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Frequency => "frequency",
            Self::Flow => "flow",
        }
    }
}

/// 一次检查的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny {
        reason: DenyReason,
        /// 建议多久后再试（毫秒）。给调用方排下一轮用，不要让它自己猜。
        retry_after_ms: u64,
    },
}

impl Verdict {
    pub fn is_allowed(self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// 追踪的键上限。超过就淘汰最久没活动的那个——漏斗自己不能变成内存泄漏。
const MAX_KEYS: usize = 256;

#[derive(Default)]
struct Inner {
    /// 键 → 窗口内的时间戳
    hits: HashMap<String, VecDeque<i64>>,
    /// 键 → 最后一次活动时间（淘汰用）
    last_seen: HashMap<String, i64>,
    /// 漏桶当前体积（字节）
    volume: i64,
    /// 上次排空时间
    drained_at: i64,
    allowed: u64,
    denied: u64,
}

pub struct Avalanche {
    cfg: AvalancheConfig,
    inner: Mutex<Inner>,
    /// 用户正在操作：全部放行，且不记账
    active: std::sync::atomic::AtomicBool,
}

impl Avalanche {
    pub fn new(cfg: AvalancheConfig) -> Self {
        Self {
            cfg,
            inner: Mutex::new(Inner::default()),
            active: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn config(&self) -> AvalancheConfig {
        self.cfg
    }

    /// 用户开始/结束主动操作。活跃期间 `check` 恒为 `Allow`。
    pub fn set_active(&self, active: bool) {
        self.active
            .store(active, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn is_active(&self) -> bool {
        self.active.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// 检查一次请求。**通过即记账**（频率与流量都记），所以不要"先检查再决定发不发"
    /// 之后又重复检查一次。
    pub fn check(&self, key: &str, bytes: usize, now_ms: i64) -> Verdict {
        if self.is_active() {
            // 旁路：不记账。用户连点 20 下之后，自动重试仍应拿到完整额度。
            return Verdict::Allow;
        }

        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.drained_at = if inner.drained_at == 0 {
            now_ms
        } else {
            inner.drained_at
        };

        // ---- 流量漏斗先算：它决定了"这个包能不能进去" ----
        drain(&mut inner, now_ms, self.cfg.flow_rate_bps);
        let want = inner.volume + bytes as i64;
        if want > self.cfg.flow_burst_bytes as i64 {
            inner.denied += 1;
            let over = want - self.cfg.flow_burst_bytes as i64;
            let wait_ms = if self.cfg.flow_rate_bps == 0 {
                u64::MAX
            } else {
                (over as u64 * 1000).div_ceil(self.cfg.flow_rate_bps)
            };
            return Verdict::Deny {
                reason: DenyReason::Flow,
                retry_after_ms: wait_ms,
            };
        }

        // ---- 频率漏斗 ----
        let window_start = now_ms - self.cfg.freq_window_ms as i64;
        let hits = inner.hits.entry(key.to_string()).or_default();
        while hits.front().is_some_and(|t| *t <= window_start) {
            hits.pop_front();
        }
        if hits.len() as u32 >= self.cfg.freq_max {
            let oldest = *hits.front().expect("非空：len >= freq_max > 0");
            let wait = (oldest + self.cfg.freq_window_ms as i64 - now_ms).max(0) as u64;
            inner.denied += 1;
            return Verdict::Deny {
                reason: DenyReason::Frequency,
                retry_after_ms: wait,
            };
        }

        // 两个漏斗都过：一起记账
        hits.push_back(now_ms);
        inner.volume = want;
        inner.last_seen.insert(key.to_string(), now_ms);
        inner.allowed += 1;
        evict_if_needed(&mut inner);
        Verdict::Allow
    }

    /// `(放行数, 拒绝数)`，给 UI / 日志看。
    pub fn stats(&self) -> (u64, u64) {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        (inner.allowed, inner.denied)
    }

    /// 当前漏桶体积（字节），测试与观测用。
    pub fn volume(&self, now_ms: i64) -> i64 {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        drain(&mut inner, now_ms, self.cfg.flow_rate_bps);
        inner.volume
    }
}

fn drain(inner: &mut Inner, now_ms: i64, rate_bps: u64) {
    let dt = (now_ms - inner.drained_at).max(0) as u64;
    if dt == 0 {
        return;
    }
    let gone = (dt * rate_bps / 1000) as i64;
    inner.volume = (inner.volume - gone).max(0);
    inner.drained_at = now_ms;
}

fn evict_if_needed(inner: &mut Inner) {
    if inner.hits.len() <= MAX_KEYS {
        return;
    }
    // 淘汰最久没活动的键
    if let Some(victim) = inner
        .last_seen
        .iter()
        .min_by_key(|(_, t)| **t)
        .map(|(k, _)| k.clone())
    {
        inner.hits.remove(&victim);
        inner.last_seen.remove(&victim);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AvalancheConfig {
        AvalancheConfig {
            freq_window_ms: 1_000,
            freq_max: 3,
            flow_rate_bps: 1_000,    // 1 KB/s
            flow_burst_bytes: 4_000, // 4 KB
        }
    }

    #[test]
    fn frequency_caps_a_single_key_then_recovers_after_the_window() {
        let a = Avalanche::new(cfg());
        for i in 0..3 {
            assert!(
                a.check("conv.a", 10, 1_000 + i).is_allowed(),
                "第 {} 次应当放行",
                i + 1
            );
        }
        let v = a.check("conv.a", 10, 1_100);
        match v {
            Verdict::Deny {
                reason,
                retry_after_ms,
            } => {
                assert_eq!(reason, DenyReason::Frequency);
                // 最早那次在 t=1000，窗口 1000ms，所以 1100 时还要等 900ms
                assert_eq!(retry_after_ms, 900);
            }
            Verdict::Allow => panic!("第 4 次必须被频率漏斗挡下"),
        }
        // 窗口滑过去后恢复
        assert!(a.check("conv.a", 10, 2_001).is_allowed());
    }

    #[test]
    fn different_keys_do_not_share_the_frequency_budget() {
        let a = Avalanche::new(cfg());
        for i in 0..3 {
            assert!(a.check("a", 1, 1_000 + i).is_allowed());
        }
        assert!(!a.check("a", 1, 1_010).is_allowed(), "a 已满");
        assert!(a.check("b", 1, 1_010).is_allowed(), "b 不受影响");
    }

    #[test]
    fn flow_funnel_denies_a_burst_and_drains_over_time() {
        let a = Avalanche::new(cfg());
        // 桶 4 KB：一次 3 KB 可以，再一次 3 KB 就超了
        assert!(a.check("k1", 3_000, 1_000).is_allowed());
        match a.check("k2", 3_000, 1_000) {
            Verdict::Deny {
                reason,
                retry_after_ms,
            } => {
                assert_eq!(reason, DenyReason::Flow);
                // 需要排掉 2000 字节，1 KB/s → 2000ms
                assert_eq!(retry_after_ms, 2_000);
            }
            Verdict::Allow => panic!("应当被流量漏斗挡下"),
        }
        // 2 秒后桶里只剩 1 KB，再放 3 KB 刚好 4 KB
        assert!(a.check("k2", 3_000, 3_000).is_allowed());
    }

    #[test]
    fn a_single_oversized_payload_is_refused_outright() {
        let a = Avalanche::new(cfg());
        match a.check("big", 5_000, 1_000) {
            Verdict::Deny { reason, .. } => assert_eq!(reason, DenyReason::Flow),
            Verdict::Allow => panic!("超过桶容量必须直接拒"),
        }
    }

    /// 用户主动操作期间全放行，且**不消耗额度**——
    /// 否则连点 20 下之后再自动重试就被限了，那不是用户的意思。
    #[test]
    fn active_bypasses_everything_and_does_not_spend_budget() {
        let a = Avalanche::new(cfg());
        a.set_active(true);
        for i in 0..50 {
            assert!(a.check("k", 10_000, 1_000 + i).is_allowed(), "活跃态全放行");
        }
        assert_eq!(a.volume(1_050), 0, "旁路期间不该记账");

        a.set_active(false);
        // 额度仍是满的：连续 3 次（freq_max）都能过
        for i in 0..3 {
            assert!(a.check("k", 10, 2_000 + i).is_allowed());
        }
        assert!(
            !a.check("k", 10, 2_010).is_allowed(),
            "旁路结束后按正常规则算"
        );
    }

    #[test]
    fn denied_requests_do_not_consume_quota() {
        let a = Avalanche::new(cfg());
        assert!(a.check("k", 3_000, 1_000).is_allowed());
        // 被流量漏斗拒的那次不该把体积推上去
        let before = a.volume(1_000);
        let _ = a.check("k", 3_000, 1_000);
        assert_eq!(a.volume(1_000), before, "被拒的请求不记账");
    }

    #[test]
    fn stats_count_both_outcomes() {
        let a = Avalanche::new(cfg());
        let _ = a.check("k", 10, 1_000);
        let _ = a.check("k", 10, 1_001);
        for _ in 0..2 {
            let _ = a.check("k", 10, 1_002);
        }
        let (allowed, denied) = a.stats();
        assert_eq!((allowed, denied), (3, 1));
    }

    /// 漏斗自己不能变成内存泄漏：键超过上限就淘汰最久没活动的。
    #[test]
    fn key_table_is_bounded() {
        let a = Avalanche::new(AvalancheConfig {
            freq_window_ms: 60_000,
            freq_max: 100,
            flow_rate_bps: 1_000_000,
            flow_burst_bytes: 1_000_000,
        });
        for i in 0..(MAX_KEYS * 2) {
            assert!(a.check(&format!("k{i}"), 1, 1_000 + i as i64).is_allowed());
        }
        let inner = a.inner.lock().unwrap();
        assert!(
            inner.hits.len() <= MAX_KEYS,
            "键表必须被限制在 {MAX_KEYS}，实际 {}",
            inner.hits.len()
        );
    }
}
