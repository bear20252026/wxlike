//! G06-02：支付意图、二维码、回调验签/去重/乱序、绑卡不落敏感数据。

mod common;

use common::{cny, harness, webhook_body};
use wxlike_commerce::cards::CardInput;
use wxlike_commerce::payments::{IntentDirection, IntentStatus};
use wxlike_commerce::{Clock, CommerceError, ProviderError};

fn order_intent(h: &common::Harness, key: &str) -> wxlike_commerce::payments::PaymentIntent {
    h.commerce
        .create_payment_intent(IntentDirection::Order, "buyer", "seller", cny(9_900), key)
        .expect("intent")
        .0
}

#[test]
fn payment_credits_the_payee_once() {
    let h = harness();
    let intent = order_intent(&h, "order-1");
    assert_eq!(intent.status, IntentStatus::Pending);
    let paid = h
        .commerce
        .pay_payment_intent("buyer", &intent.id)
        .expect("pay");
    assert_eq!(paid.status, IntentStatus::Succeeded);
    let seller = h
        .commerce
        .wallet_balance("seller", cny(0).currency())
        .expect("balance");
    assert_eq!(seller.total(), cny(9_900));
    h.commerce.verify_books().expect("balanced");

    // 重复付款直接返回已成功，不会二次入账。
    let again = h
        .commerce
        .pay_payment_intent("buyer", &intent.id)
        .expect("repeat pay");
    assert_eq!(again.status, IntentStatus::Succeeded);
    let seller = h
        .commerce
        .wallet_balance("seller", cny(0).currency())
        .expect("balance");
    assert_eq!(seller.total(), cny(9_900));
}

#[test]
fn business_key_is_idempotent_and_rejects_parameter_changes() {
    let h = harness();
    let first = order_intent(&h, "same-key");
    let second = order_intent(&h, "same-key");
    assert_eq!(first.id, second.id);

    let error = h
        .commerce
        .create_payment_intent(
            IntentDirection::Order,
            "buyer",
            "seller",
            cny(1),
            "same-key",
        )
        .expect_err("digest conflict");
    assert!(matches!(error, CommerceError::IdempotencyConflict));
}

