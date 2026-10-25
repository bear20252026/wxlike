//! 沙箱支付供应商。
//!
//! * 所有引用由幂等键确定性派生，重复调用返回同一笔；
//! * 默认支付成功、出款到账，可用 [`SandboxHandle`] 改结果或注入故障；
//! * 对账单记录真实发生的沙箱动作，也可整体替换以模拟供应商差异。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use sha2::{Digest as _, Sha256};
use wxlike_ledger::{Currency, Money};

use crate::config::ProviderMode;
use crate::ports::{
    PaymentProvider, ProviderError, ProviderIntent, ProviderIntentRequest, ProviderPayment,
    ProviderPaymentStatus, ProviderPayout, ProviderPayoutRequest, ProviderPayoutStatus,
    ProviderRefund, ProviderRefundRequest, ProviderStatementRow, StatementKind,
};

#[derive(Clone, Debug)]
struct SandboxIntent {
    provider_ref: String,
    amount: Money,
    status: ProviderPaymentStatus,
}

#[derive(Clone, Debug, Default)]
struct Faults {
    create: Option<ProviderError>,
    pay: Option<ProviderError>,
    query: Option<ProviderError>,
    refund: Option<ProviderError>,
    payout: Option<ProviderError>,
}

#[derive(Debug, Default)]
struct SandboxState {
    intents: HashMap<String, SandboxIntent>,
    by_key: HashMap<String, String>,
    payments: HashMap<String, ProviderPaymentStatus>,
    refunds: HashMap<String, ProviderRefund>,
    payouts: HashMap<String, ProviderPayout>,
    payout_by_key: HashMap<String, String>,
    statement: Vec<ProviderStatementRow>,
    statement_override: Option<Vec<ProviderStatementRow>>,
    faults: Faults,
    default_payment_status: Option<ProviderPaymentStatus>,
    default_payout_status: Option<ProviderPayoutStatus>,
}

