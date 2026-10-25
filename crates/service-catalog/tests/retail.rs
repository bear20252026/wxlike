mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;
use wxlike_service_catalog::auth::BearerAuthenticator;
use wxlike_service_catalog::providers::retail::{
    CartItem, CartQuote, Product, RetailService, RetailVendor, VendorOrder, VendorOrderStatus,
};
use wxlike_service_catalog::{
    Catalog, CreateOrderRequest, OrderService, OrderState, ProviderCancellation, ProviderError,
    ProviderOrderState, ServiceErrorCode as Code, ServiceType,
};

use common::{card, evidence, LedgerPayment, TestClock};

fn product(sku: &str, price_minor: i64, stock: u32, region: &str) -> Product {
    Product {
        sku: sku.to_owned(),
        title: format!("{sku} title"),
        price_minor,
        currency: "CNY".to_owned(),
        stock,
        region: region.to_owned(),
        refundable: true,
    }
}

#[derive(Default)]
struct VendorState {
    products: HashMap<String, Product>,
    checkout_state: Option<ProviderOrderState>,
    cancel_accept: bool,
    refund_due_minor: i64,
    search_error: Option<ProviderError>,
    checkout_error: Option<ProviderError>,
    checkout_calls: usize,
    cancel_calls: usize,
    orders: HashMap<String, VendorOrderStatus>,
}

#[derive(Clone)]
struct FakeVendor {
    state: Arc<Mutex<VendorState>>,
}

impl FakeVendor {
    fn new() -> Self {
        let mut products = HashMap::new();
        for item in [
            product("sku-1", 500, 10, "CN"),
            product("sku-2", 300, 0, "CN"),
            product("sku-us", 700, 5, "US"),
        ] {
            products.insert(item.sku.clone(), item);
        }
        Self {
            state: Arc::new(Mutex::new(VendorState {
                products,
                checkout_state: Some(ProviderOrderState::Fulfilled),
                cancel_accept: true,
                refund_due_minor: 1_000,
                ..VendorState::default()
            })),
        }
    }

    fn with<F: FnOnce(&mut VendorState)>(&self, change: F) {
        change(&mut self.state.lock().unwrap());
    }

    fn reprice(&self, sku: &str, price_minor: i64) {
        self.with(|state| {
            state.products.get_mut(sku).unwrap().price_minor = price_minor;
        });
    }
}

#[async_trait]
impl RetailVendor for FakeVendor {
    async fn search(&self, _query: &str, _region: &str) -> Result<Vec<Product>, ProviderError> {
        let state = self.state.lock().unwrap();
        if let Some(err) = state.search_error {
            return Err(err);
        }
        Ok(state.products.values().cloned().collect())
    }

    async fn product(&self, sku: &str) -> Result<Product, ProviderError> {
        let state = self.state.lock().unwrap();
        state
            .products
            .get(sku)
            .cloned()
            .ok_or(ProviderError::Rejected("unknown sku"))
    }

    async fn quote_cart(
        &self,
        items: &[CartItem],
        _region: &str,
    ) -> Result<CartQuote, ProviderError> {
        let state = self.state.lock().unwrap();
        let mut amount_minor = 0;
        for item in items {
            let product = state
                .products
                .get(&item.sku)
                .ok_or(ProviderError::Rejected("unknown sku"))?;
            amount_minor += product.price_minor * i64::from(item.quantity);
        }
        Ok(CartQuote {
            amount_minor,
            currency: "CNY".to_owned(),
            terms_version: "cart-v1".to_owned(),
        })
    }

