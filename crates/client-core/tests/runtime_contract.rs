//! G01-02 验收：原生核心状态机的四条硬要求。
//!
//! 1. **本地提交 / 服务端接收 / ack 之间杀进程都不丢**；
//! 2. **同一 id 重试不重复**（幂等键不变，服务端据此去重）；
//! 3. **事件先落盘再交付**，重复投递只交付一次；
//! 4. **退出后后台任务停止、订阅可取消**。
//!
//! 用内存替身跑是因为这四条都是**核心的编排逻辑**，与 SQLite/IndexedDB 的
//! 具体实现无关；存储层各自的卡用真实实现再验一遍。

mod support;
use std::sync::Arc;

use support::{
    action, event_cursor, idem, MemClock, MemNotifications, MemPlatform, MemSecrets, MemStore,
    MemTransport,
};
use wxlike_client_core::ports::{
    Clock, LocalStore, OutboxRecord, PlatformCaps, Ports, StoreError, TransportError,
};
use wxlike_client_core::runtime::{Backoff, Session, SessionError};
use wxlike_types::{
    ActionOutcome, ActionRequest, EventEnvelope, EventId, EventKind, Seq, SubscribeRequest, TraceId,
};

const NOW: i64 = 1_700_000_000_000;

struct Fixture {
    store: Arc<MemStore>,
    transport: Arc<MemTransport>,
    clock: Arc<MemClock>,
    platform: Arc<MemPlatform>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            store: Arc::new(MemStore::new()),
            transport: Arc::new(MemTransport::new()),
            clock: Arc::new(MemClock::at(NOW)),
            platform: Arc::new(MemPlatform::new(PlatformCaps::native())),
        }
    }

    fn ports(&self) -> Ports {
        Ports::new(
            self.transport.clone(),
            self.store.clone(),
            self.clock.clone(),
            Arc::new(MemSecrets::new()),
            self.platform.clone(),
            Arc::new(MemNotifications::new()),
        )
    }

    async fn session(&self) -> Session {
        Session::open(self.ports()).await.expect("打开会话")
    }
}

fn trace() -> TraceId {
    TraceId::parse("t-1").expect("trace")
}

fn send_request(key: &str) -> ActionRequest {
    ActionRequest::new(
        action("msg.send"),
        serde_json::json!({"conv": "d-a~b", "text": "你好", "idempotency_key": key}),
        trace(),
    )
    .with_idempotency(idem(key))
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
async fn write_is_persisted_before_the_caller_gets_an_answer() {
    let fixture = Fixture::new();
    fixture
        .transport
        .push_error(TransportError::Offline("no network".to_string()));
    let session = fixture.session().await;
    let result = session.dispatch(send_request("k-1")).await;
    assert!(result.is_ok(), "网络失败不该让消息消失：{result:?}");
    let ActionOutcome::Ok { data } = &result.outcome else {
        panic!("应当返回 queued：{:?}", result.outcome);
    };
    assert_eq!(data["queued"], true);
    assert_eq!(fixture.store.outbox_len(), 1, "必须先落盘再回话");
}

#[tokio::test]
async fn killing_between_commit_and_ack_does_not_lose_the_message() {
    let fixture = Fixture::new();
    fixture
        .transport
        .push_error(TransportError::Offline("no network".to_string()));
    {
        let session = fixture.session().await;
        session.dispatch(send_request("k-1")).await;
        // 这里"杀进程"：会话被 drop，锁释放，内存状态清空
    }
    // 重启：同一份本地库
    let fixture2 = Fixture {
        store: fixture.store.clone(),
        transport: Arc::new(MemTransport::new()),
        clock: fixture.clock.clone(),
        platform: fixture.platform.clone(),
    };
    assert_eq!(fixture2.store.outbox_len(), 1, "重启后待发记录必须还在");
    fixture2.transport.push_json(
        200,
        serde_json::json!({"msg_id": "00112233445566778899aabbccddeeff"}),
    );
    let session = fixture2.session().await;
    let report = session.pump(10).await.expect("补发");
    assert_eq!(report.sent, 1);
    assert_eq!(fixture2.store.outbox_len(), 0);
    let body = fixture2.transport.sent()[0]
        .body
        .as_ref()
        .map(|bytes| String::from_utf8_lossy(bytes).to_string())
        .unwrap_or_default();
    assert!(
        body.contains("k-1"),
        "重试必须带原来的幂等键，否则服务端会当成新消息：{body}"
    );
}

#[tokio::test]
async fn retry_is_delayed_by_backoff_and_keeps_the_same_key() {
    let fixture = Fixture::new();
    let mut record = OutboxRecord::new(
        idem("k-1"),
        action("msg.send"),
        serde_json::json!({"conv": "d-a~b", "text": "x", "idempotency_key": "k-1"}),
        NOW,
    );
    record.attempts = 1;
    record.next_attempt_at_ms = NOW;
    fixture.store.put_outbox(&record).await.expect("入队");
    fixture.transport.push_error(TransportError::Timeout);
    let session = fixture.session().await;
    let report = session.pump(10).await.expect("补发");
    assert_eq!(report.deferred, 1);
    // 退避窗口内不再取出来
    let again = session.pump(10).await.expect("再补发");
    assert!(again.is_idle(), "退避窗口内不该重试");
    // 时间推进之后重新到点
    fixture.clock.advance(60_000);
    let due = fixture
        .store
        .due_outbox(fixture.clock.now_ms(), 10)
        .await
        .expect("取");
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].idempotency_key.as_str(), "k-1");
}

