//! 支付意图、二维码与供应商回调。
//!
//! 关键约束：
//! * 供应商网络调用永远在数据库事务之外；
//! * 成功入账只在收到"成功"证据时发生，未知状态不本地写已支付；
//! * 业务键 + 请求摘要保证重复提交只产生一笔；
//! * 回调按原始 body 验签、校验时间窗、事件 ID 去重，状态只向前走。

use rusqlite::{named_params, params, Connection, OptionalExtension, TransactionBehavior};
use serde::Deserialize;
use wxlike_ledger::{Currency, Money, PostingLine, PostingRequest};

use crate::error::{CommerceError, Result};
use crate::ports::{map_provider, Entropy, ProviderIntentRequest, ProviderPaymentStatus};
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

/// 带 `operation_id` 的供应商回调信封。
///
/// `operation_id` 是平台统一的长操作把手（见
/// `docs/full-platform-delivery/01_contracts_and_data.md` 与 G15-05）：
/// 供应商对同一次业务结果的所有重试都必须复用同一个 `operation_id`，
/// 换 `operation_id` 再投同一 `event_id` 会被事件去重拒绝。
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct CallbackEnvelope {
    pub operation_id: String,
    pub event_id: String,
    pub intent_id: String,
    pub kind: String,
    pub provider_ref: String,
    /// 整数最小单位；本契约不接受浮点金额。
    pub amount_minor: i64,
    pub currency: String,
    pub timestamp_ms: i64,
}

impl CallbackEnvelope {
    /// 业务内容摘要：绑定操作与业务字段。
    ///
    /// 刻意不含 `timestamp_ms`——那是投递元数据而非业务内容；供应商用新
    /// 时间戳重签同一次结果仍算重放（返回首次结果），而金额/类别/事件号
    /// 任一变化都会触发 [`crate::CommerceError::IdempotencyConflict`]。
    #[must_use]
    pub fn digest(&self) -> String {
        store::digest_hex(&[
            CALLBACK_DIGEST_VERSION,
            &self.operation_id,
            &self.event_id,
            &self.intent_id,
            &self.kind,
            &self.provider_ref,
            &self.amount_minor.to_string(),
            &self.currency,
        ])
    }

    /// 幂等键：`operation_id` 全库唯一，作用域前缀避免与其它命令撞键。
    #[must_use]
    pub fn business_key(&self) -> String {
        format!("callback:{}", self.operation_id)
    }

    /// 事件去重用的载荷摘要，与不带 `operation_id` 的旧回调同一规范，
    /// 保证两条入口共享一套事件表去重语义。
    #[must_use]
    pub fn event_digest(&self) -> String {
        store::digest_hex(&[
            &self.event_id,
            &self.intent_id,
            &self.kind,
            &self.provider_ref,
            &self.amount_minor.to_string(),
            &self.currency,
        ])
    }

    fn payload(&self) -> WebhookPayload {
        WebhookPayload {
            event_id: self.event_id.clone(),
            intent_id: self.intent_id.clone(),
            kind: self.kind.clone(),
            provider_ref: self.provider_ref.clone(),
            amount_minor: self.amount_minor,
            currency: self.currency.clone(),
            timestamp_ms: self.timestamp_ms,
        }
    }
}

/// 回调端点的处理结果；`replayed` 为真表示本次没有产生任何副作用。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CallbackOutcome {
    pub operation_id: String,
    pub event_id: String,
    pub intent_id: String,
    pub kind: String,
    pub applied: bool,
    pub replayed: bool,
    pub status: String,
    pub reason: String,
}

impl CallbackOutcome {
    /// 供重放路径回填：库里存的是首次结果，本次是重复投递。
    #[must_use]
    fn mark_replayed(self) -> Self {
        Self {
            replayed: true,
            ..self
        }
    }
}

/// 操作台账里一行回调记录（值对象的只读视图）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallbackRecord {
    pub outcome: CallbackOutcome,
    pub received_at: i64,
}

#[derive(Clone, Debug)]
struct IntentRow {
    intent: PaymentIntent,
    nonce_hash: String,
}

/// 回调摘要的规范版本：一旦字段构成变化就 bump，旧记录自然视为异参。
const CALLBACK_DIGEST_VERSION: &str = "callback-v1";

