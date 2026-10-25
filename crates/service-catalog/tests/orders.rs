use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use wxlike_service_catalog::auth::{Authenticator, BearerAuthenticator};
use wxlike_service_catalog::{
    Availability, Catalog, Clock, CreateOrderRequest, OrderConfig, OrderService, OrderState,
    PaymentIntent, PaymentIntentRequest, PaymentPort, ProviderCancellation, ProviderError,
    ProviderOrder, ProviderOrderRequest, ProviderOrderState, ProviderReceipt, ProviderStatus,
    RefundReceipt, RefundRequest, SandboxEvidence, ServiceCard, ServiceErrorCode as Code,
    ServiceProvider, ServiceType, SupportPort,
};

struct FakeClock(Mutex<i64>);

impl Clock for FakeClock {
    fn now_ms(&self) -> i64 {
        *self.0.lock().unwrap()
    }
}

impl FakeClock {
    fn advance(&self, delta: i64) {
        *self.0.lock().unwrap() += delta;
    }
}

#[derive(Default)]
struct ProviderState {
    available: bool,
    price_minor: i64,
    place_error: Option<ProviderError>,
    place_state: Option<ProviderOrderState>,
    fulfill_error: Option<ProviderError>,
    status: Option<ProviderOrderState>,
    cancel_accepted: bool,
    refund_due_minor: i64,
    place_calls: usize,
    fulfill_calls: usize,
    cancel_calls: usize,
}

impl ProviderState {
    fn ready() -> Self {
        Self {
            available: true,
            price_minor: 1_000,
            place_state: Some(ProviderOrderState::Accepted),
            status: Some(ProviderOrderState::Fulfilled),
            cancel_accepted: true,
            ..Self::default()
        }
    }
}

struct FakeProvider {
    evidence: Option<SandboxEvidence>,
    state: Mutex<ProviderState>,
}

impl FakeProvider {
    fn ready() -> Arc<Self> {
        Arc::new(Self {
            evidence: Some(SandboxEvidence {
                environment: "sandbox".to_owned(),
                reference: "sandbox-run-2026-10-24".to_owned(),
                verified_at_ms: 1,
            }),
            state: Mutex::new(ProviderState::ready()),
        })
    }

    fn with<F: FnOnce(&mut ProviderState)>(&self, change: F) {
        change(&mut self.state.lock().unwrap());
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
impl ServiceProvider for FakeProvider {
    fn id(&self) -> &'static str {
        "fake-vendor"
    }

    fn supports(&self, kind: ServiceType) -> bool {
        matches!(kind, ServiceType::MobileTopup | ServiceType::Retail)
    }

    fn sandbox_evidence(&self) -> Option<SandboxEvidence> {
        self.evidence.clone()
    }

    fn support_hint(&self) -> Option<String> {
        Some("support://services".to_owned())
    }

    async fn availability(
        &self,
        _ctx: &wxlike_service_catalog::QuoteContext<'_>,
    ) -> Result<Availability, ProviderError> {
        let state = self.state.lock().unwrap();
        Ok(Availability {
            available: state.available,
            amount_minor: state.price_minor,
            currency: "CNY".to_owned(),
            terms_version: "terms-v1".to_owned(),
            valid_until_ms: 10_000,
            note: None,
        })
    }

    async fn place_order(
        &self,
        request: &ProviderOrderRequest,
    ) -> Result<ProviderOrder, ProviderError> {
        let mut state = self.state.lock().unwrap();
        state.place_calls += 1;
        if let Some(err) = state.place_error {
            return Err(err);
        }
        assert_eq!(request.amount_minor, 1_000);
        Ok(ProviderOrder {
            provider_order_id: format!("vendor-{}", state.place_calls),
            state: state.place_state.unwrap_or(ProviderOrderState::Accepted),
            receipt: Some(receipt("accepted")),
        })
    }

    async fn fulfill(&self, _id: &str, _key: &str) -> Result<ProviderReceipt, ProviderError> {
        let mut state = self.state.lock().unwrap();
        state.fulfill_calls += 1;
        if let Some(err) = state.fulfill_error {
            return Err(err);
        }
        Ok(receipt("fulfilled"))
    }

    async fn cancel(&self, _id: &str, _key: &str) -> Result<ProviderCancellation, ProviderError> {
        let mut state = self.state.lock().unwrap();
        state.cancel_calls += 1;
        Ok(ProviderCancellation {
            accepted: state.cancel_accepted,
            refund_due_minor: state.refund_due_minor,
            message: "cancelled".to_owned(),
        })
    }

    async fn query(&self, _id: &str) -> Result<ProviderStatus, ProviderError> {
        let state = self.state.lock().unwrap();
        Ok(ProviderStatus {
            state: state.status.unwrap_or(ProviderOrderState::Pending),
            receipt: Some(receipt("queried")),
        })
    }
}

#[derive(Default)]
struct FakePayment {
    decline: AtomicBool,
    refund_fail: AtomicBool,
    intents: AtomicUsize,
    refunds: AtomicUsize,
}

#[async_trait]
impl PaymentPort for FakePayment {
    async fn create_intent(
        &self,
        request: &PaymentIntentRequest,
    ) -> Result<PaymentIntent, ProviderError> {
        self.intents.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.amount_minor, 1_000);
        if self.decline.load(Ordering::SeqCst) {
            return Ok(PaymentIntent {
                payment_id: "pay-declined".to_owned(),
                succeeded: false,
                decline_reason: Some("insufficient".to_owned()),
            });
        }
        Ok(PaymentIntent {
            payment_id: "pay-1".to_owned(),
            succeeded: true,
            decline_reason: None,
        })
    }

