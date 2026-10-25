//! G06-04：转账确认令牌/冻结，红包预分配、并发领取与过期退回。

mod common;

use common::{cny, fund, harness};
use wxlike_commerce::redpackets::RedPacketStatus;
use wxlike_commerce::transfers::TransferStatus;
use wxlike_commerce::CommerceError;

#[test]
fn transfer_token_is_bound_and_the_nonce_cannot_be_replayed() {
    let h = harness();
    fund(&h, "alice", 10_000);
    let draft = h
        .commerce
        .prepare_transfer("alice", "bob", cny(4_000), "lunch")
        .expect("draft");

    assert!(matches!(
        h.commerce.create_transfer("alice", &draft, "forged", "t-1"),
        Err(CommerceError::InvalidSignature)
    ));
    assert!(matches!(
        h.commerce
            .create_transfer("bob", &draft, &draft.confirmation_token, "t-1"),
        Err(CommerceError::Forbidden)
    ));

    let transfer = h
        .commerce
        .create_transfer("alice", &draft, &draft.confirmation_token, "t-1")
        .expect("create");
    assert_eq!(transfer.status, TransferStatus::Pending);
    let again = h
        .commerce
        .create_transfer("alice", &draft, &draft.confirmation_token, "t-1")
        .expect("idempotent create");
    assert_eq!(again.id, transfer.id);

    let held = h
        .commerce
        .wallet_balance("alice", cny(0).currency())
        .expect("balance");
    assert_eq!(held.locked_minor, 4_000);
    assert_eq!(held.available_minor, 6_000);

    assert!(matches!(
        h.commerce
            .create_transfer("alice", &draft, &draft.confirmation_token, "t-2"),
        Err(CommerceError::Conflict(_))
    ));
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn accepting_a_transfer_moves_funds_once() {
    let h = harness();
    fund(&h, "alice", 10_000);
    let draft = h
        .commerce
        .prepare_transfer("alice", "bob", cny(4_000), "rent")
        .expect("draft");
    let transfer = h
        .commerce
        .create_transfer("alice", &draft, &draft.confirmation_token, "accept-1")
        .expect("create");

    assert!(matches!(
        h.commerce.accept_transfer("carol", &transfer.id),
        Err(CommerceError::Forbidden)
    ));
    let accepted = h
        .commerce
        .accept_transfer("bob", &transfer.id)
        .expect("accept");
    assert_eq!(accepted.status, TransferStatus::Accepted);
    let repeated = h
        .commerce
        .accept_transfer("bob", &transfer.id)
        .expect("idempotent accept");
    assert_eq!(repeated.status, TransferStatus::Accepted);

    let sender = h
        .commerce
        .wallet_balance("alice", cny(0).currency())
        .expect("sender");
    assert_eq!(sender.total_minor, 6_000);
    assert_eq!(sender.locked_minor, 0);
    let payee = h
        .commerce
        .wallet_balance("bob", cny(0).currency())
        .expect("payee");
    assert_eq!(payee.total(), cny(4_000));

    assert!(matches!(
        h.commerce.reject_transfer("bob", &transfer.id),
        Err(CommerceError::IllegalTransition(_))
    ));
    assert!(matches!(
        h.commerce.expire_transfer("alice", &transfer.id),
        Err(CommerceError::IllegalTransition(_))
    ));
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn rejecting_a_transfer_releases_the_hold() {
    let h = harness();
    fund(&h, "alice", 5_000);
    let draft = h
        .commerce
        .prepare_transfer("alice", "bob", cny(5_000), "split")
        .expect("draft");
    let transfer = h
        .commerce
        .create_transfer("alice", &draft, &draft.confirmation_token, "reject-1")
        .expect("create");
    assert!(matches!(
        h.commerce.reject_transfer("carol", &transfer.id),
        Err(CommerceError::Forbidden)
    ));
    let rejected = h
        .commerce
        .reject_transfer("bob", &transfer.id)
        .expect("reject");
    assert_eq!(rejected.status, TransferStatus::Rejected);
    assert_eq!(
        h.commerce
            .reject_transfer("bob", &transfer.id)
            .expect("idempotent")
            .status,
        TransferStatus::Rejected
    );

    let sender = h
        .commerce
        .wallet_balance("alice", cny(0).currency())
        .expect("sender");
    assert_eq!(sender.locked_minor, 0);
    assert_eq!(sender.available_minor, 5_000);
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn expired_transfer_refunds_the_sender_and_cannot_be_accepted() {
    let h = harness();
    fund(&h, "carol", 8_000);
    let draft = h
        .commerce
        .prepare_transfer("carol", "dave", cny(3_000), "deposit")
        .expect("draft");
    let transfer = h
        .commerce
        .create_transfer("carol", &draft, &draft.confirmation_token, "expire-1")
        .expect("create");

    assert!(matches!(
        h.commerce.expire_transfer("carol", &transfer.id),
        Err(CommerceError::Conflict(_))
    ));
    h.clock.advance(25 * 60 * 60 * 1000);
    assert!(matches!(
        h.commerce.accept_transfer("dave", &transfer.id),
        Err(CommerceError::Expired)
    ));
    let expired = h.commerce.transfer("carol", &transfer.id).expect("view");
    assert_eq!(expired.status, TransferStatus::Expired);

    let sender = h
        .commerce
        .wallet_balance("carol", cny(0).currency())
        .expect("sender");
    assert_eq!(sender.locked_minor, 0);
    assert_eq!(sender.available_minor, 8_000);
    assert!(matches!(
        h.commerce.accept_transfer("dave", &transfer.id),
        Err(CommerceError::IllegalTransition(_))
    ));
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn transfer_requires_funds_and_hides_from_strangers() {
    let h = harness();
    fund(&h, "alice", 100);
    let draft = h
        .commerce
        .prepare_transfer("alice", "bob", cny(500), "too much")
        .expect("draft");
    assert!(matches!(
        h.commerce
            .create_transfer("alice", &draft, &draft.confirmation_token, "poor-1"),
        Err(CommerceError::InsufficientFunds)
    ));

    let funded = h
        .commerce
        .prepare_transfer("alice", "bob", cny(100), "ok")
        .expect("draft");
    let transfer = h
        .commerce
        .create_transfer("alice", &funded, &funded.confirmation_token, "poor-2")
        .expect("create");
    assert!(matches!(
        h.commerce.transfer("carol", &transfer.id),
        Err(CommerceError::Forbidden)
    ));
    assert!(matches!(
        h.commerce
            .prepare_transfer("alice", "alice", cny(1), "self"),
        Err(CommerceError::BadInput(_))
    ));
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn red_packet_requires_membership_and_valid_shape() {
    let h = harness();
    fund(&h, "alice", 10_000);
    assert!(matches!(
        h.commerce
            .create_red_packet("alice", "group-1", cny(1_000), 2, "hi", "rp-denied"),
        Err(CommerceError::Forbidden)
    ));
    h.membership.add("group-1", "alice");
    assert!(matches!(
        h.commerce
            .create_red_packet("alice", "group-1", cny(1_000), 0, "hi", "rp-zero"),
        Err(CommerceError::BadInput(_))
    ));
    assert!(matches!(
        h.commerce
            .create_red_packet("alice", "group-1", cny(2), 3, "hi", "rp-short"),
        Err(CommerceError::BadInput(_))
    ));
}

#[test]
fn red_packet_claims_are_idempotent_and_split_the_total() {
    let h = harness();
    fund(&h, "alice", 10_000);
    for user in ["alice", "bob", "carol"] {
        h.membership.add("group-2", user);
    }
    let packet = h
        .commerce
        .create_red_packet("alice", "group-2", cny(3_000), 3, "new year", "rp-1")
        .expect("packet");
    let first = h
        .commerce
        .claim_red_packet("bob", &packet.id)
        .expect("claim");
    let retry = h
        .commerce
        .claim_red_packet("bob", &packet.id)
        .expect("idempotent claim");
    assert_eq!(first, retry);
    let second = h
        .commerce
        .claim_red_packet("carol", &packet.id)
        .expect("claim");
    let third = h
        .commerce
        .claim_red_packet("alice", &packet.id)
        .expect("claim");
    assert!(first.amount.minor() >= 1 && second.amount.minor() >= 1);
    assert!(third.amount.minor() >= 1);
    assert_eq!(
        first.amount.minor() + second.amount.minor() + third.amount.minor(),
        3_000
    );

    let view = h.commerce.red_packet("alice", &packet.id).expect("view");
    assert_eq!(view.packet.status, RedPacketStatus::Depleted);
    assert_eq!(view.claims.len(), 3);
    assert!(matches!(
        h.commerce.red_packet("dave", &packet.id),
        Err(CommerceError::Forbidden)
    ));
    h.membership.add("group-2", "dave");
    assert!(matches!(
        h.commerce.claim_red_packet("dave", &packet.id),
        Err(CommerceError::Conflict(_))
    ));
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn red_packet_last_share_goes_to_exactly_one_claimant() {
    let h = harness();
    fund(&h, "alice", 100);
    for user in ["alice", "bob", "carol"] {
        h.membership.add("group-3", user);
    }
    let packet = h
        .commerce
        .create_red_packet("alice", "group-3", cny(1), 1, "race", "rp-race")
        .expect("packet");
    let id = packet.id.clone();
    let commerce = h.commerce.clone();
    let (bob, carol) = std::thread::scope(|scope| {
        let b = scope.spawn({
            let commerce = commerce.clone();
            let id = id.clone();
            move || commerce.claim_red_packet("bob", &id)
        });
        let c = scope.spawn({
            let commerce = commerce.clone();
            let id = id.clone();
            move || commerce.claim_red_packet("carol", &id)
        });
        (b.join().expect("thread"), c.join().expect("thread"))
    });
    let wins = [bob, carol].iter().filter(|claim| claim.is_ok()).count();
    assert_eq!(wins, 1, "only one claimant may take the last share");
    let view = h.commerce.red_packet("alice", &packet.id).expect("view");
    assert_eq!(view.claims.len(), 1);
    assert_eq!(view.packet.status, RedPacketStatus::Depleted);
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn expired_red_packet_refunds_only_the_unclaimed_part() {
    let h = harness();
    fund(&h, "alice", 10_000);
    for user in ["alice", "bob", "carol"] {
        h.membership.add("group-4", user);
    }
    let packet = h
        .commerce
        .create_red_packet("alice", "group-4", cny(5_000), 2, "lucky", "rp-exp")
        .expect("packet");
    let claimed = h
        .commerce
        .claim_red_packet("bob", &packet.id)
        .expect("claim");

    assert!(matches!(
        h.commerce.expire_red_packet("alice", &packet.id),
        Err(CommerceError::Conflict(_))
    ));
    h.clock.advance(25 * 60 * 60 * 1000);
    assert!(matches!(
        h.commerce.claim_red_packet("carol", &packet.id),
        Err(CommerceError::Expired)
    ));
    assert!(matches!(
        h.commerce.expire_red_packet("bob", &packet.id),
        Err(CommerceError::Forbidden)
    ));
    let expired = h
        .commerce
        .expire_red_packet("alice", &packet.id)
        .expect("expire");
    assert_eq!(expired.status, RedPacketStatus::Expired);

    let sender = h
        .commerce
        .wallet_balance("alice", cny(0).currency())
        .expect("sender");
    assert_eq!(sender.total_minor, 10_000 - claimed.amount.minor());
    let bob = h
        .commerce
        .wallet_balance("bob", cny(0).currency())
        .expect("bob");
    assert_eq!(bob.total(), claimed.amount);
    h.commerce.verify_books().expect("balanced");
}
