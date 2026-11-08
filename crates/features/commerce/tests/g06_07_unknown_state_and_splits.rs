//! G06-02 / G06-04 补充：未知状态用供应商查询解决 + 红包拆分的有界随机与
//! 末包兜底余数 + 幂等键在账本层的单一效果（外部 SQL 取证）。
//!
//! 这里刻意不用任何 fake 幂等表：文件库 + 独立连接数分录行数，才是
//! "同幂等键不重复入账"的可核查证据。供应商通道是沙箱实现（仅用于注入
//! 超时/未知态），**不构成真实供应商证据**。

mod common;

use common::{callback_body, cny, credit, FileHarness};
use wxlike_commerce::infrastructure::sandbox::SandboxHandle;
use wxlike_commerce::payments::{IntentDirection, IntentStatus};
use wxlike_commerce::redpackets::RedPacketStatus;
use wxlike_commerce::{Clock, CommerceError, ProviderError, ProviderPaymentStatus};
use wxlike_ledger::Currency;

fn order_intent(h: &FileHarness, key: &str) -> wxlike_commerce::payments::PaymentIntent {
    h.commerce
        .create_payment_intent(IntentDirection::Order, "buyer", "seller", cny(9_900), key)
        .expect("intent")
        .0
}

fn journals(h: &FileHarness, intent_id: &str) -> i64 {
    h.count(
        "SELECT COUNT(*) FROM ledger_journals WHERE business_key = ?1",
        &format!("intent:{intent_id}:success"),
    )
}

fn seller(h: &FileHarness) -> wxlike_commerce::Money {
    h.commerce
        .wallet_balance("seller", Currency::CNY)
        .expect("balance")
        .total()
}

/// 让供应商"实际扣款成功但响应丢失"：pay 超时 + 侧写终态。
fn captured_but_unacknowledged(handle: &SandboxHandle, provider_ref: &str) {
    handle.fail_next_pay(ProviderError::Timeout);
    handle.force_payment_status(provider_ref, ProviderPaymentStatus::Succeeded);
}

#[test]
fn query_timeout_leaves_the_state_unknown_and_posts_nothing() {
    let h = FileHarness::new("q-timeout");
    let intent = order_intent(&h, "q-1");
    h.handle.fail_next_query(ProviderError::Timeout);
    let error = h
        .commerce
        .refresh_payment_intent("buyer", &intent.id)
        .expect_err("unknown is unknown");
    assert!(matches!(error, CommerceError::ProviderTimeout), "{error}");
    assert_eq!(
        h.commerce
            .payment_intent("buyer", &intent.id)
            .expect("intent")
            .status,
        IntentStatus::Pending,
        "查询失败不能改动本地状态"
    );
    assert_eq!(journals(&h, &intent.id), 0);
    assert_eq!(seller(&h), cny(0));
}