    async fn refund(&self, request: &RefundRequest) -> Result<RefundReceipt, ProviderError> {
        self.refunds.fetch_add(1, Ordering::SeqCst);
        if self.refund_fail.load(Ordering::SeqCst) {
            return Err(ProviderError::Unavailable);
        }
        Ok(RefundReceipt {
            refund_id: "refund-1".to_owned(),
            amount_minor: request.amount_minor,
            status: "succeeded".to_owned(),
        })
    }
}

#[derive(Default)]
struct FakeSupport {
    tickets: AtomicUsize,
}

#[async_trait]
impl SupportPort for FakeSupport {
    async fn open_ticket(
        &self,
        _actor: &str,
        order_id: &str,
        _reason: &str,
    ) -> Result<String, ProviderError> {
        let count = self.tickets.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(format!("ticket-{count}-{order_id}"))
    }
}

struct Harness {
    catalog: Arc<Catalog>,
    provider: Arc<FakeProvider>,
    payment: Arc<FakePayment>,
    support: Arc<FakeSupport>,
    clock: Arc<FakeClock>,
    service: OrderService,
}

fn harness_with(provider: Arc<FakeProvider>, region_enabled: bool) -> Harness {
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
    let payment = Arc::new(FakePayment::default());
    let support = Arc::new(FakeSupport::default());
    let clock = Arc::new(FakeClock(Mutex::new(1_000)));
    let auth: Arc<dyn Authenticator> =
        Arc::new(BearerAuthenticator::new(|token: &str| match token {
            "s1" => Some("u1".to_owned()),
            "s2" => Some("u2".to_owned()),
            _ => None,
        }));
    let service = OrderService::new(
        catalog.clone(),
        vec![provider.clone()],
        payment.clone(),
        auth,
        clock.clone(),
    )
    .with_support(support.clone())
    .with_config(OrderConfig {
        quote_ttl_ms: 5_000,
        max_quantity: 5,
    });
    Harness {
        catalog,
        provider,
        payment,
        support,
        clock,
        service,
    }
}

fn harness() -> Harness {
    harness_with(FakeProvider::ready(), true)
}

fn params() -> Value {
    json!({"phone": "13800138000"})
}

async fn quote(h: &Harness) -> wxlike_service_catalog::Quote {
    h.service
        .quote("s1", "topup-cn", 1, params())
        .await
        .unwrap()
}

fn request(quote_id: &str, key: &str) -> CreateOrderRequest {
    CreateOrderRequest {
        quote_id: quote_id.to_owned(),
        idempotency_key: key.to_owned(),
        confirmed_amount_minor: 1_000,
        params: params(),
    }
}

#[tokio::test]
async fn catalog_region_gate_and_empty_list_are_explicit() {
    let h = harness_with(FakeProvider::ready(), false);
    assert!(h.catalog.list(ServiceType::MobileTopup, "CN").is_empty());
    let err = h
        .service
        .quote("s1", "topup-cn", 1, params())
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::BlockedExternal);

    h.catalog
        .set_region_enabled(ServiceType::MobileTopup, "CN", true)
        .unwrap();
    assert_eq!(h.catalog.list(ServiceType::MobileTopup, "CN").len(), 1);
    assert!(h.catalog.list(ServiceType::Hotel, "CN").is_empty());
    assert!(h.catalog.list(ServiceType::MobileTopup, "US").is_empty());
}

#[tokio::test]
async fn provider_without_sandbox_evidence_stays_blocked() {
    let provider = Arc::new(FakeProvider {
        evidence: None,
        state: Mutex::new(ProviderState::ready()),
    });
    let h = harness_with(provider, true);
    let err = h
        .service
        .quote("s1", "topup-cn", 1, params())
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::BlockedExternal);
    assert!(h.catalog.list(ServiceType::MobileTopup, "CN").len() == 1);
}

