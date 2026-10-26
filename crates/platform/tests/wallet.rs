//! WP-09 A1 + A2: money conservation, the 100-concurrent transfer test, red
//! packet freeze/claim/refund, and the adversarial set.

mod common;

use axum::http::StatusCode;
use common::{app, call, req, signup, Recorder};
use serde_json::{json, Value};
use std::sync::Arc;
use wxlike_core::MsgBody;
use wxlike_platform::{Platform, PlatformOptions, PlatformState};

fn state() -> PlatformState {
    let opts = PlatformOptions {
        session_ttl_ms: 60_000,
        pbkdf2_rounds: 500,
        ..Default::default()
    };
    Platform::in_memory_with(opts).unwrap().shared()
}

async fn scene() -> (PlatformState, Recorder) {
    let st = state();
    let rec = Recorder::new();
    st.set_event_sink(Arc::new(rec.clone()));
    (st, rec)
}

async fn topup(app: &axum::Router, token: &str, to_uid: &str, amount: i64) -> String {
    let (status, body) = call(
        app,
        req(
            "POST",
            "/api/wallet/transfer",
            Some(json!({"to_uid": to_uid, "amount": amount, "memo": "seed"})),
            Some(token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "transfer: {body}");
    body["transfer_id"].as_str().unwrap().to_string()
}

async fn balance_of(app: &axum::Router, token: &str) -> i64 {
    let (status, body) = call(app, req("GET", "/api/wallet/balance", None, Some(token))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["balance"].as_i64().unwrap()
}

async fn ledger_rows(app: &axum::Router, token: &str) -> Vec<serde_json::Value> {
    let (status, body) = call(
        app,
        req(
            "GET",
            "/api/wallet/ledger?since=0&limit=200",
            None,
            Some(token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["entries"].as_array().unwrap().clone()
}

#[tokio::test]
async fn transfer_moves_money_and_writes_two_ledger_rows() {
    let (st, _rec) = scene().await;
    let app = app(st.clone());
    let (alice, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    st.fund(&alice, 1_000, "test funding").unwrap();

    let id = topup(&app, &at, &bob, 300).await;
    assert_eq!(balance_of(&app, &at).await, 700);
    assert_eq!(balance_of(&app, &bt).await, 300);
    // nothing else in the system changed
    assert_eq!(st.money_supply().unwrap(), 1_000);

    let arows = ledger_rows(&app, &at).await;
    let brows = ledger_rows(&app, &bt).await;
    assert_eq!(arows.len(), 2, "fund_in + transfer_out");
    // selected by kind, not position: the ledger is returned oldest first
    let by_kind = |rows: &[serde_json::Value], k: &str| -> serde_json::Value {
        rows.iter()
            .find(|r| r["kind"].as_str() == Some(k))
            .cloned()
            .unwrap_or_else(|| panic!("no {k} row in {rows:?}"))
    };
    let out = by_kind(&arows, "transfer_out");
    assert_eq!(out["amount"].as_i64(), Some(300));
    assert_eq!(out["counterparty"].as_str().unwrap(), bob);
    assert_eq!(out["ref_id"].as_str().unwrap(), id);
    assert_eq!(by_kind(&arows, "fund_in")["amount"].as_i64(), Some(1_000));
    assert_eq!(brows.len(), 1);
    assert_eq!(brows[0]["kind"].as_str().unwrap(), "transfer_in");
    assert_eq!(brows[0]["counterparty"].as_str().unwrap(), alice);
    // both rows share one ref_id: the two halves of one exchange
    assert_eq!(brows[0]["ref_id"].as_str().unwrap(), id);
}

/// A1: the soul test. 100 concurrent transfers of 1 unit each.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn one_hundred_concurrent_transfers_conserve_the_total() {
    let st = state();
    let app = app(st.clone());
    let (alice, at) = signup(&app, "alice").await;
    let (bob, _bt) = signup(&app, "bob").await;
    st.fund(&alice, 500, "test funding").unwrap();

    // 100 笔并发走**库层**直调（`Platform::transfer`）：HTTP 面自审计第 5 轮
    // 起把 `POST /api/wallet/transfer` 挂进了 Send 写闸门（默认窗口 20 笔就
    // 限流），再从这里打 HTTP 只能测到 429。守恒不变式属于账本，不属于
    // 传输层——HTTP 形状与幂等另有用例（transfers_over_http / idempotency_*）。
    let tasks: Vec<_> = (0..100)
        .map(|_| {
            let st = st.clone();
            let alice = alice.clone();
            let bob = bob.clone();
            tokio::task::spawn_blocking(move || st.transfer(&alice, &bob, 1, ""))
        })
        .collect();

    let results = futures_util::future::join_all(tasks).await;
    let oks = results.iter().filter(|r| matches!(r, Ok(Ok(_)))).count();
    let panics = results.iter().filter(|r| r.is_err()).count();
    assert_eq!(panics, 0, "no task may panic");
    assert_eq!(oks, 100, "every transfer succeeded: {oks}");

    // balance decreased by exactly 100
    assert_eq!(balance_of(&app, &at).await, 400);
    // and exactly 100 transfer_out rows — none lost, none duplicated
    let outs: Vec<_> = ledger_rows(&app, &at)
        .await
        .into_iter()
        .filter(|r| r["kind"].as_str() == Some("transfer_out"))
        .collect();
    assert_eq!(outs.len(), 100, "exactly 100 ledger entries");

    let ins: Vec<_> = ledger_rows(&app, &{
        // fetch bob's ledger with a fresh login
        let (_, body) = call(
            &app,
            req(
                "POST",
                "/api/login",
                Some(json!({"name": "bob", "password": "pw"})),
                None,
            ),
        )
        .await;
        body["token"].as_str().unwrap().to_string()
    })
    .await
    .into_iter()
    .filter(|r| r["kind"].as_str() == Some("transfer_in"))
    .collect();
    assert_eq!(ins.len(), 100, "recipient side has 100 matching rows");
    assert_eq!(
        st.money_supply().unwrap(),
        500,
        "money is neither created nor destroyed"
    );
}

#[tokio::test]
async fn insufficient_balance_rolls_back_with_no_ledger() {
    let (st, rec) = scene().await;
    let app = app(st.clone());
    let (alice, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    st.fund(&alice, 50, "test funding").unwrap();

    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/wallet/transfer",
            Some(json!({"to_uid": bob, "amount": 51})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(balance_of(&app, &at).await, 50, "sender untouched");
    assert_eq!(balance_of(&app, &bt).await, 0, "recipient untouched");
    assert_eq!(
        ledger_rows(&app, &at)
            .await
            .iter()
            .filter(|r| r["kind"].as_str() == Some("transfer_out"))
            .count(),
        0,
        "a failed transfer writes no flow"
    );
    assert_eq!(st.money_supply().unwrap(), 50);
    assert_eq!(rec.count(), 0, "nothing announced for a failed transfer");
}

#[tokio::test]
async fn non_positive_amounts_are_refused() {
    let (st, _rec) = scene().await;
    let app = app(st.clone());
    let (alice, at) = signup(&app, "alice").await;
    let (bob, _) = signup(&app, "bob").await;
    st.fund(&alice, 1_000, "test funding").unwrap();

    for amount in [0, -1, -9_999] {
        let (status, _) = call(
            &app,
            req(
                "POST",
                "/api/wallet/transfer",
                Some(json!({"to_uid": &bob, "amount": amount})),
                Some(&at),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "amount {amount}");
    }
    assert_eq!(balance_of(&app, &at).await, 1_000);
}

#[tokio::test]
async fn transfer_to_unknown_user_or_self_is_refused() {
    let (st, _rec) = scene().await;
    let app = app(st.clone());
    let (alice, at) = signup(&app, "alice").await;
    st.fund(&alice, 1_000, "test funding").unwrap();

    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/wallet/transfer",
            Some(json!({"to_uid": "ffffffffffffffff", "amount": 10})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown recipient");

    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/wallet/transfer",
            Some(json!({"to_uid": alice, "amount": 10})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "self transfer");

    assert_eq!(
        balance_of(&app, &at).await,
        1_000,
        "neither attempt moved money"
    );
    assert_eq!(
        ledger_rows(&app, &at).await.len(),
        1,
        "only the funding row"
    );
}

#[tokio::test]
async fn redpacket_freezes_the_full_total_at_creation() {
    let (st, rec) = scene().await;
    let app = app(st.clone());
    let (alice, at) = signup(&app, "alice").await;
    st.fund(&alice, 1_000, "test funding").unwrap();
    rec.take();

    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/wallet/redpacket",
            Some(json!({"amount": 200, "count": 4, "memo": "新年快乐"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id = body["redpacket_id"].as_str().unwrap().to_string();

    // frozen: the whole 200 left the sender immediately
    assert_eq!(balance_of(&app, &at).await, 800);
    let packet = st.redpacket(&id).unwrap().unwrap();
    assert_eq!(packet.total, 200);
    assert_eq!(packet.count, 4);
    assert_eq!(packet.claimed, 0);
    assert_eq!(packet.claimed_total, 0);
    assert_eq!(packet.status, "open");
    assert_eq!(
        packet.expire_at,
        packet.created_at + wxlike_platform::REDPACKET_TTL_MS as i64
    );
    assert_eq!(
        st.money_supply().unwrap(),
        1_000,
        "frozen money still exists"
    );

    // announcement carries the Custom payload
    let events = rec.take();
    assert_eq!(events.len(), 1);
    match &events[0].body {
        MsgBody::Custom { kind, payload } => {
            assert_eq!(kind, "wallet.redpacket");
            assert_eq!(payload["redpacket_id"], id.as_str());
        }
        other => panic!("expected Custom, got {other:?}"),
    }
}

/// A1: 3 shares, 5 claimers — exactly 3 succeed and the total is conserved.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn exactly_count_claims_succeed_and_the_total_is_conserved() {
    let st = state();
    let app = app(st.clone());
    let (alice, at) = signup(&app, "alice").await;
    // five claimers, none of them the sender (the owner may not grab their own
    // packet, which is asserted separately)
    let mut tokens = Vec::new();
    for i in 1..=5 {
        let (_uid, tk) = signup(&app, &format!("claim{i}")).await;
        tokens.push(tk);
    }
    st.fund(&alice, 1_000, "test funding").unwrap();

    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/wallet/redpacket",
            Some(json!({"amount": 100, "count": 3})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = body["redpacket_id"].as_str().unwrap().to_string();

    // five people grab three shares, concurrently
    let tasks: Vec<_> = tokens
        .iter()
        .map(|tk| {
            let app = app.clone();
            let tk = tk.clone();
            let id = id.clone();
            tokio::spawn(async move {
                call(
                    &app,
                    req(
                        "POST",
                        &format!("/api/wallet/redpacket/{id}/claim"),
                        None,
                        Some(&tk),
                    ),
                )
                .await
            })
        })
        .collect();
    let results = futures_util::future::join_all(tasks).await;

    let mut gained: Vec<i64> = Vec::new();
    let mut refused = 0;
    for r in results {
        let (status, body) = r.unwrap();
        if status == StatusCode::OK {
            gained.push(body["amount"].as_i64().unwrap());
        } else {
            assert_eq!(status, StatusCode::CONFLICT, "overflow claim: {body}");
            refused += 1;
        }
    }
    assert_eq!(gained.len(), 3, "exactly three claims succeed");
    assert_eq!(refused, 2, "the other two are told it is empty");

    // every share is positive and the shares sum to the frozen total
    assert!(gained.iter().all(|g| *g > 0), "no zero shares");
    assert_eq!(gained.iter().sum::<i64>(), 100, "total conserved exactly");

    let packet = st.redpacket(&id).unwrap().unwrap();
    assert_eq!(packet.claimed, 3);
    assert_eq!(packet.claimed_total, 100);
    assert_eq!(packet.status, "finished", "auto-closes when empty");
    assert_eq!(balance_of(&app, &at).await, 900, "sender already paid out");
    assert_eq!(st.money_supply().unwrap(), 1_000, "system total unchanged");
}

#[tokio::test]
async fn claiming_twice_is_refused_and_creates_no_flow() {
    let (st, _rec) = scene().await;
    let app = app(st.clone());
    let (alice, at) = signup(&app, "alice").await;
    let (_bob, bt) = signup(&app, "bob").await;
    st.fund(&alice, 500, "test funding").unwrap();

    let (_, body) = call(
        &app,
        req(
            "POST",
            "/api/wallet/redpacket",
            Some(json!({"amount": 60, "count": 2})),
            Some(&at),
        ),
    )
    .await;
    let id = body["redpacket_id"].as_str().unwrap().to_string();

    let (status, first) = call(
        &app,
        req(
            "POST",
            &format!("/api/wallet/redpacket/{id}/claim"),
            None,
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let share = first["amount"].as_i64().unwrap();

    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/wallet/redpacket/{id}/claim"),
            None,
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "one share per person");
    assert_eq!(balance_of(&app, &bt).await, share, "not credited twice");
    assert_eq!(
        ledger_rows(&app, &bt)
            .await
            .iter()
            .filter(|r| r["kind"].as_str() == Some("redpacket_in"))
            .count(),
        1,
        "one ledger row"
    );
}

#[tokio::test]
async fn expired_packet_refunds_the_remainder() {
    let (st, _rec) = scene().await;
    let app = app(st.clone());
    let (alice, at) = signup(&app, "alice").await;
    let (_bob, bt) = signup(&app, "bob").await;
    st.fund(&alice, 1_000, "test funding").unwrap();

    let (_, body) = call(
        &app,
        req(
            "POST",
            "/api/wallet/redpacket",
            Some(json!({"amount": 300, "count": 3})),
            Some(&at),
        ),
    )
    .await;
    let id = body["redpacket_id"].as_str().unwrap().to_string();
    call(
        &app,
        req(
            "POST",
            &format!("/api/wallet/redpacket/{id}/claim"),
            None,
            Some(&bt),
        ),
    )
    .await;

    let claimed = st.redpacket(&id).unwrap().unwrap().claimed_total;
    assert!(claimed > 0 && claimed < 300, "one of three shares claimed");

    // A1 asserts the refund path without waiting a day: force the deadline past
    {
        let c = st.db().lock();
        c.execute(
            "UPDATE redpackets SET expire_at = ?1 WHERE id = ?2",
            rusqlite::params![1i64, id],
        )
        .unwrap();
    }
    let swept = st.sweep_expired_redpackets().unwrap();
    assert_eq!(swept, 1);

    let packet = st.redpacket(&id).unwrap().unwrap();
    assert_eq!(packet.status, "expired");
    assert_eq!(
        balance_of(&app, &at).await,
        1_000 - 300 + (300 - claimed),
        "unclaimed remainder returned"
    );
    let refunds: Vec<_> = ledger_rows(&app, &at)
        .await
        .into_iter()
        .filter(|r| r["kind"].as_str() == Some("redpacket_refund"))
        .collect();
    assert_eq!(refunds.len(), 1, "the refund is itself a ledger row");
    assert_eq!(refunds[0]["amount"].as_i64(), Some(300 - claimed));
    assert_eq!(refunds[0]["ref_id"].as_str().unwrap(), id);

    // a second sweep changes nothing
    assert_eq!(st.sweep_expired_redpackets().unwrap(), 0);
    assert_eq!(
        st.money_supply().unwrap(),
        1_000,
        "conservation holds through refund"
    );

    // and an expired packet refuses further claims
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/wallet/redpacket/{id}/claim"),
            None,
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn packet_sizes_that_cannot_be_split_are_refused() {
    let (st, _rec) = scene().await;
    let app = app(st.clone());
    let (alice, at) = signup(&app, "alice").await;
    st.fund(&alice, 1_000, "test funding").unwrap();

    // 5 units across 7 people would need fractional shares
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/wallet/redpacket",
            Some(json!({"amount": 5, "count": 7})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    for (amount, count) in [(100i64, 0i64), (100, -1), (0, 1), (-5, 2)] {
        let (status, _) = call(
            &app,
            req(
                "POST",
                "/api/wallet/redpacket",
                Some(json!({"amount": amount, "count": count})),
                Some(&at),
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "amount {amount} count {count}"
        );
    }
    assert_eq!(balance_of(&app, &at).await, 1_000, "nothing was frozen");
}

#[tokio::test]
async fn claim_identity_comes_from_the_token_not_the_body() {
    // A2: forging another claimer must not be possible
    let (st, _rec) = scene().await;
    let app = app(st.clone());
    let (alice, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    let (carol, _ct) = signup(&app, "carol").await;
    st.fund(&alice, 1_000, "test funding").unwrap();

    let (_, body) = call(
        &app,
        req(
            "POST",
            "/api/wallet/redpacket",
            Some(json!({"amount": 10, "count": 2})),
            Some(&at),
        ),
    )
    .await;
    let id = body["redpacket_id"].as_str().unwrap().to_string();

    // bob claims, naming carol in the body
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/wallet/redpacket/{id}/claim"),
            Some(json!({"user_id": carol, "by": carol})),
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let packet = st.redpacket(&id).unwrap().unwrap();
    let claimers: Vec<String> = {
        let c = st.db().lock();
        let raw: String = c
            .query_row(
                "SELECT claimers FROM redpackets WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap();
        serde_json::from_str(&raw).unwrap()
    };
    assert_eq!(claimers, vec![bob.clone()], "the caller is the claimer");
    // shares are drawn randomly, so assert the invariants rather than a value
    let share = {
        let c = st.db().lock();
        c.query_row(
            "SELECT claimed_total FROM redpackets WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
    };
    assert!(
        (1..=9).contains(&share),
        "share {share} leaves >=1 for the last claimer"
    );
    assert_eq!(
        balance_of(&app, &bt).await,
        share,
        "bob holds exactly his share"
    );
    assert_eq!(
        balance_of(&app, &{
            let (_, body) = call(
                &app,
                req(
                    "POST",
                    "/api/login",
                    Some(json!({"name": "carol", "password": "pw"})),
                    None,
                ),
            )
            .await;
            body["token"].as_str().unwrap().to_string()
        })
        .await,
        0,
        "carol got nothing"
    );
    assert_eq!(packet.claimed, 1);
}

#[tokio::test]
async fn claiming_unknown_or_finished_packet_is_refused() {
    let (st, _rec) = scene().await;
    let app = app(st.clone());
    let (alice, at) = signup(&app, "alice").await;
    let (_bob, bt) = signup(&app, "bob").await;
    let (_carol, ct) = signup(&app, "carol").await;
    st.fund(&alice, 1_000, "test funding").unwrap();

    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/wallet/redpacket/deadbeef00000000/claim",
            None,
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown packet");

    let (_, body) = call(
        &app,
        req(
            "POST",
            "/api/wallet/redpacket",
            Some(json!({"amount": 4, "count": 2})),
            Some(&at),
        ),
    )
    .await;
    let id = body["redpacket_id"].as_str().unwrap().to_string();
    for tk in [&bt, &ct] {
        let (status, _) = call(
            &app,
            req(
                "POST",
                &format!("/api/wallet/redpacket/{id}/claim"),
                None,
                Some(tk),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/wallet/redpacket/{id}/claim"),
            None,
            Some(&alice_token(&app, &at).await),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "exhausted: {body}");

    // the third claimer created no ledger row at all
    assert_eq!(
        ledger_rows(&app, &alice_token(&app, &at).await)
            .await
            .iter()
            .filter(|r| r["kind"].as_str() == Some("redpacket_in"))
            .count(),
        0
    );
    assert_eq!(st.money_supply().unwrap(), 1_000);
}

/// The sender's own token, re-minted (the original is moved into other calls).
async fn alice_token(app: &axum::Router, _existing: &str) -> String {
    let (_, body) = call(
        app,
        req(
            "POST",
            "/api/login",
            Some(json!({"name": "alice", "password": "pw"})),
            None,
        ),
    )
    .await;
    body["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn ledger_is_paginated_by_cursor() {
    let (st, _rec) = scene().await;
    let app = app(st.clone());
    let (alice, at) = signup(&app, "alice").await;
    let (bob, _bt) = signup(&app, "bob").await;
    st.fund(&alice, 1_000, "test funding").unwrap();
    for _ in 0..6 {
        topup(&app, &at, &bob, 10).await;
    }

    let (status, page) = call(
        &app,
        req("GET", "/api/wallet/ledger?since=0&limit=3", None, Some(&at)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rows = page["entries"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "limit respected");
    assert_eq!(page["has_more"].as_bool(), Some(true));
    // ascending: the cursor is the newest id seen so far
    let ids: Vec<i64> = rows.iter().map(|r| r["id"].as_i64().unwrap()).collect();
    assert_eq!(
        ids,
        {
            let mut sorted = ids.clone();
            sorted.sort_unstable();
            sorted
        },
        "rows come back in ascending id order"
    );
    let cursor = *ids.last().unwrap();

    // the next page continues above the cursor with no overlap
    let (_, next) = call(
        &app,
        req(
            "GET",
            &format!("/api/wallet/ledger?since={cursor}&limit=3"),
            None,
            Some(&at),
        ),
    )
    .await;
    let nrows = next["entries"].as_array().unwrap();
    assert_eq!(nrows.len(), 3);
    assert!(
        nrows.iter().all(|r| r["id"].as_i64().unwrap() > cursor),
        "strictly newer than the cursor"
    );

    // walking the whole way yields every row exactly once
    let mut seen = std::collections::HashSet::new();
    let mut since = 0i64;
    for _ in 0..10 {
        let (_, page) = call(
            &app,
            req(
                "GET",
                &format!("/api/wallet/ledger?since={since}&limit=2"),
                None,
                Some(&at),
            ),
        )
        .await;
        let rows = page["entries"].as_array().unwrap();
        if rows.is_empty() {
            break;
        }
        for r in rows {
            let id = r["id"].as_i64().unwrap();
            assert!(seen.insert(id), "row {id} delivered twice");
            since = since.max(id);
        }
    }
    assert_eq!(seen.len(), 7, "1 funding + 6 transfers");
    assert_eq!(
        ledger_rows(&app, &at).await.len(),
        7,
        "and the default page shows all of them"
    );
}

#[tokio::test]
async fn wallet_endpoints_require_auth() {
    let (_st, _rec) = scene().await;
    let app = app(state());
    for (method, path) in [
        ("POST", "/api/wallet/transfer"),
        ("POST", "/api/wallet/redpacket"),
        ("POST", "/api/wallet/redpacket/ffffffffffffffff/claim"),
        ("GET", "/api/wallet/balance"),
        ("GET", "/api/wallet/ledger"),
    ] {
        let body = match method {
            "POST" if path.ends_with("transfer") => Some(json!({"to_uid": "x", "amount": 1})),
            "POST" if path.ends_with("redpacket") => Some(json!({"amount": 1, "count": 1})),
            _ => None,
        };
        let (status, _) = call(&app, req(method, path, body, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
    }
}

#[tokio::test]
async fn domain_layer_rejects_bad_inputs() {
    let (st, _rec) = scene().await;
    let alice = st.register("alice", "pw").unwrap();
    let bob = st.register("bob", "pw").unwrap();
    st.fund(&alice, 100, "seed").unwrap();

    // amounts
    assert!(st.fund("nouser00000000", 10, "x").is_err(), "unknown user");
    assert!(st.fund(&alice, 0, "x").is_err());
    assert!(st.fund(&alice, -5, "x").is_err());
    // ids
    assert!(st.transfer(&alice, "../../etc/passwd", 1, "").is_err());
    assert!(
        st.transfer(&alice, &bob, 1_000_000, "").is_err(),
        "over balance"
    );
    assert!(st.transfer(&bob, &alice, 1, "").is_err(), "bob has nothing");
    assert_eq!(st.wallet_balance(&alice).unwrap(), 100);
    assert_eq!(
        st.wallet_balance(&bob).unwrap(),
        0,
        "wallet-less user reads zero"
    );
    // packet without a conversation falls back to its own id
    let id = st.create_redpacket(&alice, 10, 2, "", None).unwrap();
    let packet = st.redpacket(&id).unwrap().unwrap();
    assert_eq!(packet.conv_id, id, "documented fallback");
    assert_eq!(st.redpacket("nosuchid00000000").unwrap(), None);
}

#[tokio::test]
async fn every_wallet_action_emits_a_custom_message_in_the_right_conversation() {
    let (st, rec) = scene().await;
    let app = app(st.clone());
    let (alice, at) = signup(&app, "alice").await;
    let (bob, bt) = signup(&app, "bob").await;
    st.fund(&alice, 1_000, "test funding").unwrap();
    // 红包要发进发送者可读的真实会话（authz，审计第 5 轮）：建两人的群
    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/groups",
            Some(json!({"name": "wallet-scene", "member_ids": [bob.clone()]})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create group: {body}");
    let conv = body["group_id"].as_str().unwrap().to_string();
    rec.take();

    topup(&app, &at, &bob, 100).await;
    let transfer_event = rec.take().pop().expect("one message per transfer");
    assert_eq!(
        transfer_event.conv_id,
        wxlike_platform::direct_conv_id(&alice, &bob),
        "a transfer rides the DM conversation"
    );
    assert_eq!(transfer_event.to, vec![bob.clone()]);

    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/wallet/redpacket",
            Some(json!({"amount": 20, "count": 2, "conv_id": conv})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id = body["redpacket_id"].as_str().unwrap().to_string();
    let packet_event = rec.take().pop().expect("packet announcement");
    assert_eq!(
        packet_event.conv_id, conv,
        "posted into the conversation it names"
    );

    call(
        &app,
        req(
            "POST",
            &format!("/api/wallet/redpacket/{id}/claim"),
            None,
            Some(&bt),
        ),
    )
    .await;
    let claim_event = rec.take().pop().expect("claim announcement");
    assert_eq!(
        claim_event.conv_id, conv,
        "the claim rides the same conversation"
    );
    assert_eq!(claim_event.sender, bob);

    let kinds = [transfer_event, packet_event, claim_event]
        .iter()
        .filter_map(|e| match &e.body {
            MsgBody::Custom { kind, .. } => Some(kind.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec![
            "wallet.transfer",
            "wallet.redpacket",
            "wallet.redpacket.claim"
        ]
    );
    // funding is a domain-only seam: no HTTP endpoint, no announcement
    assert_eq!(st.money_supply().unwrap(), 1_000);
}

// ---------------------------------------------------------------------------
// 审计第 5 轮：幂等键、红包 authz、GET 红包、ledger 回包形状
// ---------------------------------------------------------------------------

/// 带自定义头的 POST（`common::req` 不支持额外头）。
async fn post_with_header(
    app: &axum::Router,
    path: &str,
    body: Value,
    token: &str,
    header: (&str, &str),
) -> (StatusCode, Value) {
    use axum::body::Body;
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;

    let request = axum::http::Request::builder()
        .method("POST")
        .uri(path)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header.0, header.1)
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.clone().oneshot(request).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, value)
}

#[tokio::test]
async fn idempotency_key_replays_one_transfer_and_rejects_param_drift() {
    let (st, rec) = scene().await;
    let app = app(st.clone());
    let (alice, at) = signup(&app, "alice_idem").await;
    let (bob, _) = signup(&app, "bob_idem").await;
    st.fund(&alice, 10_000, "test").unwrap();

    let body = json!({"to_uid": bob, "amount": 100, "memo": "rent"});
    let (s1, b1) = post_with_header(
        &app,
        "/api/wallet/transfer",
        body.clone(),
        &at,
        ("Idempotency-Key", "key-1"),
    )
    .await;
    assert_eq!(s1, StatusCode::OK, "{b1}");
    let transfer_id = b1["transfer_id"].as_str().unwrap().to_string();

    // 同键同参：重放同一结果，不再扣第二笔
    let (s2, b2) = post_with_header(
        &app,
        "/api/wallet/transfer",
        body,
        &at,
        ("Idempotency-Key", "key-1"),
    )
    .await;
    assert_eq!(s2, StatusCode::OK, "{b2}");
    assert_eq!(b2["transfer_id"].as_str(), Some(transfer_id.as_str()));
    assert_eq!(
        st.wallet_balance(&alice).unwrap(),
        10_000 - 100,
        "replay must not move money twice"
    );

    // 同键异参：409
    let (s3, b3) = post_with_header(
        &app,
        "/api/wallet/transfer",
        json!({"to_uid": bob, "amount": 999, "memo": "rent"}),
        &at,
        ("Idempotency-Key", "key-1"),
    )
    .await;
    assert_eq!(s3, StatusCode::CONFLICT, "{b3}");
    assert_eq!(st.wallet_balance(&alice).unwrap(), 10_000 - 100);

    // 不带键：各转各的
    let (s4, _) = call(
        &app,
        req(
            "POST",
            "/api/wallet/transfer",
            Some(json!({"to_uid": bob, "amount": 100, "memo": "rent"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(s4, StatusCode::OK);
    assert_eq!(st.wallet_balance(&alice).unwrap(), 10_000 - 200);
    let _ = rec.take();
}

#[tokio::test]
async fn idempotency_key_applies_to_redpackets_too() {
    let (st, _rec) = scene().await;
    let app = app(st.clone());
    let (alice, at) = signup(&app, "alice_rp_idem").await;
    let (bob, _) = signup(&app, "bob_rp_idem").await;
    st.fund(&alice, 10_000, "test").unwrap();
    let conv = wxlike_platform::direct_conv_id(&alice, &bob);

    let body = json!({"amount": 300, "count": 2, "conv_id": conv});
    let (s1, b1) = post_with_header(
        &app,
        "/api/wallet/redpacket",
        body.clone(),
        &at,
        ("Idempotency-Key", "rp-1"),
    )
    .await;
    assert_eq!(s1, StatusCode::OK, "{b1}");
    let id = b1["redpacket_id"].as_str().unwrap().to_string();

    let (s2, b2) = post_with_header(
        &app,
        "/api/wallet/redpacket",
        body,
        &at,
        ("Idempotency-Key", "rp-1"),
    )
    .await;
    assert_eq!(s2, StatusCode::OK, "{b2}");
    assert_eq!(b2["redpacket_id"].as_str(), Some(id.as_str()));
    assert_eq!(
        st.wallet_balance(&alice).unwrap(),
        10_000 - 300,
        "replay must not freeze money twice"
    );
}

#[tokio::test]
async fn redpacket_into_a_conversation_you_cannot_read_is_forbidden() {
    let (st, _rec) = scene().await;
    let app = app(st.clone());
    let (alice, at) = signup(&app, "alice_authz").await;
    let (bob, bt) = signup(&app, "bob_authz").await;
    let (carol, _) = signup(&app, "carol_authz").await;
    st.fund(&alice, 10_000, "test").unwrap();

    // bob 与 carol 的直聊，alice 读不了
    let others_dm = wxlike_platform::direct_conv_id(&bob, &carol);
    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/wallet/redpacket",
            Some(json!({"amount": 100, "count": 1, "conv_id": others_dm})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(st.money_supply().unwrap(), 10_000, "钱没被动过");
    assert!(st.total_money().unwrap() >= 0);

    // 群也一样：不是成员就发不进去
    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/groups",
            Some(json!({"name": "no-alice", "member_ids": [carol.clone()]})),
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let group = body["group_id"].as_str().unwrap().to_string();
    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/wallet/redpacket",
            Some(json!({"amount": 100, "count": 1, "conv_id": group})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // 自己的直聊照常可发（对照，防止 authz 一刀切）
    let mine = wxlike_platform::direct_conv_id(&alice, &bob);
    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/wallet/redpacket",
            Some(json!({"amount": 100, "count": 1, "conv_id": mine})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn redpacket_row_is_readable_by_sender_and_members_only() {
    let (st, _rec) = scene().await;
    let app = app(st.clone());
    let (alice, at) = signup(&app, "alice_row").await;
    let (bob, bt) = signup(&app, "bob_row").await;
    let (mallory, mt) = signup(&app, "mallory_row").await;
    st.fund(&alice, 1_000, "test").unwrap();
    let conv = wxlike_platform::direct_conv_id(&alice, &bob);

    let (_, body) = call(
        &app,
        req(
            "POST",
            "/api/wallet/redpacket",
            Some(json!({"amount": 200, "count": 2, "conv_id": conv})),
            Some(&at),
        ),
    )
    .await;
    let id = body["redpacket_id"].as_str().unwrap().to_string();

    for (token, who) in [(&at, "sender"), (&bt, "member")] {
        let (status, body) = call(
            &app,
            req(
                "GET",
                &format!("/api/wallet/redpacket/{id}"),
                None,
                Some(token),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{who}: {body}");
        assert_eq!(body["id"].as_str(), Some(id.as_str()));
        assert_eq!(body["total"].as_i64(), Some(200));
        assert_eq!(body["status"].as_str(), Some("open"));
    }
    // 局外人：404（不确认存在性）
    let (status, _) = call(
        &app,
        req(
            "GET",
            &format!("/api/wallet/redpacket/{id}"),
            None,
            Some(&mt),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "mallory must not see it: {status}"
    );
    let _ = mallory;
}

/// 钉死 UI 正在映射的回包形状：`{entries, cursor, has_more}`（审计第 5 轮：
/// 服务端不改形状，用测试把它锁住，防止后续改动静默破坏客户端）。
#[tokio::test]
async fn ledger_response_shape_is_entries_cursor_has_more() {
    let (st, _rec) = scene().await;
    let app = app(st.clone());
    let (alice, at) = signup(&app, "alice_shape").await;
    let (bob, _) = signup(&app, "bob_shape").await;
    st.fund(&alice, 5_000, "test").unwrap();
    topup(&app, &at, &bob, 100).await;
    topup(&app, &at, &bob, 200).await;

    let (status, body) = call(
        &app,
        req("GET", "/api/wallet/ledger?since=0&limit=1", None, Some(&at)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for key in ["entries", "cursor", "has_more"] {
        assert!(body.get(key).is_some(), "missing `{key}`: {body}");
    }
    let entries = body["entries"].as_array().expect("entries is a list");
    assert_eq!(entries.len(), 1, "limit=1 → one entry: {body}");
    assert_eq!(body["has_more"].as_bool(), Some(true), "{body}");
    assert!(body["cursor"].as_i64().unwrap_or(0) > 0, "{body}");

    // 尾页：不足一页 → has_more=false
    let (status, body) = call(
        &app,
        req(
            "GET",
            &format!(
                "/api/wallet/ledger?since={}",
                body["cursor"].as_i64().unwrap()
            ),
            None,
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["has_more"].as_bool(), Some(false), "{body}");
}
