//! 金额与币种。
//!
//! 三条规则，任何一条破了都会变成对不上账的线上问题：
//!
//! 1. **只有整数**。`Money` 内部是最小单位（人民币为分）的 `i64`，没有任何
//!    浮点入口，也没有 `From<f64>`——想传浮点必须先自己决定怎么舍入。
//! 2. **JSON 上是字符串**。`{"minor":"9007199254740993"}`。用数字写的话，
//!    JavaScript 解析时超过 `2^53` 会静默变成 `9007199254740992`，
//!    一笔大额转账就这样少了一分钱，而且没有任何报错。
//! 3. **币种跟着金额走**。`Money` 自带 `currency`，加减前先比币种；
//!    想跨币种运算必须显式换汇，不能靠"反正都是数字"糊过去。

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 金额相关的错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoneyError {
    /// 币种不是 3 位大写字母（ISO 4217 的写法）。
    BadCurrency(String),
    /// 加减溢出 `i64`。
    Overflow,
    /// 两个不同币种做了运算。
    CurrencyMismatch {
        /// 左操作数币种。
        left: Currency,
        /// 右操作数币种。
        right: Currency,
    },
}

impl fmt::Display for MoneyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MoneyError::BadCurrency(raw) => write!(f, "币种非法：{raw:?}（期望 3 位大写字母）"),
            MoneyError::Overflow => f.write_str("金额运算溢出"),
            MoneyError::CurrencyMismatch { left, right } => {
                write!(f, "币种不一致：{left} vs {right}")
            }
        }
    }
}

impl std::error::Error for MoneyError {}

/// 币种：3 位大写字母，例如 `CNY`。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Currency([u8; 3]);

impl Currency {
    /// 人民币。
    pub const CNY: Currency = Currency(*b"CNY");

    /// 解析币种。
    ///
    /// # Errors
    /// 长度不是 3 或含非大写字母时返回 [`MoneyError::BadCurrency`]。
    pub fn parse(raw: &str) -> Result<Self, MoneyError> {
        let bytes = raw.as_bytes();
        let shaped = bytes.len() == 3 && bytes.iter().all(u8::is_ascii_uppercase);
        if !shaped {
            return Err(MoneyError::BadCurrency(raw.to_string()));
        }
        let mut out = [0u8; 3];
        out.copy_from_slice(bytes);
        Ok(Self(out))
    }

    /// 借出文本。
    #[must_use]
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("???")
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Currency({})", self.as_str())
    }
}

impl Serialize for Currency {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Currency {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// 金额：`minor` 是最小单位整数（人民币为分）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Money {
    minor: i64,
    currency: Currency,
}

impl Money {
    /// 用最小单位整数构造。
    #[must_use]
    pub fn from_minor(minor: i64, currency: Currency) -> Self {
        Self { minor, currency }
    }

    /// 人民币金额（单位：分）。
    #[must_use]
    pub fn cny_minor(minor: i64) -> Self {
        Self::from_minor(minor, Currency::CNY)
    }

    /// 最小单位整数。
    #[must_use]
    pub fn minor(&self) -> i64 {
        self.minor
    }

    /// 币种。
    #[must_use]
    pub fn currency(&self) -> Currency {
        self.currency
    }

    /// 是否为零。
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.minor == 0
    }

    /// 是否为正。
    #[must_use]
    pub fn is_positive(&self) -> bool {
        self.minor > 0
    }

    /// 加法，币种必须一致。
    ///
    /// # Errors
    /// 币种不一致或溢出。
    pub fn checked_add(self, other: Self) -> Result<Self, MoneyError> {
        self.same_currency(other)?;
        let minor = self
            .minor
            .checked_add(other.minor)
            .ok_or(MoneyError::Overflow)?;
        Ok(Self {
            minor,
            currency: self.currency,
        })
    }

    /// 减法，币种必须一致。
    ///
    /// # Errors
    /// 币种不一致或溢出。
    pub fn checked_sub(self, other: Self) -> Result<Self, MoneyError> {
        self.same_currency(other)?;
        let minor = self
            .minor
            .checked_sub(other.minor)
            .ok_or(MoneyError::Overflow)?;
        Ok(Self {
            minor,
            currency: self.currency,
        })
    }

    fn same_currency(self, other: Self) -> Result<(), MoneyError> {
        if self.currency == other.currency {
            return Ok(());
        }
        Err(MoneyError::CurrencyMismatch {
            left: self.currency,
            right: other.currency,
        })
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.minor, self.currency)
    }
}

impl fmt::Debug for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Money({} {})", self.minor, self.currency)
    }
}

impl Serialize for Money {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut out = s.serialize_struct("Money", 2)?;
        out.serialize_field("minor", &self.minor.to_string())?;
        out.serialize_field("currency", &self.currency)?;
        out.end()
    }
}

/// 反序列化用的中转结构：`minor` 收字符串。
#[derive(Deserialize)]
struct MoneyWire {
    minor: String,
    currency: Currency,
}

impl<'de> Deserialize<'de> for Money {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let wire = MoneyWire::deserialize(d)?;
        let minor = wire
            .minor
            .parse::<i64>()
            .map_err(|_| serde::de::Error::custom("money.minor 必须是十进制整数字符串"))?;
        Ok(Self {
            minor,
            currency: wire.currency,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_currency() {
        assert!(Currency::parse("cny").is_err());
        assert!(Currency::parse("CN").is_err());
        assert_eq!(Currency::parse("CNY"), Ok(Currency::CNY));
    }

    #[test]
    fn mismatch_is_an_error_not_a_silent_sum() {
        let cny = Money::cny_minor(100);
        let usd = Money::from_minor(100, Currency::parse("USD").expect("usd"));
        assert!(matches!(
            cny.checked_add(usd),
            Err(MoneyError::CurrencyMismatch { .. })
        ));
    }

    #[test]
    fn overflow_is_caught() {
        let big = Money::cny_minor(i64::MAX);
        assert_eq!(
            big.checked_add(Money::cny_minor(1)),
            Err(MoneyError::Overflow)
        );
    }

    #[test]
    fn json_keeps_full_i64_precision() {
        // 2^53 + 1：JavaScript 的 number 到这里已经丢精度
        let big = Money::cny_minor(9_007_199_254_740_993);
        let text = serde_json::to_string(&big).expect("ser");
        assert_eq!(text, r#"{"minor":"9007199254740993","currency":"CNY"}"#);
        let back: Money = serde_json::from_str(&text).expect("de");
        assert_eq!(back, big);
    }

    #[test]
    fn numeric_minor_is_rejected_so_js_cannot_round_it() {
        // 用数字传 = 调用方在 JS 里已经丢过精度了，宁可拒绝
        let err = serde_json::from_str::<Money>(r#"{"minor":1,"currency":"CNY"}"#);
        assert!(err.is_err());
    }
}
