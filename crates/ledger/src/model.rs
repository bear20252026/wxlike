//! 账本的公开数据类型。
//!
//! 金额与币种不在本模块定义：统一复用共享内核 `wxlike-types` 的
//! `Money`/`Currency`（见 crate 根的再导出），账本、commerce 与三端
//! 客户端共用同一份实现，跨 crate 传金额不再需要转换层。
//!
//! [`Asset`] 与 [`Metadata`] 是从 Formance Ledger 借来的两个形状：
//! * typed asset（`USD/2`）——资产标识必须带最小单位精度，且只有规范形式
//!   才被接受，`USD/0`、`USD/02`、`USD/2/3` 这类"换个写法指向同一个格子"
//!   的非规范后缀一律拒绝；
//! * metadata——键值对要参与幂等摘要，所以必须先规范化（按键升序、无重复
//!   键）再受大小约束。
//! 借用关系与"没抄什么"逐条记在
//! `third_party/reviews/patches/UP-FORMANCE-*.md`。

use wxlike_types::{Currency, Money};

use crate::{LedgerError, Result};

/// `Money` 的最小单位尺度：1 元 = 100 分，所以规范资产标识的精度恒为 2。
///
/// 这里不做"每种币种不同小数位"的花活：`Money` 只有一个 `i64 minor`，
/// 精度写 1 或 3 就是把同一笔钱换了一个单位记，正是 Formance 拒绝
/// 非规范精度后缀要防的那类问题。
pub const ASSET_PRECISION: u8 = 2;

/// 资产标识上限（Formance 的 `maxColorLength` 同为 32 字节）。
const ASSET_MAX_LEN: usize = 32;

/// 带精度的资产标识：`CNY` 或 `CNY/2`。
///
/// 与 [`Currency`] 的分工：`Currency` 是钱种，`Asset` 是"这笔钱按什么
/// 精度入账"。二者的幂等语义绑在一起——同一 `business_key` 换了资产精度
/// 就是换了参数，必须拒绝。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Asset {
    currency: Currency,
    precision: u8,
}

impl Asset {
    /// 规范资产（精度 = [`ASSET_PRECISION`]）。
    #[must_use]
    pub fn new(currency: Currency) -> Self {
        Self {
            currency,
            precision: ASSET_PRECISION,
        }
    }

    /// 解析并规范化。
    ///
    /// 接受的形态只有两种：`BASE` 与 `BASE/2`。其余（大小写、长度、精度
    /// 非 2、前导零、多个 `/`）全部拒绝，理由是它们会指向与规范形式不同的
    /// 余额格或不同的金额单位。
    ///
    /// # Errors
    /// [`LedgerError::BadAsset`]：不是上述规范形态之一。
    pub fn parse(raw: &str) -> Result<Self> {
        if raw.is_empty() || raw.len() > ASSET_MAX_LEN {
            return Err(LedgerError::BadAsset(raw.to_string()));
        }
        let (base, precision) = match raw.split_once('/') {
            Some((base, suffix)) => {
                let bytes = suffix.as_bytes();
                let shaped = bytes.len() == 1 && bytes[0].is_ascii_digit();
                if !shaped {
                    return Err(LedgerError::BadAsset(raw.to_string()));
                }
                // 单字符数字，parse 不会失败。
                (base, bytes[0] - b'0')
            }
            None => (raw, ASSET_PRECISION),
        };
        let currency = Currency::parse(base).map_err(|_| LedgerError::BadAsset(raw.to_string()))?;
        if precision != ASSET_PRECISION {
            return Err(LedgerError::BadAsset(raw.to_string()));
        }
        Ok(Self {
            currency,
            precision,
        })
    }

    /// 币种。
    #[must_use]
    pub const fn currency(&self) -> Currency {
        self.currency
    }

    /// 最小单位精度。
    #[must_use]
    pub const fn precision(&self) -> u8 {
        self.precision
    }

    /// 规范文本形态（幂等摘要与落库都用这个形态，不用调用方原样字符串）。
    #[must_use]
    pub fn canonical(&self) -> String {
        format!("{}/{}", self.currency, self.precision)
    }
}

/// 单笔记账允许的最大 metadata 条数（Formance `DefaultMetadataMaxEntriesPerEntity`）。
pub const MAX_METADATA_ENTRIES: usize = 128;

/// metadata 键的字节上限（Formance `DefaultMetadataMaxKeyBytes`）。
pub const MAX_METADATA_KEY_BYTES: usize = 256;

