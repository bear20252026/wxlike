//! 账本的公开数据类型。
//!
//! 金额与币种不在本模块定义：统一复用共享内核 `wxlike-types` 的
//! `Money`/`Currency`（见 crate 根的再导出），账本、commerce 与三端
//! 客户端共用同一份实现，跨 crate 传金额不再需要转换层。

use wxlike_types::{Currency, Money};

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
