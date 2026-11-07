//! G07-04 真实仓储验收：订单/报价/幂等键一律走 [`FileStore`] 落盘。
//!
//! 这里的 provider/payment 只是供应商故障注入的边界替身（issue 明确允许），
//! 持久化、恢复、幂等、并发全部由真实仓储承担。

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use wxlike_service_catalog::auth::{Authenticator, BearerAuthenticator};
use wxlike_service_catalog::{
    Availability, Catalog, Clock, CreateOrderRequest, FileStore, Order, OrderConfig, OrderService,
    OrderState, PaymentIntent, PaymentIntentRequest, PaymentPort, ProviderCancellation,
    ProviderError, ProviderOrder, ProviderOrderRequest, ProviderOrderState, ProviderReceipt,
    ProviderStatus, Quote, RefundReceipt, RefundRequest, SandboxEvidence, ServiceCard,
    ServiceErrorCode as Code, ServiceProvider, ServiceType,
};

struct FakeClock(Mutex<i64>);

impl Clock for FakeClock {
    fn now_ms(&self) -> i64 {
        *self.0.lock().unwrap()
    }
}

impl FakeClock {
    fn new(now: i64) -> Arc<Self> {
        Arc::new(Self(Mutex::new(now)))
    }

    fn advance(&self, delta: i64) {
        *self.0.lock().unwrap() += delta;
    }
}

#[derive(Default)]
struct VendorState {
    price_minor: i64,
    fulfill_error: Option<ProviderError>,
    status: Option<ProviderOrderState>,
    place_calls: usize,
    cancel_accepted: bool,
    refund_due_minor: i64,
    provider_seq: u32,
}

impl VendorState {
    fn ready() -> Self {
        Self {
            price_minor: 1_000,
            status: Some(ProviderOrderState::Fulfilled),
            cancel_accepted: true,
            ..Self::default()
        }
    }
}

/// 只用于故障注入的供应商替身；真实供应商适配器在 G07-03 侧。
struct FaultProvider {
    state: Mutex<VendorState>,
}

impl FaultProvider {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(VendorState::ready()),
        })
    }

    fn with<F: FnOnce(&mut VendorState)>(&self, change: F) {
        change(&mut self.state.lock().unwrap());
    }

    fn place_calls(&self) -> usize {
        self.state.lock().unwrap().place_calls
    }
}

fn receipt(status: &str) -> ProviderReceipt {
    ProviderReceipt {
        business_no: format!("biz-{status}"),
        status: status.to_owned(),
        message: "ok".to_owned(),
        issued_at_ms: 1,
    }
}