    async fn checkout(
        &self,
        _cart_id: &str,
        _items: &[CartItem],
        _idempotency_key: &str,
    ) -> Result<VendorOrder, ProviderError> {
        let mut state = self.state.lock().unwrap();
        state.checkout_calls += 1;
        if let Some(err) = state.checkout_error {
            return Err(err);
        }
        let order_id = format!("vendor-{}", state.checkout_calls);
        let order_state = state.checkout_state.unwrap_or(ProviderOrderState::Accepted);
        state.orders.insert(
            order_id.clone(),
            VendorOrderStatus {
                state: order_state,
                message: "accepted".to_owned(),
            },
        );
        Ok(VendorOrder {
            vendor_order_id: order_id,
            state: order_state,
            message: "accepted".to_owned(),
        })
    }

    async fn cancel(
        &self,
        vendor_order_id: &str,
        _idempotency_key: &str,
    ) -> Result<ProviderCancellation, ProviderError> {
        let mut state = self.state.lock().unwrap();
        state.cancel_calls += 1;
        if state.cancel_accept {
            if let Some(order) = state.orders.get_mut(vendor_order_id) {
                order.state = ProviderOrderState::Cancelled;
            }
        }
        Ok(ProviderCancellation {
            accepted: state.cancel_accept,
            refund_due_minor: state.refund_due_minor,
            message: "cancelled".to_owned(),
        })
    }

    async fn query(&self, vendor_order_id: &str) -> Result<VendorOrderStatus, ProviderError> {
        let state = self.state.lock().unwrap();
        state
            .orders
            .get(vendor_order_id)
            .cloned()
            .ok_or(ProviderError::Rejected("unknown order"))
    }
}

struct Harness {
    service: OrderService,
    provider: Arc<RetailService<FakeVendor>>,
    vendor: FakeVendor,
    payment: Arc<LedgerPayment>,
}

fn harness() -> Harness {
    let vendor = FakeVendor::new();
    let clock = Arc::new(TestClock::new(1_000));
    let provider = Arc::new(RetailService::new(
        vendor.clone(),
        evidence(),
        clock.clone(),
    ));
    let catalog = Arc::new(Catalog::new());
    catalog
        .upsert(card("retail-cn", ServiceType::Retail, "retail", "CN"))
        .unwrap();
    catalog
        .set_region_enabled(ServiceType::Retail, "CN", true)
        .unwrap();
    let payment = Arc::new(LedgerPayment::default());
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
        clock,
    );
    Harness {
        service,
        provider,
        vendor,
        payment,
    }
}

#[tokio::test]
async fn search_hides_out_of_stock_and_other_regions() {
    let h = harness();
    let results = h.provider.search("", "CN").await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].sku, "sku-1");
    assert!(h.provider.search("", "JP").await.unwrap().is_empty());
}

#[tokio::test]
async fn cart_enforces_owner_region_and_stock() {
    let h = harness();
    let cart = h
        .provider
        .add_to_cart("u1", "CN", "sku-1", 2)
        .await
        .unwrap();
    assert_eq!(cart.items.len(), 1);

    assert_eq!(
        h.provider
            .add_to_cart("u1", "CN", "sku-1", 9)
            .await
            .unwrap_err()
            .code(),
        Code::Conflict
    );
    assert_eq!(
        h.provider
            .add_to_cart("u1", "CN", "sku-1", 0)
            .await
            .unwrap_err()
            .code(),
        Code::InvalidArgument
    );
    assert_eq!(
        h.provider
            .add_to_cart("u2", "CN", "sku-us", 1)
            .await
            .unwrap_err()
            .code(),
        Code::BlockedExternal
    );
    assert_eq!(
        h.provider.detail("sku-404").await.unwrap_err().code(),
        Code::Conflict
    );

    let other = h
        .provider
        .add_to_cart("u2", "CN", "sku-1", 1)
        .await
        .unwrap();
    let stolen = h
        .service
        .quote("s1", "retail-cn", 1, json!({"cart_id": other.cart_id}))
        .await
        .unwrap_err();
    assert_eq!(stolen.code(), Code::Conflict);
    assert!(h.provider.clear_cart("u1"));
    assert!(h.provider.cart("u1").is_none());
    assert!(h.provider.cart("u2").is_some());
    assert_eq!(
        h.service
            .quote("s1", "retail-cn", 1, json!({"cart_id": cart.cart_id}))
            .await
            .unwrap_err()
            .code(),
        Code::Conflict
    );
}

