use async_trait::async_trait;

use crate::provider::ProviderError;

/// 失败/退款时的客服工单入口；返回工单号，页面据此展示可追踪记录。
#[async_trait]
pub trait SupportPort: Send + Sync {
    async fn open_ticket(
        &self,
        actor: &str,
        order_id: &str,
        reason: &str,
    ) -> std::result::Result<String, ProviderError>;
}
