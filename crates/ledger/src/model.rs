//! 账本的公开数据类型。
//!
//! 金额只有一种表示：`i64` 最小单位 + 显式币种。没有浮点入口，也没有
//! "先加完再说" 的跨币种运算。JSON 边界把 `minor` 写成字符串，避免
//! JavaScript 超过 2^53 后静默丢精度。

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 币种：3 位大写字母（ISO 4217 的写法）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Currency([u8; 3]);

impl Currency {
    /// 人民币。
    pub const CNY: Currency = Currency(*b"CNY");

    /// 解析币种。
    ///
    /// # Errors
    /// 长度不是 3 或含非大写字母时返回 [`crate::LedgerError::BadCurrency`]。
    pub fn parse(raw: &str) -> crate::Result<Self> {
        let bytes = raw.as_bytes();
        let shaped = bytes.len() == 3 && bytes.iter().all(u8::is_ascii_uppercase);
        if !shaped {
            return Err(crate::LedgerError::BadCurrency(raw.to_string()));
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
    /// 币种不一致或 `i64` 溢出。
    pub fn checked_add(self, other: Self) -> crate::Result<Self> {
        self.same_currency(other)?;
        let minor = self
            .minor
            .checked_add(other.minor)
            .ok_or(crate::LedgerError::Overflow)?;
        Ok(Self {
            minor,
            currency: self.currency,
        })
    }

    /// 减法，币种必须一致。
    ///
    /// # Errors
    /// 币种不一致或 `i64` 溢出。
    pub fn checked_sub(self, other: Self) -> crate::Result<Self> {
        self.same_currency(other)?;
        let minor = self
            .minor
            .checked_sub(other.minor)
            .ok_or(crate::LedgerError::Overflow)?;
        Ok(Self {
            minor,
            currency: self.currency,
        })
    }

    fn same_currency(self, other: Self) -> crate::Result<()> {
        if self.currency == other.currency {
            return Ok(());
        }
        Err(crate::LedgerError::CurrencyMismatch {
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

/// 账户类别。前四类来自 ADR-18；`Escrow`/`Clearing` 是交易域用的
/// 资金在途与外部清算账户——它们和钱包一样不允许透支。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountType {
    /// 用户钱包（资产类）。
    Wallet,
    /// 广告主账户。
    Advertiser,
    /// 营销账户。
    Marketing,
    /// 收入账户。
    Revenue,
    /// 交易托管（红包、待确认转账）。
    Escrow,
    /// 供应商清算/在途账户。
    Clearing,
}

impl AccountType {
    /// 数据库里的稳定字符串。
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Wallet => "wallet",
            Self::Advertiser => "advertiser",
            Self::Marketing => "marketing",
            Self::Revenue => "revenue",
            Self::Escrow => "escrow",
            Self::Clearing => "clearing",
        }
    }

    /// 解析数据库字符串。
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "wallet" => Some(Self::Wallet),
            "advertiser" => Some(Self::Advertiser),
            "marketing" => Some(Self::Marketing),
            "revenue" => Some(Self::Revenue),
            "escrow" => Some(Self::Escrow),
            "clearing" => Some(Self::Clearing),
            _ => None,
        }
    }

    /// 资产类账户不允许负余额；内部账户可以（复式记账的正常形态）。
    #[must_use]
    pub fn is_asset(self) -> bool {
        matches!(self, Self::Wallet | Self::Escrow | Self::Clearing)
    }
}

/// 资金性质：演示资金与真实资金永不混记。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FundClass {
    /// 演示/沙箱资金。
    Virtual,
    /// 真实资金（只在持牌通道接通、生产 profile 下使用）。
    Real,
}

impl FundClass {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Virtual => "virtual",
            Self::Real => "real",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "virtual" => Some(Self::Virtual),
            "real" => Some(Self::Real),
            _ => None,
        }
    }
}

/// 账户开户参数。同名账户的类别、币种、资金性质与属主必须完全一致。
#[derive(Clone, Debug)]
pub struct AccountSpec {
    pub name: String,
    pub kind: AccountType,
    pub currency: Currency,
    pub fund_class: FundClass,
    pub owner_id: String,
}

impl AccountSpec {
    /// 开户（默认虚拟资金、无属主）。
    #[must_use]
    pub fn new(name: impl Into<String>, kind: AccountType, currency: Currency) -> Self {
        Self {
            name: name.into(),
            kind,
            currency,
            fund_class: FundClass::Virtual,
            owner_id: String::new(),
        }
    }

    /// 指定属主。
    #[must_use]
    pub fn with_owner(mut self, owner: impl Into<String>) -> Self {
        self.owner_id = owner.into();
        self
    }

    /// 指定资金性质。
    #[must_use]
    pub fn with_fund_class(mut self, fund_class: FundClass) -> Self {
        self.fund_class = fund_class;
        self
    }
}

