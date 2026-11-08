//! G06-02 / G16-04：带 `operation_id` 的供应商回调端点与单一效果重放。
//!
//! 这一文件专门钉住 `PAYMENT-CALLBACK-REPLAY` 缺口的三件事：
//! 1. **幂等**：重放判定唯一入口是 `store::replay()`（business_key + digest），
//!    同 `operation_id` 同参一律返回首次结果且零副作用；
//! 2. **防重放**：同 `operation_id` 改参数被拒；换 `operation_id` 投同一
//!    `event_id` 被事件表去重拒绝；伪造/篡改/超时投递不留任何痕迹；
//! 3. **并发**：同一回调并发投递、不同操作号并发投递同一事件，都只有一笔分录。
//!
//! 全部走文件库 + 真实账本 + 真实 HMAC 验签，并用裸 SQL 从**独立连接**数
//! 分录行数作为外部证据（不是内存 mock，也不依赖任何 fake 幂等表）。
//! 供应商通道是沙箱实现（仅用于故障注入），**不构成真实供应商证据**。

mod common;

use common::{callback_body, cny, FileHarness};
use rusqlite::params;
use wxlike_commerce::payments::{IntentDirection, IntentStatus};
use wxlike_commerce::{Clock, CommerceConfig, CommerceError, Money, ProviderMode};
use wxlike_ledger::FundClass;

/// `intent:{id}:success` 这一笔记账分录的行数（外部 SQL 取证）。
fn capture_entry_rows(h: &FileHarness, intent_id: &str) -> i64 {
    h.count(
        "SELECT COUNT(*) FROM ledger_entries
         WHERE journal_id = (SELECT id FROM ledger_journals WHERE business_key = ?1)",
        &format!("intent:{intent_id}:success"),
    )
}

fn journal_rows(h: &FileHarness, intent_id: &str) -> i64 {
    h.count(
        "SELECT COUNT(*) FROM ledger_journals WHERE business_key = ?1",
        &format!("intent:{intent_id}:success"),
    )
}

fn callback_rows(h: &FileHarness, operation_id: &str) -> i64 {
    h.count(
        "SELECT COUNT(*) FROM commerce_provider_callbacks WHERE operation_id = ?1",
        operation_id,
    )
}

fn seller_total(h: &FileHarness) -> Money {
    h.commerce
        .wallet_balance("seller", cny(0).currency())
        .expect("balance")
        .total()
}

fn order_intent(h: &FileHarness, key: &str) -> wxlike_commerce::payments::PaymentIntent {
    h.commerce
        .create_payment_intent(IntentDirection::Order, "buyer", "seller", cny(9_900), key)
        .expect("intent")
        .0
}

fn signed(h: &FileHarness, body: &str) -> String {
    h.commerce.webhook_signature(body)
}

#[test]
fn callback_with_operation_id_settles_once() {
    let h = FileHarness::new("once");
    let intent = order_intent(&h, "cb-once");
    let provider_ref = intent.provider_ref.clone().expect("provider ref");
    let body = callback_body(
        "op-1",
        "evt-1",
        &intent.id,
        "payment.succeeded",
        &provider_ref,
        9_900,
        h.clock.now_ms(),
    );
    let sig = signed(&h, &body);
    let outcome = h
        .commerce
        .handle_provider_callback(&body, &sig)
        .expect("callback");

    assert!(outcome.applied);
    assert!(!outcome.replayed);
    assert_eq!(outcome.status, "succeeded");
    assert_eq!(outcome.operation_id, "op-1");
    assert_eq!(journal_rows(&h, &intent.id), 1);
    assert_eq!(capture_entry_rows(&h, &intent.id), 2, "一借一贷");
    assert_eq!(
        h.count_all("SELECT COUNT(*) FROM commerce_payment_events"),
        1
    );
    assert_eq!(h.count_all("SELECT COUNT(*) FROM commerce_outbox"), 1);
    assert_eq!(seller_total(&h), cny(9_900));
    h.commerce.verify_books().expect("balanced");

    // 台账可按 operation_id 回读（值班排障与对账用）。
    let record = h
        .commerce
        .provider_callback("op-1")
        .expect("read")
        .expect("record");
    assert_eq!(record.outcome.event_id, "evt-1");
    assert!(record.outcome.applied);
    assert!(h
        .commerce
        .provider_callback("op-ghost")
        .expect("read")
        .is_none());
}