/// 意图是否已经进入"不再由供应商证据推进"的终态。
///
/// 未知态自愈（查询与回调）只允许从 created/pending 前进；终态一律直接回读，
/// 否则一次迟到的回执就能把 refunded 改回 succeeded。
#[must_use]
fn is_terminal_status(status: IntentStatus) -> bool {
    matches!(
        status,
        IntentStatus::Succeeded
            | IntentStatus::Refunded
            | IntentStatus::PartiallyRefunded
            | IntentStatus::Failed
            | IntentStatus::Expired
    )
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
             VALUES(:id, :business_key, :digest, :direction, :actor_id, :payee_id,
                    :amount_minor, :currency, 'created', :provider, NULL, :nonce_hash,
                    :qr_token_hash, :expires_at, :now, :now)",
            named_params! {
                ":id": id,
                ":business_key": scoped_key,
                ":digest": digest,
                ":direction": direction.as_str(),
                ":actor_id": actor,
                ":payee_id": payee,
                ":amount_minor": amount.minor(),
                ":currency": amount.currency().as_str(),
                ":provider": self.provider_name(),
                ":nonce_hash": nonce_hash,
                ":qr_token_hash": String::new(),
                ":expires_at": expires_at,
                ":now": now,
            },
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
        if let Some(settled) = pay_status_gate(row.intent.status, &row.intent)? {
            return Ok(settled);
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
        if !store::constant_time_eq(&expected, signature) {
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

    /// 用供应商查询解决"未知状态"（验收：超时不能本地写已支付）。
    ///
    /// 只有供应商证据能推进状态：
    /// * `Succeeded` → 走与回调同一套入账逻辑（分录业务键 `intent:{id}:success`
    ///   在账本层幂等，所以与稍后到达的回执并发也不会二次入账）；
    /// * `Failed` → 只从可支付态前进为失败；
    /// * `Pending` → 原样返回，本地一个字都不改；
    /// * 查询本身超时/不可达 → 返回错误，意图继续停在未知态等下一次；
    /// * 已是终态 → 直接回读，不再发起查询，也不允许终态倒退。
    ///
    /// 没有 `provider_ref` 的意图（创建时就超时）先做幂等创建：供应商侧的
    /// 键由意图 ID 派生，重复调用不会产生第二张单。
    ///
    /// # Errors
    /// 无权查看、参数非法、供应商失败或存储失败。
    pub fn refresh_payment_intent(&self, viewer: &str, intent_id: &str) -> Result<PaymentIntent> {
        store::validate_id("viewer", viewer)?;
        store::validate_id("intent", intent_id)?;
        let row = {
            let conn = self.lock();
            load_intent(&conn, intent_id)?.ok_or(CommerceError::NotFound)?
        };
        if row.intent.actor_id != viewer && row.intent.payee_id != viewer {
            return Err(CommerceError::Forbidden);
        }
        if is_terminal_status(row.intent.status) {
            return Ok(row.intent);
        }
        let Some(provider_ref) = row.intent.provider_ref.clone() else {
            // 创建时就超时的单：先幂等补建，再按本地状态回读（鉴权已在上一步做过）。
            self.advance_provider(&row)?;
            let conn = self.lock();
            let updated = load_intent(&conn, intent_id)?.ok_or(CommerceError::NotFound)?;
            return Ok(updated.intent);
        };
        let payment = self
            .ports
            .provider
            .query_payment(&provider_ref)
            .map_err(map_provider)?;
        if payment.amount != row.intent.amount {
            return Err(CommerceError::BadInput(
                "provider query amount does not match the intent".into(),
            ));
        }
        match payment.status {
            ProviderPaymentStatus::Pending => Ok(row.intent),
            ProviderPaymentStatus::Succeeded => {
                let mut guard = self.lock();
                let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
                apply_success(&tx, self, intent_id, &provider_ref, self.now())?;
                let updated = load_intent(&tx, intent_id)?.ok_or(CommerceError::NotFound)?;
                tx.commit()?;
                Ok(updated.intent)
            }
            ProviderPaymentStatus::Failed => {
                self.mark_intent_failed(intent_id)?;
                self.payment_intent(viewer, intent_id)
            }
        }
    }

    /// 供应商证据确认失败时前进为失败态：可支付态之外的状态一律不动。
    ///
    /// 连接守卫在本函数内释放，调用方随后还要再次取锁回读意图。
    fn mark_intent_failed(&self, intent_id: &str) -> Result<()> {
        let now = self.now();
        let conn = self.lock();
        conn.execute(
            "UPDATE commerce_payment_intents
             SET status = 'failed', updated_at = ?1, version = version + 1
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

    /// 处理供应商回调（旧入口：只按 `event_id` 去重）。
    ///
    /// # Errors
    /// 签名错误、时间窗外、金额/币种不符、重复事件或存储失败。
    pub fn handle_webhook(&self, raw_body: &str, signature: &str) -> Result<WebhookOutcome> {
        let now = self.now();
        let (payload, digest) = self.verify_webhook(raw_body, signature)?;
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (applied, _reason, status) = apply_webhook_effect(&tx, self, &payload, &digest, now)?;
        tx.commit()?;
        Ok(WebhookOutcome {
            event_id: payload.event_id,
            applied: applied == 1,
            status,
        })
    }

    /// 处理带 `operation_id` 的供应商回调端点入口。
    ///
    /// 幂等与防重放走 crate 内 `store::replay()` / `store::replay_legs()`
    /// 这一套唯一入口：
    /// * 同 `operation_id` + 同业务摘要 **且事件腿也已落库** → 返回首次
    ///   结果，`replayed = true`，不写事件、不推状态、不记账、不出 outbox；
    /// * 同 `operation_id` + 异业务摘要 → [`CommerceError::IdempotencyConflict`]；
    /// * 换 `operation_id` 再投同一 `event_id` → [`CommerceError::Replay`]
    ///   （事件表去重，与新入参无关）。
    ///
    /// "每条腿都落了库才算重放"是从 Blnk 的 inflight 动作重放判定借来的
    /// （见 `third_party/reviews/patches/UP-BLNK-replay-legs.md`）：只命中
    /// 台账腿、事件腿还缺着的状态不能被当成"已经做完"。
    ///
    /// 首次投递的效果（事件行 + 状态推进 + 复式分录 + outbox + 台账行）在
    /// 同一个 `IMMEDIATE` 事务里提交：崩溃只会导致"全无"或"全无重复"，
    /// 不会出现半笔入账。
    ///
    /// # Errors
    /// 验签失败、时间窗外、参数非法、幂等冲突、事件重复或存储失败。
    pub fn handle_provider_callback(
        &self,
        raw_body: &str,
        signature: &str,
    ) -> Result<CallbackOutcome> {
        let now = self.now();
        let envelope = self.verify_callback(raw_body, signature)?;
        let scoped_key = envelope.business_key();
        let digest = envelope.digest();
        let event_digest = envelope.event_digest();
        let payload = envelope.payload();
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored_callback = load_callback(&tx, &scoped_key)?;
        let stored_event = stored_event_digest(&tx, &payload.event_id)?;
        let replayed = store::replay_legs(&[
            store::Leg {
                name: "provider callback",
                stored: stored_callback.as_ref().map(|(d, _)| d.as_str()),
                digest: &digest,
            },
            store::Leg {
                name: "provider event",
                stored: stored_event.as_deref(),
                digest: &event_digest,
            },
        ])?;
        if replayed {
            let previous = store::replay(stored_callback, &digest)?
                .ok_or_else(|| CommerceError::BadInput("callback ledger row vanished".into()))?;
            tx.commit()?;
            return Ok(previous.mark_replayed());
        }
        // 时间窗只约束"新效果"那一次：重试可能发生在很久以后，此时命中重放
        // 应当原样返回首次结果，而不是把合法重试误判成伪造。
        if (self.now() - envelope.timestamp_ms).abs() > self.config.callback_skew_ms {
            return Err(CommerceError::InvalidSignature);
        }
        let (applied, reason, status) =
            apply_webhook_effect(&tx, self, &payload, &event_digest, now)?;
        let outcome = CallbackOutcome {
            operation_id: envelope.operation_id.clone(),
            event_id: envelope.event_id.clone(),
            intent_id: envelope.intent_id.clone(),
            kind: envelope.kind.clone(),
            applied: applied == 1,
            replayed: false,
            status: status.as_str().to_string(),
            reason,
        };
        insert_callback(&tx, self.entropy(), &envelope, &digest, &outcome, now)?;
        tx.commit()?;
        Ok(outcome)
    }

    /// 按 `operation_id` 读回回调台账（值班排障、对账与端到端重放取证）。
    ///
    /// # Errors
    /// 参数非法或存储失败。
    pub fn provider_callback(&self, operation_id: &str) -> Result<Option<CallbackRecord>> {
        store::validate_id("operation", operation_id)?;
        let conn = self.lock();
        load_callback_by_operation(&conn, operation_id)
    }
}

impl Commerce {
    /// 验签、解析与时间窗校验；返回载荷与载荷摘要。
    fn verify_webhook(&self, raw_body: &str, signature: &str) -> Result<(WebhookPayload, String)> {
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
        if (self.now() - payload.timestamp_ms).abs() > self.config.callback_skew_ms {
            return Err(CommerceError::InvalidSignature);
        }
        if payload.amount_minor <= 0 {
            return Err(CommerceError::BadInput("webhook amount".into()));
        }
        Currency::parse(&payload.currency)?;
        let digest = store::digest_hex(&[
            &payload.event_id,
            &payload.intent_id,
            &payload.kind,
            &payload.provider_ref,
            &payload.amount_minor.to_string(),
            &payload.currency,
        ]);
        Ok((payload, digest))
    }

    /// 验签并校验带 `operation_id` 的回调信封。
    ///
    /// 签名对象是**原始 body 字节**（不是重新序列化的 JSON），与
    /// [`Self::webhook_signature`] 同一套规范；这样网关/中间层改写键顺序
    /// 或空白时不会出现"看起来一样但验签失败"的运维陷阱。
    ///
    /// 这里刻意不做时间窗判定：`handle_provider_callback` 需要在重放命中时
    /// 原样返回首次结果，时间窗只用于闸门新效果。
    fn verify_callback(&self, raw_body: &str, signature: &str) -> Result<CallbackEnvelope> {
        if !store::hmac_verify(
            self.config.webhook_secret.as_slice(),
            &[raw_body],
            signature,
        ) {
            return Err(CommerceError::InvalidSignature);
        }
        let envelope: CallbackEnvelope = serde_json::from_str(raw_body)
            .map_err(|_| CommerceError::BadInput("callback body".into()))?;
        store::validate_id("operation", &envelope.operation_id)?;
        store::validate_id("event", &envelope.event_id)?;
        store::validate_id("intent", &envelope.intent_id)?;
        store::validate_id("provider_ref", &envelope.provider_ref)?;
        if !matches!(
            envelope.kind.as_str(),
            "payment.succeeded" | "payment.failed"
        ) {
            return Err(CommerceError::BadInput("unknown callback kind".into()));
        }
        if envelope.amount_minor <= 0 {
            return Err(CommerceError::BadInput("callback amount".into()));
        }
        Currency::parse(&envelope.currency)?;
        Ok(envelope)
    }
}

/// 在调用方事务内落地一次回调效果。
///
/// 两条入口（[`Commerce::handle_webhook`] 与
/// [`Commerce::handle_provider_callback`]）共用这一份实现，幂等与状态推进
/// 因此只有一个语义来源；回调端点不会另造一套"看起来一样"的入账路径。
fn apply_webhook_effect(
    conn: &Connection,
    commerce: &Commerce,
    payload: &WebhookPayload,
    event_digest: &str,
    now: i64,
) -> Result<(i64, String, IntentStatus)> {
    let row = load_intent(conn, &payload.intent_id)?.ok_or(CommerceError::NotFound)?;
    ensure_webhook_matches(&row.intent, payload)?;
    record_event(conn, payload, event_digest, now)?;
    let (applied, reason) = apply_webhook_event(conn, commerce, payload, now)?;
    conn.execute(
        "UPDATE commerce_payment_events SET applied = ?1, reason = ?2 WHERE event_id = ?3",
        params![applied, reason, payload.event_id],
    )?;
    let updated = load_intent(conn, &payload.intent_id)?.ok_or(CommerceError::NotFound)?;
    Ok((applied, reason, updated.intent.status))
}

/// 读回事件表里某个 `event_id` 冻结的载荷摘要（幂等判定的第二条腿）。
///
/// Blnk 也是先按引用问"这个键占没占"再取整行
/// （`transaction_inflight.go:691-712` 的 `TransactionExistsByRef` →
/// `GetTransactionByRef` 两步式）：判定"是否重放"只需要摘要，不需要把
/// 首次效果的载荷整份读出来。
fn stored_event_digest(conn: &Connection, event_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT payload_digest FROM commerce_payment_events WHERE event_id = ?1",
            params![event_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
}

/// 读回幂等键下的既有回调结果：`(存储摘要, 首次结果)`。
fn load_callback(conn: &Connection, scoped_key: &str) -> Result<Option<(String, CallbackOutcome)>> {
    let row = conn
        .query_row(
            "SELECT request_digest, result_json FROM commerce_provider_callbacks
             WHERE business_key = ?1",
            params![scoped_key],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((digest, result_json)) = row else {
        return Ok(None);
    };
    let outcome: CallbackOutcome = serde_json::from_str(&result_json)
        .map_err(|_| CommerceError::BadInput("stored callback result".into()))?;
    Ok(Some((digest, outcome)))
}

/// 写回调台账一行。业务键唯一约束是最后一道防线：即使并发窗口里两次都
/// 判定为"新"，第二次也会在 `IMMEDIATE` 事务内撞上 UNIQUE 并整体回滚。
fn insert_callback(
    conn: &Connection,
    entropy: &dyn Entropy,
    envelope: &CallbackEnvelope,
    digest: &str,
    outcome: &CallbackOutcome,
    now: i64,
) -> Result<()> {
    let result_json = serde_json::to_string(outcome)
        .map_err(|error| CommerceError::BadInput(format!("callback result: {error}")))?;
    let id = store::new_id(entropy);
    let business_key = envelope.business_key();
    let amount_minor = envelope.amount_minor;
    let applied = i64::from(outcome.applied);
    conn.execute(
        "INSERT INTO commerce_provider_callbacks
            (id, business_key, operation_id, event_id, intent_id, kind, provider_ref,
             amount_minor, currency, request_digest, result_json, applied, reason,
             received_at)
         VALUES(:id, :business_key, :operation_id, :event_id, :intent_id, :kind,
                :provider_ref, :amount_minor, :currency, :request_digest,
                :result_json, :applied, :reason, :received_at)",
        named_params! {
            ":id": id,
            ":business_key": business_key,
            ":operation_id": envelope.operation_id,
            ":event_id": envelope.event_id,
            ":intent_id": envelope.intent_id,
            ":kind": envelope.kind,
            ":provider_ref": envelope.provider_ref,
            ":amount_minor": amount_minor,
            ":currency": envelope.currency,
            ":request_digest": digest,
            ":result_json": result_json,
            ":applied": applied,
            ":reason": outcome.reason,
            ":received_at": now,
        },
    )?;
    Ok(())
}

/// 按操作号读取台账记录。
fn load_callback_by_operation(
    conn: &Connection,
    operation_id: &str,
) -> Result<Option<CallbackRecord>> {
    let row = conn
        .query_row(
            "SELECT result_json, received_at FROM commerce_provider_callbacks
             WHERE operation_id = ?1",
            params![operation_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?;
    let Some((result_json, received_at)) = row else {
        return Ok(None);
    };
    let mut outcome: CallbackOutcome = serde_json::from_str(&result_json)
        .map_err(|_| CommerceError::BadInput("stored callback result".into()))?;
    // 库里存的是首次投递的结果；按操作号回读时它已经是"过去完成的操作"。
    outcome.replayed = true;
    Ok(Some(CallbackRecord {
        outcome,
        received_at,
    }))
}

/// 回调声称的金额/币种/供应商引用必须与本地意图一致。
fn ensure_webhook_matches(intent: &PaymentIntent, payload: &WebhookPayload) -> Result<()> {
    let currency = Currency::parse(&payload.currency)?;
    if intent.amount.minor() != payload.amount_minor || intent.amount.currency() != currency {
        return Err(CommerceError::BadInput(
            "webhook amount does not match the intent".into(),
        ));
    }
    if let Some(reference) = intent.provider_ref.as_deref() {
        if reference != payload.provider_ref {
            return Err(CommerceError::BadInput(
                "webhook provider reference does not match".into(),
            ));
        }
    }
    Ok(())
}

/// 事件去重 + 落库：同一 event_id 第二次到达直接拒绝。
fn record_event(conn: &Connection, payload: &WebhookPayload, digest: &str, now: i64) -> Result<()> {
    let duplicate: Option<String> = conn
        .query_row(
            "SELECT event_id FROM commerce_payment_events WHERE event_id = ?1",
            params![payload.event_id],
            |r| r.get(0),
        )
        .optional()?;
    if duplicate.is_some() {
        return Err(CommerceError::Replay);
    }
    conn.execute(
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
            digest,
            now
        ],
    )?;
    Ok(())
}

/// 按事件类别推进意图状态；返回 (是否生效, 原因)。
fn apply_webhook_event(
    conn: &Connection,
    commerce: &Commerce,
    payload: &WebhookPayload,
    now: i64,
) -> Result<(i64, String)> {
    let row = load_intent(conn, &payload.intent_id)?.ok_or(CommerceError::NotFound)?;
    match payload.kind.as_str() {
        "payment.succeeded" => match row.intent.status {
            IntentStatus::Created | IntentStatus::Pending => {
                apply_success(
                    conn,
                    commerce,
                    &payload.intent_id,
                    &payload.provider_ref,
                    now,
                )?;
                Ok((1, String::new()))
            }
            IntentStatus::Succeeded => Ok((0, "already succeeded".to_string())),
            _ => Ok((0, "terminal state".to_string())),
        },
        "payment.failed" => match row.intent.status {
            IntentStatus::Created | IntentStatus::Pending => {
                conn.execute(
                    "UPDATE commerce_payment_intents
                     SET status = 'failed', updated_at = ?1, version = version + 1
                     WHERE id = ?2 AND status IN ('created', 'pending')",
                    params![now, payload.intent_id],
                )?;
                Ok((1, String::new()))
            }
            _ => Ok((0, "out-of-order".to_string())),
        },
        _ => Err(CommerceError::BadInput("unknown webhook kind".into())),
    }
}

/// 付款入口的状态门：终态直接短路返回，失败拒绝，可付状态放行。
fn pay_status_gate(status: IntentStatus, current: &PaymentIntent) -> Result<Option<PaymentIntent>> {
    match status {
        IntentStatus::Expired => Err(CommerceError::Expired),
        IntentStatus::Succeeded | IntentStatus::Refunded | IntentStatus::PartiallyRefunded => {
            Ok(Some(current.clone()))
        }
        IntentStatus::Failed => Err(CommerceError::IllegalTransition("intent failed".into())),
        IntentStatus::Created | IntentStatus::Pending => Ok(None),
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
    post_capture(
        conn,
        commerce,
        intent_id,
        &row.intent.payee_id,
        row.intent.amount.minor(),
        currency,
        now,
    )?;
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

/// 把一笔成功的支付记成复式分录：借记外部世界、贷记收款钱包。
fn post_capture(
    conn: &Connection,
    commerce: &Commerce,
    intent_id: &str,
    payee_id: &str,
    amount_minor: i64,
    currency: Currency,
    now: i64,
) -> Result<()> {
    let payee = commerce.ensure_wallet_conn(conn, payee_id, currency, now)?;
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
            PostingLine::debit(external, amount_minor),
            PostingLine::credit(payee, amount_minor),
        ],
    )
    .with_memo("payment captured");
    wxlike_ledger::post_in(conn, &request, now)?;
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
        id: r.get("id")?,
        direction: r.get("direction")?,
        actor_id: r.get("actor_id")?,
        payee_id: r.get("payee_id")?,
        amount_minor: r.get("amount_minor")?,
        currency: r.get("currency")?,
        status: r.get("status")?,
        provider_ref: r.get("provider_ref")?,
        nonce_hash: r.get("nonce_hash")?,
        refunded_minor: r.get("refunded_minor")?,
        expires_at: r.get("expires_at")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
        digest: r.get("request_digest")?,
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
