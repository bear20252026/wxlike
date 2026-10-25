//! 充值、提现、退款与对账。
//!
//! 出款类动作统一"先冻结、再调供应商、成功才转正式分录"：供应商超时
//! 时资金保持冻结且状态为处理中，重试使用同一幂等键，绝不重复出款。

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use wxlike_ledger::{Currency, Money, PostingLine, PostingRequest};

use crate::error::{CommerceError, Result};
use crate::payments::{IntentDirection, IntentStatus, PaymentTicket};
use crate::ports::{map_provider, StatementKind};
use crate::store;
use crate::Commerce;

/// 充值单状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopupStatus {
    Created,
    Pending,
    Succeeded,
    Failed,
    Expired,
}

impl TopupStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Pending => "pending",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Expired => "expired",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "created" => Ok(Self::Created),
            "pending" => Ok(Self::Pending),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "expired" => Ok(Self::Expired),
            _ => Err(CommerceError::BadInput("stored topup status".into())),
        }
    }
}

/// 充值单。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Topup {
    pub id: String,
    pub actor_id: String,
    pub intent_id: String,
    pub amount: Money,
    pub status: TopupStatus,
    pub provider_ref: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 提现单状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WithdrawalStatus {
    Pending,
    Processing,
    Paid,
    Failed,
}

impl WithdrawalStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Processing => "processing",
            Self::Paid => "paid",
            Self::Failed => "failed",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "pending" => Ok(Self::Pending),
            "processing" => Ok(Self::Processing),
            "paid" => Ok(Self::Paid),
            "failed" => Ok(Self::Failed),
            _ => Err(CommerceError::BadInput("stored withdrawal status".into())),
        }
    }
}

/// 提现单。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Withdrawal {
    pub id: String,
    pub actor_id: String,
    pub amount: Money,
    pub status: WithdrawalStatus,
    pub account_ref: String,
    pub provider_ref: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 退款状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefundStatus {
    Processing,
    Succeeded,
    Failed,
}

impl RefundStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Processing => "processing",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "processing" => Ok(Self::Processing),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            _ => Err(CommerceError::BadInput("stored refund status".into())),
        }
    }
}

/// 退款单。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refund {
    pub id: String,
    pub intent_id: String,
    pub actor_id: String,
    pub amount: Money,
    pub status: RefundStatus,
    pub provider_ref: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 对账差异。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconciliationException {
    pub id: String,
    pub run_id: String,
    pub provider_ref: String,
    pub local_kind: String,
    pub local_id: String,
    pub amount: Money,
    pub reason: String,
    pub status: String,
    pub resolved_by: Option<String>,
    pub approval_ref: Option<String>,
    pub journal_key: Option<String>,
    pub created_at: i64,
}

/// 对账结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconciliationReport {
    pub run_id: String,
    pub checked: usize,
    pub exceptions: Vec<ReconciliationException>,
}

