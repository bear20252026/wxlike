//! `wxlike-service-catalog` — G07-04 的服务目录、报价与统一供应商订单。
//!
//! 价格与库存来自 [`provider::ServiceProvider`] 的实时应答，目录只负责展示与
//! 区域/启用配置。报价有 TTL，改价会使旧报价失效并强制重新确认；支付只经
//! [`payment::PaymentPort`] 调 G06，本域不自建余额。没有 sandbox 凭据的供应
//! 商返回 `BLOCKED_EXTERNAL`，不使用静态假数据或 toast 冒充完成。

pub mod auth;
pub mod catalog;
pub mod clock;
pub mod error;
pub mod ids;
pub mod order;
pub mod payment;
pub mod provider;
pub mod providers;
pub mod quote;
pub mod support;

pub use auth::{Authenticator, BearerAuthenticator};
pub use catalog::{Catalog, ServiceCard, ServiceType};
pub use clock::{Clock, SystemClock};
pub use error::{Result, ServiceError, ServiceErrorCode};
pub use order::{CreateOrderRequest, Order, OrderConfig, OrderService, OrderState};
pub use payment::{PaymentIntent, PaymentIntentRequest, PaymentPort, RefundReceipt, RefundRequest};
pub use provider::{
    Availability, ProviderCancellation, ProviderError, ProviderOrder, ProviderOrderRequest,
    ProviderOrderState, ProviderReceipt, ProviderResult, ProviderStatus, QuoteContext,
    SandboxEvidence, ServiceProvider,
};
pub use quote::Quote;
pub use support::SupportPort;