#[async_trait]
impl ServiceProvider for FaultProvider {
    fn id(&self) -> &'static str {
        "file-vendor"
    }

    fn supports(&self, kind: ServiceType) -> bool {
        matches!(kind, ServiceType::MobileTopup)
    }

    fn sandbox_evidence(&self) -> Option<SandboxEvidence> {
        Some(SandboxEvidence {
            environment: "sandbox".to_owned(),
            reference: "g07-file-store-run".to_owned(),
            verified_at_ms: 1,
        })
    }

    async fn availability(
        &self,
        _ctx: &wxlike_service_catalog::QuoteContext<'_>,
    ) -> Result<Availability, ProviderError> {
        let price = self.state.lock().unwrap().price_minor;
        Ok(Availability {
            available: true,
            amount_minor: price,
            currency: "CNY".to_owned(),
            terms_version: "terms-v1".to_owned(),
            valid_until_ms: 10_000_000,
            note: None,
        })
    }

    async fn place_order(
        &self,
        request: &ProviderOrderRequest,
    ) -> Result<ProviderOrder, ProviderError> {
        let mut state = self.state.lock().unwrap();
        state.place_calls += 1;
        state.provider_seq += 1;
        assert_eq!(request.amount_minor, state.price_minor);
        Ok(ProviderOrder {
            provider_order_id: format!("vo-{}", state.provider_seq),
            state: ProviderOrderState::Accepted,
            receipt: Some(receipt("accepted")),
        })
    }

    async fn fulfill(&self, _id: &str, _key: &str) -> Result<ProviderReceipt, ProviderError> {
        let mut state = self.state.lock().unwrap();
        if let Some(err) = state.fulfill_error.take() {
            return Err(err);
        }
        state.status = Some(ProviderOrderState::Fulfilled);
        Ok(receipt("fulfilled"))
    }

    async fn cancel(&self, _id: &str, _key: &str) -> Result<ProviderCancellation, ProviderError> {
        let state = self.state.lock().unwrap();
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
struct CountingPayment {
    intents: AtomicUsize,
    refunds: AtomicUsize,
}

#[async_trait]
impl PaymentPort for CountingPayment {
    async fn create_intent(
        &self,
        request: &PaymentIntentRequest,
    ) -> Result<PaymentIntent, ProviderError> {
        self.intents.fetch_add(1, Ordering::SeqCst);
        Ok(PaymentIntent {
            payment_id: format!("pay-{}", request.order_id),
            succeeded: true,
            decline_reason: None,
        })
    }

    async fn refund(&self, request: &RefundRequest) -> Result<RefundReceipt, ProviderError> {
        self.refunds.fetch_add(1, Ordering::SeqCst);
        Ok(RefundReceipt {
            refund_id: format!("refund-{}", request.order_id),
            amount_minor: request.amount_minor,
            status: "succeeded".to_owned(),
        })
    }
}

/// 落盘目录句柄；同一路径可以有多个句柄，drop 时统一清理。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let unique = SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "wxlike-sc-143-{tag}-{unique}-{:016x}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.path());
    }
}

struct Harness {
    /// 仅为持有同一目录句柄（drop 时清理）；读取一律经测试本地 TempDir。
    #[allow(dead_code)]
    dir: TempDir,
    provider: Arc<FaultProvider>,
    payment: Arc<CountingPayment>,
    clock: Arc<FakeClock>,
    catalog: Arc<Catalog>,
    service: OrderService,
}

/// 「重启」语义：新 OrderService + 新供应商/支付替身 + 新时钟起点，
/// 但仓储指向同一目录——订单/报价/幂等必须由落盘文件原样恢复。
async fn restart(dir: &TempDir, now: i64) -> Harness {
    let provider = FaultProvider::new();
    let clock = FakeClock::new(now);
    let catalog = Arc::new(Catalog::new());
    catalog
        .upsert(ServiceCard {
            service_id: "topup-file".to_owned(),
            kind: ServiceType::MobileTopup,
            title: "手机充值".to_owned(),
            provider: "file-vendor".to_owned(),
            region: "CN".to_owned(),
            currency: "CNY".to_owned(),
            display_price_minor: 1_000,
            terms_version: "terms-v1".to_owned(),
            enabled: true,
            revision: 0,
        })
        .unwrap();
    catalog
        .set_region_enabled(ServiceType::MobileTopup, "CN", true)
        .unwrap();
    let store = Arc::new(FileStore::open(dir.path()).await.unwrap());
    let payment = Arc::new(CountingPayment::default());
    let auth: Arc<dyn Authenticator> =
        Arc::new(BearerAuthenticator::new(|token: &str| match token {
            "s1" => Some("u1".to_owned()),
            "s2" => Some("u2".to_owned()),
            _ => None,
        }));
    let service = OrderService::with_store(
        catalog.clone(),
        vec![provider.clone()],
        payment.clone(),
        auth,
        clock.clone(),
        store,
    )
    .with_config(OrderConfig {
        quote_ttl_ms: 5_000,
        max_quantity: 5,
    });
    Harness {
        dir: TempDir(dir.path().to_path_buf()),
        provider,
        payment,
        clock,
        catalog,
        service,
    }
}

fn params() -> Value {
    json!({"phone": "13800138000", "face": 100})
}