#[test]
fn identical_callback_replay_is_single_effect() {
    let h = FileHarness::new("replay");
    let intent = order_intent(&h, "cb-replay");
    let provider_ref = intent.provider_ref.clone().expect("provider ref");
    let body = callback_body(
        "op-1",
        "evt-1",
        &intent.id,
        "payment.succeeded",
        &provider_ref,
        9_900,
        h.clock.now_ms(),
    );
    let sig = signed(&h, &body);
    let first = h
        .commerce
        .handle_provider_callback(&body, &sig)
        .expect("first");

    for _ in 0..5 {
        let again = h
            .commerce
            .handle_provider_callback(&body, &sig)
            .expect("replay must not error");
        assert!(again.replayed, "重复投递必须自报为重放");
        assert!(again.applied, "返回的是首次结果：该操作当时确实生效了");
        assert_eq!(again.status, first.status);
        assert_eq!(again.operation_id, first.operation_id);
        assert_eq!(again.event_id, first.event_id);
        assert_eq!(again.intent_id, first.intent_id);
        assert_eq!(again.kind, first.kind);
        assert_eq!(again.reason, first.reason);
    }

    // 零副作用：分录、事件、台账、outbox、余额全部还是第一次的样子。
    assert_eq!(journal_rows(&h, &intent.id), 1);
    assert_eq!(capture_entry_rows(&h, &intent.id), 2);
    assert_eq!(h.count_all("SELECT COUNT(*) FROM ledger_entries"), 2);
    assert_eq!(h.count_all("SELECT COUNT(*) FROM ledger_journals"), 1);
    assert_eq!(
        h.count_all("SELECT COUNT(*) FROM commerce_payment_events"),
        1
    );
    assert_eq!(
        h.count_all("SELECT COUNT(*) FROM commerce_provider_callbacks"),
        1
    );
    assert_eq!(h.count_all("SELECT COUNT(*) FROM commerce_outbox"), 1);
    assert_eq!(seller_total(&h), cny(9_900));
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn replay_under_same_operation_id_with_changed_parameters_is_rejected() {
    let h = FileHarness::new("conflict");
    let intent = order_intent(&h, "cb-conflict");
    let provider_ref = intent.provider_ref.clone().expect("provider ref");
    let body = callback_body(
        "op-1",
        "evt-1",
        &intent.id,
        "payment.succeeded",
        &provider_ref,
        9_900,
        h.clock.now_ms(),
    );
    let sig = signed(&h, &body);
    h.commerce
        .handle_provider_callback(&body, &sig)
        .expect("first");

    // 同一操作号换事件号 / 换类别都属于异参：直接拒绝，绝不覆盖首次结果。
    let changed_event = callback_body(
        "op-1",
        "evt-2",
        &intent.id,
        "payment.succeeded",
        &provider_ref,
        9_900,
        h.clock.now_ms(),
    );
    let sig2 = signed(&h, &changed_event);
    let error = h
        .commerce
        .handle_provider_callback(&changed_event, &sig2)
        .expect_err("digest conflict");
    assert!(
        matches!(error, CommerceError::IdempotencyConflict),
        "{error}"
    );

    let changed_kind = callback_body(
        "op-1",
        "evt-1",
        &intent.id,
        "payment.failed",
        &provider_ref,
        9_900,
        h.clock.now_ms(),
    );
    let sig3 = signed(&h, &changed_kind);
    assert!(matches!(
        h.commerce.handle_provider_callback(&changed_kind, &sig3),
        Err(CommerceError::IdempotencyConflict)
    ));

    assert_eq!(journal_rows(&h, &intent.id), 1);
    assert_eq!(
        h.count_all("SELECT COUNT(*) FROM commerce_payment_events"),
        1
    );
    assert_eq!(callback_rows(&h, "op-1"), 1);
    assert_eq!(seller_total(&h), cny(9_900));
    // 被拒的异参事件没有留下任何痕迹
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM commerce_payment_events WHERE event_id = ?1",
            "evt-2"
        ),
        0
    );
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn same_event_under_a_new_operation_id_is_refused_by_event_dedup() {
    let h = FileHarness::new("cross-op");
    let intent = order_intent(&h, "cb-cross");
    let provider_ref = intent.provider_ref.clone().expect("provider ref");
    let body = callback_body(
        "op-1",
        "evt-1",
        &intent.id,
        "payment.succeeded",
        &provider_ref,
        9_900,
        h.clock.now_ms(),
    );
    let sig = signed(&h, &body);
    h.commerce
        .handle_provider_callback(&body, &sig)
        .expect("first");

    // 供应商换个操作号再投同一个事件：事件表去重优先，拒绝且不写台账。
    let renamed = callback_body(
        "op-2",
        "evt-1",
        &intent.id,
        "payment.succeeded",
        &provider_ref,
        9_900,
        h.clock.now_ms(),
    );
    let sig2 = signed(&h, &renamed);
    assert!(matches!(
        h.commerce.handle_provider_callback(&renamed, &sig2),
        Err(CommerceError::Replay)
    ));
    assert_eq!(callback_rows(&h, "op-2"), 0, "被拒的投递不留台账行");
    assert_eq!(journal_rows(&h, &intent.id), 1);
    assert_eq!(seller_total(&h), cny(9_900));
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn forged_or_tampered_callback_leaves_no_trace() {
    let h = FileHarness::new("forged");
    let intent = order_intent(&h, "cb-forged");
    let provider_ref = intent.provider_ref.clone().expect("provider ref");
    let body = callback_body(
        "op-1",
        "evt-1",
        &intent.id,
        "payment.succeeded",
        &provider_ref,
        9_900,
        h.clock.now_ms(),
    );

    assert!(matches!(
        h.commerce.handle_provider_callback(&body, "00"),
        Err(CommerceError::InvalidSignature)
    ));

    let good_sig = signed(&h, &body);
    let tampered = body.replace("9900", "1");
    assert!(matches!(
        h.commerce.handle_provider_callback(&tampered, &good_sig),
        Err(CommerceError::InvalidSignature)
    ));

    // 金额写成浮点直接拒收：本契约只接受整数最小单位。
    let float_body = body.replace("\"amount_minor\":9900", "\"amount_minor\":99.00");
    let float_sig = signed(&h, &float_body);
    assert!(matches!(
        h.commerce.handle_provider_callback(&float_body, &float_sig),
        Err(CommerceError::BadInput(_))
    ));

    assert!(h
        .commerce
        .provider_callback("op-1")
        .expect("read")
        .is_none());
    assert_eq!(h.count_all("SELECT COUNT(*) FROM ledger_journals"), 0);
    assert_eq!(
        h.count_all("SELECT COUNT(*) FROM commerce_payment_events"),
        0
    );
    assert_eq!(
        h.count_all("SELECT COUNT(*) FROM commerce_provider_callbacks"),
        0
    );
    assert_eq!(seller_total(&h), cny(0));
    assert_eq!(
        h.commerce
            .payment_intent("buyer", &intent.id)
            .expect("intent")
            .status,
        IntentStatus::Pending
    );
}

#[test]
fn aged_new_callback_is_refused_but_an_aged_replay_still_returns_the_result() {
    let h = FileHarness::new("skew");
    let intent = order_intent(&h, "cb-skew");
    let provider_ref = intent.provider_ref.clone().expect("provider ref");

    // 迟到的新投递：超出时间窗 → 拒，且不产生任何效果。
    let stale = callback_body(
        "op-old",
        "evt-old",
        &intent.id,
        "payment.succeeded",
        &provider_ref,
        9_900,
        h.clock.now_ms() - 60 * 60 * 1000,
    );
    let stale_sig = signed(&h, &stale);
    assert!(matches!(
        h.commerce.handle_provider_callback(&stale, &stale_sig),
        Err(CommerceError::InvalidSignature)
    ));
    assert_eq!(h.count_all("SELECT COUNT(*) FROM ledger_journals"), 0);

    // 正常投递。
    let body = callback_body(
        "op-1",
        "evt-1",
        &intent.id,
        "payment.succeeded",
        &provider_ref,
        9_900,
        h.clock.now_ms(),
    );
    let sig = signed(&h, &body);
    h.commerce
        .handle_provider_callback(&body, &sig)
        .expect("first");

    // 一小时后供应商才重试：命中重放应原样返回首次结果，而不是把
    // 合法重试误判成伪造（时间窗只闸门"新效果"那一次）。
    h.clock.advance(60 * 60 * 1000);
    let late = h
        .commerce
        .handle_provider_callback(&body, &sig)
        .expect("late retry is a replay, not a forgery");
    assert!(late.replayed);
    assert_eq!(journal_rows(&h, &intent.id), 1);
    assert_eq!(seller_total(&h), cny(9_900));
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn crash_between_delivery_and_retry_does_not_double_post() {
    let mut h = FileHarness::new("crash");
    let intent = order_intent(&h, "cb-crash");
    let provider_ref = intent.provider_ref.clone().expect("provider ref");
    let body = callback_body(
        "op-1",
        "evt-1",
        &intent.id,
        "payment.succeeded",
        &provider_ref,
        9_900,
        h.clock.now_ms(),
    );
    let sig = signed(&h, &body);
    h.commerce
        .handle_provider_callback(&body, &sig)
        .expect("first");

    // 进程崩溃/重启：新连接、新端口，只有磁盘上的 SQLite 文件还在。
    h.restart();
    assert_eq!(journal_rows(&h, &intent.id), 1, "分录在文件里，不在内存里");
    assert_eq!(seller_total(&h), cny(9_900));
    assert!(
        h.commerce
            .provider_callback("op-1")
            .expect("read")
            .is_some(),
        "幂等台账必须跨重启存活"
    );

    // 重启后重投：仍然只有一次效果。
    let after = h
        .commerce
        .handle_provider_callback(&body, &sig)
        .expect("replay after restart");
    assert!(after.replayed);
    assert_eq!(journal_rows(&h, &intent.id), 1);
    assert_eq!(capture_entry_rows(&h, &intent.id), 2);
    assert_eq!(h.count_all("SELECT COUNT(*) FROM ledger_journals"), 1);
    assert_eq!(
        h.count_all("SELECT COUNT(*) FROM commerce_provider_callbacks"),
        1
    );
    assert_eq!(seller_total(&h), cny(9_900));
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn concurrent_identical_callbacks_have_exactly_one_effect() {
    let h = FileHarness::new("conc-same");
    let intent = order_intent(&h, "cb-conc-same");
    let provider_ref = intent.provider_ref.clone().expect("provider ref");
    let body = callback_body(
        "op-1",
        "evt-1",
        &intent.id,
        "payment.succeeded",
        &provider_ref,
        9_900,
        h.clock.now_ms(),
    );
    let sig = signed(&h, &body);

    let outcomes = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..8 {
            let commerce = &h.commerce;
            let body = body.clone();
            let sig = sig.clone();
            handles.push(scope.spawn(move || commerce.handle_provider_callback(&body, &sig)));
        }
        handles
            .into_iter()
            .map(|handle| handle.join().expect("thread"))
            .collect::<Vec<_>>()
    });

    for outcome in &outcomes {
        assert!(outcome.is_ok(), "并发重投不该报错：{outcome:?}");
    }
    let fresh = outcomes
        .iter()
        .filter(|outcome| !outcome.as_ref().expect("ok").replayed)
        .count();
    assert_eq!(fresh, 1, "只允许一个线程真正落地效果");
    assert_eq!(callback_rows(&h, "op-1"), 1);
    assert_eq!(journal_rows(&h, &intent.id), 1);
    assert_eq!(capture_entry_rows(&h, &intent.id), 2);
    assert_eq!(h.count_all("SELECT COUNT(*) FROM commerce_outbox"), 1);
    assert_eq!(seller_total(&h), cny(9_900));
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn concurrent_operation_ids_for_one_event_stay_single_effect() {
    let h = FileHarness::new("conc-diff");
    let intent = order_intent(&h, "cb-conc-diff");
    let provider_ref = intent.provider_ref.clone().expect("provider ref");

    let accepted = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for index in 0..8 {
            let commerce = &h.commerce;
            let body = callback_body(
                &format!("op-{index}"),
                "evt-1",
                &intent.id,
                "payment.succeeded",
                &provider_ref,
                9_900,
                h.clock.now_ms(),
            );
            let sig = signed(&h, &body);
            handles
                .push(scope.spawn(move || commerce.handle_provider_callback(&body, &sig).is_ok()));
        }
        handles
            .into_iter()
            .map(|handle| handle.join().expect("thread"))
            .filter(|ok| *ok)
            .count()
    });
    assert_eq!(accepted, 1, "同一事件并发多操作号，只接受一个");

    assert_eq!(journal_rows(&h, &intent.id), 1);
    assert_eq!(capture_entry_rows(&h, &intent.id), 2);
    assert_eq!(
        h.count_all("SELECT COUNT(*) FROM commerce_payment_events"),
        1
    );
    assert_eq!(
        h.count_all("SELECT COUNT(*) FROM commerce_provider_callbacks"),
        1,
        "被拒的投递不落台账"
    );
    assert_eq!(seller_total(&h), cny(9_900));
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn callback_after_local_success_is_recorded_but_not_applied() {
    let h = FileHarness::new("late-ok");
    let intent = order_intent(&h, "cb-late");
    let provider_ref = intent.provider_ref.clone().expect("provider ref");
    h.commerce
        .pay_payment_intent("buyer", &intent.id)
        .expect("local pay succeeds");
    assert_eq!(journal_rows(&h, &intent.id), 1);

    // 迟到的成功回执：状态不前进、不二次入账，但操作台账要有据可查。
    let body = callback_body(
        "op-late",
        "evt-late",
        &intent.id,
        "payment.succeeded",
        &provider_ref,
        9_900,
        h.clock.now_ms(),
    );
    let sig = signed(&h, &body);
    let outcome = h
        .commerce
        .handle_provider_callback(&body, &sig)
        .expect("late callback");
    assert!(!outcome.applied, "已成功的意图不能被再记一次");
    assert_eq!(outcome.status, "succeeded");
    assert_eq!(outcome.reason, "already succeeded");
    assert_eq!(journal_rows(&h, &intent.id), 1);
    assert_eq!(h.count_all("SELECT COUNT(*) FROM ledger_entries"), 2);
    assert_eq!(callback_rows(&h, "op-late"), 1);
    assert_eq!(seller_total(&h), cny(9_900));
    h.commerce.verify_books().expect("balanced");
}

#[test]
fn callback_amount_mismatching_the_intent_is_refused() {
    let h = FileHarness::new("amount");
    let intent = order_intent(&h, "cb-amount");
    let provider_ref = intent.provider_ref.clone().expect("provider ref");

    // 收款人或金额被换掉的回调必须失效（验收：二维码换收款人或金额会失效）。
    for amount in [1_i64, 9_899, 9_901] {
        let body = callback_body(
            &format!("op-{amount}"),
            &format!("evt-{amount}"),
            &intent.id,
            "payment.succeeded",
            &provider_ref,
            amount,
            h.clock.now_ms(),
        );
        let sig = signed(&h, &body);
        assert!(
            matches!(
                h.commerce.handle_provider_callback(&body, &sig),
                Err(CommerceError::BadInput(_))
            ),
            "amount {amount} must not settle a 9900 intent"
        );
    }
    assert_eq!(h.count_all("SELECT COUNT(*) FROM ledger_journals"), 0);
    assert_eq!(
        h.count_all("SELECT COUNT(*) FROM commerce_provider_callbacks"),
        0
    );
    assert_eq!(seller_total(&h), cny(0));
}

#[test]
fn http_callback_endpoint_shape_is_stable() {
    use serde_json::Value;
    use wxlike_commerce::http;

    let h = FileHarness::new("http");
    let intent = order_intent(&h, "cb-http");
    let provider_ref = intent.provider_ref.clone().expect("provider ref");
    let body = callback_body(
        "op-http",
        "evt-http",
        &intent.id,
        "payment.succeeded",
        &provider_ref,
        9_900,
        h.clock.now_ms(),
    );
    let sig = signed(&h, &body);

    let first: Value = http::provider_callback(&h.commerce, &body, &sig).expect("endpoint");
    assert_eq!(first["operation_id"], "op-http");
    assert_eq!(first["event_id"], "evt-http");
    assert_eq!(first["intent_id"], intent.id);
    assert_eq!(first["kind"], "payment.succeeded");
    assert_eq!(first["status"], "succeeded");
    assert_eq!(first["applied"], true);
    assert_eq!(first["replayed"], false);
    // 端点响应不回显金额（客户端以意图为准），也不得有浮点足迹；
    // reason 为空串表示这次真的推进了状态。
    assert!(first.get("amount").is_none());
    assert!(first.get("amount_minor").is_none());
    assert_eq!(first["reason"], "");

    let again: Value = http::provider_callback(&h.commerce, &body, &sig).expect("replay endpoint");
    assert_eq!(again["replayed"], true);
    assert_eq!(again["status"], "succeeded");

    let read: Value = http::provider_callback_get(&h.commerce, "op-http").expect("read endpoint");
    assert_eq!(read["operation_id"], "op-http");
    assert_eq!(read["replayed"], true);
    assert!(read["received_at"].is_string(), "时间戳按字符串出门");
    assert!(matches!(
        http::provider_callback_get(&h.commerce, "ghost"),
        Err(CommerceError::NotFound)
    ));

    assert_eq!(journal_rows(&h, &intent.id), 1);
    assert_eq!(seller_total(&h), cny(9_900));
}

#[test]
fn sandbox_channel_is_labelled_and_not_live_provider_evidence() {
    let h = FileHarness::new("mode");
    let config: &CommerceConfig = h.commerce.config();
    // 本文件所有回调证据都产自沙箱通道：账户名前缀带 sandbox，资金性质是虚拟。
    assert_eq!(config.credentials.mode, ProviderMode::Sandbox);
    assert_eq!(config.fund_class, FundClass::Virtual);

    let intent = order_intent(&h, "cb-mode");
    let provider_ref = intent.provider_ref.clone().expect("provider ref");
    let body = callback_body(
        "op-mode",
        "evt-mode",
        &intent.id,
        "payment.succeeded",
        &provider_ref,
        9_900,
        h.clock.now_ms(),
    );
    let sig = signed(&h, &body);
    h.commerce
        .handle_provider_callback(&body, &sig)
        .expect("callback");

    let accounts = {
        let conn = wxlike_ledger::open_connection(h.path().to_str()).expect("conn");
        let mut stmt = conn
            .prepare(
                "SELECT e.account FROM ledger_entries e
                 JOIN ledger_journals j ON j.id = e.journal_id
                 WHERE j.business_key = ?1 ORDER BY e.seq",
            )
            .expect("stmt");
        let rows: Vec<String> = stmt
            .query_map(params![format!("intent:{}:success", intent.id)], |r| {
                r.get(0)
            })
            .expect("rows")
            .map(|row| row.expect("account"))
            .collect();
        rows
    };
    assert_eq!(
        accounts,
        vec![
            "external:sandbox:CNY".to_string(),
            "wallet:seller:CNY".to_string()
        ],
        "沙箱供应商侧科目必须可辨认，不能冒充真实通道对账科目"
    );
}