/// 账户元数据。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub kind: AccountType,
    pub currency: Currency,
    pub fund_class: FundClass,
    pub owner_id: String,
    pub created_at: i64,
}

/// 一条分录。正数为贷记（资产增加），负数为借记。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: i64,
    pub journal_id: String,
    pub seq: i64,
    pub account: String,
    pub amount: i64,
    pub currency: Currency,
    pub memo: String,
    pub created_at: i64,
}

/// 一笔已入账交易的头。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Journal {
    pub id: String,
    pub business_key: String,
    pub kind: String,
    pub currency: Currency,
    pub created_at: i64,
}

/// 入账结果：头 + 全部分录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerTransaction {
    pub id: String,
    pub business_key: String,
    pub kind: String,
    pub currency: Currency,
    pub created_at: i64,
    pub entries: Vec<Entry>,
}

/// 记账请求的一条分录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostingLine {
    pub account: String,
    pub amount: i64,
}

impl PostingLine {
    /// 借记（资产减少）。
    #[must_use]
    pub fn debit(account: impl Into<String>, minor: i64) -> Self {
        Self {
            account: account.into(),
            amount: -minor,
        }
    }

    /// 贷记（资产增加）。
    #[must_use]
    pub fn credit(account: impl Into<String>, minor: i64) -> Self {
        Self {
            account: account.into(),
            amount: minor,
        }
    }
}

/// 一次记账。同一 `business_key` 只能对应同一组参数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostingRequest {
    pub business_key: String,
    pub kind: String,
    pub currency: Currency,
    pub lines: Vec<PostingLine>,
    pub memo: String,
}

impl PostingRequest {
    /// 构造记账请求。
    #[must_use]
    pub fn new(
        business_key: impl Into<String>,
        kind: impl Into<String>,
        currency: Currency,
        lines: Vec<PostingLine>,
    ) -> Self {
        Self {
            business_key: business_key.into(),
            kind: kind.into(),
            currency,
            lines,
            memo: String::new(),
        }
    }

    /// 附言。
    #[must_use]
    pub fn with_memo(mut self, memo: impl Into<String>) -> Self {
        self.memo = memo.into();
        self
    }
}

/// 锁额状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoldStatus {
    /// 资金已冻结、尚未使用。
    Held,
    /// 已转成正式分录。
    Captured,
    /// 已解冻退回可用余额。
    Released,
}

impl HoldStatus {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Captured => "captured",
            Self::Released => "released",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "held" => Some(Self::Held),
            "captured" => Some(Self::Captured),
            "released" => Some(Self::Released),
            _ => None,
        }
    }
}

/// 锁额记录。锁额不是分录，但会从 `available` 里扣除。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hold {
    pub business_key: String,
    pub account: String,
    pub amount: Money,
    pub status: HoldStatus,
    pub journal_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 流水分页。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementPage {
    pub items: Vec<Entry>,
    pub next_cursor: Option<i64>,
    pub has_more: bool,
    pub currency: Currency,
}

/// 日结报告（不可变）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosingReport {
    pub day: String,
    pub currency: Currency,
    pub start_ms: i64,
    pub end_ms: i64,
    /// 区间内借方合计（正数）。
    pub debits: i64,
    /// 区间内贷方合计（正数）。
    pub credits: i64,
    pub entry_count: i64,
    /// 日终仍在冻结中的金额。
    pub locked: i64,
    /// 日终全部账户余额之和；复式账本下必须为 0。
    pub balance_total: i64,
    pub checksum: String,
    pub closed_at: i64,
}

/// 日结时的账户余额快照。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosingBalance {
    pub account: String,
    pub balance_minor: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_is_integer_and_currency_typed() {
        let a = Money::cny_minor(100);
        let b = Money::from_minor(100, Currency::parse("USD").expect("usd"));
        assert!(matches!(
            a.checked_add(b),
            Err(crate::LedgerError::CurrencyMismatch { .. })
        ));
        assert_eq!(
            a.checked_add(Money::cny_minor(1)).expect("add").minor(),
            101
        );
    }

    #[test]
    fn money_json_keeps_i64_precision() {
        let big = Money::cny_minor(9_007_199_254_740_993);
        let text = serde_json::to_string(&big).expect("ser");
        assert_eq!(text, r#"{"minor":"9007199254740993","currency":"CNY"}"#);
        let back: Money = serde_json::from_str(&text).expect("de");
        assert_eq!(back, big);
        assert!(serde_json::from_str::<Money>(r#"{"minor":1,"currency":"CNY"}"#).is_err());
    }

    #[test]
    fn currency_rejects_lowercase_and_short_codes() {
        assert!(Currency::parse("cny").is_err());
        assert!(Currency::parse("CN").is_err());
        assert_eq!(Currency::parse("CNY").expect("cny"), Currency::CNY);
    }
}
