//! P1 store paging + substring search (the history endpoint's backing
//! queries — the protocol surface is exercised end-to-end by the server and
//! UI integration tests).

use wxlike_core::types::MsgBody;
use wxlike_core::{Envelope, Store};

/// One conversation all messages live in.
fn conv() -> &'static str {
    "d-alice~bob"
}

fn msg(i: u64, text: &str) -> Envelope {
    Envelope::text(conv(), format!("u{i}"), i, text)
}

#[test]
fn first_page_returns_newest_n_in_ascending_order() {
    let store = Store::open(None).unwrap();
    for i in 1..=10 {
        store.append(&msg(i, &format!("hello {i}")), false).unwrap();
    }
    let (rows, next) = store.page_messages(conv(), 0, 3).unwrap();
    let texts: Vec<_> = rows.iter().map(|r| r.envelope.body.clone()).collect();
    assert_eq!(texts.len(), 3);
    assert!(
        matches!(&texts[0], MsgBody::Text(t) if t == "hello 8"),
        "ascending order"
    );
    assert!(matches!(&texts[2], MsgBody::Text(t) if t == "hello 10"));
    assert_eq!(
        next,
        Some(8),
        "next page starts strictly below the oldest row"
    );
}

#[test]
fn walking_all_pages_sees_every_row_exactly_once() {
    let store = Store::open(None).unwrap();
    for i in 1..=25 {
        store.append(&msg(i, "x"), false).unwrap();
    }
    let mut seen: Vec<i64> = Vec::new();
    let mut before = 0i64;
    loop {
        let (rows, next) = store.page_messages(conv(), before, 7).unwrap();
        seen.extend(rows.iter().map(|r| r.seq));
        match next {
            Some(n) => before = n,
            None => break,
        }
    }
    assert_eq!(seen.len(), 25, "no duplicates and no gaps");
    let mut sorted = seen.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), seen.len());
}

#[test]
fn exhausted_page_reports_no_cursor() {
    let store = Store::open(None).unwrap();
    store.append(&msg(1, "only"), false).unwrap();
    let (rows, next) = store.page_messages(conv(), 0, 50).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        next, None,
        "short page means the conversation start was reached"
    );
    let (rows, next) = store.page_messages(conv(), 1, 50).unwrap();
    assert!(rows.is_empty());
    assert_eq!(next, None);
}

#[test]
fn search_matches_substrings_case_insensitively() {
    let store = Store::open(None).unwrap();
    store.append(&msg(1, "The Quick Brown Fox"), false).unwrap();
    store.append(&msg(2, "慢速模式 slow mode"), false).unwrap();
    store.append(&msg(3, "unrelated"), false).unwrap();

    let hits = store
        .search_messages("quick", &[conv().to_string()], 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(matches!(&hits[0].envelope.body, MsgBody::Text(t) if t.contains("Quick")));

    // non-ASCII substring works (byte-level LIKE, no word boundaries)
    let hits = store
        .search_messages("慢速", &[conv().to_string()], 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn search_wildcards_are_literal_and_filter_is_enforced() {
    let store = Store::open(None).unwrap();
    store.append(&msg(1, "100% done_"), false).unwrap();
    store.append(&msg(2, "1a0b0c% done_"), false).unwrap();

    // "%" and "_" must not act as wildcards: only the literal row matches
    let hits = store
        .search_messages("100% done_", &[conv().to_string()], 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(matches!(&hits[0].envelope.body, MsgBody::Text(t) if t == "100% done_"));

    // nothing outside the allowed conversation list is searched
    let hits = store
        .search_messages("done_", &["d-other~conv".to_string()], 10)
        .unwrap();
    assert!(hits.is_empty());
    // empty filter / empty needle short-circuit
    assert!(store.search_messages("done", &[], 10).unwrap().is_empty());
    assert!(store
        .search_messages("", &[conv().to_string()], 10)
        .unwrap()
        .is_empty());
}

#[test]
fn search_skips_recalled_rows() {
    let store = Store::open(None).unwrap();
    let env = msg(1, "seekret");
    store.append(&env, false).unwrap();
    assert_eq!(
        store
            .search_messages("seekret", &[conv().to_string()], 10)
            .unwrap()
            .len(),
        1
    );
    assert!(store.withdraw(&env.msg_id, "u1").unwrap());
    assert!(
        store
            .search_messages("seekret", &[conv().to_string()], 10)
            .unwrap()
            .is_empty(),
        "a recalled body must not be searchable"
    );
}
