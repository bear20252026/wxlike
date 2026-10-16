//! WP-09 A1 + A2: money conservation, the 100-concurrent transfer test, red
//! packet freeze/claim/refund, and the adversarial set.

mod common;

use axum::http::StatusCode;
use common::{app, call, req, signup, Recorder};
use serde_json::json;
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

    let tasks: Vec<_> = (0..100)
        .map(|_| {
            let app = app.clone();
            let at = at.clone();
            let bob = bob.clone();
            tokio::spawn(async move {
                let (status, body) = call(
                    &app,
                    req(
                        "POST",
                        "/api/wallet/transfer",
                        Some(json!({"to_uid": bob, "amount": 1})),
                        Some(&at),
                    ),
                )
                .await;
                (status, body)
            })
        })
        .collect();

    let results = futures_util::future::join_all(tasks).await;
    let oks = results
        .iter()
        .filter(|r| {
            r.as_ref()
                .map(|(s, _)| *s == StatusCode::OK)
                .unwrap_or(false)
        })
        .count();
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
    rec.take();

    topup(&app, &at, &bob, 100).await;
    let transfer_event = rec.take().pop().expect("one message per transfer");
    assert_eq!(
        transfer_event.conv_id,
        wxlike_platform::direct_conv_id(&alice, &bob),
        "a transfer rides the DM conversation"
    );
    assert_eq!(transfer_event.to, vec![bob.clone()]);

    let (_, body) = call(
        &app,
        req(
            "POST",
            "/api/wallet/redpacket",
            Some(json!({"amount": 20, "count": 2, "conv_id": "g1"})),
            Some(&at),
        ),
    )
    .await;
    let id = body["redpacket_id"].as_str().unwrap().to_string();
    let packet_event = rec.take().pop().expect("packet announcement");
    assert_eq!(
        packet_event.conv_id, "g1",
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
        claim_event.conv_id, "g1",
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
