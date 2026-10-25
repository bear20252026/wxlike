//! 序列号。
//!
//! 服务端给每个会话一个单调递增的 `seq`，客户端靠它做增量同步与去重。
//! 它必须是 `u64`（现有实现里就是），而 JSON 上必须是**字符串**：
//! `2^53` 之后 JavaScript 的 `number` 不再连续，序列号一旦被舍入，
//! 客户端就会"永远差一条"——表现是刷新之后少一条消息，且无法自愈。

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 会话内的单调序列号，从 1 开始（0 表示"还没有任何消息"）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Seq(u64);

impl Seq {
    /// "还没有任何消息"。
    pub const ZERO: Seq = Seq(0);
    /// 第一条消息的序列号。
    pub const FIRST: Seq = Seq(1);

    /// 直接构造。
    #[must_use]
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// 取出数值。用于比较与服务端交互，**不要**直接写进 JSON。
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }

    /// 是否为零。
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// 下一个序列号；`u64::MAX` 时为 `None`（不是回绕）。
    #[must_use]
    pub fn next(self) -> Option<Seq> {
        self.0.checked_add(1).map(Seq)
    }

    /// 区间是否连续：`self` 之后紧跟 `other`。
    #[must_use]
    pub fn is_adjacent_to(self, other: Seq) -> bool {
        other.0 == self.0.saturating_add(1)
    }
}

impl fmt::Display for Seq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Debug for Seq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Seq({})", self.0)
    }
}

impl Serialize for Seq {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for Seq {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        let value = raw
            .parse::<u64>()
            .map_err(|_| serde::de::Error::custom("seq 必须是十进制无符号整数字符串"))?;
        Ok(Self(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip_beyond_2_pow_53() {
        let seq = Seq::new(9_007_199_254_740_993);
        let text = serde_json::to_string(&seq).expect("ser");
        assert_eq!(text, "\"9007199254740993\"");
        assert_eq!(serde_json::from_str::<Seq>(&text).expect("de"), seq);
    }

    #[test]
    fn numeric_seq_is_rejected() {
        assert!(serde_json::from_str::<Seq>("12").is_err());
    }

    #[test]
    fn next_does_not_wrap() {
        assert_eq!(Seq::new(u64::MAX).next(), None);
        assert_eq!(Seq::FIRST.next(), Some(Seq::new(2)));
        assert!(Seq::FIRST.is_adjacent_to(Seq::new(2)));
        assert!(!Seq::FIRST.is_adjacent_to(Seq::new(3)));
    }
}
