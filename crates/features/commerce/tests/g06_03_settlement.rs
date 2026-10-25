//! G06-03：充值、提现超时恢复、退款上限、对账差异与审批挂账。

mod common;

use common::{cny, fund, harness};
use wxlike_commerce::payments::IntentStatus;
use wxlike_commerce::ports::{ProviderPayoutStatus, ProviderStatementRow, StatementKind};
use wxlike_commerce::settlement::{RefundStatus, TopupStatus, WithdrawalStatus};
use wxlike_commerce::{CommerceError, ProviderError};

#[test]
fn topup_is_settled_by_the_payment_intent() {
    let h = harness();
    let (topup, _) = h
        .commerce
        .create_topup("alice", cny(5_000), "topup-1")
        .expect("topup");
    assert_eq!(topup.status, TopupStatus::Pending);
    h.commerce
        .pay_payment_intent("alice", &topup.intent_id)
        .expect("pay");
    let topup = h
        .commerce
        .confirm_topup("alice", &topup.id)
        .expect("confirm");
    assert_eq!(topup.status, TopupStatus::Succeeded);
    let balance = h
        .commerce
        .wallet_balance("alice", cny(0).currency())
        .expect("balance");
    assert_eq!(balance.total(), cny(5_000));
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn withdrawal_timeout_keeps_funds_held_then_recovers_without_double_pay() {
    let h = harness();
    fund(&h, "alice", 10_000);
    h.handle.fail_next_payout(ProviderError::Timeout);
    let error = h
        .commerce
        .request_withdrawal("alice", cny(10_000), "acct-1", "wd-1")
        .expect_err("timeout");
    assert!(matches!(error, CommerceError::ProviderTimeout));

    let held = h
        .commerce
        .wallet_balance("alice", cny(0).currency())
        .expect("balance");
    assert_eq!(held.locked_minor, 10_000);
    assert_eq!(held.available_minor, 0);

    let pending = h
        .commerce
        .request_withdrawal("alice", cny(10_000), "acct-1", "wd-1")
        .expect("idempotent retry");
    assert_eq!(pending.status, WithdrawalStatus::Processing);
    let withdrawal = h
        .commerce
        .refresh_withdrawal("alice", &pending.id)
        .expect("refresh");
    assert_eq!(withdrawal.status, WithdrawalStatus::Paid);
    let balance = h
        .commerce
        .wallet_balance("alice", cny(0).currency())
        .expect("balance");
    assert_eq!(balance.total_minor, 0);
    assert_eq!(balance.locked_minor, 0);

    let retry = h
        .commerce
        .request_withdrawal("alice", cny(10_000), "acct-1", "wd-1")
        .expect("retry");
    assert_eq!(retry.id, withdrawal.id);
    let payouts = h
        .handle
        .statement()
        .into_iter()
        .filter(|row| row.kind == StatementKind::Payout)
        .count();
    assert_eq!(payouts, 1, "must not pay twice");
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn rejected_withdrawal_releases_the_hold() {
    let h = harness();
    fund(&h, "bob", 5_000);
    h.handle.fail_next_payout(ProviderError::Rejected);
    assert!(matches!(
        h.commerce
            .request_withdrawal("bob", cny(5_000), "acct-2", "wd-2"),
        Err(CommerceError::ProviderRejected)
    ));
    let balance = h
        .commerce
        .wallet_balance("bob", cny(0).currency())
        .expect("balance");
    assert_eq!(balance.available_minor, 5_000);
    let failed = h
        .commerce
        .withdrawal("bob", "missing")
        .expect_err("unknown");
    assert!(matches!(failed, CommerceError::NotFound));
}

#[test]
fn partial_refunds_respect_the_cap_and_are_idempotent() {
    let h = harness();
    let (intent, _) = h
        .commerce
        .create_payment_intent(
            wxlike_commerce::payments::IntentDirection::Order,
            "buyer",
            "seller",
            cny(10_000),
            "pay-refund",
        )
        .expect("intent");
    h.commerce
        .pay_payment_intent("buyer", &intent.id)
        .expect("pay");

    let first = h
        .commerce
        .request_refund("seller", &intent.id, cny(3_000), "refund-1")
        .expect("partial refund");
    assert_eq!(first.status, RefundStatus::Succeeded);
    let again = h
        .commerce
        .request_refund("seller", &intent.id, cny(3_000), "refund-1")
        .expect("idempotent refund");
    assert_eq!(again.id, first.id);
    let seller = h
        .commerce
        .wallet_balance("seller", cny(0).currency())
        .expect("balance");
    assert_eq!(seller.total(), cny(7_000));

    assert!(matches!(
        h.commerce
            .request_refund("seller", &intent.id, cny(7_001), "refund-too-much"),
        Err(CommerceError::BadInput(_))
    ));
    let rest = h
        .commerce
        .request_refund("seller", &intent.id, cny(7_000), "refund-2")
        .expect("rest refund");
    assert_eq!(rest.status, RefundStatus::Succeeded);
    let intent = h
        .commerce
        .payment_intent("buyer", &intent.id)
        .expect("intent");
    assert_eq!(intent.status, IntentStatus::Refunded);
    let seller = h
        .commerce
        .wallet_balance("seller", cny(0).currency())
        .expect("balance");
    assert_eq!(seller.total_minor, 0);
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn reconciliation_flags_differences_and_requires_approval_to_adjust() {
    let h = harness();
    let (intent, _) = h
        .commerce
        .create_payment_intent(
            wxlike_commerce::payments::IntentDirection::Order,
            "buyer",
            "seller",
            cny(5_000),
            "recon-pay",
        )
        .expect("intent");
    h.commerce
        .pay_payment_intent("buyer", &intent.id)
        .expect("pay");

    let clean = h.commerce.run_reconciliation().expect("run");
    assert_eq!(clean.checked, 1);
    assert!(clean.exceptions.is_empty());

    h.handle.set_statement(vec![
        ProviderStatementRow {
            provider_ref: "ghost_ref".to_string(),
            kind: StatementKind::Payment,
            amount: cny(1_234),
        },
        ProviderStatementRow {
            provider_ref: intent.provider_ref.clone().expect("ref"),
            kind: StatementKind::Payment,
            amount: cny(5_000),
        },
    ]);
    let report = h.commerce.run_reconciliation().expect("run with ghost");
    assert_eq!(report.exceptions.len(), 1);
    let ghost = &report.exceptions[0];
    assert_eq!(ghost.provider_ref, "ghost_ref");
    assert!(ghost.reason.contains("provider_only"));

    h.clock.advance(1_000);
    let resolved = h
        .commerce
        .resolve_exception("admin", &ghost.id, "AP-0001", "adj-1")
        .expect("approve");
    assert_eq!(resolved.status, "resolved");
    assert_eq!(resolved.approval_ref.as_deref(), Some("AP-0001"));
    assert!(resolved.journal_key.is_some());
    h.commerce
        .verify_books()
        .expect("balanced after adjustment");
    assert!(matches!(
        h.commerce
            .resolve_exception("admin", &ghost.id, "AP-0002", "adj-2"),
        Err(CommerceError::Conflict(_))
    ));
    h.handle.clear_statement_override();
}

#[test]
fn sandbox_and_live_provider_modes_cannot_mix() {
    use wxlike_commerce::{CommerceConfig, FundClass};
    let mut config = CommerceConfig::sandbox(b"webhook".to_vec());
    config.fund_class = FundClass::Real;
    assert!(config.validate().is_err());
    let live = CommerceConfig::live(FundClass::Real, "prod/pay");
    assert!(live.validate().is_err(), "webhook secret still unresolved");
}

#[test]
fn payout_status_override_is_still_reported() {
    let h = harness();
    fund(&h, "carol", 2_000);
    h.handle.set_payout_status(ProviderPayoutStatus::Submitted);
    let window = h
        .commerce
        .request_withdrawal("carol", cny(2_000), "acct-3", "wd-3")
        .expect("submitted");
    assert_eq!(window.status, WithdrawalStatus::Processing);
    let provider_ref = window.provider_ref.clone().expect("provider ref");
    h.handle
        .force_payout_status(&provider_ref, ProviderPayoutStatus::Paid);
    let paid = h
        .commerce
        .refresh_withdrawal("carol", &window.id)
        .expect("paid");
    assert_eq!(paid.status, WithdrawalStatus::Paid);
    h.commerce.verify_books().expect("balanced");
}
