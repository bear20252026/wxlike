//! 风控状态的存储抽象。
//!
//! 引擎只依赖 [`RiskStore`]，不关心计数放在进程内还是 Redis。默认实现
//! [`MemoryStore`] 是原来的进程内滑动窗口；多节点部署换用 `gcra` 模块的
//! Redis 实现（feature `redis`）。
//!
//! 存储层只回答"这一下超没超"，不生成 `Verdict`：Challenge 还是 Throttle
//! 由引擎按动作的 captcha 属性决定，reason 文案也归引擎。这样换存储不会
//! 改变对外裁决的形状。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use crate::{Action, Result, RiskError};

/// 一次裁决所需的窗口与两档阈值。由 [`crate::RiskConfig`] 派生，存储只读。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    pub window_ms: u64,
    pub throttle_at: usize,
    pub deny_at: usize,
}

/// 存储层裁决。不含 Challenge 与原因文案——那是引擎的职责。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreVerdict {
    Allow,
    Throttle { retry_after_ms: u64 },
    Deny { retry_after_ms: u64 },
}

/// 风控状态后端。实现必须并发安全；多节点一致性由实现自己保证。
///
/// 语义与引擎一致：`check` 只算不进账（把本次尝试计入裁决），`record`
/// 记一次事件后再算。`count` 是运维/测试用的当前窗口计数。
pub trait RiskStore: Send + Sync {
    fn check(&self, action: Action, key: &str, now_ms: u64, rule: &Rule) -> Result<StoreVerdict>;
    fn record(&self, action: Action, key: &str, now_ms: u64, rule: &Rule) -> Result<StoreVerdict>;
    fn clear(&self, action: Action, key: &str) -> Result<()>;
    fn count(&self, action: Action, key: &str, now_ms: u64, rule: &Rule) -> Result<usize>;
}

/// 进程内滑动窗口（每 `(action, key)` 一条升序时间戳队列）。
///
/// 单节点默认实现；多节点请用 `redis` feature 的 GCRA 后端，否则每个节点
/// 各算各的，全局阈值会变成"节点数 × 阈值"。
pub struct MemoryStore {
    windows: Mutex<Windows>,
}

type Windows = HashMap<(Action, String), VecDeque<u64>>;

impl MemoryStore {
    pub fn new() -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Windows>> {
        self.windows.lock().map_err(|_| RiskError::Poisoned)
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl RiskStore for MemoryStore {
    fn check(&self, action: Action, key: &str, now_ms: u64, rule: &Rule) -> Result<StoreVerdict> {
        let mut guard = self.lock()?;
        let events = guard.entry((action, key.to_string())).or_default();
        evict(events, now_ms, rule.window_ms);
        // +1：本次尝试也要计入，否则"第 N 次触发阈值"会晚一次才生效
        Ok(decide(events, now_ms, rule, 1))
    }

    fn record(&self, action: Action, key: &str, now_ms: u64, rule: &Rule) -> Result<StoreVerdict> {
        let mut guard = self.lock()?;
        let events = guard.entry((action, key.to_string())).or_default();
        evict(events, now_ms, rule.window_ms);
        events.push_back(now_ms);
        Ok(decide(events, now_ms, rule, 0))
    }

    fn clear(&self, action: Action, key: &str) -> Result<()> {
        let mut guard = self.lock()?;
        guard.remove(&(action, key.to_string()));
        Ok(())
    }

    fn count(&self, action: Action, key: &str, now_ms: u64, rule: &Rule) -> Result<usize> {
        let mut guard = self.lock()?;
        let events = guard.entry((action, key.to_string())).or_default();
        evict(events, now_ms, rule.window_ms);
        Ok(events.len())
    }
}

fn decide(events: &VecDeque<u64>, now_ms: u64, rule: &Rule, extra: usize) -> StoreVerdict {
    let n = events.len() + extra;
    let retry_after_ms = retry_after_ms(events, now_ms, rule.window_ms);
    if n >= rule.deny_at {
        StoreVerdict::Deny { retry_after_ms }
    } else if n >= rule.throttle_at {
        StoreVerdict::Throttle { retry_after_ms }
    } else {
        StoreVerdict::Allow
    }
}

fn retry_after_ms(events: &VecDeque<u64>, now_ms: u64, window_ms: u64) -> u64 {
    events
        .front()
        .map(|&oldest| window_ms.saturating_sub(now_ms.saturating_sub(oldest)))
        .unwrap_or(0)
}

fn evict(events: &mut VecDeque<u64>, now_ms: u64, window_ms: u64) {
    while let Some(&oldest) = events.front() {
        if now_ms.saturating_sub(oldest) >= window_ms {
            events.pop_front();
        } else {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule() -> Rule {
        Rule {
            window_ms: 1_000,
            throttle_at: 2,
            deny_at: 4,
        }
    }

    #[test]
    fn check_does_not_mutate_but_counts_the_attempt() {
        let s = MemoryStore::new();
        let r = rule();
        assert_eq!(s.count(Action::Send, "u", 0, &r).unwrap(), 0);
        assert_eq!(
            s.check(Action::Send, "u", 0, &r).unwrap(),
            StoreVerdict::Allow
        );
        assert_eq!(s.count(Action::Send, "u", 0, &r).unwrap(), 0);
    }

    #[test]
    fn record_escalates_throttle_then_deny() {
        let s = MemoryStore::new();
        let r = rule();
        assert_eq!(
            s.record(Action::Send, "u", 0, &r).unwrap(),
            StoreVerdict::Allow
        );
        assert!(matches!(
            s.record(Action::Send, "u", 0, &r).unwrap(),
            StoreVerdict::Throttle { .. }
        ));
        s.record(Action::Send, "u", 0, &r).unwrap();
        assert!(matches!(
            s.record(Action::Send, "u", 0, &r).unwrap(),
            StoreVerdict::Deny { .. }
        ));
    }

    #[test]
    fn clear_resets_only_that_key() {
        let s = MemoryStore::new();
        let r = rule();
        s.record(Action::Send, "u", 0, &r).unwrap();
        s.record(Action::Send, "v", 0, &r).unwrap();
        s.clear(Action::Send, "u").unwrap();
        assert_eq!(s.count(Action::Send, "u", 0, &r).unwrap(), 0);
        assert_eq!(s.count(Action::Send, "v", 0, &r).unwrap(), 1);
    }
}