#[tokio::test]
async fn quote_comes_from_live_availability_not_static_card_price() {
    let h = harness();
    h.provider.with(|state| state.price_minor = 1_200);
    h.catalog.update_price("topup-cn", 999).unwrap();
    let quote = h
        .service
        .quote("s1", "topup-cn", 2, params())
        .await
        .unwrap();
    assert_eq!(quote.amount_minor, 2_400);
    assert_eq!(quote.quantity, 2);
    assert_eq!(quote.terms_version, "terms-v1");
    assert_eq!(quote.expires_at_ms, 6_000);
    assert_eq!(h.service.get_quote("s1", &quote.quote_id).unwrap(), quote);
    assert_eq!(
        h.service
            .get_quote("s2", &quote.quote_id)
            .unwrap_err()
            .code(),
        Code::NotFound
    );
}

#[tokio::test]
async fn invalid_requests_and_unavailable_service_are_rejected() {
    let h = harness();
    assert_eq!(
        h.service
            .quote("bad", "topup-cn", 1, params())
            .await
            .unwrap_err()
            .code(),
        Code::Unauthorized
    );
    assert_eq!(
        h.service
            .quote("s1", "nope", 1, params())
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );
    assert_eq!(
        h.service
            .quote("s1", "topup-cn", 0, params())
            .await
            .unwrap_err()
            .code(),
        Code::InvalidArgument
    );
    assert_eq!(
        h.service
            .quote("s1", "topup-cn", 6, params())
            .await
            .unwrap_err()
            .code(),
        Code::InvalidArgument
    );
    h.provider.with(|state| state.available = false);
    assert_eq!(
        h.service
            .quote("s1", "topup-cn", 1, params())
            .await
            .unwrap_err()
            .code(),
        Code::Conflict
    );
}

