//! 支付意图、二维码与供应商回调。
//!
//! 关键约束：
//! * 供应商网络调用永远在数据库事务之外；
//! * 成功入账只在收到"成功"证据时发生，未知状态不本地写已支付；
//! * 业务键 + 请求摘要保证重复提交只产生一笔；
//! * 回调按原始 body 验签、校验时间窗、事件 ID 去重，状态只向前走。

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Deserialize;
use wxlike_ledger::{Currency, Money, PostingLine, PostingRequest};

use crate::error::{CommerceError, Result};
use crate::ports::{map_provider, ProviderIntentRequest, ProviderPaymentStatus};
use crate::store;
use crate::Commerce;

/// 支付意图状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntentStatus {
    Created,
    Pending,
    Succeeded,
    Failed,
    Expired,
    PartiallyRefunded,
    Refunded,
}

impl IntentStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Pending => "pending",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Expired => "expired",
            Self::PartiallyRefunded => "partially_refunded",
            Self::Refunded => "refunded",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "created" => Some(Self::Created),
            "pending" => Some(Self::Pending),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "expired" => Some(Self::Expired),
            "partially_refunded" => Some(Self::PartiallyRefunded),
            "refunded" => Some(Self::Refunded),
            _ => None,
        }
    }
}

/// 资金方向。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntentDirection {
    /// 用户充值。
    Topup,
    /// 订单收款。
    Order,
}

impl IntentDirection {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Topup => "topup",
            Self::Order => "order",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "topup" => Some(Self::Topup),
            "order" => Some(Self::Order),
            _ => None,
        }
    }
}

/// 支付意图。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaymentIntent {
    pub id: String,
    pub direction: IntentDirection,
    pub actor_id: String,
    pub payee_id: String,
    pub amount: Money,
    pub status: IntentStatus,
    pub provider_ref: Option<String>,
    pub refunded_minor: i64,
    pub expires_at: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl PaymentIntent {
    /// 可退金额。
    #[must_use]
    pub fn refundable(&self) -> Money {
        Money::from_minor(
            (self.amount.minor() - self.refunded_minor).max(0),
            self.amount.currency(),
        )
    }
}

/// 二维码票据：明文只返回一次，库存只保存签名所需的摘要。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaymentTicket {
    pub intent_id: String,
    pub token: String,
    pub expires_at: i64,
}

/// 回调处理结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebhookOutcome {
    pub event_id: String,
    pub applied: bool,
    pub status: IntentStatus,
}

#[derive(Clone, Debug)]
struct IntentRow {
    intent: PaymentIntent,
    nonce_hash: String,
}

#[derive(Deserialize)]
struct WebhookPayload {
    event_id: String,
    intent_id: String,
    kind: String,
    provider_ref: String,
    amount_minor: i64,
    currency: String,
    timestamp_ms: i64,
}

impl Commerce {
    /// 创建支付意图并返回二维码票据。
    ///
    /// `payer` 是扫码/下单方，`payee` 是收款方；充值场景两者相同。
    ///
    /// # Errors
    /// 参数非法、金额超限、业务键冲突或供应商失败。
    pub fn create_payment_intent(
        &self,
        direction: IntentDirection,
        actor: &str,
        payee: &str,
        amount: Money,
        business_key: &str,
    ) -> Result<(PaymentIntent, PaymentTicket)> {
        store::validate_id("actor", actor)?;
        store::validate_id("payee", payee)?;
        store::validate_key(business_key)?;
        store::check_limit(amount, self.config.max_payment_minor)?;
        let now = self.now();
        let row = self.prepare_intent(direction, actor, payee, amount, business_key, now)?;
        let ticket = self.ticket(&row)?;
        let intent = self.advance_provider(&row)?;
        Ok((intent, ticket))
    }

