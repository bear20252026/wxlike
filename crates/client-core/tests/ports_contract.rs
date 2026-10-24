//! 端口契约测试：这些不变量由**实现**保证，核心按它们写逻辑。
//!
//! 用内存替身跑，因为它们检验的是"契约本身有没有说清楚"，不是"SQLite /
//! IndexedDB 有没有实现对"——后者各自的卡里用真实实现验。
//!
//! 五条被钉住的不变量：
//!
//! 1. 同一幂等键入队两次只留一条（否则重试会产生两条消息）；
//! 2. 同一 `event_id` 追加两次只落一条，且第二次返回 `false`；
//! 3. 游标只许前进（回退 = 重放已确认事件）；
//! 4. 配额用尽是**错误**，不是静默丢弃；
//! 5. 传输错误到领域错误的映射只有一处，且 `cancelled` 不进重试。

use wxlike_client_core::ports::PlatformCaps;
use wxlike_client_core::ports::{
    Clock, LocalStore, StoreError, Transport, TransportError, TransportRequest, TransportResponse,
};
use wxlike_client_core::testing::{
    action, event_cursor, idem, MemClock, MemNotifications, MemPlatform, MemSecrets, MemStore,
    MemTransport,
};
use wxlike_types::{ConvId, DomainError, EventEnvelope, EventId, EventKind, Seq};

fn sample_event(id: &str, seq: u64) -> EventEnvelope {
    EventEnvelope::new(
        EventId::parse(id).expect("event id"),
        Seq::new(seq),
        EventKind::MessageNew,
        serde_json::json!({"text": "hi"}),
        1_700_000_000_000,
    )
}

#[tokio::test]
async fn outbox_is_idempotent_on_the_key() {
    let store = MemStore::new();
    let mut record = wxlike_client_core::ports::OutboxRecord::new(
        idem("k-1"),
        action("msg.send"),
        serde_json::json!({"conv": "d-a~b", "text": "hi"}),
        1_000,
    );
    store.put_outbox(&record).await.expect("第一次入队");
    record.attempts = 3;
    store.put_outbox(&record).await.expect("重复入队");
    assert_eq!(store.outbox_len(), 1, "同一幂等键不该产生第二条");
    let due = store.due_outbox(1_000, 10).await.expect("取到点的");
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].attempts, 3, "重复入队应当覆盖而不是新增");
}

#[tokio::test]
async fn events_are_deduped_by_event_id() {
    let store = MemStore::new();
    let event = sample_event("evt-1", 1);
    assert!(store.append_event(&event).await.expect("首次"));
    assert!(!store.append_event(&event).await.expect("重放"));
    assert_eq!(store.event_count(), 1);
}

#[tokio::test]
async fn events_after_cursor_never_replays_consumed_ones() {
    let store = MemStore::new();
    for seq in 1..=3 {
        store
            .append_event(&sample_event(&format!("evt-{seq}"), seq))
            .await
            .expect("append");
    }
    let cursor = event_cursor(2);
    let rows = store.events_after(Some(&cursor), 10).await.expect("读");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].seq, Seq::new(3));
}

#[tokio::test]
async fn cursor_cannot_go_backwards() {
    let store = MemStore::new();
    store.put_cursor(&event_cursor(5)).await.expect("前进");
    let err = store.put_cursor(&event_cursor(4)).await.expect_err("回退");
    assert!(matches!(err, StoreError::Corrupt(_)), "{err:?}");
    // 原地重复推进是允许的（幂等）
    store.put_cursor(&event_cursor(5)).await.expect("原地");
}

#[tokio::test]
async fn quota_exhaustion_is_reported_not_swallowed() {
    let store = MemStore::new();
    store.fail_next_writes(StoreError::QuotaExceeded);
    let err = store.put_cache("k", b"v", true).await.expect_err("配额");
    assert_eq!(err, StoreError::QuotaExceeded);
    assert!(!err.is_retryable(), "配额用尽重试也不会好");
    assert!(StoreError::Busy.is_retryable());
}

