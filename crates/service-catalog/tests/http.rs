//! G07/D6：服务目录控制面 HTTP 投影。用 tower::oneshot 打真实路由，钉住：
//! 1. 没带凭据 → 401，且服务层一个 actor 都换不到；
//! 2. 目录/报价/下单/查单/列举/退款 往返，形状投影到前端契约；
//! 3. 无供应商凭据 → 501（BLOCKED_EXTERNAL），不伪造价格/订单；
//! 4. 订单只读自己的，别人的订单 404。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use serde_json::{json, Value};
use tower::ServiceExt as _;
use wxlike_service_catalog::auth::{Authenticator, BearerAuthenticator};
use wxlike_service_catalog::http::{AppState, CatalogApp, FnVerifier, SessionVerifier};
use wxlike_service_catalog::{
    Availability, Catalog, Clock, OrderConfig, OrderService, PaymentIntent, PaymentIntentRequest,
    PaymentPort, ProviderCancellation, ProviderError, ProviderOrder, ProviderOrderRequest,
    ProviderOrderState, ProviderReceipt, ProviderStatus, QuoteContext, RefundReceipt,
    RefundRequest, SandboxEvidence, ServiceCard, ServiceProvider, ServiceType,
};

struct FakeClock(Mutex<i64>);
impl Clock for FakeClock {
    fn now_ms(&self) -> i64 {
        *self.0.lock().unwrap()
    }
}

struct ReadyProvider {
    evidence: Option<SandboxEvidence>,
}
impl ReadyProvider {
    fn ready() -> Arc<Self> {
        Arc::new(Self {
            evidence: Some(SandboxEvidence {
                environment: "sandbox".to_owned(),
                reference: "g07-http-run".to_owned(),
                verified_at_ms: 1,
            }),
        })
    }
    fn blocked() -> Arc<Self> {
        Arc::new(Self { evidence: None })
    }
}

fn receipt(status: &str) -> ProviderReceipt {
    ProviderReceipt {
        business_no: "biz-1".to_owned(),
        status: status.to_owned(),
        message: "ok".to_owned(),
        issued_at_ms: 1,
    }
}

#[async_trait]
impl ServiceProvider for ReadyProvider {
    fn id(&self) -> &'static str {
        "fake-vendor"
    }
    fn supports(&self, kind: ServiceType) -> bool {
        matches!(kind, ServiceType::MobileTopup)
    }
    fn sandbox_evidence(&self) -> Option<SandboxEvidence> {
        self.evidence.clone()
    }
    async fn availability(&self, _ctx: &QuoteContext<'_>) -> Result<Availability, ProviderError> {
        Ok(Availability {
            available: true,
            amount_minor: 1_000,
            currency: "CNY".to_owned(),
            terms_version: "terms-v1".to_owned(),
            valid_until_ms: 10_000,
            note: None,
        })
    }
    async fn place_order(
        &self,
        _req: &ProviderOrderRequest,
    ) -> Result<ProviderOrder, ProviderError> {
        Ok(ProviderOrder {
            provider_order_id: "vendor-1".to_owned(),
            state: ProviderOrderState::Fulfilled,
            receipt: Some(receipt("fulfilled")),
        })
    }
    async fn fulfill(&self, _id: &str, _key: &str) -> Result<ProviderReceipt, ProviderError> {
        Ok(receipt("fulfilled"))
    }
    async fn cancel(&self, _id: &str, _key: &str) -> Result<ProviderCancellation, ProviderError> {
        Ok(ProviderCancellation {
            accepted: true,
            refund_due_minor: 1_000,
            message: "ok".to_owned(),
        })
    }
    async fn query(&self, _id: &str) -> Result<ProviderStatus, ProviderError> {
        Ok(ProviderStatus {
            state: ProviderOrderState::Fulfilled,
            receipt: Some(receipt("q")),
        })
    }
}

#[derive(Default)]
struct LedgerPayment;
#[async_trait]
impl PaymentPort for LedgerPayment {
    async fn create_intent(
        &self,
        req: &PaymentIntentRequest,
    ) -> Result<PaymentIntent, ProviderError> {
        Ok(PaymentIntent {
            payment_id: format!("pay-{}", req.order_id),
            succeeded: true,
            decline_reason: None,
        })
    }
    async fn refund(&self, req: &RefundRequest) -> Result<RefundReceipt, ProviderError> {
        Ok(RefundReceipt {
            refund_id: format!("refund-{}", req.order_id),
            amount_minor: req.amount_minor,
            status: "succeeded".to_owned(),
        })
    }
}

