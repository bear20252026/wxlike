//! G06-05：规则版本、权益认领、一次性核销、票证条码与订单状态机。

mod common;

use common::{cny, harness};
use serde_json::json;
use wxlike_commerce::entitlements::{EntitlementKind, EntitlementStatus};
use wxlike_commerce::orders::OrderStatus;
use wxlike_commerce::payments::IntentStatus;
use wxlike_commerce::{Clock, CommerceError, ProviderError};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

#[test]
fn rules_are_versioned_and_issues_pin_the_latest_version() {
    let h = harness();
    let first = h
        .commerce
        .publish_entitlement_rule(
            "shop",
            EntitlementKind::Card,
            &json!({"valid_days": 30, "tier": "silver"}),
        )
        .expect("rule v1");
    let second = h
        .commerce
        .publish_entitlement_rule(
            "shop",
            EntitlementKind::Card,
            &json!({"valid_days": 60, "tier": "gold"}),
        )
        .expect("rule v2");
    assert_eq!((first.version, second.version), (1, 2));
    assert_ne!(first.digest, second.digest);

    let issued = h
        .commerce
        .issue_entitlement(
            "shop",
            Some("alice"),
            EntitlementKind::Card,
            &json!({"seat": "A1"}),
            "card-1",
        )
        .expect("issue");
    let now = h.clock.now_ms();
    assert_eq!(issued.rule_version, 2);
    assert_eq!(issued.valid_to, now + 60 * DAY_MS);
    assert_eq!(issued.status, EntitlementStatus::Active);

    assert!(matches!(
        h.commerce.issue_entitlement(
            "nobody",
            Some("alice"),
            EntitlementKind::Card,
            &json!({}),
            "card-2"
        ),
        Err(CommerceError::BadInput(_))
    ));
    assert!(matches!(
        h.commerce
            .publish_entitlement_rule("shop", EntitlementKind::Coupon, &json!([1, 2])),
        Err(CommerceError::BadInput(_))
    ));
    let conflict = h.commerce.issue_entitlement(
        "shop",
        Some("alice"),
        EntitlementKind::Card,
        &json!({"seat": "B2"}),
        "card-1",
    );
    assert!(matches!(conflict, Err(CommerceError::IdempotencyConflict)));
}

#[test]
fn unowned_entitlement_is_claimed_once_then_redeemed_once() {
    let h = harness();
    h.commerce
        .publish_entitlement_rule("shop", EntitlementKind::Coupon, &json!({"valid_days": 10}))
        .expect("rule");
    let issued = h
        .commerce
        .issue_entitlement(
            "shop",
            None,
            EntitlementKind::Coupon,
            &json!({"discount": 20}),
            "coupon-1",
        )
        .expect("issue");
    assert_eq!(issued.owner_id, None);
    assert!(matches!(
        h.commerce.entitlement("bob", &issued.id),
        Err(CommerceError::Forbidden)
    ));

    let claimed = h
        .commerce
        .claim_entitlement("bob", &issued.id)
        .expect("claim");
    assert_eq!(claimed.owner_id.as_deref(), Some("bob"));
    assert_eq!(
        h.commerce
            .claim_entitlement("bob", &issued.id)
            .expect("idempotent"),
        claimed
    );
    assert!(matches!(
        h.commerce.claim_entitlement("carol", &issued.id),
        Err(CommerceError::Forbidden)
    ));
    assert_eq!(
        h.commerce
            .entitlement("shop", &issued.id)
            .expect("issuer view")
            .id,
        issued.id
    );

    assert!(matches!(
        h.commerce.request_redemption_token("carol", &issued.id),
        Err(CommerceError::Forbidden)
    ));
    let token = h
        .commerce
        .request_redemption_token("bob", &issued.id)
        .expect("token");
    assert!(matches!(
        h.commerce
            .redeem_entitlement("carol", &token.token, "redeem-stolen"),
        Err(CommerceError::Forbidden)
    ));
    let redeemed = h
        .commerce
        .redeem_entitlement("bob", &token.token, "redeem-1")
        .expect("redeem");
    assert_eq!(redeemed.entitlement_id, issued.id);
    assert_eq!(
        h.commerce
            .redeem_entitlement("bob", &token.token, "redeem-1")
            .expect("idempotent retry"),
        redeemed
    );
    assert!(matches!(
        h.commerce
            .redeem_entitlement("bob", &token.token, "redeem-2"),
        Err(CommerceError::Conflict(_))
    ));
    assert_eq!(
        h.commerce
            .entitlement("bob", &issued.id)
            .expect("view")
            .status,
        EntitlementStatus::Redeemed
    );
    assert!(matches!(
        h.commerce.request_redemption_token("bob", &issued.id),
        Err(CommerceError::Conflict(_))
    ));
}