#[tokio::test]
async fn transport_errors_map_to_domain_codes_in_one_place() {
    let cases = [
        (TransportError::Offline("dns".into()), "unavailable"),
        (TransportError::Timeout, "timeout"),
        (TransportError::Protocol("tls".into()), "unavailable"),
        (TransportError::Cancelled, "cancelled"),
    ];
    for (error, code) in cases {
        assert_eq!(error.into_domain().code(), code);
    }
    // 取消与协议错误不该被重试
    assert!(!DomainError::cancelled().is_retryable());
}

#[tokio::test]
async fn http_status_becomes_a_domain_error_with_a_retry_hint() {
    let forbidden = TransportResponse {
        status: 403,
        body: b"{\"error\":\"not a member\"}".to_vec(),
    };
    let err = forbidden.json().expect_err("403");
    assert_eq!(err.code(), "forbidden");
    assert!(!err.is_retryable());

    let rate_limited = TransportResponse {
        status: 429,
        body: Vec::new(),
    };
    assert!(rate_limited.json().expect_err("429").is_retryable());

    let broken = TransportResponse {
        status: 200,
        body: b"not json".to_vec(),
    };
    assert_eq!(broken.json().expect_err("坏 JSON").code(), "unavailable");
}

#[tokio::test]
async fn clock_is_injectable_so_backoff_tests_do_not_wait() {
    let clock = MemClock::at(1_000);
    assert_eq!(clock.now_ms(), 1_000);
    clock.sleep(30_000).await;
    assert_eq!(clock.now_ms(), 31_000);
    assert_eq!(clock.sleeps(), vec![30_000]);
}

#[tokio::test]
async fn transport_records_what_it_sent() {
    let transport = MemTransport::new();
    transport.push_json(200, serde_json::json!({"ok": true}));
    let request = TransportRequest::get("/api/me").with_command(action("me.info"));
    let response = transport.send(request).await.expect("发送");
    assert_eq!(response.json().expect("json")["ok"], true);
    let sent = transport.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].command.as_ref().map(|c| c.as_str()),
        Some("me.info"),
        "审计标记必须跟着请求走"
    );
}

#[tokio::test]
async fn single_instance_lock_is_exclusive_and_released_on_drop() {
    use wxlike_client_core::ports::Platform;
    let platform = MemPlatform::new(PlatformCaps::native());
    let lock = platform.acquire_instance_lock().await.expect("第一把");
    let err = platform.acquire_instance_lock().await.expect_err("第二把");
    assert_eq!(
        err,
        wxlike_client_core::ports::PlatformError::AlreadyRunning
    );
    drop(lock);
    platform
        .acquire_instance_lock()
        .await
        .expect("释放后可再抢");
}

#[tokio::test]
async fn secrets_round_trip_and_debug_stays_redacted() {
    use wxlike_client_core::ports::{Secret, Secrets};
    let secrets = MemSecrets::new();
    secrets
        .put("token", Secret::from_text("abc"))
        .await
        .expect("写");
    let got = secrets.get("token").await.expect("读").expect("存在");
    assert_eq!(got.expose(), b"abc");
    assert!(!format!("{got:?}").contains("abc"));
    assert!(secrets.get("missing").await.expect("读空").is_none());
}

#[tokio::test]
async fn notifications_are_suppressed_in_foreground() {
    use wxlike_client_core::ports::{Notification, NotificationSink};
    let sink = MemNotifications::new();
    sink.set_foreground(true);
    sink.notify(Notification {
        title: "t".into(),
        body: "b".into(),
        conv: ConvId::parse("d-a~b").ok(),
        event_id: None,
    })
    .await
    .expect("前台");
    assert!(sink.sent().is_empty());
    sink.set_foreground(false);
    sink.notify(Notification {
        title: "t".into(),
        body: "b".into(),
        conv: None,
        event_id: None,
    })
    .await
    .expect("后台");
    assert_eq!(sink.sent().len(), 1);
}
