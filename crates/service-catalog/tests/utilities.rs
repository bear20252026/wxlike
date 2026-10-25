mod common;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use wxlike_service_catalog::auth::BearerAuthenticator;
use wxlike_service_catalog::providers::utilities::{
    sign_bill, DonationProject, ReceiptState, UtilitiesProvider, UtilitiesVendor, VendorBill,
    VendorDonation, VendorReceipt,
};
use wxlike_service_catalog::{
    Catalog, CreateOrderRequest, OrderService, OrderState, PaymentPort, ProviderCancellation,
    ProviderError, SandboxEvidence, ServiceErrorCode as Code, ServiceType, SupportPort,
};

use common::{card, evidence, LedgerPayment, RecordingSupport, TestClock};

const BILL_KEY: &[u8] = b"g07-bill-hmac-key-0001";

struct VendorState {
    bills: HashMap<String, VendorBill>,
    topup_error: Option<ProviderError>,
    lookup_error: Option<ProviderError>,
    pay_error: Option<ProviderError>,
    receipt_state: ReceiptState,
    query_state: ReceiptState,
    donation_override: Option<(String, String)>,
    refund_due_minor: i64,
    topup_calls: usize,
    pay_calls: usize,
    cancel_calls: usize,
}

impl Default for VendorState {
    fn default() -> Self {
        Self {
            bills: HashMap::new(),
            topup_error: None,
            lookup_error: None,
            pay_error: None,
            receipt_state: ReceiptState::Completed,
            query_state: ReceiptState::Completed,
            donation_override: None,
            refund_due_minor: 0,
            topup_calls: 0,
            pay_calls: 0,
            cancel_calls: 0,
        }
    }
}

impl VendorState {
    fn ready() -> Self {
        Self::default()
    }
}

#[derive(Clone)]
struct FakeVendor {
    state: Arc<Mutex<VendorState>>,
    calls: Arc<AtomicUsize>,
}

impl FakeVendor {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(VendorState::ready())),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn with<F: FnOnce(&mut VendorState)>(&self, change: F) {
        change(&mut self.state.lock().unwrap());
    }

    fn set_bill(&self, account: &str, amount_minor: i64, signed: bool) {
        let mut bill = VendorBill {
            account: account.to_owned(),
            payee: "state-grid".to_owned(),
            amount_minor,
            currency: "CNY".to_owned(),
            bill_ref: format!("ref-{amount_minor}"),
            signature: String::new(),
        };
        bill.signature = if signed {
            sign_bill(BILL_KEY, &bill)
        } else {
            "00".repeat(32)
        };
        self.with(|state| {
            state.bills.insert(account.to_owned(), bill);
        });
    }
}

fn receipt(state: ReceiptState, business_no: &str) -> VendorReceipt {
    VendorReceipt {
        business_no: business_no.to_owned(),
        state,
        message: format!("{state:?}"),
        issued_at_ms: 1_000,
    }
}

#[async_trait]
impl UtilitiesVendor for FakeVendor {
    async fn lookup_bill(&self, account: &str) -> Result<VendorBill, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let state = self.state.lock().unwrap();
        if let Some(err) = state.lookup_error {
            return Err(err);
        }
        state
            .bills
            .get(account)
            .cloned()
            .ok_or(ProviderError::Rejected("unknown account"))
    }

    async fn mobile_topup(
        &self,
        _phone: &str,
        _amount_minor: i64,
        _idempotency_key: &str,
    ) -> Result<VendorReceipt, ProviderError> {
        let mut state = self.state.lock().unwrap();
        state.topup_calls += 1;
        if let Some(err) = state.topup_error {
            return Err(err);
        }
        Ok(receipt(
            state.receipt_state,
            &format!("topup-{}", state.topup_calls),
        ))
    }

    async fn pay_bill(
        &self,
        bill: &VendorBill,
        _amount_minor: i64,
        _idempotency_key: &str,
    ) -> Result<VendorReceipt, ProviderError> {
        let mut state = self.state.lock().unwrap();
        state.pay_calls += 1;
        if let Some(err) = state.pay_error {
            return Err(err);
        }
        if sign_bill(BILL_KEY, bill) != bill.signature {
            return Err(ProviderError::Rejected("bad signature"));
        }
        Ok(receipt(state.receipt_state, "bill-1"))
    }

    async fn donate(
        &self,
        project_id: &str,
        _amount_minor: i64,
        _idempotency_key: &str,
    ) -> Result<VendorDonation, ProviderError> {
        let state = self.state.lock().unwrap();
        let (institution, project) = match &state.donation_override {
            Some((institution, project)) => (institution.clone(), project.clone()),
            None => ("women-foundation".to_owned(), project_id.to_owned()),
        };
        Ok(VendorDonation {
            receipt: receipt(ReceiptState::Completed, "donation-1"),
            institution,
            project,
        })
    }

    async fn cancel(
        &self,
        _business_no: &str,
        _idempotency_key: &str,
    ) -> Result<ProviderCancellation, ProviderError> {
        let mut state = self.state.lock().unwrap();
        state.cancel_calls += 1;
        let refund_due_minor = state.refund_due_minor;
        Ok(ProviderCancellation {
            accepted: true,
            refund_due_minor,
            message: "cancelled".to_owned(),
        })
    }

    async fn query(&self, _business_no: &str) -> Result<VendorReceipt, ProviderError> {
        let state = self.state.lock().unwrap();
        Ok(receipt(state.query_state, "queried"))
    }
}

