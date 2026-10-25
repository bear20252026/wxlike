//! 订单：支付成功与交付成功是两个状态，不互相代签。

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use wxlike_ledger::{Currency, Money};

use crate::error::{CommerceError, Result};
use crate::payments::{IntentDirection, IntentStatus, PaymentIntent};
use crate::store;
use crate::Commerce;

/// 订单状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderStatus {
    Created,
    Paid,
    Fulfilled,
    Cancelled,
}

impl OrderStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Paid => "paid",
            Self::Fulfilled => "fulfilled",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "created" => Ok(Self::Created),
            "paid" => Ok(Self::Paid),
            "fulfilled" => Ok(Self::Fulfilled),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(CommerceError::BadInput("stored order status".into())),
        }
    }
}

/// 订单。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Order {
    pub id: String,
    pub actor_id: String,
    pub payee_id: String,
    pub quote_ref: String,
    pub amount: Money,
    pub status: OrderStatus,
    pub payment_intent_id: Option<String>,
    pub provider_order_ref: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Commerce {
    /// 创建订单。订单本身不动钱。
    ///
    /// # Errors
    /// 参数非法、金额超限、幂等键冲突或存储失败。
    pub fn create_order(
        &self,
        actor: &str,
        payee: &str,
        quote_ref: &str,
        amount: Money,
        business_key: &str,
    ) -> Result<Order> {
        store::validate_id("actor", actor)?;
        store::validate_id("payee", payee)?;
        store::validate_id("quote", quote_ref)?;
        store::validate_key(business_key)?;
        store::check_limit(amount, self.config.max_payment_minor)?;
        if actor == payee {
            return Err(CommerceError::BadInput(
                "buyer and merchant must differ".into(),
            ));
        }
        let digest = store::digest_hex(&[
            actor,
            payee,
            quote_ref,
            &amount.minor().to_string(),
            amount.currency().as_str(),
        ]);
        let scoped_key = format!("order:{business_key}");
        let now = self.now();
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = load_by_key(&tx, &scoped_key)? {
            if existing.0 != digest {
                return Err(CommerceError::IdempotencyConflict);
            }
            return Ok(existing.1);
        }
        let id = store::new_id(self.entropy());
        tx.execute(
            "INSERT INTO commerce_orders
                (id, business_key, actor_id, payee_id, quote_ref, amount_minor, currency,
                 status, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 'created', ?8, ?8)",
            params![
                id,
                scoped_key,
                actor,
                payee,
                quote_ref,
                amount.minor(),
                amount.currency().as_str(),
                now
            ],
        )?;
        store::emit(
            &tx,
            self.entropy(),
            "order.created",
            &id,
            &serde_json::json!({ "order_id": id, "quote_ref": quote_ref }),
            now,
        )?;
        let order = load(&tx, &id)?.ok_or(CommerceError::NotFound)?;
        tx.commit()?;
        Ok(order)
    }

    /// 支付订单。支付成功只把订单推进到 `paid`，不代替交付。
    ///
    /// # Errors
    /// 无权支付、订单已取消或供应商失败。
    pub fn pay_order(&self, actor: &str, order_id: &str, business_key: &str) -> Result<Order> {
        store::validate_key(business_key)?;
        let order = {
            let conn = self.lock();
            load(&conn, order_id)?.ok_or(CommerceError::NotFound)?
        };
        if order.actor_id != actor {
            return Err(CommerceError::Forbidden);
        }
        match order.status {
            OrderStatus::Paid | OrderStatus::Fulfilled => return Ok(order),
            OrderStatus::Cancelled => {
                return Err(CommerceError::IllegalTransition(
                    "order is cancelled".into(),
                ))
            }
            OrderStatus::Created => {}
        }
        let intent = match order.payment_intent_id.as_deref() {
            Some(intent_id) => self.payment_intent(actor, intent_id)?,
            None => {
                let (intent, _) = self.create_payment_intent(
                    IntentDirection::Order,
                    actor,
                    &order.payee_id,
                    order.amount,
                    &format!("order:{}:{business_key}", order.id),
                )?;
                intent
            }
        };
        let intent = self.pay_payment_intent(actor, &intent.id)?;
        if intent.status != IntentStatus::Succeeded {
            return Ok(order);
        }
        let now = self.now();
        let conn = self.lock();
        conn.execute(
            "UPDATE commerce_orders
             SET status = 'paid', payment_intent_id = ?1, updated_at = ?2,
                 version = version + 1
             WHERE id = ?3 AND status = 'created'",
            params![intent.id, now, order.id],
        )?;
        load(&conn, order_id)?.ok_or(CommerceError::NotFound)
    }

    /// 服务方确认交付完成。
    ///
    /// # Errors
    /// 无权操作、订单未支付、单号非法或状态不允许。
    pub fn fulfil_order(
        &self,
        operator: &str,
        order_id: &str,
        provider_order_ref: &str,
    ) -> Result<Order> {
        store::validate_id("operator", operator)?;
        store::validate_id("provider_ref", provider_order_ref)?;
        let now = self.now();
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let order = load(&tx, order_id)?.ok_or(CommerceError::NotFound)?;
        if order.payee_id != operator {
            return Err(CommerceError::Forbidden);
        }
        match order.status {
            OrderStatus::Fulfilled => {
                if order.provider_order_ref.as_deref() == Some(provider_order_ref) {
                    return Ok(order);
                }
                return Err(CommerceError::Conflict(
                    "order already fulfilled with another reference".into(),
                ));
            }
            OrderStatus::Paid => {}
            _ => return Err(CommerceError::IllegalTransition("order is not paid".into())),
        }
        tx.execute(
            "UPDATE commerce_orders
             SET status = 'fulfilled', provider_order_ref = ?1, updated_at = ?2,
                 version = version + 1
             WHERE id = ?3 AND status = 'paid'",
            params![provider_order_ref, now, order_id],
        )?;
        store::emit(
            &tx,
            self.entropy(),
            "order.fulfilled",
            order_id,
            &serde_json::json!({ "order_id": order_id }),
            now,
        )?;
        let updated = load(&tx, order_id)?.ok_or(CommerceError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }

    /// 取消未支付订单；已支付订单走退款流程。
    ///
    /// # Errors
    /// 无权取消或订单已支付/已交付。
    pub fn cancel_order(&self, actor: &str, order_id: &str) -> Result<Order> {
        let now = self.now();
        let conn = self.lock();
        let order = load(&conn, order_id)?.ok_or(CommerceError::NotFound)?;
        if order.actor_id != actor {
            return Err(CommerceError::Forbidden);
        }
        match order.status {
            OrderStatus::Cancelled => return Ok(order),
            OrderStatus::Created => {}
            _ => {
                return Err(CommerceError::IllegalTransition(
                    "paid or fulfilled orders must be refunded".into(),
                ))
            }
        }
        conn.execute(
            "UPDATE commerce_orders
             SET status = 'cancelled', updated_at = ?1, version = version + 1
             WHERE id = ?2 AND status = 'created'",
            params![now, order_id],
        )?;
        load(&conn, order_id)?.ok_or(CommerceError::NotFound)
    }

    /// 查询订单（买卖双方可见）。
    ///
    /// # Errors
    /// 不存在或无权查看。
    pub fn order(&self, viewer: &str, order_id: &str) -> Result<Order> {
        store::validate_id("viewer", viewer)?;
        let conn = self.lock();
        let order = load(&conn, order_id)?.ok_or(CommerceError::NotFound)?;
        if order.actor_id != viewer && order.payee_id != viewer {
            return Err(CommerceError::Forbidden);
        }
        Ok(order)
    }

    /// 订单对应的支付意图（用于补偿/查账）。
    ///
    /// # Errors
    /// 不存在或无权查看。
    pub fn order_payment(&self, viewer: &str, order_id: &str) -> Result<Option<PaymentIntent>> {
        let order = self.order(viewer, order_id)?;
        match order.payment_intent_id {
            Some(intent_id) => self.payment_intent(viewer, &intent_id).map(Some),
            None => Ok(None),
        }
    }
}

fn load(conn: &Connection, id: &str) -> Result<Option<Order>> {
    let raw = conn
        .query_row(
            "SELECT id, actor_id, payee_id, quote_ref, amount_minor, currency, status,
                    payment_intent_id, provider_order_ref, created_at, updated_at
             FROM commerce_orders WHERE id = ?1",
            params![id],
            scan,
        )
        .optional()?;
    raw.map(build).transpose()
}

fn load_by_key(conn: &Connection, key: &str) -> Result<Option<(String, Order)>> {
    let raw = conn
        .query_row(
            "SELECT id, actor_id, payee_id, quote_ref, amount_minor, currency, status,
                    payment_intent_id, provider_order_ref, created_at, updated_at
             FROM commerce_orders WHERE business_key = ?1",
            params![key],
            scan,
        )
        .optional()?;
    raw.map(|raw| {
        let digest = store::digest_hex(&[
            &raw.actor_id,
            &raw.payee_id,
            &raw.quote_ref,
            &raw.amount_minor.to_string(),
            &raw.currency,
        ]);
        Ok((digest, build(raw)?))
    })
    .transpose()
}

struct RawOrder {
    id: String,
    actor_id: String,
    payee_id: String,
    quote_ref: String,
    amount_minor: i64,
    currency: String,
    status: String,
    payment_intent_id: Option<String>,
    provider_order_ref: Option<String>,
    created_at: i64,
    updated_at: i64,
}

fn scan(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawOrder> {
    Ok(RawOrder {
        id: r.get(0)?,
        actor_id: r.get(1)?,
        payee_id: r.get(2)?,
        quote_ref: r.get(3)?,
        amount_minor: r.get(4)?,
        currency: r.get(5)?,
        status: r.get(6)?,
        payment_intent_id: r.get(7)?,
        provider_order_ref: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
    })
}

fn build(raw: RawOrder) -> Result<Order> {
    Ok(Order {
        id: raw.id,
        actor_id: raw.actor_id,
        payee_id: raw.payee_id,
        quote_ref: raw.quote_ref,
        amount: Money::from_minor(raw.amount_minor, Currency::parse(&raw.currency)?),
        status: OrderStatus::parse(&raw.status)?,
        payment_intent_id: raw.payment_intent_id,
        provider_order_ref: raw.provider_order_ref,
        created_at: raw.created_at,
        updated_at: raw.updated_at,
    })
}