/// metadata 值的字节上限（Formance `DefaultMetadataMaxValueBytes` = 16 KiB）。
pub const MAX_METADATA_VALUE_BYTES: usize = 16 * 1024;

/// 单笔记账 metadata 的总字节上限（Formance `DefaultMetadataMaxEntityBytes` = 64 KiB）。
pub const MAX_METADATA_TOTAL_BYTES: usize = 64 * 1024;

/// 记账的键值附加信息。
///
/// 内部永远保持"按 key 升序、key 唯一"的规范形态：metadata 要进幂等摘要，
/// 插入顺序不同的同一组键值必须算出同一个摘要，否则 `business_key` 重放
/// 会被误判成参数变更。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    entries: Vec<(String, String)>,
}

impl Metadata {
    /// 空集。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 条数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 按键取值。
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// 规范顺序的迭代器。
    pub fn iter(&self) -> std::slice::Iter<'_, (String, String)> {
        self.entries.iter()
    }

    /// 从任意顺序的键值对构造：排序 + 去重校验 + 大小校验。
    ///
    /// # Errors
    /// 键或值非法、重复键、超出条数或总字节上限。
    pub fn from_pairs(
        pairs: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self> {
        let mut entries: Vec<(String, String)> = pairs.into_iter().collect();
        for (key, value) in &entries {
            validate_metadata_key(key)?;
            validate_metadata_value(key, value)?;
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        // 排序后重复键必然相邻；同键同值也只当一次输入错误，不静默合并。
        if entries.windows(2).any(|w| w[0].0 == w[1].0) {
            let dup = entries
                .windows(2)
                .find(|w| w[0].0 == w[1].0)
                .map(|w| w[0].0.clone())
                .unwrap_or_default();
            return Err(LedgerError::DuplicateMetadataKey(dup));
        }
        check_metadata_budget(&entries)?;
        Ok(Self { entries })
    }

    /// 参与幂等摘要的规范化片段：一行一条 `key|value`。
    pub fn digest_parts(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|(key, value)| format!("m{key}|{value}"))
            .collect()
    }

    /// 落库文本：`<条数>;<键长>:<键><值长>:<值>…`。
    ///
    /// 用长度前缀而不是 JSON，是为了不在这个 crate 里引入序列化依赖，
    /// 同时保证编解码是单射（旧库该列为空串时解出空集）。
    #[must_use]
    pub fn encode(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.entries.len().to_string());
        out.push(';');
        for (key, value) in &self.entries {
            out.push_str(&key.len().to_string());
            out.push(':');
            out.push_str(key);
            out.push_str(&value.len().to_string());
            out.push(':');
            out.push_str(value);
        }
        out
    }

    /// 解析 [`Metadata::encode`] 的文本。任何不一致都视为账本损坏，直接报错。
    ///
    /// # Errors
    /// [`LedgerError::BadStoredMetadata`]：文本被改写或截断。
    pub fn decode(raw: &str) -> Result<Self> {
        if raw.is_empty() {
            return Ok(Self::default());
        }
        let (head, rest) = raw
            .split_once(';')
            .ok_or_else(|| LedgerError::BadStoredMetadata(raw.to_string()))?;
        let count = head
            .parse::<usize>()
            .map_err(|_| LedgerError::BadStoredMetadata(raw.to_string()))?;
        let mut entries = Vec::with_capacity(count.min(1024));
        let mut cursor = rest;
        for _ in 0..count {
            let (key, tail) = take_length_prefixed(cursor)?;
            let (value, tail) = take_length_prefixed(tail)?;
            entries.push((key, value));
            cursor = tail;
        }
        if !cursor.is_empty() {
            return Err(LedgerError::BadStoredMetadata(raw.to_string()));
        }
        check_metadata_budget(&entries)?;
        Ok(Self { entries })
    }
}

fn take_length_prefixed(raw: &str) -> Result<(String, &str)> {
    let colon = raw
        .bytes()
        .position(|b| b == b':')
        .ok_or_else(|| LedgerError::BadStoredMetadata(raw.to_string()))?;
    let (len_text, rest) = raw.split_at(colon);
    // 长度字段自己也必须是规范形态：`007:` 这种带前导零的写法与 `7:` 指向
    // 同一条目，却是两种字节 —— 留到查询层才炸。
    let shaped = len_text.bytes().all(|b| b.is_ascii_digit())
        && !(len_text.len() > 1 && len_text.starts_with('0'));
    if !shaped {
        return Err(LedgerError::BadStoredMetadata(raw.to_string()));
    }
    let rest = &rest[1..];
    let len = len_text
        .parse::<usize>()
        .map_err(|_| LedgerError::BadStoredMetadata(raw.to_string()))?;
    // `len` 是字节数：越界或切在多字节字符中间都算存储被改写，绝不能 panic。
    if len > rest.len() || !rest.is_char_boundary(len) {
        return Err(LedgerError::BadStoredMetadata(raw.to_string()));
    }
    Ok((rest[..len].to_string(), &rest[len..]))
}