struct Harness {
    service: OrderService,
    provider: Arc<UtilitiesProvider<FakeVendor>>,
    vendor: FakeVendor,
    payment: Arc<LedgerPayment>,
    support: Arc<RecordingSupport>,
    #[allow(dead_code)]
    clock: Arc<TestClock>,
}

fn harness() -> Harness {
    harness_with(evidence())
}

fn harness_with(evidence: Option<SandboxEvidence>) -> Harness {
    let vendor = FakeVendor::new();
    let payment = Arc::new(LedgerPayment::default());
    let support = Arc::new(RecordingSupport::default());
    let clock = Arc::new(TestClock::new(1_000));
    let provider = Arc::new(
        UtilitiesProvider::new(
            vendor.clone(),
            BILL_KEY.to_vec(),
            evidence,
            clock.clone(),
            vec![
                DonationProject {
                    project_id: "proj-1".to_owned(),
                    institution: "women-foundation".to_owned(),
                    title: "乡村女童助学".to_owned(),
                    authorized: true,
                },
                DonationProject {
                    project_id: "proj-2".to_owned(),
                    institution: "unknown".to_owned(),
                    title: "未审核项目".to_owned(),
                    authorized: false,
                },
            ],
        )
        .unwrap(),
    );
    let catalog = Arc::new(Catalog::new());
    for (id, kind) in [
        ("topup-cn", ServiceType::MobileTopup),
        ("bill-cn", ServiceType::UtilityBill),
        ("donation-cn", ServiceType::Donation),
    ] {
        catalog.upsert(card(id, kind, "utilities", "CN")).unwrap();
        catalog.set_region_enabled(kind, "CN", true).unwrap();
    }
    let auth = Arc::new(BearerAuthenticator::new(|token: &str| match token {
        "s1" => Some("u1".to_owned()),
        "s2" => Some("u2".to_owned()),
        _ => None,
    }));
    let service = OrderService::new(
        catalog,
        vec![provider.clone()],
        payment.clone(),
        auth,
        clock.clone(),
    )
    .with_support(support.clone());
    Harness {
        service,
        provider,
        vendor,
        payment,
        support,
        clock,
    }
}

fn topup_params(amount_minor: i64) -> Value {
    json!({"phone": "13800138000", "amount_minor": amount_minor})
}

fn request(quote_id: &str, key: &str, amount_minor: i64, params: Value) -> CreateOrderRequest {
    CreateOrderRequest {
        quote_id: quote_id.to_owned(),
        idempotency_key: key.to_owned(),
        confirmed_amount_minor: amount_minor,
        params,
    }
}