#[test]
fn redemption_token_expires_and_offline_mode_is_refused() {
    let h = harness();
    h.commerce
        .publish_entitlement_rule("shop", EntitlementKind::Coupon, &json!({"valid_days": 10}))
        .expect("rule");
    let coupon = h
        .commerce
        .issue_entitlement(
            "shop",
            Some("alice"),
            EntitlementKind::Coupon,
            &json!({"discount": 10}),
            "coupon-exp",
        )
        .expect("issue");
    let token = h
        .commerce
        .request_redemption_token("alice", &coupon.id)
        .expect("token");
    h.clock.advance(6 * 60 * 1000);
    assert!(matches!(
        h.commerce
            .redeem_entitlement("alice", &token.token, "late-1"),
        Err(CommerceError::Expired)
    ));
    assert!(matches!(
        h.commerce.redeem_offline("alice", &token.token),
        Err(CommerceError::BadInput(_))
    ));
    assert_eq!(
        h.commerce
            .entitlement("alice", &coupon.id)
            .expect("view")
            .status,
        EntitlementStatus::Active
    );

    h.clock.advance(10 * DAY_MS);
    assert!(matches!(
        h.commerce.request_redemption_token("alice", &coupon.id),
        Err(CommerceError::Expired)
    ));
}

#[test]
fn ticket_barcode_exposes_no_owner_or_credentials() {
    let h = harness();
    h.commerce
        .publish_entitlement_rule("venue", EntitlementKind::Ticket, &json!({"valid_days": 1}))
        .expect("ticket rule");
    h.commerce
        .publish_entitlement_rule("venue", EntitlementKind::Coupon, &json!({"valid_days": 1}))
        .expect("coupon rule");
    let ticket = h
        .commerce
        .issue_entitlement(
            "venue",
            Some("alice"),
            EntitlementKind::Ticket,
            &json!({"seat": "12A"}),
            "ticket-1",
        )
        .expect("issue");
    let barcode = h
        .commerce
        .ticket_barcode("alice", &ticket.id)
        .expect("barcode");
    assert_eq!(barcode.format, "wxlike.ticket.v1");
    assert!(barcode.value.starts_with("wx1."));
    assert!(barcode.value.contains(&ticket.id));
    assert!(!barcode.value.contains("alice"));
    assert!(!barcode.value.contains("12A"));
    assert!(matches!(
        h.commerce.ticket_barcode("bob", &ticket.id),
        Err(CommerceError::Forbidden)
    ));

    let coupon = h
        .commerce
        .issue_entitlement(
            "venue",
            Some("alice"),
            EntitlementKind::Coupon,
            &json!({"discount": 5}),
            "coupon-barcode",
        )
        .expect("issue coupon");
    assert!(matches!(
        h.commerce.ticket_barcode("alice", &coupon.id),
        Err(CommerceError::BadInput(_))
    ));
}

