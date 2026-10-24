//! 截止时间。
//!
//! 客户端每个请求都带一个绝对截止时间（unix 毫秒），传输层、重试层、
//! 核心状态机共用同一个值。相对超时（"30 秒"）在重试链上会累积：
//! 一次 3 次重试的调用就变成 90 秒，用户看到的却是"界面卡住"。绝对时间不会。

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 绝对截止时间（unix 毫秒）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Deadline(i64);

impl Deadline {
    /// 用绝对时间构造。
    #[must_use]
    pub fn at(unix_ms: i64) -> Self {
        Self(unix_ms)
    }

    /// 从"现在"起算的预算。
    #[must_use]
    pub fn after(now_ms: i64, budget_ms: u64) -> Self {
        let budget = i64::try_from(budget_ms).unwrap_or(i64::MAX);
        Self(now_ms.saturating_add(budget))
    }

    /// 绝对时间。
    #[must_use]
    pub fn unix_ms(self) -> i64 {
        self.0
    }

    /// 是否已经过期（`now >= deadline`）。
    #[must_use]
    pub fn expired(self, now_ms: i64) -> bool {
        now_ms >= self.0
    }

    /// 还剩多少毫秒；已过期为 0。
    #[must_use]
    pub fn remaining_ms(self, now_ms: i64) -> u64 {
        let left = self.0.saturating_sub(now_ms);
        u64::try_from(left).unwrap_or(0)
    }
}

impl fmt::Display for Deadline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "deadline@{}", self.0)
    }
}

impl Serialize for Deadline {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for Deadline {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        let value = raw
            .parse::<i64>()
            .map_err(|_| serde::de::Error::custom("deadline 必须是十进制毫秒时间戳字符串"))?;
        Ok(Self(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaining_never_negative() {
        let d = Deadline::after(1_000, 500);
        assert_eq!(d.remaining_ms(1_000), 500);
        assert_eq!(d.remaining_ms(1_600), 0);
        assert!(d.expired(1_500));
        assert!(!d.expired(1_499));
    }

    #[test]
    fn json_is_a_string() {
        let d = Deadline::at(1_700_000_000_000);
        assert_eq!(serde_json::to_string(&d).expect("ser"), "\"1700000000000\"");
        assert_eq!(
            serde_json::from_str::<Deadline>("\"1700000000000\"").expect("de"),
            d
        );
    }
}