    fn prepare_intent(
        &self,
        direction: IntentDirection,
        actor: &str,
        payee: &str,
        amount: Money,
        business_key: &str,
        now: i64,
    ) -> Result<IntentRow> {
        let scoped_key = format!("intent:{business_key}");
        let digest = store::digest_hex(&[
            direction.as_str(),
            actor,
            payee,
            &amount.minor().to_string(),
            amount.currency().as_str(),
        ]);
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = load_by_business_key(&tx, &scoped_key)? {
            if existing.0 != digest {
                return Err(CommerceError::IdempotencyConflict);
            }
            return Ok(existing.1);
        }
        let id = store::new_id(self.entropy());
        let mut nonce = [0u8; 16];
        self.entropy().fill(&mut nonce);
        let nonce_hash = store::digest_hex(&["nonce", &hex::encode(nonce)]);
        let expires_at = now + self.config.payment_ttl_ms;
        self.ensure_wallet_conn(&tx, actor, amount.currency(), now)?;
        self.ensure_wallet_conn(&tx, payee, amount.currency(), now)?;
        tx.execute(
            "INSERT INTO commerce_payment_intents
                (id, business_key, request_digest, direction, actor_id, payee_id,
                 amount_minor, currency, status, provider, provider_ref, nonce_hash,
                 qr_token_hash, expires_at, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'created', ?9, NULL, ?10, ?11, ?12, ?13, ?13)",
            params![
                id,
                scoped_key,
                digest,
                direction.as_str(),
                actor,
                payee,
                amount.minor(),
                amount.currency().as_str(),
                self.provider_name(),
                nonce_hash,
                String::new(),
                expires_at,
                now
            ],
        )?;
        let row = load_intent(&tx, &id)?.ok_or(CommerceError::NotFound)?;
        tx.commit()?;
        Ok(row)
    }

    /// 推进供应商侧状态（网络调用在事务外）。
    fn advance_provider(&self, row: &IntentRow) -> Result<PaymentIntent> {
        if row.intent.provider_ref.is_some() || row.intent.status != IntentStatus::Created {
            return Ok(row.intent.clone());
        }
        let request = ProviderIntentRequest {
            idempotency_key: format!("intent:{}", row.intent.id),
            amount: row.intent.amount,
            payee: row.intent.payee_id.clone(),
            metadata_ref: row.intent.id.clone(),
        };
        match self.ports.provider.create_intent(&request) {
            Ok(created) => {
                if created.status == ProviderPaymentStatus::Succeeded {
                    let mut guard = self.lock();
                    let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    apply_success(&tx, self, &row.intent.id, &created.provider_ref, self.now())?;
                    tx.commit()?;
                } else {
                    let now = self.now();
                    let mut guard = self.lock();
                    let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    tx.execute(
                        "UPDATE commerce_payment_intents
                         SET status = 'pending', provider_ref = ?1, updated_at = ?2,
                             version = version + 1
                         WHERE id = ?3 AND status = 'created'",
                        params![created.provider_ref, now, row.intent.id],
                    )?;
                    tx.commit()?;
                }
                self.payment_intent(&row.intent.actor_id, &row.intent.id)
            }
            Err(error) => {
                if error == crate::ProviderError::Rejected {
                    let now = self.now();
                    let guard = self.lock();
                    guard.execute(
                        "UPDATE commerce_payment_intents
                         SET status = 'failed', updated_at = ?1, version = version + 1
                         WHERE id = ?2 AND status = 'created'",
                        params![now, row.intent.id],
                    )?;
                }
                Err(map_provider(error))
            }
        }
    }

    /// 读取支付意图（下单方或收款方可见）。
    ///
    /// # Errors
    /// 不存在或无权查看。
    pub fn payment_intent(&self, viewer: &str, intent_id: &str) -> Result<PaymentIntent> {
        store::validate_id("viewer", viewer)?;
        store::validate_id("intent", intent_id)?;
        let conn = self.lock();
        let row = load_intent(&conn, intent_id)?.ok_or(CommerceError::NotFound)?;
        if row.intent.actor_id != viewer && row.intent.payee_id != viewer {
            return Err(CommerceError::Forbidden);
        }
        Ok(row.intent)
    }