fn validate_metadata_key(key: &str) -> Result<()> {
    if key.is_empty() {
        return Err(LedgerError::MetadataKey(key.to_string()));
    }
    if key.len() > MAX_METADATA_KEY_BYTES {
        return Err(LedgerError::MetadataKey(key.to_string()));
    }
    // Formance 的键规则要求"不能与分隔符撞车"；这里用"无控制字符、无空白"
    // 覆盖同一条防线，并顺带把 `|` 挡在键外，避免摘要片段自歧义。
    if key
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || c == '|')
    {
        return Err(LedgerError::MetadataKey(key.to_string()));
    }
    Ok(())
}

fn validate_metadata_value(key: &str, value: &str) -> Result<()> {
    if value.len() > MAX_METADATA_VALUE_BYTES {
        return Err(LedgerError::MetadataValue(key.to_string()));
    }
    if value.contains('\0') {
        return Err(LedgerError::MetadataValue(key.to_string()));
    }
    Ok(())
}

fn check_metadata_budget(entries: &[(String, String)]) -> Result<()> {
    if entries.len() > MAX_METADATA_ENTRIES {
        return Err(LedgerError::MetadataEntryLimit);
    }
    let total = entries.iter().fold(0usize, |acc, (k, v)| acc + k.len() + v.len());
    if total > MAX_METADATA_TOTAL_BYTES {
        return Err(LedgerError::MetadataSizeLimit(total));
    }
    Ok(())
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
    ///
    /// 这条分界对应 Formance 的 `world` 账户豁免
    /// （`internal/domain/processing/processor_posting.go:120`：只有源账户
    /// 是 world 时才跳过余额检查）：非资产类科目就是我们的"外部世界"，
    /// 资产类科目一律不允许透支。
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
    /// 规范资产标识；`None` 表示这笔 journal 没有声明 typed asset。
    pub asset: Option<Asset>,
    /// 记账时的附加键值信息（已入幂等摘要）。
    pub metadata: Metadata,
    pub created_at: i64,
}

/// 入账结果：头 + 全部分录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerTransaction {
    pub id: String,
    pub business_key: String,
    pub kind: String,
    pub currency: Currency,
    pub asset: Option<Asset>,
    pub metadata: Metadata,
    pub created_at: i64,
    pub entries: Vec<Entry>,
}

/// 这笔结果是"新入账"还是"幂等重放"。
///
/// 借 Formance `machine.go` 的 `replayed` 分流：重放直接把既有结果原样
/// 返回，不再跑一遍校验管道。区分二者是为了让调用方能够断言"重试没有
/// 产生新行"，而不是只能靠数行数反推。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// 本次调用真正写入了分录。
    Applied,
    /// 本次调用命中既有 `business_key`，零写入。
    Replayed,
}

/// [`crate::store::post_checked_in`] 的返回：交易 + 是否重放。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostReport {
    pub transaction: LedgerTransaction,
    pub outcome: ApplyOutcome,
}