#[tokio::test]
async fn quote_uses_server_cart_and_price_change_blocks_checkout() {
    let h = harness();
    let cart = h
        .provider
        .add_to_cart("u1", "CN", "sku-1", 2)
        .await
        .unwrap();
    let params = json!({"cart_id": cart.cart_id});
    let quote = h
        .service
        .quote("s1", "retail-cn", 1, params.clone())
        .await
        .unwrap();
    assert_eq!(quote.amount_minor, 1_000);

    h.vendor.reprice("sku-1", 600);
    let err = h
        .service
        .create(
            "s1",
            CreateOrderRequest {
                quote_id: quote.quote_id,
                idempotency_key: "retail-1".to_owned(),
                confirmed_amount_minor: 1_000,
                params,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Conflict);
    assert_eq!(h.payment.intent_count(), 0);
    assert_eq!(h.vendor.state.lock().unwrap().checkout_calls, 0);
}

#[tokio::test]
async fn retail_order_checkout_is_fulfilled_with_provider_receipt() {
    let h = harness();
    let cart = h
        .provider
        .add_to_cart("u1", "CN", "sku-1", 2)
        .await
        .unwrap();
    let params = json!({"cart_id": cart.cart_id});
    let quote = h
        .service
        .quote("s1", "retail-cn", 1, params.clone())
        .await
        .unwrap();
    let order = h
        .service
        .create(
            "s1",
            CreateOrderRequest {
                quote_id: quote.quote_id,
                idempotency_key: "retail-2".to_owned(),
                confirmed_amount_minor: 1_000,
                params,
            },
        )
        .await
        .unwrap();
    assert_eq!(order.state, OrderState::Fulfilled);
    assert_eq!(order.receipt.as_ref().unwrap().status, "fulfilled");
    assert_eq!(h.payment.intent_count(), 1);
}

#[tokio::test]
async fn retail_cancel_before_delivery_refunds() {
    let h = harness();
    h.vendor.with(|state| {
        state.checkout_state = Some(ProviderOrderState::Accepted);
        state.refund_due_minor = 1_000;
    });
    let cart = h
        .provider
        .add_to_cart("u1", "CN", "sku-1", 2)
        .await
        .unwrap();
    let params = json!({"cart_id": cart.cart_id});
    let quote = h
        .service
        .quote("s1", "retail-cn", 1, params.clone())
        .await
        .unwrap();
    let order = h
        .service
        .create(
            "s1",
            CreateOrderRequest {
                quote_id: quote.quote_id,
                idempotency_key: "retail-3".to_owned(),
                confirmed_amount_minor: 1_000,
                params,
            },
        )
        .await
        .unwrap();
    assert_eq!(order.state, OrderState::Paid);

    let cancelled = h
        .service
        .cancel("s1", &order.order_id, "user changed mind")
        .await
        .unwrap();
    assert_eq!(cancelled.state, OrderState::Refunded);
    assert_eq!(h.payment.refunded_minor(), 1_000);
    assert_eq!(h.vendor.state.lock().unwrap().cancel_calls, 1);
}

#[tokio::test]
async fn cart_caps_at_twenty_distinct_skus() {
    let h = harness();
    h.vendor.with(|state| {
        for index in 0..21 {
            let sku = format!("bulk-{index}");
            state
                .products
                .insert(sku.clone(), product(&sku, 100, 1, "CN"));
        }
    });
    for index in 0..20 {
        let sku = format!("bulk-{index}");
        h.provider.add_to_cart("u1", "CN", &sku, 1).await.unwrap();
    }
    let err = h
        .provider
        .add_to_cart("u1", "CN", "bulk-20", 1)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Conflict);
    assert_eq!(h.provider.cart("u1").unwrap().items.len(), 20);
}