    /// 付款（沙箱按钮或钱包确认）。金额与收款人已在意图里冻结。
    ///
    /// # Errors
    /// 过期、非法状态、供应商超时/拒绝。
    pub fn pay_payment_intent(&self, payer: &str, intent_id: &str) -> Result<PaymentIntent> {
        store::validate_id("payer", payer)?;
        store::validate_id("intent", intent_id)?;
        let now = self.now();
        let row = {
            let conn = self.lock();
            load_intent(&conn, intent_id)?.ok_or(CommerceError::NotFound)?
        };
        if row.intent.payee_id == payer && row.intent.direction == IntentDirection::Order {
            return Err(CommerceError::Forbidden);
        }
        match row.intent.status {
            IntentStatus::Expired => return Err(CommerceError::Expired),
            IntentStatus::Succeeded | IntentStatus::Refunded | IntentStatus::PartiallyRefunded => {
                return Ok(row.intent)
            }
            IntentStatus::Failed => {
                return Err(CommerceError::IllegalTransition("intent failed".into()))
            }
            IntentStatus::Created | IntentStatus::Pending => {}
        }
        if now >= row.intent.expires_at {
            self.expire_intent(intent_id)?;
            return Err(CommerceError::Expired);
        }
        let provider_ref = match row.intent.provider_ref.clone() {
            Some(reference) => reference,
            None => {
                let updated = self.advance_provider(&row)?;
                match updated.provider_ref.clone() {
                    Some(reference) => reference,
                    None => return Err(CommerceError::ProviderUnavailable),
                }
            }
        };
        let payment = self
            .ports
            .provider
            .pay(&provider_ref)
            .map_err(map_provider)?;
        if payment.amount != row.intent.amount {
            return Err(CommerceError::BadInput(
                "provider amount does not match the intent".into(),
            ));
        }
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        match payment.status {
            ProviderPaymentStatus::Succeeded => {
                apply_success(&tx, self, intent_id, &provider_ref, self.now())?;
            }
            ProviderPaymentStatus::Failed => {
                let now = self.now();
                tx.execute(
                    "UPDATE commerce_payment_intents
                     SET status = 'failed', updated_at = ?1, version = version + 1
                     WHERE id = ?2 AND status IN ('created', 'pending')",
                    params![now, intent_id],
                )?;
            }
            ProviderPaymentStatus::Pending => {}
        }
        let row = load_intent(&tx, intent_id)?.ok_or(CommerceError::NotFound)?;
        tx.commit()?;
        Ok(row.intent)
    }

    /// 扫码付款：票据绑定意图 ID、金额、币种、收款方与过期时间。
    ///
    /// # Errors
    /// 票据被篡改/过期、意图不可支付或供应商失败。
    pub fn pay_with_ticket(&self, payer: &str, ticket: &str) -> Result<PaymentIntent> {
        let intent_id = self.verify_ticket(ticket)?;
        self.pay_payment_intent(payer, &intent_id)
    }

    /// 校验票据并返回意图 ID。
    ///
    /// # Errors
    /// 格式非法、签名不符、字段与意图不一致或已过期。
    pub fn verify_ticket(&self, ticket: &str) -> Result<String> {
        let parts: Vec<&str> = ticket.split('.').collect();
        if parts.len() != 7 || parts[0] != "v1" {
            return Err(CommerceError::InvalidSignature);
        }
        let intent_id = parts[1];
        let amount: i64 = parts[2]
            .parse()
            .map_err(|_| CommerceError::InvalidSignature)?;
        let currency = Currency::parse(parts[3]).map_err(|_| CommerceError::InvalidSignature)?;
        let payee_hash = parts[4];
        let expires: i64 = parts[5]
            .parse()
            .map_err(|_| CommerceError::InvalidSignature)?;
        let signature = parts[6];
        let conn = self.lock();
        let row = load_intent(&conn, intent_id)?.ok_or(CommerceError::NotFound)?;
        let now = self.now();
        if expires != row.intent.expires_at
            || amount != row.intent.amount.minor()
            || currency != row.intent.amount.currency()
            || payee_hash != payee_tag(&row.intent.payee_id)
        {
            return Err(CommerceError::InvalidSignature);
        }
        let expected = ticket_signature(self, &row);
        if !constant_time_eq(&expected, signature) {
            return Err(CommerceError::InvalidSignature);
        }
        if now >= expires {
            return Err(CommerceError::Expired);
        }
        Ok(intent_id.to_string())
    }