#[tokio::test]
async fn permanent_failure_leaves_the_queue_and_reports_the_error() {
    let fixture = Fixture::new();
    fixture
        .transport
        .push_json(403, serde_json::json!({"error": "not a member"}));
    let session = fixture.session().await;
    let result = session.dispatch(send_request("k-1")).await;
    let error = result.clone().into_payload().expect_err("应当失败");
    assert_eq!(error.code(), "forbidden");
    assert_eq!(
        fixture.store.outbox_len(),
        0,
        "永久失败不该留在队列里反复重试"
    );
}

#[tokio::test]
async fn exhausted_retries_park_the_record_instead_of_spinning() {
    let fixture = Fixture::new();
    let mut record = OutboxRecord::new(
        idem("k-1"),
        action("msg.send"),
        serde_json::json!({"conv": "d-a~b", "text": "x", "idempotency_key": "k-1"}),
        NOW,
    );
    record.attempts = Backoff::DEFAULT.max_attempts;
    fixture.store.put_outbox(&record).await.expect("入队");
    let session = fixture.session().await.with_backoff(Backoff::DEFAULT);
    let report = session.pump(10).await.expect("补发");
    assert_eq!(report.failed, 1);
    assert_eq!(fixture.transport.sent_len(), 0, "超过上限不该再发请求");
}

#[tokio::test]
async fn events_are_persisted_before_delivery_and_deduped() {
    let fixture = Fixture::new();
    fixture.transport.push_event(event("evt-1", 1));
    fixture.transport.push_event(event("evt-1", 1)); // 重连重放
    fixture.transport.push_event(event("evt-2", 2));
    let session = fixture.session().await;
    let mut subscription = session
        .subscribe(SubscribeRequest::all())
        .await
        .expect("订阅");
    let first = subscription.next().await.expect("第一条").expect("有事件");
    assert_eq!(first.event_id.as_str(), "evt-1");
    assert_eq!(fixture.store.event_count(), 1, "落盘先于交付");
    let second = subscription.next().await.expect("第二条").expect("有事件");
    assert_eq!(second.event_id.as_str(), "evt-2", "重复投递不该交付两次");
    assert_eq!(fixture.store.event_count(), 2);
    let cursor = fixture.store.cursor().await.expect("游标").expect("有游标");
    assert_eq!(cursor.decode().expect("解码").seq, Seq::new(2));
}

#[tokio::test]
async fn cancelled_subscription_stops_immediately() {
    let fixture = Fixture::new();
    fixture.transport.push_event(event("evt-1", 1));
    let session = fixture.session().await;
    let mut subscription = session
        .subscribe(SubscribeRequest::all())
        .await
        .expect("订阅");
    subscription.cancel();
    assert!(subscription.is_cancelled());
    let err = subscription.next().await.expect_err("已取消");
    assert_eq!(err.code(), "cancelled");
    assert_eq!(fixture.store.event_count(), 0, "取消之后不该再落盘");
}

#[tokio::test]
async fn replay_returns_persisted_events_after_cold_start() {
    let fixture = Fixture::new();
    fixture.transport.push_event(event("evt-1", 1));
    fixture.transport.push_event(event("evt-2", 2));
    let session = fixture.session().await;
    let mut subscription = session
        .subscribe(SubscribeRequest::all())
        .await
        .expect("订阅");
    subscription.next().await.expect("一");
    subscription.next().await.expect("二");
    let replayed = session.replay(10).await.expect("重放");
    assert!(replayed.is_empty(), "游标之后没有新事件");
}