impl PostReport {
    /// 是否为幂等重放。
    #[must_use]
    pub fn is_replay(&self) -> bool {
        self.outcome == ApplyOutcome::Replayed
    }
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
///
/// `asset` 与 `metadata` 默认是"未声明"：二者都为空时，幂等摘要的输入与
/// 旧版本逐字节一致，升级前写入的 `request_digest` 在升级后仍然命中重放，
/// 不会把历史重试误判成参数变更。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostingRequest {
    pub business_key: String,
    pub kind: String,
    pub currency: Currency,
    pub lines: Vec<PostingLine>,
    pub memo: String,
    /// 声明式 typed asset（`CNY` / `CNY/2`）；必须与 `currency` 一致。
    pub asset: Option<Asset>,
    /// 附加键值信息；参与幂等摘要。
    pub metadata: Metadata,
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
            asset: None,
            metadata: Metadata::new(),
        }
    }

    /// 附言。
    #[must_use]
    pub fn with_memo(mut self, memo: impl Into<String>) -> Self {
        self.memo = memo.into();
        self
    }

    /// 声明 typed asset（按规范形态解析后的结果）。
    #[must_use]
    pub fn with_asset(mut self, asset: Asset) -> Self {
        self.asset = Some(asset);
        self
    }

    /// 声明 typed asset；非规范写法直接失败。
    ///
    /// # Errors
    /// [`LedgerError::BadAsset`]。
    pub fn try_with_asset(mut self, raw: &str) -> Result<Self> {
        self.asset = Some(Asset::parse(raw)?);
        Ok(self)
    }

    /// 附上 metadata。
    #[must_use]
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// 换业务键（测试与"同一组参数换幂等键重投"的路由代码用）。
    #[must_use]
    pub fn with_business_key(mut self, business_key: impl Into<String>) -> Self {
        self.business_key = business_key.into();
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
    use crate::LedgerError;

    #[test]
    fn asset_accepts_only_canonical_forms() {
        for good in ["CNY", "CNY/2"] {
            let parsed = Asset::parse(good).expect("canonical asset");
            assert_eq!(parsed.currency(), Currency::CNY);
            assert_eq!(parsed.precision(), ASSET_PRECISION);
        }
        // `CNY` 与 `CNY/2` 必须规范化成同一个串：否则同一笔钱有两种摘要，
        // 重投会被误判成参数变更。
        assert_eq!(
            Asset::parse("CNY").expect("a").canonical(),
            Asset::parse("CNY/2").expect("b").canonical()
        );
        for bad in [
            "", "usd", "CNY/0", "CNY/1", "CNY/3", "CNY/02", "CNY/2/3", "CNY/", "/2", "CNYK/2",
            "CN", "人民币",
        ] {
            assert!(
                matches!(Asset::parse(bad), Err(LedgerError::BadAsset(_))),
                "{bad} must be refused"
            );
        }
    }

    #[test]
    fn metadata_is_canonicalised_and_bounded() {
        let a = Metadata::from_pairs([
            ("b".to_string(), "2".to_string()),
            ("a".to_string(), "1".to_string()),
        ])
        .expect("pairs");
        let b = Metadata::from_pairs([
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string()),
        ])
        .expect("pairs");
        // 插入顺序不影响规范形态与摘要片段。
        assert_eq!(a, b);
        assert_eq!(a.digest_parts(), b.digest_parts());
        assert_eq!(a.get("a"), Some("1"));

        // 空键、重复键、超条数、超值长都要拒。
        assert!(matches!(
            Metadata::from_pairs([("".to_string(), "1".to_string())]),
            Err(LedgerError::MetadataKey(_))
        ));
        assert!(matches!(
            Metadata::from_pairs([
                ("k".to_string(), "1".to_string()),
                ("k".to_string(), "2".to_string())
            ]),
            Err(LedgerError::DuplicateMetadataKey(_))
        ));
        assert!(matches!(
            Metadata::from_pairs(
                (0..=MAX_METADATA_ENTRIES)
                    .map(|i| (format!("k{i}"), "v".to_string()))
                    .collect::<Vec<_>>()
            ),
            Err(LedgerError::MetadataEntryLimit | LedgerError::MetadataSizeLimit(_))
        ));
        assert!(matches!(
            Metadata::from_pairs([("k".to_string(), "v".repeat(MAX_METADATA_VALUE_BYTES + 1))]),
            Err(LedgerError::MetadataValue(_))
        ));
        // NUL 字节会把键值对编码撑成两条：直接拒。
        assert!(matches!(
            Metadata::from_pairs([("k".to_string(), "a\0b".to_string())]),
            Err(LedgerError::MetadataValue(_))
        ));
    }

    #[test]
    fn metadata_encoding_roundtrips() {
        assert_eq!(Metadata::new().len(), 0);
        assert!(Metadata::decode("").expect("legacy empty").is_empty());
        for pairs in [
            vec![],
            vec![("k".to_string(), "v".to_string())],
            vec![
                ("a".to_string(), "中文:冒号1".to_string()),
                ("b".to_string(), String::new()),
            ],
        ] {
            let meta = Metadata::from_pairs(pairs).expect("pairs");
            assert_eq!(Metadata::decode(&meta.encode()).expect("decode"), meta);
        }
        // 被截断/改写的存储值必须报错，不能静默解析成空集。
        for bad in ["1;3:abc", "2;1:a1:b", ";", "abc"] {
            assert!(
                matches!(
                    Metadata::decode(bad),
                    Err(LedgerError::BadStoredMetadata(_))
                ),
                "{bad}"
            );
        }
    }
}
