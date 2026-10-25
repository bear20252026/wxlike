use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::auth::Authenticator;
use crate::catalog::{Catalog, ServiceCard, ServiceType};
use crate::clock::Clock;
use crate::error::{Result, ServiceError, ServiceErrorCode as Code};
use crate::ids::new_id;
use crate::payment::{
    PaymentIntent, PaymentIntentRequest, PaymentPort, RefundReceipt, RefundRequest,
};
use crate::provider::{
    map_provider_error, Availability, ProviderCancellation, ProviderError, ProviderOrder,
    ProviderOrderRequest, ProviderOrderState, ProviderReceipt, ProviderResult, QuoteContext,
    ServiceProvider,
};
use crate::quote::{Quote, QuoteBook};
use crate::support::SupportPort;

pub const DEFAULT_QUOTE_TTL_MS: i64 = 5 * 60 * 1000;
pub const DEFAULT_MAX_QUANTITY: u32 = 20;
const MAX_IDEM_KEY_LEN: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderState {
    Created,
    Paid,
    Fulfilling,
    Fulfilled,
    Cancelling,
    Cancelled,
    Refunding,
    Refunded,
    Failed,
    PendingReconciliation,
}

impl OrderState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Paid => "paid",
            Self::Fulfilling => "fulfilling",
            Self::Fulfilled => "fulfilled",
            Self::Cancelling => "cancelling",
            Self::Cancelled => "cancelled",
            Self::Refunding => "refunding",
            Self::Refunded => "refunded",
            Self::Failed => "failed",
            Self::PendingReconciliation => "pending_reconciliation",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Fulfilled | Self::Cancelled | Self::Refunded | Self::Failed
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderEvent {
    pub state: OrderState,
    pub at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Order {
    pub order_id: String,
    pub actor: String,
    pub quote_id: String,
    pub service_id: String,
    pub kind: ServiceType,
    pub provider: String,
    pub provider_order_id: String,
    pub amount_minor: i64,
    pub currency: String,
    pub state: OrderState,
    pub receipt: Option<ProviderReceipt>,
    pub payment_id: Option<String>,
    pub refund: Option<RefundReceipt>,
    pub support_ticket: Option<String>,
    pub events: Vec<OrderEvent>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl Order {
    fn transition(&mut self, state: OrderState, now_ms: i64) {
        self.state = state;
        self.updated_at_ms = now_ms;
        self.events.push(OrderEvent {
            state,
            at_ms: now_ms,
        });
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateOrderRequest {
    pub quote_id: String,
    pub idempotency_key: String,
    pub confirmed_amount_minor: i64,
    pub params: Value,
}

#[derive(Clone, Debug)]
pub struct OrderConfig {
    pub quote_ttl_ms: i64,
    pub max_quantity: u32,
}

impl Default for OrderConfig {
    fn default() -> Self {
        Self {
            quote_ttl_ms: DEFAULT_QUOTE_TTL_MS,
            max_quantity: DEFAULT_MAX_QUANTITY,
        }
    }
}

enum IdemEntry {
    InFlight,
    Done { hash: String, order_id: String },
}

pub struct OrderService {
    catalog: Arc<Catalog>,
    providers: Vec<Arc<dyn ServiceProvider>>,
    payments: Arc<dyn PaymentPort>,
    auth: Arc<dyn Authenticator>,
    clock: Arc<dyn Clock>,
    support: Option<Arc<dyn SupportPort>>,
    config: OrderConfig,
    quotes: QuoteBook,
    orders: Mutex<HashMap<String, Order>>,
    idem: Mutex<HashMap<String, IdemEntry>>,
}

impl OrderService {
    pub fn new(
        catalog: Arc<Catalog>,
        providers: Vec<Arc<dyn ServiceProvider>>,
        payments: Arc<dyn PaymentPort>,
        auth: Arc<dyn Authenticator>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            catalog,
            providers,
            payments,
            auth,
            clock,
            support: None,
            config: OrderConfig::default(),
            quotes: QuoteBook::new(),
            orders: Mutex::new(HashMap::new()),
            idem: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_support(mut self, support: Arc<dyn SupportPort>) -> Self {
        self.support = Some(support);
        self
    }

    pub fn with_config(mut self, config: OrderConfig) -> Self {
        self.config = config;
        self
    }

    pub async fn quote(
        &self,
        session: &str,
        service_id: &str,
        quantity: u32,
        params: Value,
    ) -> Result<Quote> {
        let actor = self.actor(session)?;
        if quantity == 0 || quantity > self.config.max_quantity {
            return Err(ServiceError::new(Code::InvalidArgument, "quantity"));
        }
        let card = self.catalog.get(service_id)?;
        self.require_card_available(&card)?;
        let provider = self.require_provider(&card)?;
        let availability = provider
            .availability(&QuoteContext {
                actor: &actor,
                card: &card,
                params: &params,
            })
            .await
            .map_err(|err| map_provider_error(provider.id(), err))?;
        let quote = self.build_quote(actor, provider.id(), card, quantity, params, availability)?;
        self.quotes.insert(quote.clone())?;
        Ok(quote)
    }

    pub fn get_quote(&self, session: &str, quote_id: &str) -> Result<Quote> {
        let actor = self.actor(session)?;
        let quote = self.quotes.get(quote_id)?;
        if quote.actor != actor {
            return Err(ServiceError::new(Code::NotFound, "quote not found"));
        }
        Ok(quote)
    }

    pub async fn create(&self, session: &str, request: CreateOrderRequest) -> Result<Order> {
        let actor = self.actor(session)?;
        self.validate_create_input(&request)?;
        let (quote, card) = self.validated_quote(&actor, &request)?;
        let key = format!("{actor}:{}", request.idempotency_key);
        let hash = request_hash(&quote, request.confirmed_amount_minor);
        if let Some(existing) = self.reserve_idem(&key, &hash)? {
            return self.order_by_id(&existing);
        }
        let outcome = self.place(&actor, &quote, &card, &key).await;
        match &outcome {
            Ok(order) => self.commit_idem(&key, &hash, &order.order_id)?,
            Err(_) => {
                if let Some(order) = self.order_for_quote(&actor, &quote.quote_id) {
                    self.commit_idem(&key, &hash, &order.order_id)?;
                } else {
                    self.abort_idem(&key);
                }
            }
        }
        outcome
    }

    pub async fn fulfill(&self, session: &str, order_id: &str) -> Result<Order> {
        let actor = self.actor(session)?;
        let mut order = self.owned_order(&actor, order_id)?;
        if order.state == OrderState::Fulfilled {
            return Ok(order);
        }
        if !matches!(order.state, OrderState::Paid | OrderState::Created) {
            return Err(ServiceError::new(Code::Conflict, "order not fulfillable"));
        }
        let provider = self.provider_by_id(&order.provider)?;
        let now = self.clock.now_ms();
        order.transition(OrderState::Fulfilling, now);
        self.store(order.clone())?;
        let key = format!("{order_id}:fulfill");
        match provider.fulfill(&order.provider_order_id, &key).await {
            Ok(receipt) if receipt.validate() => {
                order.receipt = Some(receipt);
                order.transition(OrderState::Fulfilled, now);
                self.store(order.clone())?;
                Ok(order)
            }
            Ok(_) => Err(ServiceError::new(
                Code::Internal,
                "invalid provider receipt",
            )),
            Err(err) => {
                self.reconcile_failure(&actor, order, provider.id(), err, "fulfill_failed")
                    .await
            }
        }
    }

    pub async fn cancel(&self, session: &str, order_id: &str, reason: &str) -> Result<Order> {
        let actor = self.actor(session)?;
        let mut order = self.owned_order(&actor, order_id)?;
        match order.state {
            OrderState::Cancelled | OrderState::Refunded => return Ok(order),
            OrderState::Fulfilled => {
                return Err(ServiceError::new(
                    Code::Conflict,
                    "delivered order needs refund",
                ));
            }
            OrderState::PendingReconciliation => {
                return Err(ServiceError::new(
                    Code::Conflict,
                    "order awaits reconciliation",
                ));
            }
            _ => {}
        }
        let provider = self.provider_by_id(&order.provider)?;
        order.transition(OrderState::Cancelling, self.clock.now_ms());
        self.store(order.clone())?;
        let key = format!("{order_id}:cancel");
        match provider.cancel(&order.provider_order_id, &key).await {
            Ok(cancellation) if cancellation.accepted => {
                self.settle_cancellation(&actor, order, cancellation, reason)
                    .await
            }
            Ok(_) => {
                let err = ProviderError::Rejected("provider rejected cancellation");
                self.reconcile_failure(&actor, order, provider.id(), err, "cancel_rejected")
                    .await
            }
            Err(err) => {
                self.reconcile_failure(&actor, order, provider.id(), err, "cancel_failed")
                    .await
            }
        }
    }

    pub async fn refund(&self, session: &str, order_id: &str, reason: &str) -> Result<Order> {
        let actor = self.actor(session)?;
        let mut order = self.owned_order(&actor, order_id)?;
        if order.state == OrderState::Refunded {
            return Ok(order);
        }
        if !matches!(order.state, OrderState::Fulfilled | OrderState::Paid) {
            return Err(ServiceError::new(Code::Conflict, "order not refundable"));
        }
        let Some(payment_id) = order.payment_id.clone() else {
            return Err(ServiceError::new(Code::Conflict, "order has no payment"));
        };
        let provider = self.provider_by_id(&order.provider)?;
        let _ = provider
            .cancel(
                &order.provider_order_id,
                &format!("{order_id}:refund-provider"),
            )
            .await;
        order.transition(OrderState::Refunding, self.clock.now_ms());
        self.store(order.clone())?;
        let request = RefundRequest {
            actor: actor.to_owned(),
            order_id: order_id.to_owned(),
            payment_id,
            amount_minor: order.amount_minor,
            currency: order.currency.clone(),
            idempotency_key: format!("{order_id}:refund"),
        };
        match self.payments.refund(&request).await {
            Ok(receipt) => {
                order.refund = Some(receipt);
                order.transition(OrderState::Refunded, self.clock.now_ms());
                self.store(order.clone())?;
                Ok(order)
            }
            Err(err) => {
                self.reconcile_failure(&actor, order, provider.id(), err, reason)
                    .await
            }
        }
    }

    pub async fn query(&self, session: &str, order_id: &str) -> Result<Order> {
        let actor = self.actor(session)?;
        let mut order = self.owned_order(&actor, order_id)?;
        if matches!(
            order.state,
            OrderState::Cancelled | OrderState::Refunded | OrderState::Failed
        ) {
            return Ok(order);
        }
        let provider = self.provider_by_id(&order.provider)?;
        let status = provider
            .query(&order.provider_order_id)
            .await
            .map_err(|err| map_provider_error(provider.id(), err))?;
        self.apply_provider_status(&mut order, status.state, status.receipt, &actor)
            .await?;
        self.store(order.clone())?;
        Ok(order)
    }

    pub fn order(&self, session: &str, order_id: &str) -> Result<Order> {
        let actor = self.actor(session)?;
        self.owned_order(&actor, order_id)
    }

    pub fn purge_expired_quotes(&self) -> usize {
        self.quotes.purge_expired(self.clock.now_ms())
    }

    fn actor(&self, session: &str) -> Result<String> {
        self.auth.authenticate(session)
    }

    fn validate_create_input(&self, request: &CreateOrderRequest) -> Result<()> {
        if request.idempotency_key.is_empty() || request.idempotency_key.len() > MAX_IDEM_KEY_LEN {
            return Err(ServiceError::new(Code::InvalidArgument, "idempotency key"));
        }
        if request.confirmed_amount_minor <= 0 {
            return Err(ServiceError::new(Code::InvalidArgument, "confirmed amount"));
        }
        if request.params.is_null() {
            return Err(ServiceError::new(Code::InvalidArgument, "params"));
        }
        Ok(())
    }

    fn validated_quote(
        &self,
        actor: &str,
        request: &CreateOrderRequest,
    ) -> Result<(Quote, ServiceCard)> {
        let quote = self.quotes.get(&request.quote_id)?;
        if quote.actor != actor {
            return Err(ServiceError::new(Code::NotFound, "quote not found"));
        }
        if quote.expires_at_ms <= self.clock.now_ms() {
            return Err(ServiceError::new(Code::Expired, "quote expired"));
        }
        if request.confirmed_amount_minor != quote.amount_minor {
            return Err(ServiceError::new(
                Code::Conflict,
                "amount changed; re-confirm",
            ));
        }
        if request.params != quote.params {
            return Err(ServiceError::new(Code::InvalidArgument, "params changed"));
        }
        let card = self.catalog.get(&quote.service_id)?;
        if card.revision != quote.card_revision {
            return Err(ServiceError::new(Code::Conflict, "price changed; re-quote"));
        }
        self.require_card_available(&card)?;
        Ok((quote, card))
    }

    fn require_card_available(&self, card: &ServiceCard) -> Result<()> {
        if !card.enabled {
            return Err(ServiceError::new(Code::BlockedExternal, "service disabled"));
        }
        if !self.catalog.region_enabled(card.kind, &card.region) {
            return Err(ServiceError::new(
                Code::BlockedExternal,
                "region unavailable",
            ));
        }
        Ok(())
    }

    fn require_provider(&self, card: &ServiceCard) -> Result<&Arc<dyn ServiceProvider>> {
        let provider = self
            .providers
            .iter()
            .find(|candidate| candidate.id() == card.provider && candidate.supports(card.kind))
            .ok_or_else(|| ServiceError::new(Code::BlockedExternal, "no provider configured"))?;
        let authorized = provider
            .sandbox_evidence()
            .is_some_and(|evidence| evidence.is_usable());
        if !authorized {
            return Err(ServiceError::new(
                Code::BlockedExternal,
                format!("{}: sandbox evidence missing", provider.id()),
            ));
        }
        Ok(provider)
    }

    fn provider_by_id(&self, id: &str) -> Result<Arc<dyn ServiceProvider>> {
        self.providers
            .iter()
            .find(|candidate| candidate.id() == id)
            .cloned()
            .ok_or_else(|| ServiceError::new(Code::BlockedExternal, "provider unavailable"))
    }

    fn build_quote(
        &self,
        actor: String,
        provider: &str,
        card: ServiceCard,
        quantity: u32,
        params: Value,
        availability: Availability,
    ) -> Result<Quote> {
        let now = self.clock.now_ms();
        if !availability.available {
            return Err(ServiceError::new(Code::Conflict, "service unavailable"));
        }
        if availability.amount_minor <= 0 || availability.currency != card.currency {
            return Err(ServiceError::new(
                Code::Internal,
                "provider quoted invalid price",
            ));
        }
        if availability.terms_version.is_empty() || availability.valid_until_ms <= now {
            return Err(ServiceError::new(
                Code::DependencyUnavailable,
                "offer expired",
            ));
        }
        let amount_minor = availability
            .amount_minor
            .checked_mul(i64::from(quantity))
            .ok_or_else(|| ServiceError::new(Code::InvalidArgument, "amount overflow"))?;
        Ok(Quote {
            quote_id: new_id("q"),
            actor,
            service_id: card.service_id,
            kind: card.kind,
            provider: provider.to_owned(),
            region: card.region,
            params,
            quantity,
            amount_minor,
            currency: card.currency,
            terms_version: availability.terms_version,
            card_revision: card.revision,
            created_at_ms: now,
            expires_at_ms: now + self.config.quote_ttl_ms.max(1),
        })
    }

    async fn place(
        &self,
        actor: &str,
        quote: &Quote,
        card: &ServiceCard,
        idem_key: &str,
    ) -> Result<Order> {
        let provider = self.require_provider(card)?;
        let now = self.clock.now_ms();
        let order_id = new_id("ord");
        let request = provider_request(actor, quote, card, idem_key);
        let placed = provider
            .place_order(&request)
            .await
            .map_err(|err| map_provider_error(provider.id(), err))?;
        validate_placed_receipt(&placed)?;
        let mut order = new_order(
            order_id.clone(),
            actor,
            quote,
            provider.id(),
            placed.provider_order_id,
            now,
        );
        let intent = self
            .payments
            .create_intent(&PaymentIntentRequest {
                actor: actor.to_owned(),
                order_id,
                amount_minor: quote.amount_minor,
                currency: quote.currency.clone(),
                idempotency_key: format!("{idem_key}:pay"),
            })
            .await;
        let settled = self
            .settle_intent(actor, &mut order, placed.state, placed.receipt, intent)
            .await;
        if settled
            .as_ref()
            .is_err_and(|err| err.code() == Code::PaymentDeclined)
        {
            let _ = provider
                .cancel(&order.provider_order_id, &format!("{idem_key}:cancel"))
                .await;
        }
        settled
    }

    async fn settle_intent(
        &self,
        actor: &str,
        order: &mut Order,
        provider_state: ProviderOrderState,
        receipt: Option<ProviderReceipt>,
        intent: ProviderResult<PaymentIntent>,
    ) -> Result<Order> {
        let intent = match intent {
            Ok(intent) => intent,
            Err(err) => {
                let failed = order.clone();
                return self
                    .reconcile_failure(actor, failed, order.provider.as_str(), err, "payment_error")
                    .await;
            }
        };
        order.payment_id = Some(intent.payment_id);
        if !intent.succeeded {
            order.transition(OrderState::Failed, self.clock.now_ms());
            order.support_ticket = self
                .ticket(actor, &order.order_id, "payment_declined")
                .await;
            self.store(order.clone())?;
            return Err(ServiceError::new(Code::PaymentDeclined, "payment declined"));
        }
        if provider_state == ProviderOrderState::Fulfilled {
            order.receipt = receipt;
            order.transition(OrderState::Fulfilled, self.clock.now_ms());
        } else if matches!(
            provider_state,
            ProviderOrderState::Accepted | ProviderOrderState::Pending
        ) {
            order.transition(OrderState::Paid, self.clock.now_ms());
        } else {
            order.transition(OrderState::PendingReconciliation, self.clock.now_ms());
            order.support_ticket = self
                .ticket(actor, &order.order_id, "provider_pending")
                .await;
        }
        self.store(order.clone())?;
        Ok(order.clone())
    }

    async fn settle_cancellation(
        &self,
        actor: &str,
        mut order: Order,
        cancellation: ProviderCancellation,
        reason: &str,
    ) -> Result<Order> {
        let refund_due = cancellation.refund_due_minor.min(order.amount_minor);
        let Some(payment_id) = order.payment_id.clone().filter(|_| refund_due > 0) else {
            order.transition(OrderState::Cancelled, self.clock.now_ms());
            self.store(order.clone())?;
            return Ok(order);
        };
        order.transition(OrderState::Refunding, self.clock.now_ms());
        self.store(order.clone())?;
        let request = RefundRequest {
            actor: actor.to_owned(),
            order_id: order.order_id.clone(),
            payment_id,
            amount_minor: refund_due,
            currency: order.currency.clone(),
            idempotency_key: format!("{}:cancel-refund", order.order_id),
        };
        match self.payments.refund(&request).await {
            Ok(receipt) => {
                order.refund = Some(receipt);
                order.transition(OrderState::Refunded, self.clock.now_ms());
                self.store(order.clone())?;
                Ok(order)
            }
            Err(err) => {
                let provider = order.provider.clone();
                self.reconcile_failure(actor, order, &provider, err, reason)
                    .await
            }
        }
    }

    async fn reconcile_failure(
        &self,
        actor: &str,
        mut order: Order,
        provider: &str,
        err: ProviderError,
        reason: &str,
    ) -> Result<Order> {
        order.transition(OrderState::PendingReconciliation, self.clock.now_ms());
        order.support_ticket = self.ticket(actor, &order.order_id, reason).await;
        self.store(order)?;
        Err(map_provider_error(provider, err))
    }

    async fn apply_provider_status(
        &self,
        order: &mut Order,
        state: ProviderOrderState,
        receipt: Option<ProviderReceipt>,
        actor: &str,
    ) -> Result<()> {
        match state {
            ProviderOrderState::Fulfilled => {
                order.receipt = receipt;
                order.transition(OrderState::Fulfilled, self.clock.now_ms());
            }
            ProviderOrderState::Cancelled => {
                order.transition(OrderState::Cancelled, self.clock.now_ms());
            }
            ProviderOrderState::Failed => {
                order.transition(OrderState::Failed, self.clock.now_ms());
                order.support_ticket = self.ticket(actor, &order.order_id, "provider_failed").await;
            }
            ProviderOrderState::Accepted | ProviderOrderState::Pending => {}
        }
        Ok(())
    }

    async fn ticket(&self, actor: &str, order_id: &str, reason: &str) -> Option<String> {
        let support = self.support.as_ref()?;
        support.open_ticket(actor, order_id, reason).await.ok()
    }

    fn owned_order(&self, actor: &str, order_id: &str) -> Result<Order> {
        let orders = self.orders.lock().map_err(|_| internal())?;
        let order = orders
            .get(order_id)
            .ok_or_else(|| ServiceError::new(Code::NotFound, "order not found"))?;
        if order.actor != actor {
            return Err(ServiceError::new(Code::NotFound, "order not found"));
        }
        Ok(order.clone())
    }

    fn order_by_id(&self, order_id: &str) -> Result<Order> {
        self.orders
            .lock()
            .map_err(|_| internal())?
            .get(order_id)
            .cloned()
            .ok_or_else(|| ServiceError::new(Code::NotFound, "order not found"))
    }

    fn order_for_quote(&self, actor: &str, quote_id: &str) -> Option<Order> {
        let orders = self.orders.lock().ok()?;
        orders
            .values()
            .find(|order| order.actor == actor && order.quote_id == quote_id)
            .cloned()
    }

    fn store(&self, order: Order) -> Result<()> {
        self.orders
            .lock()
            .map_err(|_| internal())?
            .insert(order.order_id.clone(), order);
        Ok(())
    }

    fn reserve_idem(&self, key: &str, hash: &str) -> Result<Option<String>> {
        let mut entries = self.idem.lock().map_err(|_| internal())?;
        match entries.get(key) {
            Some(IdemEntry::Done {
                hash: existing,
                order_id,
            }) if existing == hash => Ok(Some(order_id.clone())),
            Some(_) => Err(ServiceError::new(Code::Conflict, "idempotency key reused")),
            None => {
                entries.insert(key.to_owned(), IdemEntry::InFlight);
                Ok(None)
            }
        }
    }

    fn commit_idem(&self, key: &str, hash: &str, order_id: &str) -> Result<()> {
        self.idem.lock().map_err(|_| internal())?.insert(
            key.to_owned(),
            IdemEntry::Done {
                hash: hash.to_owned(),
                order_id: order_id.to_owned(),
            },
        );
        Ok(())
    }

    fn abort_idem(&self, key: &str) {
        if let Ok(mut entries) = self.idem.lock() {
            entries.remove(key);
        }
    }
}

fn provider_request(
    actor: &str,
    quote: &Quote,
    card: &ServiceCard,
    idem_key: &str,
) -> ProviderOrderRequest {
    ProviderOrderRequest {
        actor: actor.to_owned(),
        service_id: card.service_id.clone(),
        kind: card.kind,
        region: card.region.clone(),
        amount_minor: quote.amount_minor,
        currency: quote.currency.clone(),
        params: quote.params.clone(),
        idempotency_key: idem_key.to_owned(),
    }
}

fn validate_placed_receipt(placed: &ProviderOrder) -> Result<()> {
    if placed
        .receipt
        .as_ref()
        .is_some_and(|receipt| !receipt.validate())
    {
        return Err(ServiceError::new(
            Code::Internal,
            "invalid provider receipt",
        ));
    }
    Ok(())
}

fn new_order(
    order_id: String,
    actor: &str,
    quote: &Quote,
    provider: &str,
    provider_order_id: String,
    now_ms: i64,
) -> Order {
    Order {
        order_id,
        actor: actor.to_owned(),
        quote_id: quote.quote_id.clone(),
        service_id: quote.service_id.clone(),
        kind: quote.kind,
        provider: provider.to_owned(),
        provider_order_id,
        amount_minor: quote.amount_minor,
        currency: quote.currency.clone(),
        state: OrderState::Created,
        receipt: None,
        payment_id: None,
        refund: None,
        support_ticket: None,
        events: vec![OrderEvent {
            state: OrderState::Created,
            at_ms: now_ms,
        }],
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    }
}

fn request_hash(quote: &Quote, confirmed_amount_minor: i64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(quote.quote_id.as_bytes());
    hasher.update(quote.service_id.as_bytes());
    hasher.update(confirmed_amount_minor.to_le_bytes());
    hasher.update(quote.params.to_string().as_bytes());
    hex::encode(hasher.finalize())
}

fn internal() -> ServiceError {
    ServiceError::new(Code::Internal, "order state")
}