    /// 当前票据字符串（同一个意图每次派生结果一致，不存明文）。
    fn ticket(&self, row: &IntentRow) -> Result<PaymentTicket> {
        Ok(PaymentTicket {
            intent_id: row.intent.id.clone(),
            token: format!(
                "v1.{}.{}.{}.{}.{}.{}",
                row.intent.id,
                row.intent.amount.minor(),
                row.intent.amount.currency(),
                payee_tag(&row.intent.payee_id),
                row.intent.expires_at,
                ticket_signature(self, row)
            ),
            expires_at: row.intent.expires_at,
        })
    }

    /// 过期未支付的意图标记为过期（不发供应商请求）。
    ///
    /// # Errors
    /// 存储失败。
    pub fn expire_payment_intents(&self) -> Result<usize> {
        let now = self.now();
        let conn = self.lock();
        Ok(conn.execute(
            "UPDATE commerce_payment_intents
             SET status = 'expired', updated_at = ?1, version = version + 1
             WHERE status IN ('created', 'pending') AND expires_at <= ?1",
            params![now],
        )?)
    }

    fn expire_intent(&self, intent_id: &str) -> Result<()> {
        let now = self.now();
        let conn = self.lock();
        conn.execute(
            "UPDATE commerce_payment_intents
             SET status = 'expired', updated_at = ?1, version = version + 1
             WHERE id = ?2 AND status IN ('created', 'pending')",
            params![now, intent_id],
        )?;
        Ok(())
    }

    /// 回调签名（供应商与接入方共用同一规范）。
    #[must_use]
    pub fn webhook_signature(&self, raw_body: &str) -> String {
        store::hmac_hex(self.config.webhook_secret.as_slice(), &[raw_body])
    }