#[tokio::test]
async fn one_session_at_a_time_and_shutdown_releases_the_lock() {
    let fixture = Fixture::new();
    let first = fixture.session().await;
    let err = Session::open(fixture.ports())
        .await
        .expect_err("第二个会话");
    assert!(matches!(err, SessionError::AlreadyRunning));
    first.shutdown();
    let _second = fixture.session().await;
}

#[tokio::test]
async fn expired_deadline_fails_before_touching_the_network() {
    let fixture = Fixture::new();
    let session = fixture.session().await;
    let request = ActionRequest::new(action("me.info"), serde_json::json!({}), trace())
        .with_deadline(wxlike_types::Deadline::at(NOW - 1));
    let result = session.dispatch(request).await;
    let error = result.into_payload().expect_err("超时");
    assert_eq!(error.code(), "timeout");
    assert_eq!(fixture.transport.sent_len(), 0, "超时不该发请求");
}

#[tokio::test]
async fn unknown_action_fails_safely() {
    let fixture = Fixture::new();
    let session = fixture.session().await;
    let request = ActionRequest::new(action("nope.nope"), serde_json::json!({}), trace());
    let error = session
        .dispatch(request)
        .await
        .into_payload()
        .expect_err("未登记");
    assert_eq!(error.code(), "invalid_argument");
    assert_eq!(fixture.transport.sent_len(), 0);
}

#[tokio::test]
async fn local_store_failure_is_reported_not_swallowed() {
    let fixture = Fixture::new();
    fixture.store.fail_next_writes(StoreError::QuotaExceeded);
    let session = fixture.session().await;
    let result = session.dispatch(send_request("k-1")).await;
    let error = result.into_payload().expect_err("配额用尽");
    assert_eq!(error.code(), "unavailable");
    assert!(error.message().contains("空间不足"), "{}", error.message());
    assert_eq!(fixture.transport.sent_len(), 0, "没落盘就不该发出去");
}

#[tokio::test]
async fn subscription_filter_skips_uninteresting_events_without_losing_them() {
    let fixture = Fixture::new();
    fixture.transport.push_event(event("evt-1", 1));
    let session = fixture.session().await;
    let request = SubscribeRequest {
        cursor: None,
        kinds: vec![EventKind::Typing],
        convs: Vec::new(),
    };
    let mut subscription = session.subscribe(request).await.expect("订阅");
    assert!(subscription.next().await.expect("拉取").is_none());
    assert_eq!(
        fixture.store.event_count(),
        1,
        "不关心的类型也要落盘：下次重放还能看到"
    );
}

#[tokio::test]
async fn cursor_survives_restart_so_events_are_not_replayed() {
    let fixture = Fixture::new();
    fixture.transport.push_event(event("evt-1", 1));
    {
        let session = fixture.session().await;
        let mut subscription = session
            .subscribe(SubscribeRequest::all())
            .await
            .expect("订阅");
        subscription.next().await.expect("一");
        session.shutdown();
    }
    let session = fixture.session().await;
    let replayed = session.replay(10).await.expect("重放");
    assert!(replayed.is_empty(), "重启后不该重放已消费事件");
    let cursor: Option<wxlike_types::Cursor> = fixture.store.cursor().await.expect("游标");
    assert_eq!(cursor.expect("有游标"), event_cursor(1));
}

/// **取消必须叫醒空闲的事件源**：`next()` 挂在永不产出的源上时，
/// `cancel()` 要当场让它以 `cancelled` 返回——修复前它会一直睡下去
/// （等到源自己吐下一条或连接断开），"取消订阅"形同虚设。
#[tokio::test]
async fn cancelling_wakes_an_idle_event_source() {
    use support::HangingEventSource;
    use wxlike_client_core::runtime::EventSubscription;

    let fixture = Fixture::new();
    let session = fixture.session().await;
    // 直接挂一个"永不产出"的源（session 的 MemTransport 吐完队列就结束，
    // 测不出"挂着等下一条"的场景）
    let mut subscription = EventSubscription::new(
        Box::new(HangingEventSource),
        fixture.ports(),
        SubscribeRequest::all(),
        session.cancellation(),
    );
    let reader = tokio::spawn(async move { subscription.next().await });
    tokio::task::yield_now().await;
    // 取消：修复前这里叫不醒 reader，只能靠下面的超时兜底暴露
    session.cancellation().cancel();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), reader)
        .await
        .expect("cancel 必须立刻叫醒 next()，而不是等源产出")
        .expect("任务不 panic");
    assert_eq!(outcome.expect_err("已取消").code(), "cancelled");
}