async fn quote(h: &Harness) -> Quote {
    h.service
        .quote("s1", "topup-file", 1, params())
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

fn order_docs(dir: &std::path::Path) -> Vec<String> {
    let mut texts = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir.join("orders")) {
        for entry in entries.flatten() {
            if let Ok(bytes) = std::fs::read(entry.path()) {
                texts.push(String::from_utf8_lossy(&bytes).into_owned());
            }
        }
    }
    texts
}

#[tokio::test]
async fn full_flow_facts_survive_restart() {
    let dir = TempDir::new("full");
    let h = restart(&dir, 1_000).await;
    let q = quote(&h).await;
    let created = h
        .service
        .create("s1", request(&q.quote_id, "k1"))
        .await
        .unwrap();
    assert_eq!(created.state, OrderState::Paid);
    let fulfilled = h.service.fulfill("s1", &created.order_id).await.unwrap();
    assert_eq!(fulfilled.state, OrderState::Fulfilled);

    let h2 = restart(&dir, 1_000).await;
    let restored: Order = h2.service.order("s1", &created.order_id).await.unwrap();
    assert_eq!(restored, fulfilled);
    assert_eq!(restored.provider_order_id, "vo-1");
    assert_eq!(
        restored.payment_id.as_deref(),
        Some(format!("pay-{}", restored.order_id).as_str())
    );
    // 重启后重复履约请求直接命中终态，不再触达供应商。
    let again = h2.service.fulfill("s1", &created.order_id).await.unwrap();
    assert_eq!(again.state, OrderState::Fulfilled);
    assert_eq!(h2.provider.place_calls(), 0);
}

