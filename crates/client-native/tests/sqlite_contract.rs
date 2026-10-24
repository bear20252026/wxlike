//! SQLite 本地库的契约测试：**真实实现**，不 mock。
//!
//! 这里验的是"杀进程不丢消息"的物理前提：
//!
//! * 入队是一条 SQL（原子），同一幂等键重复入队只留一条；
//! * `synchronous = FULL` 之后，进程被杀、机器断电，已提交的事务还在；
//! * 事件按 `event_id` 幂等，游标只前进。

use wxlike_client_core::ports::{LocalStore, OutboxRecord, StoreError};
use wxlike_client_native::SqliteStore;
use wxlike_types::{
    ActionId, Cursor, CursorKind, EventEnvelope, EventId, EventKind, IdempotencyKey, Seq,
};

fn action(id: &str) -> ActionId {
    ActionId::parse(id).expect("action")
}

fn key(raw: &str) -> IdempotencyKey {
    IdempotencyKey::parse(raw).expect("key")
}

fn record(raw_key: &str, now_ms: i64) -> OutboxRecord {
    OutboxRecord::new(
        key(raw_key),
        action("msg.send"),
        serde_json::json!({"conv": "d-a~b", "text": "你好", "idempotency_key": raw_key}),
        now_ms,
    )
}

fn event(id: &str, seq: u64) -> EventEnvelope {
    EventEnvelope::new(
        EventId::parse(id).expect("event id"),
        Seq::new(seq),
        EventKind::MessageNew,
        serde_json::json!({"text": "hi"}),
        1_700_000_000_000,
    )
}

#[tokio::test]
async fn same_key_twice_keeps_one_row() {
    let store = SqliteStore::open_in_memory().expect("开库");
    let mut row = record("k-1", 1_000);
    store.put_outbox(&row).await.expect("首次入队");
    row.attempts = 2;
    store.put_outbox(&row).await.expect("重复入队");
    let due = store.due_outbox(1_000, 10).await.expect("取");
    assert_eq!(due.len(), 1, "同一幂等键不该有第二行");
    assert_eq!(due[0].attempts, 2, "重复入队应当覆盖");
}

#[tokio::test]
async fn committed_rows_survive_a_reopen() {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("client.db");
    {
        let store = SqliteStore::open(&path).expect("开库");
        store.put_outbox(&record("k-1", 1_000)).await.expect("入队");
        store
            .put_cursor(&Cursor::at(CursorKind::Event, Seq::new(7)))
            .await
            .expect("游标");
        // 这里"杀进程"：连接被 drop，不调用任何关闭接口
    }
    let reopened = SqliteStore::open(&path).expect("重开");
    let due = reopened.due_outbox(1_000, 10).await.expect("取");
    assert_eq!(due.len(), 1, "重启后待发记录必须还在");
    assert_eq!(due[0].idempotency_key.as_str(), "k-1");
    let cursor = reopened.cursor().await.expect("游标").expect("有游标");
    assert_eq!(cursor.decode().expect("解码").seq, Seq::new(7));
}

#[tokio::test]
async fn defer_pushes_the_record_out_of_the_due_window() {
    let store = SqliteStore::open_in_memory().expect("开库");
    store.put_outbox(&record("k-1", 1_000)).await.expect("入队");
    store
        .defer_outbox(&key("k-1"), 5_000, "网络不可用")
        .await
        .expect("推迟");
    assert!(store.due_outbox(1_000, 10).await.expect("取").is_empty());
    let due = store.due_outbox(5_000, 10).await.expect("取");
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].attempts, 1);
    assert_eq!(due[0].last_error.as_deref(), Some("网络不可用"));
}

#[tokio::test]
async fn mark_done_removes_the_row() {
    let store = SqliteStore::open_in_memory().expect("开库");
    store.put_outbox(&record("k-1", 1_000)).await.expect("入队");
    store.mark_outbox_done(&key("k-1")).await.expect("出队");
    assert!(store.due_outbox(10_000, 10).await.expect("取").is_empty());
}

#[tokio::test]
async fn events_are_idempotent_and_ordered() {
    let store = SqliteStore::open_in_memory().expect("开库");
    assert!(store.append_event(&event("evt-1", 1)).await.expect("首次"));
    assert!(!store.append_event(&event("evt-1", 1)).await.expect("重复"));
    store
        .append_event(&event("evt-2", 2))
        .await
        .expect("第二条");
    let rows = store.events_after(None, 10).await.expect("读");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].seq, Seq::new(1));
    let after = store
        .events_after(Some(&Cursor::at(CursorKind::Event, Seq::new(1))), 10)
        .await
        .expect("游标之后");
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].event_id.as_str(), "evt-2");
}

#[tokio::test]
async fn cursor_never_moves_backwards() {
    let store = SqliteStore::open_in_memory().expect("开库");
    store
        .put_cursor(&Cursor::at(CursorKind::Event, Seq::new(5)))
        .await
        .expect("前进");
    let err = store
        .put_cursor(&Cursor::at(CursorKind::Event, Seq::new(4)))
        .await
        .expect_err("回退");
    assert!(matches!(err, StoreError::Corrupt(_)), "{err:?}");
    store
        .put_cursor(&Cursor::at(CursorKind::Event, Seq::new(5)))
        .await
        .expect("原地重复是幂等的");
}

#[tokio::test]
async fn cache_round_trips_and_purge_clears_everything() {
    let store = SqliteStore::open_in_memory().expect("开库");
    store.put_cache("k", b"v", true).await.expect("写缓存");
    assert_eq!(
        store.get_cache("k").await.expect("读").as_deref(),
        Some(&b"v"[..])
    );
    store.put_outbox(&record("k-1", 1_000)).await.expect("入队");
    store.append_event(&event("evt-1", 1)).await.expect("事件");
    store.purge().await.expect("清空");
    assert!(store.get_cache("k").await.expect("读").is_none());
    assert!(store.due_outbox(10_000, 10).await.expect("取").is_empty());
    assert!(store.events_after(None, 10).await.expect("读").is_empty());
    assert!(store.cursor().await.expect("游标").is_none());
}

#[tokio::test]
async fn corrupt_params_are_reported_not_panicked() {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("client.db");
    {
        let store = SqliteStore::open(&path).expect("开库");
        store.put_outbox(&record("k-1", 1_000)).await.expect("入队");
    }
    // 模拟"库被人动过"：直接把 params 写成非法 JSON
    {
        let conn = rusqlite::Connection::open(&path).expect("直连");
        conn.execute("UPDATE outbox SET params = 'not json'", [])
            .expect("破坏数据");
    }
    let store = SqliteStore::open(&path).expect("重开");
    let err = store.due_outbox(10_000, 10).await.expect_err("应当报损坏");
    assert!(matches!(err, StoreError::Corrupt(_)), "{err:?}");
}