impl SandboxState {
    fn take(fault: &mut Option<ProviderError>) -> std::result::Result<(), ProviderError> {
        match fault.take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn reference(prefix: &str, key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    format!("{prefix}_{}", hex::encode(&digest[..12]))
}

/// 沙箱供应商。克隆共享同一份状态。
#[derive(Clone, Debug)]
pub struct SandboxProvider {
    state: Arc<Mutex<SandboxState>>,
}

impl SandboxProvider {
    /// 全新沙箱。
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(SandboxState::default())),
        }
    }

    /// 拿到故障注入/对账控制句柄。
    #[must_use]
    pub fn handle(&self) -> SandboxHandle {
        SandboxHandle {
            state: Arc::clone(&self.state),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SandboxState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for SandboxProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// 沙箱控制句柄：故障注入、状态覆盖、对账单替换。
#[derive(Clone, Debug)]
pub struct SandboxHandle {
    state: Arc<Mutex<SandboxState>>,
}

impl SandboxHandle {
    fn lock(&self) -> std::sync::MutexGuard<'_, SandboxState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 下一次创建支付单失败。
    pub fn fail_next_create(&self, error: ProviderError) {
        self.lock().faults.create = Some(error);
    }

    /// 下一次付款动作失败。
    pub fn fail_next_pay(&self, error: ProviderError) {
        self.lock().faults.pay = Some(error);
    }

    /// 下一次查询失败。
    pub fn fail_next_query(&self, error: ProviderError) {
        self.lock().faults.query = Some(error);
    }

    /// 下一次退款失败。
    pub fn fail_next_refund(&self, error: ProviderError) {
        self.lock().faults.refund = Some(error);
    }

    /// 下一次出款失败。
    pub fn fail_next_payout(&self, error: ProviderError) {
        self.lock().faults.payout = Some(error);
    }

    /// 覆盖后续付款动作的默认结果。
    pub fn set_payment_status(&self, status: ProviderPaymentStatus) {
        self.lock().default_payment_status = Some(status);
    }

    /// 覆盖后续出款的默认结果。
    pub fn set_payout_status(&self, status: ProviderPayoutStatus) {
        self.lock().default_payout_status = Some(status);
    }

    /// 把某笔支付单改成一个终态，用于模拟乱序回调后的真实状态。
    pub fn force_payment_status(&self, provider_ref: &str, status: ProviderPaymentStatus) {
        self.lock()
            .payments
            .insert(provider_ref.to_string(), status);
    }

    /// 把某笔出款单改成一个新状态，用于模拟供应商侧状态推进。
    pub fn force_payout_status(&self, provider_ref: &str, status: ProviderPayoutStatus) {
        let mut state = self.lock();
        let Some(payout) = state.payouts.get_mut(provider_ref) else {
            return;
        };
        payout.status = status;
        let amount = payout.amount;
        if status == ProviderPayoutStatus::Paid
            && !state
                .statement
                .iter()
                .any(|row| row.provider_ref == provider_ref)
        {
            state.statement.push(ProviderStatementRow {
                provider_ref: provider_ref.to_string(),
                kind: StatementKind::Payout,
                amount,
            });
        }
    }

    /// 整体替换对账单（模拟供应商多记/少记/金额不符）。
    pub fn set_statement(&self, rows: Vec<ProviderStatementRow>) {
        self.lock().statement_override = Some(rows);
    }

    /// 清空对账单替换，回到按真实动作记录。
    pub fn clear_statement_override(&self) {
        self.lock().statement_override = None;
    }

    /// 当前真实的沙箱动作对账单。
    #[must_use]
    pub fn statement(&self) -> Vec<ProviderStatementRow> {
        self.lock().statement.clone()
    }
}

impl PaymentProvider for SandboxProvider {
    fn mode(&self) -> ProviderMode {
        ProviderMode::Sandbox
    }

    fn create_intent(
        &self,
        req: &ProviderIntentRequest,
    ) -> std::result::Result<ProviderIntent, ProviderError> {
        let mut state = self.lock();
        SandboxState::take(&mut state.faults.create)?;
        if let Some(reference) = state.by_key.get(&req.idempotency_key) {
            let intent = state
                .intents
                .get(reference)
                .ok_or(ProviderError::Unavailable)?;
            return Ok(ProviderIntent {
                provider_ref: intent.provider_ref.clone(),
                status: intent.status,
            });
        }
        let provider_ref = reference("sbx_pi", &req.idempotency_key);
        let intent = SandboxIntent {
            provider_ref: provider_ref.clone(),
            amount: req.amount,
            status: ProviderPaymentStatus::Pending,
        };
        state
            .by_key
            .insert(req.idempotency_key.clone(), provider_ref.clone());
        state
            .payments
            .insert(provider_ref.clone(), ProviderPaymentStatus::Pending);
        state.intents.insert(provider_ref.clone(), intent);
        Ok(ProviderIntent {
            provider_ref,
            status: ProviderPaymentStatus::Pending,
        })
    }

    fn pay(&self, provider_ref: &str) -> std::result::Result<ProviderPayment, ProviderError> {
        let mut state = self.lock();
        SandboxState::take(&mut state.faults.pay)?;
        let intent = state
            .intents
            .get(provider_ref)
            .cloned()
            .ok_or(ProviderError::Rejected)?;
        let status = state
            .default_payment_status
            .unwrap_or(ProviderPaymentStatus::Succeeded);
        state.payments.insert(provider_ref.to_string(), status);
        if status == ProviderPaymentStatus::Succeeded {
            state.statement.push(ProviderStatementRow {
                provider_ref: intent.provider_ref.clone(),
                kind: StatementKind::Payment,
                amount: intent.amount,
            });
        }
        Ok(ProviderPayment {
            provider_ref: intent.provider_ref,
            status,
            amount: intent.amount,
        })
    }

    fn query_payment(
        &self,
        provider_ref: &str,
    ) -> std::result::Result<ProviderPayment, ProviderError> {
        let mut state = self.lock();
        SandboxState::take(&mut state.faults.query)?;
        let intent = state
            .intents
            .get(provider_ref)
            .cloned()
            .ok_or(ProviderError::Rejected)?;
        let status = state
            .payments
            .get(provider_ref)
            .copied()
            .unwrap_or(intent.status);
        Ok(ProviderPayment {
            provider_ref: intent.provider_ref,
            status,
            amount: intent.amount,
        })
    }

    fn refund(
        &self,
        req: &ProviderRefundRequest,
    ) -> std::result::Result<ProviderRefund, ProviderError> {
        let mut state = self.lock();
        SandboxState::take(&mut state.faults.refund)?;
        if let Some(existing) = state.refunds.get(&req.idempotency_key) {
            return Ok(existing.clone());
        }
        if !state.intents.contains_key(&req.provider_ref) {
            return Err(ProviderError::Rejected);
        }
        let result = ProviderRefund {
            provider_ref: req.provider_ref.clone(),
            refunded: req.amount,
        };
        state
            .refunds
            .insert(req.idempotency_key.clone(), result.clone());
        state.statement.push(ProviderStatementRow {
            provider_ref: req.provider_ref.clone(),
            kind: StatementKind::Refund,
            amount: req.amount,
        });
        Ok(result)
    }

    fn payout(
        &self,
        req: &ProviderPayoutRequest,
    ) -> std::result::Result<ProviderPayout, ProviderError> {
        let mut state = self.lock();
        SandboxState::take(&mut state.faults.payout)?;
        if let Some(reference) = state.payout_by_key.get(&req.idempotency_key) {
            return state
                .payouts
                .get(reference)
                .cloned()
                .ok_or(ProviderError::Unavailable);
        }
        let provider_ref = reference("sbx_po", &req.idempotency_key);
        let status = state
            .default_payout_status
            .unwrap_or(ProviderPayoutStatus::Paid);
        let payout = ProviderPayout {
            provider_ref: provider_ref.clone(),
            status,
            amount: req.amount,
        };
        state
            .payout_by_key
            .insert(req.idempotency_key.clone(), provider_ref.clone());
        state.payouts.insert(provider_ref.clone(), payout.clone());
        if status == ProviderPayoutStatus::Paid {
            state.statement.push(ProviderStatementRow {
                provider_ref,
                kind: StatementKind::Payout,
                amount: req.amount,
            });
        }
        Ok(payout)
    }

    fn statement(&self) -> std::result::Result<Vec<ProviderStatementRow>, ProviderError> {
        let state = self.lock();
        Ok(state
            .statement_override
            .clone()
            .unwrap_or_else(|| state.statement.clone()))
    }
}

/// 沙箱人民币金额快捷构造（测试用）。
#[must_use]
pub fn cny(minor: i64) -> Money {
    Money::from_minor(minor, Currency::CNY)
}
