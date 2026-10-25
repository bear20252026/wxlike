#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use wxlike_service_catalog::{
    Clock, PaymentIntent, PaymentIntentRequest, PaymentPort, ProviderError, RefundReceipt,
    RefundRequest, SandboxEvidence, ServiceCard, ServiceType, SupportPort,
};

pub struct TestClock(Mutex<i64>);

impl TestClock {
    pub fn new(now_ms: i64) -> Self {
        Self(Mutex::new(now_ms))
    }

    pub fn advance(&self, delta_ms: i64) {
        *self.0.lock().unwrap() += delta_ms;
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        *self.0.lock().unwrap()
    }
}

pub fn evidence() -> Option<SandboxEvidence> {
    Some(SandboxEvidence {
        environment: "sandbox".to_owned(),
        reference: "g07-sandbox-run".to_owned(),
        verified_at_ms: 1_000,
    })
}

pub fn card(service_id: &str, kind: ServiceType, provider: &str, region: &str) -> ServiceCard {
    ServiceCard {
        service_id: service_id.to_owned(),
        kind,
        title: format!("{service_id} title"),
        provider: provider.to_owned(),
        region: region.to_owned(),
        currency: "CNY".to_owned(),
        display_price_minor: 0,
        terms_version: "terms-v1".to_owned(),
        enabled: true,
        revision: 0,
    }
}

#[derive(Default)]
pub struct LedgerPayment {
    pub decline: AtomicBool,
    pub refund_fail: AtomicBool,
    intents: Mutex<Vec<PaymentIntentRequest>>,
    refunds: Mutex<Vec<RefundRequest>>,
}

impl LedgerPayment {
    pub fn intent_count(&self) -> usize {
        self.intents.lock().unwrap().len()
    }

    pub fn refund_count(&self) -> usize {
        self.refunds.lock().unwrap().len()
    }

    pub fn refunded_minor(&self) -> i64 {
        self.refunds
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.amount_minor)
            .sum()
    }
}

#[async_trait]
impl PaymentPort for LedgerPayment {
    async fn create_intent(
        &self,
        request: &PaymentIntentRequest,
    ) -> Result<PaymentIntent, ProviderError> {
        self.intents.lock().unwrap().push(request.clone());
        let succeeded = !self.decline.load(Ordering::SeqCst);
        Ok(PaymentIntent {
            payment_id: format!("pay-{}", request.order_id),
            succeeded,
            decline_reason: if succeeded {
                None
            } else {
                Some("insufficient".to_owned())
            },
        })
    }

    async fn refund(&self, request: &RefundRequest) -> Result<RefundReceipt, ProviderError> {
        if self.refund_fail.load(Ordering::SeqCst) {
            return Err(ProviderError::Unavailable);
        }
        self.refunds.lock().unwrap().push(request.clone());
        Ok(RefundReceipt {
            refund_id: format!("refund-{}", request.order_id),
            amount_minor: request.amount_minor,
            status: "succeeded".to_owned(),
        })
    }
}

#[derive(Default)]
pub struct RecordingSupport {
    tickets: Mutex<Vec<String>>,
}

impl RecordingSupport {
    pub fn count(&self) -> usize {
        self.tickets.lock().unwrap().len()
    }
}

#[async_trait]
impl SupportPort for RecordingSupport {
    async fn open_ticket(
        &self,
        actor: &str,
        order_id: &str,
        reason: &str,
    ) -> Result<String, ProviderError> {
        self.tickets
            .lock()
            .unwrap()
            .push(format!("{actor}:{order_id}:{reason}"));
        Ok(format!("ticket-{order_id}"))
    }
}