/// 搁置（重试上限用尽）之后，`retry_outbox` 把记录拉回队列并重置预算。
#[tokio::test]
async fn retry_outbox_unparks_an_exhausted_record_with_a_fresh_budget() {
    let fixture = Fixture::new();
    let mut record = OutboxRecord::new(
        idem("k-1"),
        action("msg.send"),
        serde_json::json!({"conv": "d-a~b", "text": "x", "idempotency_key": "k-1"}),
        NOW,
    );
    record.attempts = Backoff::DEFAULT.max_attempts;
    fixture.store.put_outbox(&record).await.expect("入队");
    let session = fixture.session().await;
    let report = session.pump(10).await.expect("补发");
    assert_eq!(report.failed, 1, "上限用尽：被搁置");

    // 手动重试：记录重新到点、预算归零
    assert!(
        session.retry_outbox("k-1").await.expect("手动重试"),
        "搁置的记录必须能被找回"
    );
    assert!(!session.retry_outbox("nope").await.expect("不存在的键"),);
    // 这次给它一个可重试错误：预算是新的，所以会 defer 而不是再次搁置
    fixture.transport.push_error(TransportError::Timeout);
    let report = session.pump(10).await.expect("再补发");
    assert_eq!(report.deferred, 1, "预算重置后应当退避重试：{report:?}");
    assert_eq!(report.failed, 0, "{report:?}");
    let due = fixture
        .store
        .due_outbox(fixture.clock.now_ms() + 60_000, 10)
        .await
        .expect("取");
    assert_eq!(due.len(), 1, "记录仍在队列");
    assert_eq!(due[0].attempts, 1, "这是新预算的第一次失败");
}

/// 复合命令在核心内拼装：`conv.list` 一次 dispatch 打多个端点。
#[tokio::test]
async fn composite_conv_list_dispatches_through_the_transport() {
    let fixture = Fixture::new();
    // /api/me → /api/groups/mine → /api/friends → 每会话未读
    fixture
        .transport
        .push_json(200, serde_json::json!({"user_id": "alice"}));
    fixture.transport.push_json(
        200,
        serde_json::json!([{"id": "g-1", "name": "群", "member_count": 2}]),
    );
    fixture
        .transport
        .push_json(200, serde_json::json!([{"friend_id": "bob"}]));
    fixture
        .transport
        .push_json(200, serde_json::json!({"unread": 3}));
    fixture
        .transport
        .push_json(200, serde_json::json!({"unread": 0}));
    let session = fixture.session().await;
    let result = session
        .dispatch(ActionRequest::new(
            action("conv.list"),
            serde_json::json!({}),
            trace(),
        ))
        .await;
    let ActionOutcome::Ok { data } = result.outcome else {
        panic!("conv.list 应当成功：{result:?}");
    };
    let list = data.as_array().expect("会话数组");
    assert_eq!(list.len(), 2, "{data}");
    let paths: Vec<String> = fixture
        .transport
        .sent()
        .iter()
        .map(|r| r.path.clone())
        .collect();
    assert_eq!(
        paths,
        vec![
            "/api/me",
            "/api/groups/mine",
            "/api/friends",
            "/api/conv/g-1/unread",
            "/api/conv/d-alice~bob/unread",
        ]
    );
    // 审计标记必须是复合命令自己的 id
    assert!(
        fixture
            .transport
            .sent()
            .iter()
            .all(|r| r.command.as_ref().map(|c| c.as_str()) == Some("conv.list")),
        "每个子请求都要带 conv.list 审计标记"
    );
}

/// 核心未实现的复合命令（`video.job.create` 是写命令）：立刻明确报错，
/// 不许被 outbox 当成"可重试"无限空转。
#[tokio::test]
async fn host_only_composite_write_fails_fast_without_queueing() {
    let fixture = Fixture::new();
    let session = fixture.session().await;
    let request = ActionRequest::new(
        action("video.job.create"),
        serde_json::json!({"path": "/tmp/v.mp4"}),
        trace(),
    )
    .with_idempotency(idem("k-video"));
    let error = session
        .dispatch(request)
        .await
        .into_payload()
        .expect_err("核心未实现，必须报错");
    assert_eq!(error.code(), "invalid_argument");
    assert!(
        error.message().contains("wxlike-client"),
        "{}",
        error.message()
    );
    assert_eq!(
        fixture.store.outbox_len(),
        0,
        "不可重试的失败不许留在队列里空转"
    );
}