#[test]
fn query_resolves_a_lost_payment_response_to_exactly_one_journal() {
    let h = FileHarness::new("q-resolve");
    let intent = order_intent(&h, "q-2");
    let provider_ref = intent.provider_ref.clone().expect("provider ref");
    captured_but_unacknowledged(&h.handle, &provider_ref);

    // pay 侧看不到结果 → 本地仍是 pending，一分钱都没记。
    assert!(matches!(
        h.commerce.pay_payment_intent("buyer", &intent.id),
        Err(CommerceError::ProviderTimeout)
    ));
    assert_eq!(journals(&h, &intent.id), 0);
    assert_eq!(seller(&h), cny(0));

    // 用查询取回真实终态：此时才允许入账，而且只有一笔。
    let resolved = h
        .commerce
        .refresh_payment_intent("buyer", &intent.id)
        .expect("query resolves it");
    assert_eq!(resolved.status, IntentStatus::Succeeded);
    assert_eq!(journals(&h, &intent.id), 1);
    assert_eq!(seller(&h), cny(9_900));

    // 再查、再刷新都不得二次入账。
    for _ in 0..3 {
        let again = h
            .commerce
            .refresh_payment_intent("buyer", &intent.id)
            .expect("idempotent refresh");
        assert_eq!(again.status, IntentStatus::Succeeded);
    }
    assert_eq!(journals(&h, &intent.id), 1);
    h.commerce.verify_books().expect("balanced");

    // 迟到的回执走同一条幂等链：不生效，但要有台账。
    let body = callback_body(
        "op-late",
        "evt-late",
        &intent.id,
        "payment.succeeded",
        &provider_ref,
        9_900,
        h.clock.now_ms(),
    );
    let sig = h.commerce.webhook_signature(&body);
    let outcome = h
        .commerce
        .handle_provider_callback(&body, &sig)
        .expect("late webhook");
    assert!(!outcome.applied);
    assert_eq!(outcome.reason, "already succeeded");
    assert_eq!(journals(&h, &intent.id), 1);
    assert_eq!(seller(&h), cny(9_900));
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn query_can_confirm_failure_without_creating_any_entry() {
    let h = FileHarness::new("q-fail");
    let intent = order_intent(&h, "q-3");
    let provider_ref = intent.provider_ref.clone().expect("provider ref");
    h.handle.fail_next_pay(ProviderError::Timeout);
    assert!(matches!(
        h.commerce.pay_payment_intent("buyer", &intent.id),
        Err(CommerceError::ProviderTimeout)
    ));
    h.handle
        .force_payment_status(&provider_ref, ProviderPaymentStatus::Failed);
    let failed = h
        .commerce
        .refresh_payment_intent("seller", &intent.id)
        .expect("failure resolved");
    assert_eq!(failed.status, IntentStatus::Failed);
    assert_eq!(journals(&h, &intent.id), 0);
    assert_eq!(h.count_all("SELECT COUNT(*) FROM ledger_journals"), 0);
    assert_eq!(seller(&h), cny(0));

    // 失败是终态：不得被后到的"成功"证据倒退。
    h.handle
        .force_payment_status(&provider_ref, ProviderPaymentStatus::Succeeded);
    let still = h
        .commerce
        .refresh_payment_intent("seller", &intent.id)
        .expect("no regression");
    assert_eq!(still.status, IntentStatus::Failed);
    assert_eq!(journals(&h, &intent.id), 0);
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn pending_query_changes_nothing_locally() {
    let h = FileHarness::new("q-pending");
    let intent = order_intent(&h, "q-4");
    let refreshed = h
        .commerce
        .refresh_payment_intent("buyer", &intent.id)
        .expect("still pending");
    assert_eq!(refreshed.status, IntentStatus::Pending);
    assert_eq!(journals(&h, &intent.id), 0);
    assert_eq!(h.count_all("SELECT COUNT(*) FROM ledger_journals"), 0);

    // 越权者既读不到也不该触发供应商查询。
    assert!(matches!(
        h.commerce.refresh_payment_intent("stranger", &intent.id),
        Err(CommerceError::Forbidden)
    ));
}

#[test]
fn amount_drift_in_the_query_result_is_refused() {
    let h = FileHarness::new("q-drift");
    let intent = order_intent(&h, "q-5");
    let provider_ref = intent.provider_ref.clone().expect("provider ref");
    // 供应商回执金额与本地冻结的意图不符：拒，且不入账。
    let body = callback_body(
        "op-drift",
        "evt-drift",
        &intent.id,
        "payment.succeeded",
        &provider_ref,
        9_901,
        h.clock.now_ms(),
    );
    let sig = h.commerce.webhook_signature(&body);
    assert!(matches!(
        h.commerce.handle_provider_callback(&body, &sig),
        Err(CommerceError::BadInput(_))
    ));
    assert_eq!(journals(&h, &intent.id), 0);
    assert_eq!(
        h.commerce
            .payment_intent("buyer", &intent.id)
            .expect("intent")
            .status,
        IntentStatus::Pending
    );
}

#[test]
fn red_packet_split_is_positive_bounded_and_exhausts_the_total() {
    let h = FileHarness::new("split");
    // (总额, 份数)：末包兜底余数 + 有界随机，四种边界都要成立。
    let cases: [(i64, u32); 5] = [
        (100, 10),
        (1_000_000, 100),
        (100, 1),
        (199, 3),
        (50_000, 99),
    ];
    for (index, (total_minor, count)) in cases.into_iter().enumerate() {
        let conversation = format!("group-split-{index}");
        let sender = format!("sender-{index}");
        for slot in 0..count {
            h.membership
                .add(&conversation, &format!("member-{index}-{slot}"));
        }
        h.membership.add(&conversation, &sender);
        credit(&h.commerce, &sender, total_minor, &format!("fund-{index}"));
        let packet = h
            .commerce
            .create_red_packet(
                &sender,
                &conversation,
                cny(total_minor),
                count,
                "红包",
                &format!("split-{index}"),
            )
            .expect("packet");

        let mut shares = Vec::new();
        for slot in 0..count {
            let claim = h
                .commerce
                .claim_red_packet(&format!("member-{index}-{slot}"), &packet.id)
                .expect("claim");
            shares.push(claim.amount.minor());
        }
        assert_eq!(shares.iter().sum::<i64>(), total_minor, "总额必须刚好分完");
        assert!(
            shares.iter().all(|share| *share >= 1),
            "任何一份都不能是 0 或负数：{shares:?}"
        );
        // 有界随机的真正含义：任一份额都不能把后面的份数饿死。
        let mut handed_out = 0_i64;
        for (index, share) in shares.iter().enumerate().take(shares.len() - 1) {
            let slots_left = i64::from(count) - i64::try_from(index + 1).expect("index");
            let ceiling = total_minor - handed_out - slots_left;
            assert!(
                *share <= ceiling,
                "第 {} 份 {share} 超过可分配上限 {ceiling}：{shares:?}",
                index + 1
            );
            handed_out += share;
        }
        // 末包兜底余数：最后一份 = 总额 - 前面之和，恒为正。
        let head: i64 = shares.iter().take(shares.len() - 1).sum();
        assert_eq!(*shares.last().expect("last"), total_minor - head);
        let view = h.commerce.red_packet(&sender, &packet.id).expect("view");
        assert_eq!(view.packet.status, RedPacketStatus::Depleted);
        assert_eq!(view.claims.len(), usize::try_from(count).expect("count"));
    }
    h.commerce.verify_books().expect("balanced");
    // escrow 必须被领空：托管科目余额为 0。
    let escrow_left = h.count_all(
        "SELECT COALESCE(SUM(e.amount_minor), 0) FROM ledger_entries e
         WHERE e.account LIKE 'escrow:redpacket:%'",
    );
    assert_eq!(escrow_left, 0, "领完后托管科目必须归零");
}

#[test]
fn red_packet_shape_is_immutable_and_rejects_uncovered_totals() {
    let h = FileHarness::new("rp-shape");
    h.membership.add("group-x", "alice");
    credit(&h.commerce, "alice", 10_000, "fund-alice");

    // 总额 < 份数：无法保证每份至少 1 分，直接拒绝。
    assert!(matches!(
        h.commerce
            .create_red_packet("alice", "group-x", cny(2), 3, "小气", "rp-tiny"),
        Err(CommerceError::BadInput(_))
    ));
    for count in [0_u32, 101] {
        assert!(matches!(
            h.commerce
                .create_red_packet("alice", "group-x", cny(1_000), count, "越界", "rp-count"),
            Err(CommerceError::BadInput(_))
        ));
    }
    assert!(matches!(
        h.commerce.create_red_packet(
            "alice",
            "group-x",
            cny(1_000),
            2,
            &"字".repeat(21),
            "rp-note"
        ),
        Err(CommerceError::BadInput(_))
    ));
    assert_eq!(h.count_all("SELECT COUNT(*) FROM commerce_redpackets"), 0);

    // 同业务键重放：拿到同一个红包，托管记账只有一笔。
    let first = h
        .commerce
        .create_red_packet("alice", "group-x", cny(1_000), 2, "红包", "rp-key")
        .expect("packet");
    let again = h
        .commerce
        .create_red_packet("alice", "group-x", cny(1_000), 2, "红包", "rp-key")
        .expect("replay");
    assert_eq!(first.id, again.id);
    assert_eq!(first.total, again.total);
    assert_eq!(first.count, again.count);
    assert_eq!(first.expires_at, again.expires_at);
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM ledger_journals WHERE business_key = ?1",
            &format!("redpacket:{}:fund", first.id)
        ),
        1,
        "同幂等键不重复入账"
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM commerce_redpacket_shares WHERE packet_id = ?1",
            &first.id
        ),
        2
    );

    // 换参数复用同一个键：拒绝，且不能悄悄改包体。
    assert!(matches!(
        h.commerce
            .create_red_packet("alice", "group-x", cny(2_000), 2, "红包", "rp-key"),
        Err(CommerceError::IdempotencyConflict)
    ));
    let view = h.commerce.red_packet("alice", &first.id).expect("view");
    assert_eq!(view.packet.total, cny(1_000));
    assert_eq!(view.packet.count, 2);
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn concurrent_claims_of_a_full_packet_never_exceed_the_total() {
    let h = FileHarness::new("rp-race");
    h.membership.add("group-race", "alice");
    credit(&h.commerce, "alice", 10_000, "fund-race");
    for index in 0..10 {
        h.membership.add("group-race", &format!("claim-{index}"));
    }
    let packet = h
        .commerce
        .create_red_packet("alice", "group-race", cny(100), 4, "抢", "rp-race")
        .expect("packet");
    let id = packet.id.clone();

    let claims = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..10)
            .map(|index| {
                let commerce = &h.commerce;
                let packet = id.clone();
                scope.spawn(move || commerce.claim_red_packet(&format!("claim-{index}"), &packet))
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("thread"))
            .collect::<Vec<_>>()
    });

    let won: Vec<i64> = claims
        .iter()
        .filter_map(|claim| claim.as_ref().ok().map(|amount| amount.amount.minor()))
        .collect();
    assert_eq!(won.len(), 4, "四份只能被四个人领走");
    assert_eq!(won.iter().sum::<i64>(), 100, "领走的总额不能超过红包");
    assert!(won.iter().all(|share| *share >= 1));
    let view = h.commerce.red_packet("alice", &id).expect("view");
    assert_eq!(view.packet.status, RedPacketStatus::Depleted);
    // 记账侧：领取笔数与分录笔数一一对应，没有多付。
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM commerce_redpacket_claims WHERE packet_id = ?1",
            &id
        ),
        4
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM ledger_journals WHERE business_key LIKE ?1",
            &format!("redpacket:{id}:claim:%")
        ),
        4
    );
    h.commerce.verify_books().expect("balanced");
}