#[tokio::test]
async fn offline_between_payment_and_delivery_recovers_after_restart() {
    let dir = TempDir::new("offline");
    let h = restart(&dir, 1_000).await;
    let q = quote(&h).await;
    let created = h
        .service
        .create("s1", request(&q.quote_id, "k1"))
        .await
        .unwrap();
    // 付款成功后、交付确认前断网一次（一次性故障注入）。
    h.provider
        .with(|state| state.fulfill_error = Some(ProviderError::Timeout));
    let err = h
        .service
        .fulfill("s1", &created.order_id)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Timeout);

    // 重启：待对账状态必须还在，而不是丢单或假装失败。
    let h2 = restart(&dir, 1_000).await;
    let parked = h2.service.order("s1", &created.order_id).await.unwrap();
    assert_eq!(parked.state, OrderState::PendingReconciliation);

    // 网络恢复后查询即收敛到履约终态；不重复扣款、不重复下单。
    let recovered = h2.service.query("s1", &created.order_id).await.unwrap();
    assert_eq!(recovered.state, OrderState::Fulfilled);
    assert_eq!(recovered.receipt.as_ref().unwrap().status, "queried");
    assert_eq!(h2.provider.place_calls(), 0);
    assert_eq!(h2.payment.intents.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn duplicate_submit_returns_same_stored_order_even_after_restart() {
    let dir = TempDir::new("dup");
    let h = restart(&dir, 1_000).await;
    let q = quote(&h).await;
    let first = h
        .service
        .create("s1", request(&q.quote_id, "k1"))
        .await
        .unwrap();
    let dup = h
        .service
        .create("s1", request(&q.quote_id, "k1"))
        .await
        .unwrap();
    assert_eq!(dup.order_id, first.order_id);
    assert_eq!(h.provider.place_calls(), 1);

    // 重启后重放同一幂等键：仍命中同一条落盘订单，不新增供应商订单、不二次扣款。
    let h2 = restart(&dir, 1_000).await;
    let replay = h2
        .service
        .create("s1", request(&q.quote_id, "k1"))
        .await
        .unwrap();
    assert_eq!(replay.order_id, first.order_id);
    assert_eq!(h2.provider.place_calls(), 0);
    assert_eq!(h2.payment.intents.load(Ordering::SeqCst), 0);

    // 同键不同请求（新报价）必须撞 Conflict，且不得破坏已定稿记录。
    let q2 = quote(&h2).await;
    let clash = h2
        .service
        .create("s1", request(&q2.quote_id, "k1"))
        .await
        .unwrap_err();
    assert_eq!(clash.code(), Code::Conflict);
    let still = h2
        .service
        .create("s1", request(&q.quote_id, "k1"))
        .await
        .unwrap();
    assert_eq!(still.order_id, first.order_id);
}

#[tokio::test]
async fn concurrent_submits_reserve_once() {
    let dir = TempDir::new("conc");
    let h = restart(&dir, 1_000).await;
    let q = quote(&h).await;
    let service = Arc::new(h.service);
    let mut tasks = Vec::new();
    for _ in 0..5 {
        let service = Arc::clone(&service);
        let q = q.clone();
        tasks.push(tokio::spawn(async move {
            service.create("s1", request(&q.quote_id, "race")).await
        }));
    }
    let mut succeeded = 0;
    let mut conflicts = 0;
    let mut order_id = String::new();
    for task in tasks {
        match task.await.expect("task join") {
            Ok(order) => {
                succeeded += 1;
                order_id = order.order_id;
            }
            Err(err) if err.code() == Code::Conflict => conflicts += 1,
            Err(err) => panic!("unexpected create error: {err}"),
        }
    }
    assert_eq!(succeeded, 1, "exactly one submit may create the order");
    assert_eq!(conflicts, 4, "the rest must be rejected, not duplicated");
    let docs = order_docs(dir.path());
    assert_eq!(docs.len(), 1);
    assert!(docs[0].contains(&format!("\"order_id\":\"{order_id}\"")));

    // 定稿后同键同请求重复提交：返回同一订单而不是失败。
    let after = service
        .create("s1", request(&q.quote_id, "race"))
        .await
        .unwrap();
    assert_eq!(after.order_id, order_id);
}

#[tokio::test]
async fn other_account_cannot_read_or_operate_stored_orders() {
    let dir = TempDir::new("aos");
    let h = restart(&dir, 1_000).await;
    let q = quote(&h).await;
    let created = h
        .service
        .create("s1", request(&q.quote_id, "k1"))
        .await
        .unwrap();

    let h2 = restart(&dir, 1_000).await;
    assert_eq!(
        h2.service
            .order("s2", &created.order_id)
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );
    assert_eq!(
        h2.service
            .fulfill("s2", &created.order_id)
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );
    assert_eq!(
        h2.service
            .cancel("s2", &created.order_id, "x")
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );
    assert_eq!(
        h2.service
            .get_quote("s2", &q.quote_id)
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );
    assert_eq!(
        h2.service
            .order("bad", &created.order_id)
            .await
            .unwrap_err()
            .code(),
        Code::Unauthorized
    );
    // 越权者即便直读仓储文件，也拿不到别人的幂等定稿：键含 actor 前缀。
    let foreign_docs = order_docs(dir.path());
    assert!(foreign_docs
        .iter()
        .all(|text| text.contains("\"actor\":\"u1\"")));
}

#[tokio::test]
async fn bad_inputs_and_missing_rows_are_explicit() {
    let dir = TempDir::new("bad");
    let h = restart(&dir, 1_000).await;
    let q = quote(&h).await;
    let empty_key = CreateOrderRequest {
        idempotency_key: String::new(),
        ..request(&q.quote_id, "k")
    };
    assert_eq!(
        h.service.create("s1", empty_key).await.unwrap_err().code(),
        Code::InvalidArgument
    );
    let bad_amount = CreateOrderRequest {
        confirmed_amount_minor: 0,
        ..request(&q.quote_id, "k")
    };
    assert_eq!(
        h.service.create("s1", bad_amount).await.unwrap_err().code(),
        Code::InvalidArgument
    );
    // 空结果：不存在的订单/报价给出显式错误码，而不是 panic 或假数据。
    assert_eq!(
        h.service
            .order("s1", "ord_missing")
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );
    assert_eq!(
        h.service
            .get_quote("s1", "q_missing")
            .await
            .unwrap_err()
            .code(),
        Code::Expired
    );
    // 报价过期：TTL 后建单被拒，purge 把报价文档物理删除。
    h.clock.advance(6_000);
    assert_eq!(
        h.service
            .create("s1", request(&q.quote_id, "k9"))
            .await
            .unwrap_err()
            .code(),
        Code::Expired
    );
    assert_eq!(h.service.purge_expired_quotes().await, 1);
    assert_eq!(
        h.service
            .get_quote("s1", &q.quote_id)
            .await
            .unwrap_err()
            .code(),
        Code::Expired
    );
    assert_eq!(h.service.purge_expired_quotes().await, 0);
}

#[tokio::test]
async fn cancel_with_refund_is_persisted_and_idempotent() {
    let dir = TempDir::new("cancel");
    let h = restart(&dir, 1_000).await;
    let q = quote(&h).await;
    let created = h
        .service
        .create("s1", request(&q.quote_id, "k1"))
        .await
        .unwrap();
    h.provider.with(|state| state.refund_due_minor = 1_000);
    let cancelled = h
        .service
        .cancel("s1", &created.order_id, "user_cancel")
        .await
        .unwrap();
    assert_eq!(cancelled.state, OrderState::Refunded);
    assert_eq!(h.payment.refunds.load(Ordering::SeqCst), 1);

    let h2 = restart(&dir, 1_000).await;
    let again = h2
        .service
        .cancel("s1", &created.order_id, "user_cancel")
        .await
        .unwrap();
    assert_eq!(again, cancelled);
    assert_eq!(h2.payment.refunds.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn stored_facts_are_minimal_and_free_of_raw_pii() {
    let dir = TempDir::new("pii");
    let h = restart(&dir, 1_000).await;
    let q = quote(&h).await;
    let created = h
        .service
        .create("s1", request(&q.quote_id, "k1"))
        .await
        .unwrap();
    h.service.fulfill("s1", &created.order_id).await.unwrap();

    let docs = order_docs(dir.path());
    assert_eq!(docs.len(), 1);
    let all = docs.concat();
    // 订单文档只存事实与 quote 引用：手机号等业务参数不得落进订单记录。
    assert!(!all.contains("13800138000"), "order doc leaked raw phone");
    // 手机号只存在于短生命周期的报价文档中，过期即被 purge 物理删除。
    let quote_file = dir
        .path()
        .join("quotes")
        .join(format!("{}.json", q.quote_id));
    let quote_text = String::from_utf8_lossy(&std::fs::read(&quote_file).unwrap()).into_owned();
    assert!(quote_text.contains("13800138000"));
    h.clock.advance(6_000);
    assert_eq!(h.service.purge_expired_quotes().await, 1);
    assert!(
        !quote_file.exists(),
        "expired quote must be deleted, not just hidden"
    );
}

#[tokio::test]
async fn price_change_blocks_stale_quote_confirmation() {
    let dir = TempDir::new("price");
    let h = restart(&dir, 1_000).await;
    let q = quote(&h).await;
    h.catalog.update_price("topup-file", 1_100).unwrap();
    // 改价不静默生效：旧报价建单一律 Conflict，要求重新确认。
    let err = h
        .service
        .create("s1", request(&q.quote_id, "k1"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Conflict);
    assert_eq!(h.provider.place_calls(), 0);

    // 「重启」后管理端 bootstrap 同样带上最新 revision，旧报价依旧被拒。
    let h2 = restart(&dir, 1_000).await;
    h2.catalog.update_price("topup-file", 1_100).unwrap();
    let err = h2
        .service
        .create("s1", request(&q.quote_id, "k1"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Conflict);
    assert_eq!(h2.provider.place_calls(), 0);
    // 新报价 + 明确确认金额才放行，且价格来自供应商实时应答。
    let q2 = quote(&h2).await;
    let ok = h2
        .service
        .create("s1", request(&q2.quote_id, "k2"))
        .await
        .unwrap();
    assert_eq!(ok.state, OrderState::Paid);
    assert_eq!(h2.provider.place_calls(), 1);
}