#[tokio::test]
async fn price_change_requires_explicit_requote_and_confirmation() {
    let h = harness();
    let old = quote(&h).await;
    h.catalog.update_price("topup-cn", 1_100).unwrap();
    let err = h
        .service
        .create("s1", request(&old.quote_id, "k1"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Conflict);

    let new_quote = quote(&h).await;
    let unconfirmed = CreateOrderRequest {
        confirmed_amount_minor: 900,
        ..request(&new_quote.quote_id, "k1")
    };
    let err = h.service.create("s1", unconfirmed).await.unwrap_err();
    assert_eq!(err.code(), Code::Conflict);
    assert_eq!(h.provider.state.lock().unwrap().place_calls, 0);
    assert_eq!(h.payment.intents.load(Ordering::SeqCst), 0);

    let confirmed = request(&new_quote.quote_id, "k2");
    let order = h.service.create("s1", confirmed).await.unwrap();
    assert_eq!(order.amount_minor, 1_000);
    assert_eq!(order.state, OrderState::Paid);
}

#[tokio::test]
async fn expired_quote_cannot_be_ordered() {
    let h = harness();
    let quote = quote(&h).await;
    h.clock.advance(5_000);
    let err = h
        .service
        .create("s1", request(&quote.quote_id, "k1"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Expired);
}

#[tokio::test]
async fn create_is_idempotent_and_returns_the_same_order() {
    let h = harness();
    let first_quote = quote(&h).await;
    let first = h
        .service
        .create("s1", request(&first_quote.quote_id, "k1"))
        .await
        .unwrap();
    assert_eq!(first.state, OrderState::Paid);
    assert_eq!(h.payment.intents.load(Ordering::SeqCst), 1);

    let second = h
        .service
        .create("s1", request(&first_quote.quote_id, "k1"))
        .await
        .unwrap();
    assert_eq!(second.order_id, first.order_id);
    assert_eq!(h.provider.state.lock().unwrap().place_calls, 1);
    assert_eq!(h.payment.intents.load(Ordering::SeqCst), 1);

    let other_quote = quote(&h).await;
    let conflict = h
        .service
        .create("s1", request(&other_quote.quote_id, "k1"))
        .await
        .unwrap_err();
    assert_eq!(conflict.code(), Code::Conflict);
}

#[tokio::test]
async fn fulfill_is_idempotent_and_records_receipt() {
    let h = harness();
    let quote = quote(&h).await;
    let order = h
        .service
        .create("s1", request(&quote.quote_id, "k1"))
        .await
        .unwrap();
    let fulfilled = h.service.fulfill("s1", &order.order_id).await.unwrap();
    assert_eq!(fulfilled.state, OrderState::Fulfilled);
    assert_eq!(fulfilled.receipt.unwrap().status, "fulfilled");

    let again = h.service.fulfill("s1", &order.order_id).await.unwrap();
    assert_eq!(again.state, OrderState::Fulfilled);
    assert_eq!(h.provider.state.lock().unwrap().fulfill_calls, 1);
}

#[tokio::test]
async fn offline_between_payment_and_delivery_recovers_by_query() {
    let h = harness();
    let quote = quote(&h).await;
    let order = h
        .service
        .create("s1", request(&quote.quote_id, "k1"))
        .await
        .unwrap();
    h.provider
        .with(|state| state.fulfill_error = Some(ProviderError::Unavailable));
    let err = h.service.fulfill("s1", &order.order_id).await.unwrap_err();
    assert_eq!(err.code(), Code::DependencyUnavailable);

    let pending = h.service.order("s1", &order.order_id).unwrap();
    assert_eq!(pending.state, OrderState::PendingReconciliation);
    assert!(pending.support_ticket.is_some());

    let recovered = h.service.query("s1", &order.order_id).await.unwrap();
    assert_eq!(recovered.state, OrderState::Fulfilled);
    assert_eq!(recovered.receipt.unwrap().status, "queried");
}

#[tokio::test]
async fn cancel_paid_order_refunds_via_payment_port() {
    let h = harness();
    let quote = quote(&h).await;
    let order = h
        .service
        .create("s1", request(&quote.quote_id, "k1"))
        .await
        .unwrap();
    h.provider.with(|state| state.refund_due_minor = 1_000);
    let cancelled = h
        .service
        .cancel("s1", &order.order_id, "user_cancel")
        .await
        .unwrap();
    assert_eq!(cancelled.state, OrderState::Refunded);
    assert_eq!(cancelled.refund.unwrap().amount_minor, 1_000);
    assert_eq!(h.payment.refunds.load(Ordering::SeqCst), 1);

    let again = h
        .service
        .cancel("s1", &order.order_id, "user_cancel")
        .await
        .unwrap();
    assert_eq!(again.state, OrderState::Refunded);
    assert_eq!(h.payment.refunds.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn fulfilled_order_cannot_cancel_but_can_refund() {
    let h = harness();
    let quote = quote(&h).await;
    let order = h
        .service
        .create("s1", request(&quote.quote_id, "k1"))
        .await
        .unwrap();
    h.service.fulfill("s1", &order.order_id).await.unwrap();
    assert_eq!(
        h.service
            .cancel("s1", &order.order_id, "too_late")
            .await
            .unwrap_err()
            .code(),
        Code::Conflict
    );
    let refunded = h
        .service
        .refund("s1", &order.order_id, "after_sale")
        .await
        .unwrap();
    assert_eq!(refunded.state, OrderState::Refunded);
    assert_eq!(h.payment.refunds.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn payment_decline_cancels_provider_order_and_opens_ticket() {
    let h = harness();
    h.payment.decline.store(true, Ordering::SeqCst);
    let quote = quote(&h).await;
    let err = h
        .service
        .create("s1", request(&quote.quote_id, "k1"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PaymentDeclined);
    assert_eq!(h.provider.state.lock().unwrap().cancel_calls, 1);
    assert_eq!(h.support.tickets.load(Ordering::SeqCst), 1);

    let retry = h
        .service
        .create("s1", request(&quote.quote_id, "k1"))
        .await
        .unwrap();
    assert_eq!(retry.state, OrderState::Failed);
    assert_eq!(
        retry.order_id,
        h.service.order("s1", &retry.order_id).unwrap().order_id
    );
    assert_eq!(h.provider.state.lock().unwrap().place_calls, 1);
}

#[tokio::test]
async fn provider_errors_are_mapped_and_leave_no_order() {
    let h = harness();
    h.provider
        .with(|state| state.place_error = Some(ProviderError::Timeout));
    let quote = quote(&h).await;
    let err = h
        .service
        .create("s1", request(&quote.quote_id, "k1"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Timeout);
    assert_eq!(h.payment.intents.load(Ordering::SeqCst), 0);

    h.provider
        .with(|state| state.place_error = Some(ProviderError::BadGateway));
    let quote = h
        .service
        .quote("s1", "topup-cn", 1, params())
        .await
        .unwrap();
    let err = h
        .service
        .create("s1", request(&quote.quote_id, "k2"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::DependencyUnavailable);
}

#[tokio::test]
async fn other_users_cannot_see_or_operate_on_orders() {
    let h = harness();
    let quote = quote(&h).await;
    let order = h
        .service
        .create("s1", request(&quote.quote_id, "k1"))
        .await
        .unwrap();
    assert_eq!(
        h.service.order("s2", &order.order_id).unwrap_err().code(),
        Code::NotFound
    );
    assert_eq!(
        h.service
            .fulfill("s2", &order.order_id)
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );
    assert_eq!(
        h.service
            .create("s2", request(&quote.quote_id, "k1"))
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );
}

#[tokio::test]
async fn expired_quotes_are_purged() {
    let h = harness();
    quote(&h).await;
    h.clock.advance(6_000);
    assert_eq!(h.service.purge_expired_quotes(), 1);
    assert_eq!(h.service.purge_expired_quotes(), 0);
}
