use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::provider::ProviderError;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentIntentRequest {
    pub actor: String,
    pub order_id: String,
    pub amount_minor: i64,
    pub currency: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentIntent {
    pub payment_id: String,
    pub succeeded: bool,
    pub decline_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundRequest {
    pub actor: String,
    pub order_id: String,
    pub payment_id: String,
    pub amount_minor: i64,
    pub currency: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundReceipt {
    pub refund_id: String,
    pub amount_minor: i64,
    pub status: String,
}

/// G06 支付域的端口；本域只发起意图与退款，不维护余额。
#[async_trait]
pub trait PaymentPort: Send + Sync {
    async fn create_intent(
        &self,
        request: &PaymentIntentRequest,
    ) -> std::result::Result<PaymentIntent, ProviderError>;

    async fn refund(
        &self,
        request: &RefundRequest,
    ) -> std::result::Result<RefundReceipt, ProviderError>;
}