#[tokio::test]
async fn topup_quote_is_live_and_duplicate_submission_charges_once() {
    let h = harness();
    let quote = h
        .service
        .quote("s1", "topup-cn", 1, topup_params(1_000))
        .await
        .unwrap();
    assert_eq!(quote.amount_minor, 1_000);
    assert_eq!(quote.provider, "utilities");

    let first = h
        .service
        .create(
            "s1",
            request(&quote.quote_id, "k1", 1_000, topup_params(1_000)),
        )
        .await
        .unwrap();
    assert_eq!(first.state, OrderState::Fulfilled);
    assert_eq!(first.provider_order_id, "topup-1");
    let repeated = h
        .service
        .create(
            "s1",
            request(&quote.quote_id, "k1", 1_000, topup_params(1_000)),
        )
        .await
        .unwrap();
    assert_eq!(repeated.order_id, first.order_id);
    assert_eq!(h.vendor.state.lock().unwrap().topup_calls, 1);
    assert_eq!(h.payment.intent_count(), 1);

    let delivered = h.service.cancel("s1", &first.order_id, "too late").await;
    assert_eq!(delivered.unwrap_err().code(), Code::Conflict);
}

#[tokio::test]
async fn topup_validates_phone_and_amount_before_calling_vendor() {
    let h = harness();
    for params in [
        json!({"phone": "12345", "amount_minor": 1_000}),
        json!({"phone": "13800138000", "amount_minor": 50}),
        json!({"phone": "13800138000", "amount_minor": 1_000_000}),
    ] {
        let err = h
            .service
            .quote("s1", "topup-cn", 1, params)
            .await
            .unwrap_err();
        assert_eq!(err.code(), Code::Conflict);
    }
    assert_eq!(h.vendor.state.lock().unwrap().topup_calls, 0);
}

#[tokio::test]
async fn bill_requires_server_lookup_and_signature_check() {
    let h = harness();
    let params = json!({"account": "A100"});

    let missing = h
        .service
        .quote("s1", "bill-cn", 1, params.clone())
        .await
        .unwrap_err();
    assert_eq!(missing.code(), Code::Conflict);

    h.vendor.set_bill("A100", 1_200, false);
    let tampered = h.provider.lookup_bill("A100").await.unwrap_err();
    assert_eq!(tampered.code(), Code::InvalidArgument);

    h.vendor.set_bill("A100", 1_200, true);
    let bill = h.provider.lookup_bill("A100").await.unwrap();
    assert_eq!(bill.amount_minor, 1_200);

    let quote = h
        .service
        .quote("s1", "bill-cn", 1, params.clone())
        .await
        .unwrap();
    assert_eq!(quote.amount_minor, 1_200);
    let order = h
        .service
        .create("s1", request(&quote.quote_id, "bill-1", 1_200, params))
        .await
        .unwrap();
    assert_eq!(order.state, OrderState::Fulfilled);
    assert_eq!(h.vendor.state.lock().unwrap().pay_calls, 1);
    assert_eq!(h.payment.intent_count(), 1);
}

#[tokio::test]
async fn bill_amount_change_after_quote_blocks_payment() {
    let h = harness();
    let params = json!({"account": "A100"});
    h.vendor.set_bill("A100", 1_200, true);
    h.provider.lookup_bill("A100").await.unwrap();
    let stale = h
        .service
        .quote("s1", "bill-cn", 1, params.clone())
        .await
        .unwrap();
    assert_eq!(stale.amount_minor, 1_200);

    h.vendor.set_bill("A100", 1_500, true);
    h.provider.lookup_bill("A100").await.unwrap();
    let err = h
        .service
        .create("s1", request(&stale.quote_id, "bill-1", 1_200, params))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Conflict);
    assert_eq!(h.payment.intent_count(), 0);
    assert_eq!(h.vendor.state.lock().unwrap().pay_calls, 0);
}

#[tokio::test]
async fn donation_requires_authorized_project_and_matching_payee() {
    let h = harness();
    for project in ["proj-2", "proj-404"] {
        let err = h
            .service
            .quote(
                "s1",
                "donation-cn",
                1,
                json!({"project_id": project, "amount_minor": 500}),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code(), Code::Conflict);
    }

    h.vendor.with(|state| {
        state.donation_override = Some(("offshore-account".to_owned(), "proj-1".to_owned()));
    });
    let params = json!({"project_id": "proj-1", "amount_minor": 500});
    let quote = h
        .service
        .quote("s1", "donation-cn", 1, params.clone())
        .await
        .unwrap();
    let err = h
        .service
        .create("s1", request(&quote.quote_id, "don-1", 500, params))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Conflict);
    assert_eq!(h.payment.intent_count(), 0);
}

