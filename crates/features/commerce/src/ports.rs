//! 领域端口。基础设施（SQLite、供应商、身份）通过 trait 注入。
//!
//! 支付供应商是唯一允许在测试里注入故障的外部依赖；账本、状态机、
//! 幂等与权限全部由本 crate 的真实实现承担。

use std::sync::Arc;

use wxlike_ledger::Money;

use crate::error::{CommerceError, Result};

/// 时钟。测试可用固定时钟推进过期边界。
pub trait Clock: Send + Sync {
    /// UTC 毫秒。
    fn now_ms(&self) -> i64;
}

/// 系统时钟。
#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        wxlike_ledger::now_ms()
    }
}

/// 熵源。nonce、业务 ID、红包份额都从这里取随机数。
pub trait Entropy: Send + Sync {
    /// 填满缓冲区。
    fn fill(&self, out: &mut [u8]);
}

/// 操作系统熵源。
#[derive(Debug, Default)]
pub struct OsEntropy;

impl Entropy for OsEntropy {
    fn fill(&self, out: &mut [u8]) {
        use rand::RngCore as _;
        rand::thread_rng().fill_bytes(out);
    }
}

/// 群成员校验。G02 合并前用冻结 fixture 实现，生产必须注入真实端口。
pub trait GroupMembership: Send + Sync {
    /// `user` 是否是 `conversation` 的当前成员。
    fn is_member(&self, conversation: &str, user: &str) -> Result<bool>;
}

/// 默认拒绝：没有注入真实成员端口时，红包创建/领取失败关闭。
#[derive(Debug, Default)]
pub struct DenyAllMembership;

impl GroupMembership for DenyAllMembership {
    fn is_member(&self, _conversation: &str, _user: &str) -> Result<bool> {
        Ok(false)
    }
}

/// 供应商支付状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderPaymentStatus {
    /// 已受理、等待回调。
    Pending,
    /// 已成功。
    Succeeded,
    /// 已失败。
    Failed,
}

/// 创建支付单的请求。
#[derive(Clone, Debug)]
pub struct ProviderIntentRequest {
    /// 供应商幂等键（由 commerce 的业务键派生）。
    pub idempotency_key: String,
    pub amount: Money,
    pub payee: String,
    /// 本地业务对象 ID，仅用于供应商侧对账。
    pub metadata_ref: String,
}

/// 供应商支付单。
#[derive(Clone, Debug)]
pub struct ProviderIntent {
    pub provider_ref: String,
    pub status: ProviderPaymentStatus,
}

/// 一次支付状态查询/动作的结果。
#[derive(Clone, Debug)]
pub struct ProviderPayment {
    pub provider_ref: String,
    pub status: ProviderPaymentStatus,
    pub amount: Money,
}

/// 退款请求。
#[derive(Clone, Debug)]
pub struct ProviderRefundRequest {
    pub idempotency_key: String,
    pub provider_ref: String,
    pub amount: Money,
}

/// 退款结果。
#[derive(Clone, Debug)]
pub struct ProviderRefund {
    pub provider_ref: String,
    pub refunded: Money,
}

/// 提现请求。
#[derive(Clone, Debug)]
pub struct ProviderPayoutRequest {
    pub idempotency_key: String,
    pub amount: Money,
    pub account_ref: String,
}

/// 供应商出款状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderPayoutStatus {
    /// 已提交、终态未知。
    Submitted,
    /// 已到账。
    Paid,
    /// 明确失败。
    Failed,
}

/// 提现结果。
#[derive(Clone, Debug)]
pub struct ProviderPayout {
    pub provider_ref: String,
    pub status: ProviderPayoutStatus,
    pub amount: Money,
}

/// 对账单方向。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatementKind {
    Payment,
    Refund,
    Payout,
}

impl StatementKind {
    /// 数据库里的稳定字符串。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Payment => "payment",
            Self::Refund => "refund",
            Self::Payout => "payout",
        }
    }
}

/// 供应商对账单的一行。
#[derive(Clone, Debug)]
pub struct ProviderStatementRow {
    pub provider_ref: String,
    pub kind: StatementKind,
    pub amount: Money,
}

/// 供应商故障。
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    /// 网络不可达。
    #[error("provider unavailable")]
    Unavailable,
    /// 超时，终态未知。
    #[error("provider timeout")]
    Timeout,
    /// 供应商明确拒绝。
    #[error("provider rejected the request")]
    Rejected,
}

/// 支付供应商端口。
pub trait PaymentProvider: Send + Sync {
    /// 供应商模式；组合根据此拒绝沙箱/生产凭据混用。
    fn mode(&self) -> crate::config::ProviderMode;

    fn create_intent(
        &self,
        req: &ProviderIntentRequest,
    ) -> std::result::Result<ProviderIntent, ProviderError>;
    fn pay(&self, provider_ref: &str) -> std::result::Result<ProviderPayment, ProviderError>;
    fn query_payment(
        &self,
        provider_ref: &str,
    ) -> std::result::Result<ProviderPayment, ProviderError>;
    fn refund(
        &self,
        req: &ProviderRefundRequest,
    ) -> std::result::Result<ProviderRefund, ProviderError>;
    fn payout(
        &self,
        req: &ProviderPayoutRequest,
    ) -> std::result::Result<ProviderPayout, ProviderError>;
    fn statement(&self) -> std::result::Result<Vec<ProviderStatementRow>, ProviderError>;
}

/// 端口集合。
#[derive(Clone)]
pub struct Ports {
    pub provider: Arc<dyn PaymentProvider>,
    pub membership: Arc<dyn GroupMembership>,
    pub clock: Arc<dyn Clock>,
    pub entropy: Arc<dyn Entropy>,
}

impl Ports {
    /// 默认时钟/熵源 + 指定供应商与成员端口。
    #[must_use]
    pub fn new(provider: Arc<dyn PaymentProvider>, membership: Arc<dyn GroupMembership>) -> Self {
        Self {
            provider,
            membership,
            clock: Arc::new(SystemClock),
            entropy: Arc::new(OsEntropy),
        }
    }

    /// 替换时钟/熵源（测试）。
    #[must_use]
    pub fn with_clock_entropy(mut self, clock: Arc<dyn Clock>, entropy: Arc<dyn Entropy>) -> Self {
        self.clock = clock;
        self.entropy = entropy;
        self
    }
}

pub(crate) fn map_provider(error: ProviderError) -> CommerceError {
    match error {
        ProviderError::Unavailable => CommerceError::ProviderUnavailable,
        ProviderError::Timeout => CommerceError::ProviderTimeout,
        ProviderError::Rejected => CommerceError::ProviderRejected,
    }
}