/// 装配一个真实路由：目录里有一张启用卡 + 区域开关；供应商可选 ready/blocked。
fn app(provider: Arc<ReadyProvider>, region_enabled: bool) -> axum::Router {
    let catalog = Arc::new(Catalog::new());
    catalog
        .upsert(ServiceCard {
            service_id: "topup-cn".to_owned(),
            kind: ServiceType::MobileTopup,
            title: "手机充值".to_owned(),
            provider: "fake-vendor".to_owned(),
            region: "CN".to_owned(),
            currency: "CNY".to_owned(),
            display_price_minor: 1_000,
            terms_version: "terms-v1".to_owned(),
            enabled: true,
            revision: 0,
        })
        .unwrap();
    if region_enabled {
        catalog
            .set_region_enabled(ServiceType::MobileTopup, "CN", true)
            .unwrap();
    }
    let auth: Arc<dyn Authenticator> =
        Arc::new(BearerAuthenticator::new(|token: &str| match token {
            "s1" => Some("u1".to_owned()),
            "s2" => Some("u2".to_owned()),
            _ => None,
        }));
    let orders = Arc::new(
        OrderService::new(
            catalog.clone(),
            vec![provider],
            Arc::new(LedgerPayment),
            auth,
            Arc::new(FakeClock(Mutex::new(1_000))),
        )
        .with_config(OrderConfig {
            quote_ttl_ms: 5_000,
            max_quantity: 5,
        }),
    );
    // 会话校验端口：与平台 sessions 同口径（这里用 fixture token→user）。
    let verifier: Arc<dyn SessionVerifier> =
        Arc::new(FnVerifier(|headers: &axum::http::HeaderMap| {
            let raw = headers.get("authorization")?.to_str().ok()?;
            let token = raw
                .trim()
                .strip_prefix("Bearer ")
                .or_else(|| raw.trim().strip_prefix("bearer "))?;
            match token.trim() {
                "s1" => Some("u1".to_owned()),
                "s2" => Some("u2".to_owned()),
                _ => None,
            }
        }));
    CatalogApp::new(AppState {
        catalog,
        orders,
        verifier,
        region: "CN".to_owned(),
    })
    .router()
}

async fn send(
    router: &axum::Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
    idem: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        builder = builder.header("authorization", format!("Bearer {t}"));
    }
    if let Some(k) = idem {
        builder = builder.header("idempotency-key", k);
    }
    let req = match body {
        Some(v) => builder
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&bytes).unwrap_or(json!({}))
    };
    (status, value)
}

#[tokio::test]
async fn unauthenticated_requests_are_rejected() {
    let router = app(ReadyProvider::ready(), true);
    for (method, uri) in [
        ("GET", "/api/services"),
        ("GET", "/api/orders"),
        ("GET", "/api/quotes?service_id=topup-cn"),
    ] {
        let (status, _) = send(&router, method, uri, None, None, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {uri} 没带凭据必须 401"
        );
    }
}

#[tokio::test]
async fn services_projects_cards_and_empty_catalog_is_explicit() {
    let router = app(ReadyProvider::ready(), true);
    let (status, body) = send(&router, "GET", "/api/services", Some("s1"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["service_id"], json!("topup-cn"));
    assert_eq!(items[0]["name"], json!("手机充值"));
    assert_eq!(items[0]["target"]["kind"], json!("quote"));

    // 区域未开通：目录诚实返回空，不拿假数据填充。
    let gated = app(ReadyProvider::ready(), false);
    let (status, body) = send(&gated, "GET", "/api/services", Some("s1"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn quote_order_refund_round_trip() {
    let router = app(ReadyProvider::ready(), true);

    // 报价：amount_minor→price_minor、expires_at_ms→expires_at。
    let (status, quote) = send(
        &router,
        "GET",
        "/api/quotes?service_id=topup-cn",
        Some("s1"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{quote}");
    assert_eq!(quote["price_minor"], json!(1_000));
    assert_eq!(quote["currency"], json!("CNY"));
    assert!(quote["expires_at"].as_i64().unwrap() > 1_000);
    let quote_id = quote["quote_id"].as_str().unwrap().to_string();

    // 下单：补 confirmed_amount_minor，状态投影 fulfilled。
    let (status, order) = send(
        &router,
        "POST",
        "/api/orders",
        Some("s1"),
        Some(json!({"service_id": "topup-cn", "quote_id": quote_id})),
        Some("ord-1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{order}");
    assert_eq!(order["state"], json!("fulfilled"));
    assert_eq!(order["service_id"], json!("topup-cn"));
    let order_id = order["order_id"].as_str().unwrap().to_string();

    // 查单。
    let (status, fetched) = send(
        &router,
        "GET",
        &format!("/api/orders/{order_id}"),
        Some("s1"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["order_id"], json!(order_id));

    // 列举：含刚下的单。
    let (status, list) = send(&router, "GET", "/api/orders", Some("s1"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(list["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["order_id"] == json!(order_id)));

    // 退款：fulfilled → refunded。
    let (status, refunded) = send(
        &router,
        "POST",
        &format!("/api/orders/{order_id}/refund"),
        Some("s1"),
        None,
        Some("rf-1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{refunded}");
    assert_eq!(refunded["state"], json!("refunded"));
}

#[tokio::test]
async fn orders_are_owner_scoped() {
    let router = app(ReadyProvider::ready(), true);
    let (_, quote) = send(
        &router,
        "GET",
        "/api/quotes?service_id=topup-cn",
        Some("s1"),
        None,
        None,
    )
    .await;
    let quote_id = quote["quote_id"].as_str().unwrap();
    let (_, order) = send(
        &router,
        "POST",
        "/api/orders",
        Some("s1"),
        Some(json!({"service_id": "topup-cn", "quote_id": quote_id})),
        Some("ord-own"),
    )
    .await;
    let order_id = order["order_id"].as_str().unwrap();

    // 别人读不到这张单（404），列举也为空。
    let (status, _) = send(
        &router,
        "GET",
        &format!("/api/orders/{order_id}"),
        Some("s2"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, list) = send(&router, "GET", "/api/orders", Some("s2"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn no_provider_evidence_is_honest_501() {
    let router = app(ReadyProvider::blocked(), true);
    // 目录仍可投影（卡片启用），但报价没有供应商凭据 → 501，不伪造价格。
    let (status, body) = send(
        &router,
        "GET",
        "/api/quotes?service_id=topup-cn",
        Some("s1"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("sandbox"),
        "{body}"
    );
}