    /// 处理供应商回调。
    ///
    /// # Errors
    /// 签名错误、时间窗外、金额/币种不符、重复事件或存储失败。
    pub fn handle_webhook(&self, raw_body: &str, signature: &str) -> Result<WebhookOutcome> {
        if !store::hmac_verify(
            self.config.webhook_secret.as_slice(),
            &[raw_body],
            signature,
        ) {
            return Err(CommerceError::InvalidSignature);
        }
        let payload: WebhookPayload = serde_json::from_str(raw_body)
            .map_err(|_| CommerceError::BadInput("webhook body".into()))?;
        store::validate_id("event", &payload.event_id)?;
        store::validate_id("intent", &payload.intent_id)?;
        let now = self.now();
        if (now - payload.timestamp_ms).abs() > self.config.callback_skew_ms {
            return Err(CommerceError::InvalidSignature);
        }
        if payload.amount_minor <= 0 {
            return Err(CommerceError::BadInput("webhook amount".into()));
        }
        let currency = Currency::parse(&payload.currency)?;
        let payload_digest = store::digest_hex(&[
            &payload.event_id,
            &payload.intent_id,
            &payload.kind,
            &payload.provider_ref,
            &payload.amount_minor.to_string(),
            &payload.currency,
        ]);
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = load_intent(&tx, &payload.intent_id)?.ok_or(CommerceError::NotFound)?;
        if row.intent.amount.minor() != payload.amount_minor
            || row.intent.amount.currency() != currency
        {
            return Err(CommerceError::BadInput(
                "webhook amount does not match the intent".into(),
            ));
        }
        if let Some(reference) = row.intent.provider_ref.as_deref() {
            if reference != payload.provider_ref {
                return Err(CommerceError::BadInput(
                    "webhook provider reference does not match".into(),
                ));
            }
        }
        let duplicate: Option<String> = tx
            .query_row(
                "SELECT event_id FROM commerce_payment_events WHERE event_id = ?1",
                params![payload.event_id],
                |r| r.get(0),
            )
            .optional()?;
        if duplicate.is_some() {
            return Err(CommerceError::Replay);
        }
        tx.execute(
            "INSERT INTO commerce_payment_events
                (event_id, intent_id, kind, provider_ref, amount_minor, currency,
                 payload_digest, received_at, applied, reason)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, '')",
            params![
                payload.event_id,
                payload.intent_id,
                payload.kind,
                payload.provider_ref,
                payload.amount_minor,
                payload.currency,
                payload_digest,
                now
            ],
        )?;
        let (applied, reason) = match payload.kind.as_str() {
            "payment.succeeded" => match row.intent.status {
                IntentStatus::Created | IntentStatus::Pending => {
                    apply_success(&tx, self, &payload.intent_id, &payload.provider_ref, now)?;
                    (1, String::new())
                }
                IntentStatus::Succeeded => (0, "already succeeded".to_string()),
                _ => (0, "terminal state".to_string()),
            },
            "payment.failed" => match row.intent.status {
                IntentStatus::Created | IntentStatus::Pending => {
                    tx.execute(
                        "UPDATE commerce_payment_intents
                         SET status = 'failed', updated_at = ?1, version = version + 1
                         WHERE id = ?2 AND status IN ('created', 'pending')",
                        params![now, payload.intent_id],
                    )?;
                    (1, String::new())
                }
                _ => (0, "out-of-order".to_string()),
            },
            _ => return Err(CommerceError::BadInput("unknown webhook kind".into())),
        };
        tx.execute(
            "UPDATE commerce_payment_events SET applied = ?1, reason = ?2 WHERE event_id = ?3",
            params![applied, reason, payload.event_id],
        )?;
        let updated = load_intent(&tx, &payload.intent_id)?.ok_or(CommerceError::NotFound)?;
        tx.commit()?;
        Ok(WebhookOutcome {
            event_id: payload.event_id,
            applied: applied == 1,
            status: updated.intent.status,
        })
    }
}

/// 成功入账：外部世界 → 收款钱包。业务键保证重复回调只入账一次。
fn apply_success(
    conn: &Connection,
    commerce: &Commerce,
    intent_id: &str,
    provider_ref: &str,
    now: i64,
) -> Result<()> {
    let row = load_intent(conn, intent_id)?.ok_or(CommerceError::NotFound)?;
    if row.intent.status == IntentStatus::Succeeded {
        return Ok(());
    }
    if !matches!(
        row.intent.status,
        IntentStatus::Created | IntentStatus::Pending
    ) {
        return Err(CommerceError::IllegalTransition(format!(
            "cannot settle intent in state {}",
            row.intent.status.as_str()
        )));
    }
    let currency = row.intent.amount.currency();
    let payee = commerce.ensure_wallet_conn(conn, &row.intent.payee_id, currency, now)?;
    let external = store::ensure_external(
        conn,
        commerce.provider_name(),
        currency,
        commerce.config.fund_class,
        now,
    )?;
    let request = PostingRequest::new(
        format!("intent:{intent_id}:success"),
        "payment.succeeded",
        currency,
        vec![
            PostingLine::debit(external, row.intent.amount.minor()),
            PostingLine::credit(payee, row.intent.amount.minor()),
        ],
    )
    .with_memo("payment captured");
    wxlike_ledger::post_in(conn, &request, now)?;
    conn.execute(
        "UPDATE commerce_payment_intents
         SET status = 'succeeded',
             provider_ref = COALESCE(provider_ref, ?1),
             updated_at = ?2,
             version = version + 1
         WHERE id = ?3 AND status IN ('created', 'pending')",
        params![provider_ref, now, intent_id],
    )?;
    store::emit(
        conn,
        commerce.entropy(),
        "payment.succeeded",
        intent_id,
        &serde_json::json!({
            "intent_id": intent_id,
            "amount_minor": row.intent.amount.minor().to_string(),
            "currency": currency.as_str(),
        }),
        now,
    )?;
    Ok(())
}

