//! 把服务目录域（`crates/service-catalog`）装进运行中的 server —— G07 / D6。
//!
//! 这个 crate 之前是"孤儿特性域"：目录/报价/订单/退款逻辑完备，但工作区里
//! 没有任何 crate 依赖它，所以线上打不到。本模块是**组合根装配**，三条硬规矩
//! 与 media 域（#295）一致：
//!
//! 1. **鉴权只有一条路**。`Authorization: Bearer` → `Platform::lookup_token`，
//!    读的就是 `Platform::router` 里 `Authed` 提取器读的同一张 `sessions` 表。
//!    crate 自带的 `BearerAuthenticator` fixture 在这里一次都不出现。
//! 2. **没有供应商凭据就诚实阻塞**。生产默认不配置任何供应商沙箱凭据，报价/
//!    下单经服务层如实报 `BLOCKED_EXTERNAL`（→ HTTP 501），不拿静态假数据冒充。
//!    目录由管理后台配置（默认空），空目录就是空数组。
//! 3. **支付只经 G06 端口**。本域不自建余额；组合根尚未接入 G06 支付适配器，
//!    装配一个显式拒绝的支付端口——任何走到支付的订单都如实失败，不伪造扣款。

use std::sync::Arc;

use crate::platform_sessions::PlatformSessions;
use axum::Router;
use wxlike_platform::{AccountSessions, PlatformState};
use wxlike_service_catalog::auth::Authenticator;
use wxlike_service_catalog::http::{AppState, CatalogApp};
use wxlike_service_catalog::{
    Catalog, Clock, OrderService, PaymentIntent, PaymentIntentRequest, PaymentPort, ProviderError,
    RefundReceipt, RefundRequest, ServiceProvider, SystemClock,
};

/// 默认服务区域：目录按区域开关投影。组合根可经 `WXLIKE_SERVICE_REGION` 覆盖。
pub const DEFAULT_REGION: &str = "CN";

/// 服务层鉴权器：把 bearer token 交给平台 sessions 表换可信 actor。
struct PlatformAuth(PlatformState);

impl Authenticator for PlatformAuth {
    fn authenticate(&self, session_token: &str) -> wxlike_service_catalog::Result<String> {
        match self.0.lookup_token(session_token) {
            Ok(Some(user_id)) => Ok(user_id),
            _ => Err(wxlike_service_catalog::ServiceError::new(
                wxlike_service_catalog::ServiceErrorCode::Unauthorized,
                "session rejected",
            )),
        }
    }
}

/// 显式拒绝的支付端口：组合根尚未接入 G06 支付适配器。任何走到支付/退款的
/// 订单都如实报 `Unavailable`（→ 服务层对账态/依赖不可用），绝不伪造扣款成功。
struct UnwiredPayment;

#[async_trait::async_trait]
impl PaymentPort for UnwiredPayment {
    async fn create_intent(
        &self,
        _req: &PaymentIntentRequest,
    ) -> Result<PaymentIntent, ProviderError> {
        Err(ProviderError::Unavailable)
    }
    async fn refund(&self, _req: &RefundRequest) -> Result<RefundReceipt, ProviderError> {
        Err(ProviderError::Unavailable)
    }
}

/// 生产供应商列表：C-03 的 owner 决策是**不接外部 provider**。
/// 这里单独成函数，让测试能钉住“生产装配不会悄悄塞进供应商”。
fn production_providers() -> Vec<Arc<dyn ServiceProvider>> {
    Vec::new()
}

/// 装配服务目录域路由。返回值由 `build_app` 直接 `merge`。
///
/// 生产默认：空目录（管理后台配置）+ 空供应商列表（无凭据→报价/下单诚实阻塞）
/// + 显式拒绝的支付端口。订单事实落盘到 sidecar 目录（重启可恢复）。
pub async fn assemble(platform: &PlatformState, db_path: Option<&str>) -> anyhow::Result<Router> {
    let catalog = Arc::new(Catalog::new());
    let region =
        crate::non_empty_env("WXLIKE_SERVICE_REGION").unwrap_or_else(|| DEFAULT_REGION.to_string());

    let store = wxlike_service_catalog::FileStore::open(crate::sidecar_dir(db_path, "discover"))
        .await
        .map_err(|err| anyhow::anyhow!("discover store open failed: {err}"))?;
    let auth: Arc<dyn Authenticator> = Arc::new(PlatformAuth(platform.clone()));
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let orders = Arc::new(OrderService::with_store(
        Arc::clone(&catalog),
        production_providers(),
        Arc::new(UnwiredPayment),
        auth,
        clock,
        Arc::new(store),
    ));

    Ok(CatalogApp::new(AppState {
        catalog,
        orders,
        verifier: Arc::new(PlatformSessions::new(platform.clone())),
        region,
    })
    .router())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_has_no_external_provider() {
        // C-03：生产必须保持空供应商；一旦有人往这里塞 provider，
        // “先履约后付款”的条件路径就重新可达，必须红。
        assert!(production_providers().is_empty());
    }
}
