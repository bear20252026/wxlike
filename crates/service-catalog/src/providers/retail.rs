use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::prelude::*;

pub const OFFER_TTL_MS: i64 = 3 * 60 * 1000;
pub const MAX_CART_ITEMS: usize = 20;
pub const MAX_ITEM_QUANTITY: u32 = 99;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Product {
    pub sku: String,
    pub title: String,
    pub price_minor: i64,
    pub currency: String,
    pub stock: u32,
    pub region: String,
    pub refundable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CartItem {
    pub sku: String,
    pub quantity: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cart {
    pub cart_id: String,
    pub actor: String,
    pub region: String,
    pub items: Vec<CartItem>,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CartQuote {
    pub amount_minor: i64,
    pub currency: String,
    pub terms_version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VendorOrder {
    pub vendor_order_id: String,
    pub state: ProviderOrderState,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VendorOrderStatus {
    pub state: ProviderOrderState,
    pub message: String,
}

#[async_trait]
pub trait RetailVendor: Send + Sync {
    async fn search(&self, query: &str, region: &str) -> ProviderResult<Vec<Product>>;

    async fn product(&self, sku: &str) -> ProviderResult<Product>;

    async fn quote_cart(&self, items: &[CartItem], region: &str) -> ProviderResult<CartQuote>;

    async fn checkout(
        &self,
        cart_id: &str,
        items: &[CartItem],
        idempotency_key: &str,
    ) -> ProviderResult<VendorOrder>;

    async fn cancel(
        &self,
        vendor_order_id: &str,
        idempotency_key: &str,
    ) -> ProviderResult<ProviderCancellation>;

    async fn query(&self, vendor_order_id: &str) -> ProviderResult<VendorOrderStatus>;
}

#[derive(Deserialize)]
struct CartParams {
    cart_id: String,
}

pub struct RetailService<V: RetailVendor> {
    vendor: V,
    evidence: Option<SandboxEvidence>,
    clock: Arc<dyn Clock>,
    carts: Mutex<HashMap<String, Cart>>,
}

impl<V: RetailVendor> RetailService<V> {
    pub fn new(vendor: V, evidence: Option<SandboxEvidence>, clock: Arc<dyn Clock>) -> Self {
        Self {
            vendor,
            evidence,
            clock,
            carts: Mutex::new(HashMap::new()),
        }
    }

    pub async fn search(&self, query: &str, region: &str) -> Result<Vec<Product>> {
        let products = self
            .vendor
            .search(query, region)
            .await
            .map_err(|err| crate::provider::map_provider_error(self.id(), err))?;
        Ok(products
            .into_iter()
            .filter(|product| product.region == region && product.stock > 0)
            .collect())
    }

    pub async fn detail(&self, sku: &str) -> Result<Product> {
        self.vendor
            .product(sku)
            .await
            .map_err(|err| crate::provider::map_provider_error(self.id(), err))
    }

    pub async fn add_to_cart(
        &self,
        actor: &str,
        region: &str,
        sku: &str,
        quantity: u32,
    ) -> Result<Cart> {
        if quantity == 0 || quantity > MAX_ITEM_QUANTITY {
            return Err(ServiceError::new(Code::InvalidArgument, "quantity"));
        }
        let product = self.detail(sku).await?;
        if product.region != region {
            return Err(ServiceError::new(
                Code::BlockedExternal,
                "region unavailable",
            ));
        }
        let mut carts = self.carts.lock().map_err(|_| internal())?;
        let now = self.clock.now_ms();
        let cart = carts.entry(actor.to_owned()).or_insert_with(|| Cart {
            cart_id: new_id("cart"),
            actor: actor.to_owned(),
            region: region.to_owned(),
            items: Vec::new(),
            updated_at_ms: now,
        });
        if cart.region != region {
            return Err(ServiceError::new(Code::InvalidArgument, "cart region"));
        }
        let existing = cart.items.iter().position(|item| item.sku == sku);
        let total = match existing {
            Some(index) => cart.items[index].quantity.saturating_add(quantity),
            None => quantity,
        };
        if total > MAX_ITEM_QUANTITY || total > product.stock {
            return Err(ServiceError::new(Code::Conflict, "out of stock"));
        }
        if cart.items.len() >= MAX_CART_ITEMS && existing.is_none() {
            return Err(ServiceError::new(Code::Conflict, "cart full"));
        }
        match existing {
            Some(index) => cart.items[index].quantity = total,
            None => cart.items.push(CartItem {
                sku: sku.to_owned(),
                quantity,
            }),
        }
        cart.updated_at_ms = now;
        Ok(cart.clone())
    }

    pub fn cart(&self, actor: &str) -> Option<Cart> {
        self.carts.lock().ok()?.get(actor).cloned()
    }

    pub fn clear_cart(&self, actor: &str) -> bool {
        self.carts
            .lock()
            .map(|mut carts| carts.remove(actor).is_some())
            .unwrap_or(false)
    }

    fn cart_for(&self, actor: &str, params: &Value) -> ProviderResult<Cart> {
        let parsed: CartParams = serde_json::from_value(params.clone())
            .map_err(|_| ProviderError::Rejected("cart params"))?;
        let cart = self
            .carts
            .lock()
            .ok()
            .and_then(|carts| carts.get(actor).cloned())
            .ok_or(ProviderError::Rejected("cart missing"))?;
        if cart.cart_id != parsed.cart_id || cart.items.is_empty() {
            return Err(ProviderError::Rejected("cart mismatch"));
        }
        Ok(cart)
    }
}

fn status_of(state: ProviderOrderState) -> &'static str {
    match state {
        ProviderOrderState::Accepted => "accepted",
        ProviderOrderState::Fulfilled => "fulfilled",
        ProviderOrderState::Pending => "pending",
        ProviderOrderState::Cancelled => "cancelled",
        ProviderOrderState::Failed => "failed",
    }
}

#[async_trait]
impl<V: RetailVendor> ServiceProvider for RetailService<V> {
    fn id(&self) -> &'static str {
        "retail"
    }

    fn supports(&self, kind: ServiceType) -> bool {
        kind == ServiceType::Retail
    }

    fn sandbox_evidence(&self) -> Option<SandboxEvidence> {
        self.evidence.clone()
    }

    fn support_hint(&self) -> Option<String> {
        Some("support://service?provider=retail".to_owned())
    }

    async fn availability(&self, ctx: &QuoteContext<'_>) -> ProviderResult<Availability> {
        let cart = self.cart_for(ctx.actor, ctx.params)?;
        let quote = self.vendor.quote_cart(&cart.items, &cart.region).await?;
        if quote.amount_minor <= 0 {
            return Err(ProviderError::Rejected("empty cart"));
        }
        Ok(Availability {
            available: true,
            amount_minor: quote.amount_minor,
            currency: quote.currency,
            terms_version: quote.terms_version,
            valid_until_ms: self.clock.now_ms() + OFFER_TTL_MS,
            note: None,
        })
    }

    async fn place_order(&self, request: &ProviderOrderRequest) -> ProviderResult<ProviderOrder> {
        let cart = self.cart_for(&request.actor, &request.params)?;
        let quote = self.vendor.quote_cart(&cart.items, &cart.region).await?;
        if quote.amount_minor != request.amount_minor || quote.currency != request.currency {
            return Err(ProviderError::Rejected("cart price changed"));
        }
        let order = self
            .vendor
            .checkout(&cart.cart_id, &cart.items, &request.idempotency_key)
            .await?;
        if order.vendor_order_id.is_empty() {
            return Err(ProviderError::Rejected("missing vendor order"));
        }
        Ok(ProviderOrder {
            provider_order_id: order.vendor_order_id.clone(),
            state: order.state,
            receipt: Some(ProviderReceipt {
                business_no: order.vendor_order_id,
                status: status_of(order.state).to_owned(),
                message: order.message,
                issued_at_ms: self.clock.now_ms(),
            }),
        })
    }

    async fn fulfill(&self, order_ref: &str, _key: &str) -> ProviderResult<ProviderReceipt> {
        let status = self.vendor.query(order_ref).await?;
        if status.state != ProviderOrderState::Fulfilled {
            return Err(ProviderError::Rejected("not delivered"));
        }
        Ok(ProviderReceipt {
            business_no: order_ref.to_owned(),
            status: "fulfilled".to_owned(),
            message: status.message,
            issued_at_ms: self.clock.now_ms(),
        })
    }

    async fn cancel(&self, order_ref: &str, key: &str) -> ProviderResult<ProviderCancellation> {
        self.vendor.cancel(order_ref, key).await
    }

    async fn query(&self, order_ref: &str) -> ProviderResult<ProviderStatus> {
        let status = self.vendor.query(order_ref).await?;
        Ok(ProviderStatus {
            state: status.state,
            receipt: Some(ProviderReceipt {
                business_no: order_ref.to_owned(),
                status: status_of(status.state).to_owned(),
                message: status.message,
                issued_at_ms: self.clock.now_ms(),
            }),
        })
    }
}

fn internal() -> ServiceError {
    ServiceError::new(Code::Internal, "cart state")
}