fn load_intent(conn: &Connection, id: &str) -> Result<Option<IntentRow>> {
    let row = conn
        .query_row(
            "SELECT id, business_key, direction, actor_id, payee_id, amount_minor, currency,
                    status, provider_ref, nonce_hash, refunded_minor, expires_at,
                    created_at, updated_at, request_digest
             FROM commerce_payment_intents WHERE id = ?1",
            params![id],
            scan_row,
        )
        .optional()?;
    row.map(build_row).transpose()
}

fn load_by_business_key(conn: &Connection, key: &str) -> Result<Option<(String, IntentRow)>> {
    let row = conn
        .query_row(
            "SELECT id, business_key, direction, actor_id, payee_id, amount_minor, currency,
                    status, provider_ref, nonce_hash, refunded_minor, expires_at,
                    created_at, updated_at, request_digest
             FROM commerce_payment_intents WHERE business_key = ?1",
            params![key],
            scan_row,
        )
        .optional()?;
    row.map(|raw| Ok((raw.digest.clone(), build_row(raw)?)))
        .transpose()
}

struct RawIntent {
    id: String,
    direction: String,
    actor_id: String,
    payee_id: String,
    amount_minor: i64,
    currency: String,
    status: String,
    provider_ref: Option<String>,
    nonce_hash: String,
    refunded_minor: i64,
    expires_at: i64,
    created_at: i64,
    updated_at: i64,
    digest: String,
}

fn scan_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawIntent> {
    Ok(RawIntent {
        id: r.get(0)?,
        direction: r.get(2)?,
        actor_id: r.get(3)?,
        payee_id: r.get(4)?,
        amount_minor: r.get(5)?,
        currency: r.get(6)?,
        status: r.get(7)?,
        provider_ref: r.get(8)?,
        nonce_hash: r.get(9)?,
        refunded_minor: r.get(10)?,
        expires_at: r.get(11)?,
        created_at: r.get(12)?,
        updated_at: r.get(13)?,
        digest: r.get(14)?,
    })
}

fn build_row(raw: RawIntent) -> Result<IntentRow> {
    let direction = IntentDirection::parse(&raw.direction)
        .ok_or_else(|| CommerceError::BadInput("stored intent direction".into()))?;
    let status = IntentStatus::parse(&raw.status)
        .ok_or_else(|| CommerceError::BadInput("stored intent status".into()))?;
    let currency = Currency::parse(&raw.currency)?;
    Ok(IntentRow {
        intent: PaymentIntent {
            id: raw.id,
            direction,
            actor_id: raw.actor_id,
            payee_id: raw.payee_id,
            amount: Money::from_minor(raw.amount_minor, currency),
            status,
            provider_ref: raw.provider_ref,
            refunded_minor: raw.refunded_minor,
            expires_at: raw.expires_at,
            created_at: raw.created_at,
            updated_at: raw.updated_at,
        },
        nonce_hash: raw.nonce_hash,
    })
}

fn payee_tag(payee: &str) -> String {
    store::digest_hex(&["payee", payee])[..16].to_string()
}

fn ticket_signature(commerce: &Commerce, row: &IntentRow) -> String {
    store::hmac_hex(
        commerce.config.qr_secret.as_slice(),
        &[
            "v1",
            &row.intent.id,
            &row.intent.amount.minor().to_string(),
            row.intent.amount.currency().as_str(),
            &payee_tag(&row.intent.payee_id),
            &row.intent.expires_at.to_string(),
            &row.nonce_hash,
        ],
    )
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}