#[test]
fn paying_an_order_does_not_fulfil_it() {
    let h = harness();
    let order = h
        .commerce
        .create_order("buyer", "seller", "quote-1", cny(9_900), "order-1")
        .expect("order");
    assert_eq!(order.status, OrderStatus::Created);

    assert!(matches!(
        h.commerce.fulfil_order("seller", &order.id, "ship-0"),
        Err(CommerceError::IllegalTransition(_))
    ));
    let paid = h
        .commerce
        .pay_order("buyer", &order.id, "pay-1")
        .expect("pay");
    assert_eq!(paid.status, OrderStatus::Paid);
    assert!(h
        .commerce
        .order_payment("buyer", &order.id)
        .expect("intent")
        .is_some());
    assert_eq!(
        h.commerce
            .order_payment("buyer", &order.id)
            .expect("intent")
            .expect("some")
            .status,
        IntentStatus::Succeeded
    );
    assert!(matches!(
        h.commerce.fulfil_order("buyer", &order.id, "ship-1"),
        Err(CommerceError::Forbidden)
    ));
    let fulfilled = h
        .commerce
        .fulfil_order("seller", &order.id, "ship-1")
        .expect("fulfil");
    assert_eq!(fulfilled.status, OrderStatus::Fulfilled);
    assert_eq!(
        h.commerce
            .fulfil_order("seller", &order.id, "ship-1")
            .expect("idempotent fulfil"),
        fulfilled
    );
    assert!(matches!(
        h.commerce.fulfil_order("seller", &order.id, "ship-2"),
        Err(CommerceError::Conflict(_))
    ));
    assert!(matches!(
        h.commerce.order("stranger", &order.id),
        Err(CommerceError::Forbidden)
    ));

    let again = h
        .commerce
        .pay_order("buyer", &order.id, "pay-2")
        .expect("paid order is terminal");
    assert_eq!(again.status, OrderStatus::Fulfilled);
    let seller = h
        .commerce
        .wallet_balance("seller", cny(0).currency())
        .expect("seller");
    assert_eq!(seller.total(), cny(9_900), "payment must post once");
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn only_unpaid_orders_can_be_cancelled() {
    let h = harness();
    let order = h
        .commerce
        .create_order("buyer", "seller", "quote-2", cny(500), "order-cancel")
        .expect("order");
    let cancelled = h.commerce.cancel_order("buyer", &order.id).expect("cancel");
    assert_eq!(cancelled.status, OrderStatus::Cancelled);
    assert_eq!(
        h.commerce
            .cancel_order("buyer", &order.id)
            .expect("idempotent")
            .status,
        OrderStatus::Cancelled
    );
    assert!(matches!(
        h.commerce.pay_order("buyer", &order.id, "pay-cancelled"),
        Err(CommerceError::IllegalTransition(_))
    ));

    let paid_order = h
        .commerce
        .create_order("buyer", "seller", "quote-3", cny(600), "order-paid")
        .expect("order");
    h.commerce
        .pay_order("buyer", &paid_order.id, "pay-paid")
        .expect("pay");
    assert!(matches!(
        h.commerce.cancel_order("buyer", &paid_order.id),
        Err(CommerceError::IllegalTransition(_))
    ));
    assert!(matches!(
        h.commerce.cancel_order("seller", &paid_order.id),
        Err(CommerceError::Forbidden)
    ));
}

#[test]
fn provider_timeout_leaves_the_order_unpaid_until_retry_succeeds() {
    let h = harness();
    let order = h
        .commerce
        .create_order("buyer", "seller", "quote-4", cny(1_200), "order-timeout")
        .expect("order");
    h.handle.fail_next_pay(ProviderError::Timeout);
    assert!(matches!(
        h.commerce.pay_order("buyer", &order.id, "pay-timeout"),
        Err(CommerceError::ProviderTimeout)
    ));
    let pending = h.commerce.order("buyer", &order.id).expect("order");
    assert_eq!(pending.status, OrderStatus::Created);
    assert_eq!(pending.payment_intent_id, None);

    let paid = h
        .commerce
        .pay_order("buyer", &order.id, "pay-timeout")
        .expect("retry");
    assert_eq!(paid.status, OrderStatus::Paid);
    let seller = h
        .commerce
        .wallet_balance("seller", cny(0).currency())
        .expect("seller");
    assert_eq!(seller.total(), cny(1_200));
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn order_creation_is_idempotent_and_rejects_self_dealing() {
    let h = harness();
    let order = h
        .commerce
        .create_order("buyer", "seller", "quote-5", cny(800), "order-dup")
        .expect("order");
    let same = h
        .commerce
        .create_order("buyer", "seller", "quote-5", cny(800), "order-dup")
        .expect("idempotent");
    assert_eq!(same.id, order.id);
    assert!(matches!(
        h.commerce
            .create_order("buyer", "seller", "quote-5", cny(900), "order-dup"),
        Err(CommerceError::IdempotencyConflict)
    ));
    assert!(matches!(
        h.commerce
            .create_order("buyer", "buyer", "quote-6", cny(800), "order-self"),
        Err(CommerceError::BadInput(_))
    ));
    assert!(matches!(
        h.commerce.create_order(
            "buyer",
            "seller",
            "quote-7",
            cny(100_000_000_000),
            "order-big"
        ),
        Err(CommerceError::AmountTooLarge)
    ));
}