impl Commerce {
    /// 创建充值单：内部是一张对本人收款的支付意图。
    ///
    /// # Errors
    /// 参数非法、金额超限或供应商失败。
    pub fn create_topup(
        &self,
        actor: &str,
        amount: Money,
        business_key: &str,
    ) -> Result<(Topup, PaymentTicket)> {
        store::validate_id("actor", actor)?;
        store::validate_key(business_key)?;
        store::check_limit(amount, self.config.max_payment_minor)?;
        let intent_key = format!("topup:{business_key}");
        let (intent, ticket) =
            self.create_payment_intent(IntentDirection::Topup, actor, actor, amount, &intent_key)?;
        let digest = store::digest_hex(&[
            actor,
            &amount.minor().to_string(),
            amount.currency().as_str(),
            &intent.id,
        ]);
        let scoped_key = format!("topup:{business_key}");
        let now = self.now();
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = load_topup_by_key(&tx, &scoped_key)? {
            if existing.0 != digest {
                return Err(CommerceError::IdempotencyConflict);
            }
            return Ok((existing.1, ticket));
        }
        let id = store::new_id(self.entropy());
        let status = map_intent_status(intent.status);
        tx.execute(
            "INSERT INTO commerce_topups
                (id, business_key, actor_id, intent_id, amount_minor, currency, status,
                 provider_ref, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            params![
                id,
                scoped_key,
                actor,
                intent.id,
                amount.minor(),
                amount.currency().as_str(),
                status.as_str(),
                intent.provider_ref,
                now
            ],
        )?;
        let topup = load_topup(&tx, &id)?.ok_or(CommerceError::NotFound)?;
        tx.commit()?;
        Ok((topup, ticket))
    }

    /// 查充值单。
    ///
    /// # Errors
    /// 不存在或无权查看。
    pub fn topup(&self, actor: &str, topup_id: &str) -> Result<Topup> {
        store::validate_id("actor", actor)?;
        let conn = self.lock();
        let topup = load_topup(&conn, topup_id)?.ok_or(CommerceError::NotFound)?;
        if topup.actor_id != actor {
            return Err(CommerceError::Forbidden);
        }
        Ok(topup)
    }

    /// 以支付意图的真实状态刷新充值单。
    ///
    /// # Errors
    /// 不存在、无权查看或存储失败。
    pub fn confirm_topup(&self, actor: &str, topup_id: &str) -> Result<Topup> {
        store::validate_id("actor", actor)?;
        let topup = {
            let conn = self.lock();
            load_topup(&conn, topup_id)?.ok_or(CommerceError::NotFound)?
        };
        if topup.actor_id != actor {
            return Err(CommerceError::Forbidden);
        }
        let intent = self.payment_intent(actor, &topup.intent_id)?;
        let status = map_intent_status(intent.status);
        let now = self.now();
        let conn = self.lock();
        conn.execute(
            "UPDATE commerce_topups
             SET status = ?1, provider_ref = COALESCE(?2, provider_ref), updated_at = ?3
             WHERE id = ?4",
            params![status.as_str(), intent.provider_ref, now, topup_id],
        )?;
        load_topup(&conn, topup_id)?.ok_or(CommerceError::NotFound)
    }

    /// 发起提现：冻结 → 供应商出款 → 到账才入账。
    ///
    /// # Errors
    /// 余额不足、参数非法、供应商超时/拒绝或存储失败。
    pub fn request_withdrawal(
        &self,
        actor: &str,
        amount: Money,
        account_ref: &str,
        business_key: &str,
    ) -> Result<Withdrawal> {
        store::validate_id("actor", actor)?;
        store::validate_id("account", account_ref)?;
        store::validate_key(business_key)?;
        store::check_limit(amount, self.config.max_transfer_minor)?;
        let digest = store::digest_hex(&[
            actor,
            account_ref,
            &amount.minor().to_string(),
            amount.currency().as_str(),
        ]);
        let now = self.now();
        let withdrawal = {
            let mut guard = self.lock();
            let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if let Some(existing) = load_withdrawal_by_key(&tx, business_key)? {
                if existing.0 != digest {
                    return Err(CommerceError::IdempotencyConflict);
                }
                return Ok(existing.1);
            }
            let id = store::new_id(self.entropy());
            let hold_key = format!("withdrawal:{id}:hold");
            let wallet = self.ensure_wallet_conn(&tx, actor, amount.currency(), now)?;
            wxlike_ledger::hold_in(&tx, &hold_key, &wallet, amount, now)
                .map_err(store::map_ledger)?;
            tx.execute(
                "INSERT INTO commerce_withdrawals
                    (id, business_key, actor_id, amount_minor, currency, status, hold_key,
                     provider_ref, account_ref, created_at, updated_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, 'pending', ?6, NULL, ?7, ?8, ?8)",
                params![
                    id,
                    business_key,
                    actor,
                    amount.minor(),
                    amount.currency().as_str(),
                    hold_key,
                    account_ref,
                    now
                ],
            )?;
            let withdrawal = load_withdrawal(&tx, &id)?.ok_or(CommerceError::NotFound)?;
            tx.commit()?;
            withdrawal
        };
        let request = crate::ports::ProviderPayoutRequest {
            idempotency_key: format!("withdrawal:{}", withdrawal.id),
            amount: withdrawal.amount,
            account_ref: withdrawal.account_ref.clone(),
        };
        match self.ports.provider.payout(&request) {
            Ok(payout) => self.apply_payout(&withdrawal.id, payout.status, &payout.provider_ref),
            Err(error) => {
                if error == crate::ProviderError::Rejected {
                    let conn = self.lock();
                    release_hold(&conn, &withdrawal.id, "failed", self.now())?;
                } else {
                    let conn = self.lock();
                    conn.execute(
                        "UPDATE commerce_withdrawals SET status = 'processing', updated_at = ?1
                         WHERE id = ?2 AND status = 'pending'",
                        params![self.now(), withdrawal.id],
                    )?;
                }
                Err(map_provider(error))
            }
        }
    }

    /// 查询提现单。
    ///
    /// # Errors
    /// 不存在或无权查看。
    pub fn withdrawal(&self, actor: &str, withdrawal_id: &str) -> Result<Withdrawal> {
        store::validate_id("actor", actor)?;
        let conn = self.lock();
        let withdrawal = load_withdrawal(&conn, withdrawal_id)?.ok_or(CommerceError::NotFound)?;
        if withdrawal.actor_id != actor {
            return Err(CommerceError::Forbidden);
        }
        Ok(withdrawal)
    }

    /// 用同一幂等键重新查询/提交出款，解决超时后的未知状态。
    ///
    /// # Errors
    /// 不存在、无权查看、已终态或供应商失败。
    pub fn refresh_withdrawal(&self, actor: &str, withdrawal_id: &str) -> Result<Withdrawal> {
        let current = self.withdrawal(actor, withdrawal_id)?;
        if matches!(
            current.status,
            WithdrawalStatus::Paid | WithdrawalStatus::Failed
        ) {
            return Ok(current);
        }
        let request = crate::ports::ProviderPayoutRequest {
            idempotency_key: format!("withdrawal:{}", current.id),
            amount: current.amount,
            account_ref: current.account_ref.clone(),
        };
        match self.ports.provider.payout(&request) {
            Ok(payout) => self.apply_payout(&current.id, payout.status, &payout.provider_ref),
            Err(error) => Err(map_provider(error)),
        }
    }

    fn apply_payout(
        &self,
        withdrawal_id: &str,
        status: crate::ports::ProviderPayoutStatus,
        provider_ref: &str,
    ) -> Result<Withdrawal> {
        let now = self.now();
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let withdrawal = load_withdrawal(&tx, withdrawal_id)?.ok_or(CommerceError::NotFound)?;
        if matches!(
            withdrawal.status,
            WithdrawalStatus::Paid | WithdrawalStatus::Failed
        ) {
            return Ok(withdrawal);
        }
        match status {
            crate::ports::ProviderPayoutStatus::Paid => {
                let external = store::ensure_external(
                    &tx,
                    self.provider_name(),
                    withdrawal.amount.currency(),
                    self.config.fund_class,
                    now,
                )?;
                let wallet =
                    store::wallet_account(&withdrawal.actor_id, withdrawal.amount.currency());
                let request = PostingRequest::new(
                    format!("withdrawal:{withdrawal_id}:paid"),
                    "withdrawal.paid",
                    withdrawal.amount.currency(),
                    vec![
                        PostingLine::debit(wallet, withdrawal.amount.minor()),
                        PostingLine::credit(external, withdrawal.amount.minor()),
                    ],
                )
                .with_memo("withdrawal paid");
                let hold_key = format!("withdrawal:{withdrawal_id}:hold");
                wxlike_ledger::capture_hold_in(&tx, &hold_key, &request, now)
                    .map_err(store::map_ledger)?;
                tx.execute(
                    "UPDATE commerce_withdrawals
                     SET status = 'paid', provider_ref = ?1, updated_at = ?2
                     WHERE id = ?3",
                    params![provider_ref, now, withdrawal_id],
                )?;
                store::emit(
                    &tx,
                    self.entropy(),
                    "withdrawal.paid",
                    withdrawal_id,
                    &serde_json::json!({
                        "withdrawal_id": withdrawal_id,
                        "amount_minor": withdrawal.amount.minor().to_string(),
                    }),
                    now,
                )?;
            }
            crate::ports::ProviderPayoutStatus::Failed => {
                wxlike_ledger::release_hold_in(
                    &tx,
                    &format!("withdrawal:{withdrawal_id}:hold"),
                    now,
                )?;
                tx.execute(
                    "UPDATE commerce_withdrawals
                     SET status = 'failed', provider_ref = ?1, updated_at = ?2
                     WHERE id = ?3",
                    params![provider_ref, now, withdrawal_id],
                )?;
            }
            crate::ports::ProviderPayoutStatus::Submitted => {
                tx.execute(
                    "UPDATE commerce_withdrawals
                     SET status = 'processing', provider_ref = ?1, updated_at = ?2
                     WHERE id = ?3",
                    params![provider_ref, now, withdrawal_id],
                )?;
            }
        }
        let updated = load_withdrawal(&tx, withdrawal_id)?.ok_or(CommerceError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }

    /// 发起退款：冻结收款方余额 → 供应商退款 → 成功才入账。
    ///
    /// # Errors
    /// 意图不可退、金额超可退额、余额不足、供应商失败或存储失败。
    pub fn request_refund(
        &self,
        actor: &str,
        intent_id: &str,
        amount: Money,
        business_key: &str,
    ) -> Result<Refund> {
        store::validate_id("actor", actor)?;
        store::validate_id("intent", intent_id)?;
        store::validate_key(business_key)?;
        store::check_limit(amount, self.config.max_payment_minor)?;
        let digest = store::digest_hex(&[
            intent_id,
            actor,
            &amount.minor().to_string(),
            amount.currency().as_str(),
        ]);
        let now = self.now();
        let refund = {
            let mut guard = self.lock();
            let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if let Some(existing) = load_refund_by_key(&tx, business_key)? {
                if existing.0 != digest {
                    return Err(CommerceError::IdempotencyConflict);
                }
                return Ok(existing.1);
            }
            let intent = load_intent_row(&tx, intent_id)?.ok_or(CommerceError::NotFound)?;
            let refundable = intent.amount.minor() - intent.refunded_minor;
            if refundable <= 0 || amount.minor() > refundable {
                return Err(CommerceError::BadInput(
                    "refund exceeds the refundable amount".into(),
                ));
            }
            if amount.currency() != intent.amount.currency() {
                return Err(CommerceError::BadInput("refund currency mismatch".into()));
            }
            if !matches!(
                intent.status,
                IntentStatus::Succeeded | IntentStatus::PartiallyRefunded
            ) {
                return Err(CommerceError::IllegalTransition(
                    "intent is not refundable".into(),
                ));
            }
            let authorized = actor == intent.payee_id
                || (intent.direction == IntentDirection::Topup && actor == intent.actor_id);
            if !authorized {
                return Err(CommerceError::Forbidden);
            }
            let id = store::new_id(self.entropy());
            let hold_key = format!("refund:{id}:hold");
            let wallet = self.ensure_wallet_conn(&tx, &intent.payee_id, amount.currency(), now)?;
            wxlike_ledger::hold_in(&tx, &hold_key, &wallet, amount, now)
                .map_err(store::map_ledger)?;
            tx.execute(
                "INSERT INTO commerce_refunds
                    (id, business_key, intent_id, actor_id, amount_minor, currency, status,
                     hold_key, provider_ref, created_at, updated_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'processing', ?7, NULL, ?8, ?8)",
                params![
                    id,
                    business_key,
                    intent_id,
                    actor,
                    amount.minor(),
                    amount.currency().as_str(),
                    hold_key,
                    now
                ],
            )?;
            let refund = load_refund(&tx, &id)?.ok_or(CommerceError::NotFound)?;
            tx.commit()?;
            refund
        };
        self.dispatch_refund(&refund)
    }

    /// 用同一幂等键重试供应商退款。
    ///
    /// # Errors
    /// 不存在或供应商失败。
    pub fn refresh_refund(&self, actor: &str, refund_id: &str) -> Result<Refund> {
        let current = {
            let conn = self.lock();
            let refund = load_refund(&conn, refund_id)?.ok_or(CommerceError::NotFound)?;
            if refund.actor_id != actor {
                return Err(CommerceError::Forbidden);
            }
            refund
        };
        if current.status != RefundStatus::Processing {
            return Ok(current);
        }
        self.dispatch_refund(&current)
    }

    fn dispatch_refund(&self, refund: &Refund) -> Result<Refund> {
        let intent = {
            let guard = self.lock();
            load_intent_row(&guard, &refund.intent_id)?.ok_or(CommerceError::NotFound)?
        };
        let Some(provider_ref) = intent.provider_ref else {
            return Err(CommerceError::ProviderNotConfigured);
        };
        let request = crate::ports::ProviderRefundRequest {
            idempotency_key: format!("refund:{}", refund.id),
            provider_ref,
            amount: refund.amount,
        };
        match self.ports.provider.refund(&request) {
            Ok(result) => {
                if result.refunded != refund.amount {
                    return Err(CommerceError::BadInput(
                        "provider refunded amount mismatch".into(),
                    ));
                }
                self.apply_refund(&refund.id)
            }
            Err(error) => {
                if error == crate::ProviderError::Rejected {
                    let conn = self.lock();
                    release_refund_hold(&conn, &refund.id, self.now())?;
                }
                Err(map_provider(error))
            }
        }
    }

    fn apply_refund(&self, refund_id: &str) -> Result<Refund> {
        let now = self.now();
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let refund = load_refund(&tx, refund_id)?.ok_or(CommerceError::NotFound)?;
        if refund.status == RefundStatus::Succeeded {
            return Ok(refund);
        }
        let intent = load_intent_row(&tx, &refund.intent_id)?.ok_or(CommerceError::NotFound)?;
        let Some(provider_ref) = intent.provider_ref.clone() else {
            return Err(CommerceError::ProviderNotConfigured);
        };
        let external = store::ensure_external(
            &tx,
            self.provider_name(),
            refund.amount.currency(),
            self.config.fund_class,
            now,
        )?;
        let wallet = store::wallet_account(&intent.payee_id, refund.amount.currency());
        let request = PostingRequest::new(
            format!("refund:{refund_id}:settled"),
            "payment.refunded",
            refund.amount.currency(),
            vec![
                PostingLine::debit(wallet, refund.amount.minor()),
                PostingLine::credit(external, refund.amount.minor()),
            ],
        )
        .with_memo("refund settled");
        let hold_key = format!("refund:{refund_id}:hold");
        wxlike_ledger::capture_hold_in(&tx, &hold_key, &request, now).map_err(store::map_ledger)?;
        tx.execute(
            "UPDATE commerce_refunds
             SET status = 'succeeded', provider_ref = ?1, updated_at = ?2
             WHERE id = ?3",
            params![provider_ref, now, refund_id],
        )?;
        let refunded = intent.refunded_minor + refund.amount.minor();
        let status = if refunded >= intent.amount.minor() {
            IntentStatus::Refunded
        } else {
            IntentStatus::PartiallyRefunded
        };
        tx.execute(
            "UPDATE commerce_payment_intents
             SET refunded_minor = ?1, status = ?2, updated_at = ?3, version = version + 1
             WHERE id = ?4",
            params![refunded, status.as_str(), now, intent.id],
        )?;
        store::emit(
            &tx,
            self.entropy(),
            "payment.refunded",
            &intent.id,
            &serde_json::json!({
                "intent_id": intent.id,
                "refund_id": refund_id,
                "amount_minor": refund.amount.minor().to_string(),
            }),
            now,
        )?;
        let updated = load_refund(&tx, refund_id)?.ok_or(CommerceError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }

    /// 跑一次对账：以供应商对账单为准生成差异队列，不改任何账。
    ///
    /// # Errors
    /// 供应商不可用或存储失败。
    pub fn run_reconciliation(&self) -> Result<ReconciliationReport> {
        let provider_rows = self.ports.provider.statement().map_err(map_provider)?;
        #[derive(Clone)]
        struct Row {
            provider_ref: String,
            kind: StatementKind,
            currency: Currency,
            local_id: String,
        }
        let mut local = local_statement_rows(self)?;
        let mut checked = 0usize;
        let mut pending: Vec<(Row, String, i64)> = Vec::new();
        for row in &provider_rows {
            if !row.amount.is_positive() {
                continue;
            }
            checked += 1;
            if let Some(index) = local.iter().position(|candidate| {
                candidate.kind == row.kind
                    && candidate.provider_ref == row.provider_ref
                    && candidate.currency == row.amount.currency()
            }) {
                let candidate = local.remove(index);
                let delta = row.amount.minor() - candidate.amount_minor;
                if delta == 0 {
                    continue;
                }
                let external_delta = match row.kind {
                    StatementKind::Payment => delta,
                    StatementKind::Refund | StatementKind::Payout => -delta,
                };
                pending.push((
                    Row {
                        provider_ref: row.provider_ref.clone(),
                        kind: row.kind,
                        currency: row.amount.currency(),
                        local_id: candidate.local_id,
                    },
                    format!(
                        "amount_mismatch: provider {} local {}",
                        row.amount.minor(),
                        candidate.amount_minor
                    ),
                    external_delta,
                ));
            } else {
                let external_delta = match row.kind {
                    StatementKind::Payment => row.amount.minor(),
                    StatementKind::Refund | StatementKind::Payout => -row.amount.minor(),
                };
                pending.push((
                    Row {
                        provider_ref: row.provider_ref.clone(),
                        kind: row.kind,
                        currency: row.amount.currency(),
                        local_id: String::new(),
                    },
                    "provider_only".to_string(),
                    external_delta,
                ));
            }
        }
        for row in local {
            let external_delta = match row.kind {
                StatementKind::Payment => -row.amount_minor,
                StatementKind::Refund | StatementKind::Payout => row.amount_minor,
            };
            pending.push((
                Row {
                    provider_ref: row.provider_ref.clone(),
                    kind: row.kind,
                    currency: row.currency,
                    local_id: row.local_id,
                },
                "local_only".to_string(),
                external_delta,
            ));
        }
        let run_id = store::new_id(self.entropy());
        let now = self.now();
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut exceptions = Vec::new();
        for (row, reason, signed) in pending {
            let id = store::new_id(self.entropy());
            let kind = match row.kind {
                StatementKind::Payment => "payment",
                StatementKind::Refund => "refund",
                StatementKind::Payout => "payout",
            };
            tx.execute(
                "INSERT INTO commerce_reconciliation_exceptions
                    (id, run_id, provider_ref, local_kind, local_id, amount_minor, currency,
                     reason, status, created_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'open', ?9)",
                params![
                    id,
                    run_id,
                    row.provider_ref,
                    kind,
                    row.local_id,
                    signed,
                    row.currency.as_str(),
                    reason,
                    now
                ],
            )?;
            exceptions.push(load_exception(&tx, &id)?.ok_or(CommerceError::NotFound)?);
        }
        tx.commit()?;
        Ok(ReconciliationReport {
            run_id,
            checked,
            exceptions,
        })
    }

    /// 列出某次对账的差异。
    ///
    /// # Errors
    /// 存储失败。
    pub fn reconciliation_exceptions(&self, run_id: &str) -> Result<Vec<ReconciliationException>> {
        store::validate_id("run", run_id)?;
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, run_id, provider_ref, local_kind, local_id, amount_minor, currency,
                    reason, status, resolved_by, approval_ref, journal_key, created_at
             FROM commerce_reconciliation_exceptions
             WHERE run_id = ?1 ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![run_id], scan_exception)?;
        let mut items = Vec::new();
        for row in rows {
            items.push(build_exception(row?)?);
        }
        Ok(items)
    }

    /// 审批并挂账一条差异。补账本身是不可变分录，不删除任何历史。
    ///
    /// # Errors
    /// 差异不存在/已处理、缺少审批引用、参数非法或存储失败。
    pub fn resolve_exception(
        &self,
        approver: &str,
        exception_id: &str,
        approval_ref: &str,
        business_key: &str,
    ) -> Result<ReconciliationException> {
        store::validate_id("approver", approver)?;
        store::validate_key(approval_ref)?;
        store::validate_key(business_key)?;
        let now = self.now();
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exception = load_exception(&tx, exception_id)?.ok_or(CommerceError::NotFound)?;
        if exception.status != "open" {
            return Err(CommerceError::Conflict("exception already resolved".into()));
        }
        let currency = exception.amount.currency();
        let external = store::ensure_external(
            &tx,
            self.provider_name(),
            currency,
            self.config.fund_class,
            now,
        )?;
        let suspense = store::ensure_suspense(&tx, currency, self.config.fund_class, now)?;
        let amount = exception.amount.minor();
        if amount == 0 {
            return Err(CommerceError::BadInput("exception has no amount".into()));
        }
        let mut lines = if amount > 0 {
            vec![
                PostingLine::debit(external, amount),
                PostingLine::credit(suspense, amount),
            ]
        } else {
            vec![
                PostingLine::debit(suspense, -amount),
                PostingLine::credit(external, -amount),
            ]
        };
        lines.retain(|line| line.amount != 0);
        let request = PostingRequest::new(
            format!("recon:{exception_id}:{business_key}"),
            "reconciliation.adjustment",
            currency,
            lines,
        )
        .with_memo(format!("approval {approval_ref}"));
        let posted = wxlike_ledger::post_in(&tx, &request, now)?;
        tx.execute(
            "UPDATE commerce_reconciliation_exceptions
             SET status = 'resolved', resolved_by = ?1, approval_ref = ?2, journal_key = ?3
             WHERE id = ?4 AND status = 'open'",
            params![approver, approval_ref, posted.id, exception_id],
        )?;
        let updated = load_exception(&tx, exception_id)?.ok_or(CommerceError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }
}

fn map_intent_status(status: IntentStatus) -> TopupStatus {
    match status {
        IntentStatus::Created => TopupStatus::Created,
        IntentStatus::Pending => TopupStatus::Pending,
        IntentStatus::Succeeded => TopupStatus::Succeeded,
        IntentStatus::Failed => TopupStatus::Failed,
        IntentStatus::Expired => TopupStatus::Expired,
        IntentStatus::PartiallyRefunded | IntentStatus::Refunded => TopupStatus::Succeeded,
    }
}

fn release_hold(conn: &Connection, withdrawal_id: &str, status: &str, now: i64) -> Result<()> {
    wxlike_ledger::release_hold_in(conn, &format!("withdrawal:{withdrawal_id}:hold"), now)?;
    conn.execute(
        "UPDATE commerce_withdrawals SET status = ?1, updated_at = ?2 WHERE id = ?3",
        params![status, now, withdrawal_id],
    )?;
    Ok(())
}

fn release_refund_hold(conn: &Connection, refund_id: &str, now: i64) -> Result<()> {
    wxlike_ledger::release_hold_in(conn, &format!("refund:{refund_id}:hold"), now)?;
    conn.execute(
        "UPDATE commerce_refunds SET status = 'failed', updated_at = ?1 WHERE id = ?2",
        params![now, refund_id],
    )?;
    Ok(())
}

struct IntentRow {
    id: String,
    direction: IntentDirection,
    actor_id: String,
    payee_id: String,
    amount: Money,
    status: IntentStatus,
    provider_ref: Option<String>,
    refunded_minor: i64,
}

fn load_intent_row(conn: &Connection, id: &str) -> Result<Option<IntentRow>> {
    let raw = conn
        .query_row(
            "SELECT id, direction, actor_id, payee_id, amount_minor, currency, status,
                    provider_ref, refunded_minor
             FROM commerce_payment_intents WHERE id = ?1",
            params![id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, i64>(8)?,
                ))
            },
        )
        .optional()?;
    raw.map(
        |(id, direction, actor, payee, minor, currency, status, provider_ref, refunded)| {
            Ok(IntentRow {
                id,
                direction: IntentDirection::parse(&direction)
                    .ok_or_else(|| CommerceError::BadInput("stored intent direction".into()))?,
                actor_id: actor,
                payee_id: payee,
                amount: Money::from_minor(minor, Currency::parse(&currency)?),
                status: IntentStatus::parse(&status)
                    .ok_or_else(|| CommerceError::BadInput("stored intent status".into()))?,
                provider_ref,
                refunded_minor: refunded,
            })
        },
    )
    .transpose()
}

fn load_topup(conn: &Connection, id: &str) -> Result<Option<Topup>> {
    let raw = conn
        .query_row(
            "SELECT id, actor_id, intent_id, amount_minor, currency, status, provider_ref,
                    created_at, updated_at
             FROM commerce_topups WHERE id = ?1",
            params![id],
            scan_topup,
        )
        .optional()?;
    raw.map(build_topup).transpose()
}

fn load_topup_by_key(conn: &Connection, key: &str) -> Result<Option<(String, Topup)>> {
    let raw = conn
        .query_row(
            "SELECT id, actor_id, intent_id, amount_minor, currency, status, provider_ref,
                    created_at, updated_at
             FROM commerce_topups WHERE business_key = ?1",
            params![key],
            scan_topup,
        )
        .optional()?;
    raw.map(|raw| {
        let digest = store::digest_hex(&[
            &raw.actor_id,
            &raw.amount_minor.to_string(),
            &raw.currency,
            &raw.intent_id,
        ]);
        Ok((digest, build_topup(raw)?))
    })
    .transpose()
}

struct RawTopup {
    id: String,
    actor_id: String,
    intent_id: String,
    amount_minor: i64,
    currency: String,
    status: String,
    provider_ref: Option<String>,
    created_at: i64,
    updated_at: i64,
}

fn scan_topup(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawTopup> {
    Ok(RawTopup {
        id: r.get(0)?,
        actor_id: r.get(1)?,
        intent_id: r.get(2)?,
        amount_minor: r.get(3)?,
        currency: r.get(4)?,
        status: r.get(5)?,
        provider_ref: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

fn build_topup(raw: RawTopup) -> Result<Topup> {
    Ok(Topup {
        id: raw.id,
        actor_id: raw.actor_id,
        intent_id: raw.intent_id,
        amount: Money::from_minor(raw.amount_minor, Currency::parse(&raw.currency)?),
        status: TopupStatus::parse(&raw.status)?,
        provider_ref: raw.provider_ref,
        created_at: raw.created_at,
        updated_at: raw.updated_at,
    })
}

fn load_withdrawal(conn: &Connection, id: &str) -> Result<Option<Withdrawal>> {
    let raw = conn
        .query_row(
            "SELECT id, actor_id, amount_minor, currency, status, account_ref, provider_ref,
                    created_at, updated_at
             FROM commerce_withdrawals WHERE id = ?1",
            params![id],
            scan_withdrawal,
        )
        .optional()?;
    raw.map(build_withdrawal).transpose()
}

fn load_withdrawal_by_key(conn: &Connection, key: &str) -> Result<Option<(String, Withdrawal)>> {
    let raw = conn
        .query_row(
            "SELECT id, actor_id, amount_minor, currency, status, account_ref, provider_ref,
                    created_at, updated_at
             FROM commerce_withdrawals WHERE business_key = ?1",
            params![key],
            scan_withdrawal,
        )
        .optional()?;
    raw.map(|raw| {
        let digest = store::digest_hex(&[
            &raw.actor_id,
            &raw.account_ref,
            &raw.amount_minor.to_string(),
            &raw.currency,
        ]);
        Ok((digest, build_withdrawal(raw)?))
    })
    .transpose()
}

struct RawWithdrawal {
    id: String,
    actor_id: String,
    amount_minor: i64,
    currency: String,
    status: String,
    account_ref: String,
    provider_ref: Option<String>,
    created_at: i64,
    updated_at: i64,
}

fn scan_withdrawal(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawWithdrawal> {
    Ok(RawWithdrawal {
        id: r.get(0)?,
        actor_id: r.get(1)?,
        amount_minor: r.get(2)?,
        currency: r.get(3)?,
        status: r.get(4)?,
        account_ref: r.get(5)?,
        provider_ref: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

fn build_withdrawal(raw: RawWithdrawal) -> Result<Withdrawal> {
    Ok(Withdrawal {
        id: raw.id,
        actor_id: raw.actor_id,
        amount: Money::from_minor(raw.amount_minor, Currency::parse(&raw.currency)?),
        status: WithdrawalStatus::parse(&raw.status)?,
        account_ref: raw.account_ref,
        provider_ref: raw.provider_ref,
        created_at: raw.created_at,
        updated_at: raw.updated_at,
    })
}

fn load_refund(conn: &Connection, id: &str) -> Result<Option<Refund>> {
    let raw = conn
        .query_row(
            "SELECT id, intent_id, actor_id, amount_minor, currency, status, provider_ref,
                    created_at, updated_at
             FROM commerce_refunds WHERE id = ?1",
            params![id],
            scan_refund,
        )
        .optional()?;
    raw.map(build_refund).transpose()
}

fn load_refund_by_key(conn: &Connection, key: &str) -> Result<Option<(String, Refund)>> {
    let raw = conn
        .query_row(
            "SELECT id, intent_id, actor_id, amount_minor, currency, status, provider_ref,
                    created_at, updated_at
             FROM commerce_refunds WHERE business_key = ?1",
            params![key],
            scan_refund,
        )
        .optional()?;
    raw.map(|raw| {
        let digest = store::digest_hex(&[
            &raw.intent_id,
            &raw.actor_id,
            &raw.amount_minor.to_string(),
            &raw.currency,
        ]);
        Ok((digest, build_refund(raw)?))
    })
    .transpose()
}

struct RawRefund {
    id: String,
    intent_id: String,
    actor_id: String,
    amount_minor: i64,
    currency: String,
    status: String,
    provider_ref: Option<String>,
    created_at: i64,
    updated_at: i64,
}

fn scan_refund(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawRefund> {
    Ok(RawRefund {
        id: r.get(0)?,
        intent_id: r.get(1)?,
        actor_id: r.get(2)?,
        amount_minor: r.get(3)?,
        currency: r.get(4)?,
        status: r.get(5)?,
        provider_ref: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

fn build_refund(raw: RawRefund) -> Result<Refund> {
    Ok(Refund {
        id: raw.id,
        intent_id: raw.intent_id,
        actor_id: raw.actor_id,
        amount: Money::from_minor(raw.amount_minor, Currency::parse(&raw.currency)?),
        status: RefundStatus::parse(&raw.status)?,
        provider_ref: raw.provider_ref,
        created_at: raw.created_at,
        updated_at: raw.updated_at,
    })
}

fn load_exception(conn: &Connection, id: &str) -> Result<Option<ReconciliationException>> {
    let raw = conn
        .query_row(
            "SELECT id, run_id, provider_ref, local_kind, local_id, amount_minor, currency,
                    reason, status, resolved_by, approval_ref, journal_key, created_at
             FROM commerce_reconciliation_exceptions WHERE id = ?1",
            params![id],
            scan_exception,
        )
        .optional()?;
    raw.map(build_exception).transpose()
}

struct RawException {
    id: String,
    run_id: String,
    provider_ref: String,
    local_kind: String,
    local_id: String,
    amount_minor: i64,
    currency: String,
    reason: String,
    status: String,
    resolved_by: Option<String>,
    approval_ref: Option<String>,
    journal_key: Option<String>,
    created_at: i64,
}

fn scan_exception(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawException> {
    Ok(RawException {
        id: r.get(0)?,
        run_id: r.get(1)?,
        provider_ref: r.get(2)?,
        local_kind: r.get(3)?,
        local_id: r.get(4)?,
        amount_minor: r.get(5)?,
        currency: r.get(6)?,
        reason: r.get(7)?,
        status: r.get(8)?,
        resolved_by: r.get(9)?,
        approval_ref: r.get(10)?,
        journal_key: r.get(11)?,
        created_at: r.get(12)?,
    })
}

fn build_exception(raw: RawException) -> Result<ReconciliationException> {
    Ok(ReconciliationException {
        id: raw.id,
        run_id: raw.run_id,
        provider_ref: raw.provider_ref,
        local_kind: raw.local_kind,
        local_id: raw.local_id,
        amount: Money::from_minor(raw.amount_minor, Currency::parse(&raw.currency)?),
        reason: raw.reason,
        status: raw.status,
        resolved_by: raw.resolved_by,
        approval_ref: raw.approval_ref,
        journal_key: raw.journal_key,
        created_at: raw.created_at,
    })
}

struct LocalStatementRow {
    provider_ref: String,
    kind: StatementKind,
    amount_minor: i64,
    currency: Currency,
    local_id: String,
}

fn local_statement_rows(commerce: &Commerce) -> Result<Vec<LocalStatementRow>> {
    let conn = commerce.lock();
    let mut rows = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, provider_ref, amount_minor, currency FROM commerce_payment_intents
             WHERE provider_ref IS NOT NULL AND status IN ('succeeded', 'partially_refunded', 'refunded')",
        )?;
        let mapped = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in mapped {
            let (id, reference, amount, currency) = row?;
            rows.push(LocalStatementRow {
                provider_ref: reference,
                kind: StatementKind::Payment,
                amount_minor: amount,
                currency: Currency::parse(&currency)?,
                local_id: id,
            });
        }
    }
    {
        let mut stmt = conn.prepare(
            "SELECT r.id, i.provider_ref, r.amount_minor, r.currency
             FROM commerce_refunds r
             JOIN commerce_payment_intents i ON i.id = r.intent_id
             WHERE r.status = 'succeeded' AND i.provider_ref IS NOT NULL",
        )?;
        let mapped = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in mapped {
            let (id, reference, amount, currency) = row?;
            rows.push(LocalStatementRow {
                provider_ref: reference,
                kind: StatementKind::Refund,
                amount_minor: amount,
                currency: Currency::parse(&currency)?,
                local_id: id,
            });
        }
    }
    {
        let mut stmt = conn.prepare(
            "SELECT id, provider_ref, amount_minor, currency FROM commerce_withdrawals
             WHERE status = 'paid' AND provider_ref IS NOT NULL",
        )?;
        let mapped = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in mapped {
            let (id, reference, amount, currency) = row?;
            rows.push(LocalStatementRow {
                provider_ref: reference,
                kind: StatementKind::Payout,
                amount_minor: amount,
                currency: Currency::parse(&currency)?,
                local_id: id,
            });
        }
    }
    Ok(rows)
}
