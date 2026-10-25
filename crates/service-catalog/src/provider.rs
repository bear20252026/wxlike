use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::catalog::{ServiceCard, ServiceType};
use crate::error::{ServiceError, ServiceErrorCode as Code};

pub const SANDBOX_ENV: &str = "sandbox";
pub const PRODUCTION_ENV: &str = "production";
pub const MAX_BUSINESS_NO_LEN: usize = 128;
pub const MAX_STATUS_LEN: usize = 64;

/// 供应商端口的统一返回形态；签名因此可以收成单行，各适配器少抄一遍样板。
pub type ProviderResult<T> = std::result::Result<T, ProviderError>;

/// 供应商联调凭据与最近一次验证记录；没有它只能停留在 BLOCKED_EXTERNAL。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxEvidence {
    pub environment: String,
    pub reference: String,
    pub verified_at_ms: i64,
}

impl SandboxEvidence {
    pub fn is_usable(&self) -> bool {
        !self.reference.is_empty()
            && (self.environment == SANDBOX_ENV || self.environment == PRODUCTION_ENV)
            && self.verified_at_ms > 0
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Availability {
    pub available: bool,
    pub amount_minor: i64,
    pub currency: String,
    pub terms_version: String,
    pub valid_until_ms: i64,
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderOrderRequest {
    pub actor: String,
    pub service_id: String,
    pub kind: ServiceType,
    pub region: String,
    pub amount_minor: i64,
    pub currency: String,
    pub params: Value,
    pub idempotency_key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderOrderState {
    Accepted,
    Fulfilled,
    Pending,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderReceipt {
    pub business_no: String,
    pub status: String,
    pub message: String,
    pub issued_at_ms: i64,
}

impl ProviderReceipt {
    pub fn validate(&self) -> bool {
        !self.business_no.is_empty()
            && self.business_no.len() <= MAX_BUSINESS_NO_LEN
            && !self.status.is_empty()
            && self.status.len() <= MAX_STATUS_LEN
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderOrder {
    pub provider_order_id: String,
    pub state: ProviderOrderState,
    pub receipt: Option<ProviderReceipt>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub state: ProviderOrderState,
    pub receipt: Option<ProviderReceipt>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderCancellation {
    pub accepted: bool,
    pub refund_due_minor: i64,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderError {
    Unavailable,
    Timeout,
    Cancelled,
    BadGateway,
    Rejected(&'static str),
}

/// 报价上下文：actor/服务卡/业务参数打包成单参，端口签名保持单行。
pub struct QuoteContext<'a> {
    pub actor: &'a str,
    pub card: &'a ServiceCard,
    pub params: &'a Value,
}

#[async_trait]
pub trait ServiceProvider: Send + Sync {
    fn id(&self) -> &'static str;

    fn supports(&self, kind: ServiceType) -> bool;

    /// `None` 或缺字段表示尚未取得供应商授权/未完成 sandbox 验证。
    fn sandbox_evidence(&self) -> Option<SandboxEvidence>;

    fn support_hint(&self) -> Option<String> {
        None
    }

    async fn availability(&self, ctx: &QuoteContext<'_>) -> ProviderResult<Availability>;

    async fn place_order(&self, request: &ProviderOrderRequest) -> ProviderResult<ProviderOrder>;

    async fn fulfill(&self, order_ref: &str, key: &str) -> ProviderResult<ProviderReceipt>;

    async fn cancel(&self, order_ref: &str, key: &str) -> ProviderResult<ProviderCancellation>;

    async fn query(&self, order_ref: &str) -> ProviderResult<ProviderStatus>;
}

pub fn map_provider_error(provider: &str, error: ProviderError) -> ServiceError {
    let detail = match error {
        ProviderError::Unavailable => "provider unavailable",
        ProviderError::Timeout => "provider timeout",
        ProviderError::Cancelled => "provider cancelled",
        ProviderError::BadGateway => "provider bad gateway",
        ProviderError::Rejected(reason) => reason,
    };
    let code = match error {
        ProviderError::Unavailable | ProviderError::BadGateway => Code::DependencyUnavailable,
        ProviderError::Timeout => Code::Timeout,
        ProviderError::Cancelled => Code::Cancelled,
        ProviderError::Rejected(_) => Code::Conflict,
    };
    ServiceError::new(code, format!("{provider}: {detail}"))
}