#[tokio::test]
async fn vendor_timeout_offline_and_unknown_bill_are_explicit() {
    let h = harness();
    h.vendor
        .with(|state| state.topup_error = Some(ProviderError::Timeout));
    let quote = h
        .service
        .quote("s1", "topup-cn", 1, topup_params(1_000))
        .await
        .unwrap();
    let timeout = h
        .service
        .create(
            "s1",
            request(&quote.quote_id, "k1", 1_000, topup_params(1_000)),
        )
        .await
        .unwrap_err();
    assert_eq!(timeout.code(), Code::Timeout);
    assert_eq!(h.payment.intent_count(), 0);

    h.vendor.with(|state| {
        state.topup_error = Some(ProviderError::Unavailable);
    });
    let offline = h
        .service
        .create(
            "s1",
            request(&quote.quote_id, "k2", 1_000, topup_params(1_000)),
        )
        .await
        .unwrap_err();
    assert_eq!(offline.code(), Code::DependencyUnavailable);

    h.vendor.with(|state| {
        state.lookup_error = Some(ProviderError::Unavailable);
    });
    let lookup = h.provider.lookup_bill("A100").await.unwrap_err();
    assert_eq!(lookup.code(), Code::DependencyUnavailable);

    h.vendor.with(|state| state.lookup_error = None);
    let unknown = h.provider.lookup_bill("A100").await.unwrap_err();
    assert_eq!(unknown.code(), Code::Conflict);
}

#[tokio::test]
async fn cancel_before_delivery_refunds_exactly_once() {
    let h = harness();
    h.vendor.with(|state| {
        state.receipt_state = ReceiptState::Pending;
        state.refund_due_minor = 1_000;
    });
    let quote = h
        .service
        .quote("s1", "topup-cn", 1, topup_params(1_000))
        .await
        .unwrap();
    let order = h
        .service
        .create(
            "s1",
            request(&quote.quote_id, "k1", 1_000, topup_params(1_000)),
        )
        .await
        .unwrap();
    assert_eq!(order.state, OrderState::Paid);

    let cancelled = h
        .service
        .cancel("s1", &order.order_id, "changed mind")
        .await
        .unwrap();
    assert_eq!(cancelled.state, OrderState::Refunded);
    assert_eq!(h.payment.refunded_minor(), 1_000);
    assert_eq!(h.payment.refund_count(), 1);

    let again = h
        .service
        .cancel("s1", &order.order_id, "changed mind")
        .await
        .unwrap();
    assert_eq!(again.state, OrderState::Refunded);
    assert_eq!(h.payment.refund_count(), 1);
}

#[tokio::test]
async fn refund_failure_parks_order_for_reconciliation_with_ticket() {
    let h = harness();
    h.vendor.with(|state| {
        state.receipt_state = ReceiptState::Pending;
        state.refund_due_minor = 1_000;
    });
    let quote = h
        .service
        .quote("s1", "topup-cn", 1, topup_params(1_000))
        .await
        .unwrap();
    let order = h
        .service
        .create(
            "s1",
            request(&quote.quote_id, "k1", 1_000, topup_params(1_000)),
        )
        .await
        .unwrap();
    h.payment.refund_fail.store(true, Ordering::SeqCst);

    let err = h
        .service
        .cancel("s1", &order.order_id, "vendor refund down")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::DependencyUnavailable);
    let parked = h.service.order("s1", &order.order_id).unwrap();
    assert_eq!(parked.state, OrderState::PendingReconciliation);
    assert!(parked.support_ticket.is_some());
    assert_eq!(h.support.count(), 1);
    assert_eq!(h.payment.refund_count(), 0);
}

#[tokio::test]
async fn provider_without_evidence_is_blocked_before_vendor_call() {
    let h = harness_with(None);
    let err = h
        .service
        .quote("s1", "topup-cn", 1, topup_params(1_000))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::BlockedExternal);
    assert_eq!(h.vendor.state.lock().unwrap().topup_calls, 0);
}

#[tokio::test]
async fn provider_routes_only_supported_kinds() {
    let h = harness();
    assert!(wxlike_service_catalog::ServiceProvider::supports(
        h.provider.as_ref(),
        ServiceType::Donation
    ));
    assert!(!wxlike_service_catalog::ServiceProvider::supports(
        h.provider.as_ref(),
        ServiceType::Game
    ));
    let _: &dyn PaymentPort = h.payment.as_ref();
    let _: &dyn SupportPort = h.support.as_ref();
}
