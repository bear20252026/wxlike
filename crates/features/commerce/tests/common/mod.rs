//! G06 集成测试夹具：真实 SQLite + 真实领域实现 + 沙箱供应商。

#![allow(dead_code)]

use std::sync::Arc;

use wxlike_commerce::infrastructure::sandbox::{SandboxHandle, SandboxProvider};
use wxlike_commerce::testkit::{FixedEntropy, ManualClock, StaticMembership};
use wxlike_commerce::{Commerce, CommerceConfig, Money, Ports};
use wxlike_ledger::Currency;

pub struct Harness {
    pub commerce: Arc<Commerce>,
    pub handle: SandboxHandle,
    pub clock: Arc<ManualClock>,
    pub membership: Arc<StaticMembership>,
}

pub fn harness() -> Harness {
    let provider = SandboxProvider::new();
    let handle = provider.handle();
    let clock = Arc::new(ManualClock::new(1_700_000_000_000));
    let membership = Arc::new(StaticMembership::new());
    let ports = Ports::new(Arc::new(provider), membership.clone())
        .with_clock_entropy(clock.clone(), Arc::new(FixedEntropy::default()));
    let commerce = Commerce::in_memory(CommerceConfig::sandbox(b"webhook-secret".to_vec()), ports)
        .expect("commerce");
    Harness {
        commerce: Arc::new(commerce),
        handle,
        clock,
        membership,
    }
}

pub fn cny(minor: i64) -> Money {
    Money::from_minor(minor, Currency::CNY)
}

pub fn fund(h: &Harness, user: &str, minor: i64) {
    h.commerce
        .sandbox_credit(user, cny(minor), &format!("fund:{user}:{minor}"))
        .expect("sandbox credit");
}

#[allow(clippy::too_many_arguments)]
pub fn webhook_body(
    event_id: &str,
    intent_id: &str,
    kind: &str,
    provider_ref: &str,
    amount_minor: i64,
    timestamp_ms: i64,
) -> String {
    format!(
        "{{\"event_id\":\"{event_id}\",\"intent_id\":\"{intent_id}\",\"kind\":\"{kind}\",\"provider_ref\":\"{provider_ref}\",\"amount_minor\":{amount_minor},\"currency\":\"CNY\",\"timestamp_ms\":{timestamp_ms}}}"
    )
}
