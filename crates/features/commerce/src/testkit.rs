//! 测试/演示用的端口实现。
//!
//! 这些不是生产路径：`StaticMembership` 是 G02 合并前的冻结 fixture，
//! `ManualClock`/`FixedEntropy` 只用于把过期与并发边界变成确定性用例。

use std::collections::HashSet;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use sha2::{Digest as _, Sha256};

use crate::error::Result;
use crate::ports::{Clock, Entropy, GroupMembership};

/// 可手动推进的时钟。
#[derive(Clone, Debug)]
pub struct ManualClock(Arc<AtomicI64>);

impl ManualClock {
    /// 从指定毫秒开始。
    #[must_use]
    pub fn new(now_ms: i64) -> Self {
        Self(Arc::new(AtomicI64::new(now_ms)))
    }

    /// 前进指定毫秒。
    pub fn advance(&self, delta_ms: i64) {
        self.0.fetch_add(delta_ms, Ordering::SeqCst);
    }

    /// 设为指定毫秒。
    pub fn set(&self, now_ms: i64) {
        self.0.store(now_ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// 确定性熵源：每次调用输出 `SHA-256(counter)`，便于复现红包分配。
#[derive(Clone, Debug, Default)]
pub struct FixedEntropy(Arc<AtomicU64>);

impl Entropy for FixedEntropy {
    fn fill(&self, out: &mut [u8]) {
        let counter = self.0.fetch_add(1, Ordering::SeqCst);
        let mut seed = counter.to_be_bytes().to_vec();
        while out.len() > seed.len() {
            seed.extend_from_slice(&Sha256::digest(&seed));
        }
        let digest = Sha256::digest(&seed);
        out.copy_from_slice(&digest[..out.len()]);
    }
}

/// 冻结的成员快照。
#[derive(Clone, Debug, Default)]
pub struct StaticMembership {
    rows: Arc<Mutex<HashSet<(String, String)>>>,
}

impl StaticMembership {
    /// 空快照（默认拒绝）。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 加入一个成员。
    pub fn add(&self, conversation: &str, user: &str) {
        self.rows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((conversation.to_string(), user.to_string()));
    }

    /// 移除一个成员。
    pub fn remove(&self, conversation: &str, user: &str) {
        self.rows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&(conversation.to_string(), user.to_string()));
    }
}

impl GroupMembership for StaticMembership {
    fn is_member(&self, conversation: &str, user: &str) -> Result<bool> {
        Ok(self
            .rows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&(conversation.to_string(), user.to_string())))
    }
}