#[test]
fn signed_webhook_settles_and_replays_have_no_side_effects() {
    let h = harness();
    let intent = order_intent(&h, "hook-1");
    let provider_ref = intent.provider_ref.clone().expect("provider ref");
    let body = webhook_body(
        "evt-1",
        &intent.id,
        "payment.succeeded",
        &provider_ref,
        9_900,
        h.clock.now_ms(),
    );
    let signature = h.commerce.webhook_signature(&body);
    let outcome = h
        .commerce
        .handle_webhook(&body, &signature)
        .expect("webhook");
    assert!(outcome.applied);
    assert_eq!(outcome.status, IntentStatus::Succeeded);

    let replay = h.commerce.handle_webhook(&body, &signature);
    assert!(matches!(replay, Err(CommerceError::Replay)));
    let seller = h
        .commerce
        .wallet_balance("seller", cny(0).currency())
        .expect("balance");
    assert_eq!(seller.total(), cny(9_900));

    let failed_body = webhook_body(
        "evt-2",
        &intent.id,
        "payment.failed",
        &provider_ref,
        9_900,
        h.clock.now_ms(),
    );
    let signature = h.commerce.webhook_signature(&failed_body);
    let outcome = h
        .commerce
        .handle_webhook(&failed_body, &signature)
        .expect("late failure");
    assert!(!outcome.applied, "failed event must not regress a success");
    assert_eq!(outcome.status, IntentStatus::Succeeded);
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn forged_or_stale_webhooks_are_rejected() {
    let h = harness();
    let intent = order_intent(&h, "hook-2");
    let provider_ref = intent.provider_ref.clone().expect("provider ref");
    let body = webhook_body(
        "evt-forged",
        &intent.id,
        "payment.succeeded",
        &provider_ref,
        9_900,
        h.clock.now_ms(),
    );
    assert!(matches!(
        h.commerce.handle_webhook(&body, "00"),
        Err(CommerceError::InvalidSignature)
    ));

    let signature = h.commerce.webhook_signature(&body);
    let mut tampered = body.clone();
    tampered = tampered.replace("9900", "1");
    assert!(matches!(
        h.commerce.handle_webhook(&tampered, &signature),
        Err(CommerceError::InvalidSignature)
    ));

    let stale = webhook_body(
        "evt-stale",
        &intent.id,
        "payment.succeeded",
        &provider_ref,
        9_900,
        h.clock.now_ms() - 60 * 60 * 1000,
    );
    let signature = h.commerce.webhook_signature(&stale);
    assert!(matches!(
        h.commerce.handle_webhook(&stale, &signature),
        Err(CommerceError::InvalidSignature)
    ));
}

#[test]
fn lost_payment_response_recovers_via_webhook_without_double_posting() {
    let h = harness();
    let intent = order_intent(&h, "lost-response");
    let provider_ref = intent.provider_ref.clone().expect("provider ref");
    h.handle.fail_next_pay(ProviderError::Timeout);
    assert!(matches!(
        h.commerce.pay_payment_intent("buyer", &intent.id),
        Err(CommerceError::ProviderTimeout)
    ));
    let pending = h
        .commerce
        .payment_intent("buyer", &intent.id)
        .expect("intent");
    assert_eq!(pending.status, IntentStatus::Pending);

    let body = webhook_body(
        "evt-recovered",
        &intent.id,
        "payment.succeeded",
        &provider_ref,
        9_900,
        h.clock.now_ms(),
    );
    let signature = h.commerce.webhook_signature(&body);
    assert!(
        h.commerce
            .handle_webhook(&body, &signature)
            .expect("webhook")
            .applied
    );
    let seller = h
        .commerce
        .wallet_balance("seller", cny(0).currency())
        .expect("balance");
    assert_eq!(seller.total(), cny(9_900));
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn qr_ticket_is_bound_to_intent_amount_currency_and_payee() {
    let h = harness();
    let (_, ticket) = h
        .commerce
        .create_payment_intent(IntentDirection::Order, "buyer", "seller", cny(500), "qr-1")
        .expect("intent");
    let parts: Vec<&str> = ticket.token.split('.').collect();
    let tampered = format!(
        "{}.{}.1.{}.{}.{}.{}",
        parts[0], parts[1], parts[3], parts[4], parts[5], parts[6]
    );
    assert!(matches!(
        h.commerce.verify_ticket(&tampered),
        Err(CommerceError::InvalidSignature)
    ));

    let paid = h
        .commerce
        .pay_with_ticket("buyer", &ticket.token)
        .expect("scan to pay");
    assert_eq!(paid.status, IntentStatus::Succeeded);

    let (_, expired) = h
        .commerce
        .create_payment_intent(IntentDirection::Order, "buyer", "seller", cny(500), "qr-2")
        .expect("intent");
    h.clock.advance(16 * 60 * 1000);
    assert!(matches!(
        h.commerce.verify_ticket(&expired.token),
        Err(CommerceError::Expired)
    ));
}

#[test]
fn only_participants_can_read_a_payment_intent() {
    let h = harness();
    let intent = order_intent(&h, "authz-1");
    assert!(matches!(
        h.commerce.payment_intent("stranger", &intent.id),
        Err(CommerceError::Forbidden)
    ));
    assert!(matches!(
        h.commerce.pay_payment_intent("seller", &intent.id),
        Err(CommerceError::Forbidden)
    ));
}

#[test]
fn cards_store_only_tokens_and_masks() {
    let h = harness();
    let pan = "4242424242424242";
    let input = CardInput {
        pan,
        cvv: "123",
        brand: "visa",
        exp_month: 12,
        exp_year: 2030,
    };
    let card = h
        .commerce
        .bind_card("alice", &input, "card-1")
        .expect("bind");
    assert_eq!(card.last4, "4242");
    let again = h
        .commerce
        .bind_card("alice", &input, "card-1")
        .expect("idempotent bind");
    assert_eq!(card.id, again.id);
    assert_eq!(h.commerce.list_cards("alice").expect("list").len(), 1);

    let bad_pan = CardInput {
        pan: "4242424242424241",
        cvv: "123",
        brand: "visa",
        exp_month: 12,
        exp_year: 2030,
    };
    assert!(h.commerce.bind_card("alice", &bad_pan, "card-2").is_err());

    let expired = CardInput {
        pan,
        cvv: "123",
        brand: "visa",
        exp_month: 12,
        exp_year: 2020,
    };
    assert!(h.commerce.bind_card("alice", &expired, "card-3").is_err());

    h.commerce.unbind_card("alice", &card.id).expect("unbind");
    h.commerce
        .unbind_card("alice", &card.id)
        .expect("idempotent");
    assert!(h.commerce.list_cards("alice").expect("list").is_empty());
    assert!(matches!(
        h.commerce.unbind_card("bob", &card.id),
        Err(CommerceError::Forbidden)
    ));
}

#[test]
fn funded_payee_can_refund_but_strangers_cannot() {
    let h = harness();
    let intent = order_intent(&h, "refund-auth");
    h.commerce
        .pay_payment_intent("buyer", &intent.id)
        .expect("pay");
    assert!(matches!(
        h.commerce
            .request_refund("buyer", &intent.id, cny(100), "r-1"),
        Err(CommerceError::Forbidden)
    ));
}
