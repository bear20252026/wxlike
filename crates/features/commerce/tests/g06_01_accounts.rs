//! G06-01：资金账户、余额导出、流水分页、沙箱隔离。

mod common;

use common::{cny, fund, harness};
use wxlike_commerce::CommerceError;

#[test]
fn balance_is_derived_and_grant_is_idempotent() {
    let h = harness();
    let first = h
        .commerce
        .sandbox_credit("alice", cny(12_345), "grant-1")
        .expect("grant");
    assert_eq!(first.total_minor, 12_345);
    assert_eq!(first.available_minor, 12_345);
    assert_eq!(first.locked_minor, 0);

    let again = h
        .commerce
        .sandbox_credit("alice", cny(12_345), "grant-1")
        .expect("idempotent grant");
    assert_eq!(again.total_minor, 12_345);

    let balance = h
        .commerce
        .wallet_balance("alice", cny(0).currency())
        .expect("balance");
    assert_eq!(balance.total(), cny(12_345));
    h.commerce.verify_books().expect("books balance");
}

#[test]
fn empty_wallet_and_statement_are_explicit() {
    let h = harness();
    let balance = h
        .commerce
        .wallet_balance("nobody", cny(0).currency())
        .expect("empty balance");
    assert_eq!(balance.total_minor, 0);
    let page = h
        .commerce
        .wallet_statement("nobody", cny(0).currency(), None, 20)
        .expect("empty statement");
    assert!(page.items.is_empty());
    assert!(!page.has_more);
    assert!(page.next_cursor.is_none());
}

#[test]
fn statement_pages_use_a_monotonic_cursor() {
    let h = harness();
    for index in 0..5 {
        fund(&h, "alice", 100 + index);
    }
    let first = h
        .commerce
        .wallet_statement("alice", cny(0).currency(), None, 2)
        .expect("page 1");
    assert_eq!(first.items.len(), 2);
    assert!(first.has_more);
    let second = h
        .commerce
        .wallet_statement("alice", cny(0).currency(), first.next_cursor, 2)
        .expect("page 2");
    assert_eq!(second.items.len(), 2);
    assert_ne!(first.items[0].id, second.items[0].id);
    let third = h
        .commerce
        .wallet_statement("alice", cny(0).currency(), second.next_cursor, 2)
        .expect("page 3");
    assert_eq!(third.items.len(), 1);
    assert!(!third.has_more);
}

#[test]
fn bad_owner_and_amount_are_rejected() {
    let h = harness();
    assert!(matches!(
        h.commerce.wallet_balance("bad user", cny(0).currency()),
        Err(CommerceError::BadInput(_))
    ));
    assert!(matches!(
        h.commerce.sandbox_credit("alice", cny(0), "zero"),
        Err(CommerceError::BadInput(_))
    ));
    assert!(matches!(
        h.commerce.sandbox_credit("alice", cny(-1), "negative"),
        Err(CommerceError::BadInput(_))
    ));
}

#[test]
fn grant_rejects_the_same_key_with_a_different_amount() {
    let h = harness();
    fund(&h, "alice", 500);
    let error = h
        .commerce
        .sandbox_credit("alice", cny(600), "fund:alice:500")
        .expect_err("conflict");
    assert!(matches!(error, CommerceError::Ledger(_)));
}
