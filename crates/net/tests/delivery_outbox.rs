//! 投递可靠性（issue #9 / #11）：确认只在**真的交出去**之后发生，
//! 同一条 msg_id 的重试不会重复投给已收到的人，但会补齐没收到的人。
//!
//! 这里的故障都是注入出来的，不是"看起来应该没问题"：
//! - 收件人不在线 → 必须落他的离线队列（durable inbox），不能只剩源端归档；
//! - 落队列失败 → 投递任务保持 pending，同 ID 重试时补上；
//! - 重试成功之后，已经收到的人**一条都不该再收**。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use wxlike_core::{Envelope, Store};
use wxlike_mq::{memory::MemoryMq, memory::MemoryRoutes, MqRouter, RouteTable as _};
use wxlike_net::offline::{MemoryOffline, OfflineStore};
use wxlike_net::{run_mq_hub_with, Client, MqHubOptions};

fn router(mq: Arc<MemoryMq>, routes: Arc<MemoryRoutes>) -> MqRouter {
    MqRouter::new(mq, routes)
}

/// 一个会"坏一次"的离线队列：第一次 enqueue 失败，之后正常。
///
/// 用来模拟 #11 说的"临时错误"——它不该变成永久的"已处理"。
struct FlakyOffline {
    inner: Arc<MemoryOffline>,
    fail_next: AtomicBool,
    calls: AtomicUsize,
}

impl FlakyOffline {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: MemoryOffline::new(100),
            fail_next: AtomicBool::new(true),
            calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait::async_trait]
impl OfflineStore for FlakyOffline {
    async fn enqueue(&self, user: &str, frame: Vec<u8>) -> anyhow::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_next.swap(false, Ordering::SeqCst) {
            anyhow::bail!("injected: disk full");
        }
        self.inner.enqueue(user, frame).await
    }

    async fn claim(&self, user: &str, limit: usize) -> anyhow::Result<Vec<(i64, Vec<u8>)>> {
        self.inner.claim(user, limit).await
    }

    async fn ack(&self, id: i64) -> anyhow::Result<()> {
        self.inner.ack(id).await
    }

    async fn release(&self, id: i64) -> anyhow::Result<()> {
        self.inner.release(id).await
    }

    async fn depth(&self, user: &str) -> anyhow::Result<usize> {
        self.inner.depth(user).await
    }
}

async fn connect(addr: std::net::SocketAddr, user: &str) -> Client {
    Client::connect(addr, "localhost", user).await.unwrap()
}

async fn drain(client: &Client, within: Duration) -> Vec<Envelope> {
    let mut out = Vec::new();
    loop {
        match tokio::time::timeout(within, client.next_envelope()).await {
            Ok(Ok(env)) => out.push(env),
            _ => return out,
        }
    }
}

/// #9：收件人的路由指向本 hub、但他根本没连着——消息必须落进离线队列，
/// 而不是被 ack 掉之后消失。
#[tokio::test]
async fn offline_recipient_gets_a_durable_copy_instead_of_a_lost_ack() {
    let store = Arc::new(Store::open(None).unwrap());
    let offline = MemoryOffline::new(100);
    let hub = run_mq_hub_with(
        "hub-1",
        "127.0.0.1:0".parse().unwrap(),
        router(Arc::new(MemoryMq::new()), MemoryRoutes::new()),
        MqHubOptions {
            auth: Some(Arc::new(wxlike_net::auth::StaticAuth)),
            storage: Some(store.clone()),
            offline: offline.clone(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let addr = hub.local_addr().unwrap();
    std::mem::forget(hub);

    let alice = connect(addr, "alice").await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let msg = Envelope::direct("dm", "alice", vec!["bob".into()], 1, "for offline bob");
    alice.send(&msg).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;

    // bob 从没连过：路由里没有他 → 落他的离线队列（durable）
    assert_eq!(
        offline.depth("bob").await.unwrap(),
        1,
        "不在线的收件人必须有持久副本"
    );
    // 投递任务记成 delivered（已经交给离线队列了）
    assert!(
        store
            .pending_deliveries(&hex::encode(msg.msg_id))
            .unwrap()
            .is_empty(),
        "交给离线队列就算投递完成"
    );

    // 他连上来之后能拿到（claim/ack 走的是 #10 那条路径）
    let bob = connect(addr, "bob").await;
    let got = drain(&bob, Duration::from_millis(1_500)).await;
    assert_eq!(got.len(), 1, "离线队列里的那条要送到");
    assert_eq!(got[0].msg_id, msg.msg_id);
}

/// #11：落队列失败 → 投递任务留在 pending；同 ID 重试时补投，
/// 但已经收到的人一条都不再收。
#[tokio::test]
async fn replay_fills_the_missing_recipient_without_duplicating() {
    let store = Arc::new(Store::open(None).unwrap());
    let offline = FlakyOffline::new();
    let hub = run_mq_hub_with(
        "hub-1",
        "127.0.0.1:0".parse().unwrap(),
        router(Arc::new(MemoryMq::new()), MemoryRoutes::new()),
        MqHubOptions {
            auth: Some(Arc::new(wxlike_net::auth::StaticAuth)),
            storage: Some(store.clone()),
            offline: offline.clone(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let addr = hub.local_addr().unwrap();
    std::mem::forget(hub);

    let alice = connect(addr, "alice").await;
    let bob = connect(addr, "bob").await; // 在线
    tokio::time::sleep(Duration::from_millis(250)).await;

    let msg = Envelope::direct(
        "dm-group",
        "alice",
        vec!["bob".into(), "carol".into()],
        1,
        "two recipients",
    );
    alice.send(&msg).await.unwrap();

    // bob 收到；carol 的落队列被注入失败
    let first = drain(&bob, Duration::from_millis(800)).await;
    assert_eq!(first.len(), 1, "在线的收件人先收到");
    assert_eq!(offline.depth("carol").await.unwrap(), 0, "第一次落盘失败");

    let key = hex::encode(msg.msg_id);
    let pending = store.pending_deliveries(&key).unwrap();
    assert_eq!(pending, vec!["carol".to_string()], "只有 carol 还没投出去");

    // 客户端用**同一个 msg_id** 重试（幂等重发）
    for _ in 0..3 {
        alice.send(&msg).await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    // bob 不该再收到任何东西
    let again = drain(&bob, Duration::from_millis(600)).await;
    assert!(
        again.is_empty(),
        "已投递的收件人不能被重放再打一遍：{} 条",
        again.len()
    );
    // carol 的那条被补上了，而且只有一条
    assert_eq!(
        offline.depth("carol").await.unwrap(),
        1,
        "重试要补齐没收到的人，且只补一次"
    );
    assert!(store.pending_deliveries(&key).unwrap().is_empty());
    // 归档仍然只有一条
    let rows = store.list_messages("dm-group").unwrap();
    assert_eq!(rows.len(), 1, "重放不会重复落消息");
}

/// #9 跨节点：路由说人在 hub-b，但他此刻不在 hub-b 上——hub-b 消费到这一帧
/// 之后必须落进**自己的**离线队列（而不是当成"广播没人听"直接 ack）。
#[tokio::test]
async fn cross_hub_frame_for_an_absent_user_is_parked_not_dropped() {
    let mq = Arc::new(MemoryMq::new());
    let routes = MemoryRoutes::new();
    let offline_a = MemoryOffline::new(100);
    let offline_b = MemoryOffline::new(100);

    let hub_a = run_mq_hub_with(
        "hub-a",
        "127.0.0.1:0".parse().unwrap(),
        router(mq.clone(), routes.clone()),
        MqHubOptions {
            auth: Some(Arc::new(wxlike_net::auth::StaticAuth)),
            offline: offline_a.clone(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let hub_b = run_mq_hub_with(
        "hub-b",
        "127.0.0.1:0".parse().unwrap(),
        router(mq, routes.clone()),
        MqHubOptions {
            auth: Some(Arc::new(wxlike_net::auth::StaticAuth)),
            offline: offline_b.clone(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let addr_a = hub_a.local_addr().unwrap();
    std::mem::forget(hub_a);
    std::mem::forget(hub_b);

    // 路由表说 bob 在 hub-b，但他并没有连着（连接刚断/路由还没过期）
    routes.bind_user("bob", "hub-b").await.unwrap();

    let alice = connect(addr_a, "alice").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let msg = Envelope::direct("dm", "alice", vec!["bob".into()], 1, "cross hub, bob away");
    alice.send(&msg).await.unwrap();

    // 给 hub-b 的消费循环一点时间
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        offline_b.depth("bob").await.unwrap(),
        1,
        "目标 hub 上没人连着时，必须落他的离线队列"
    );
    assert_eq!(
        offline_a.depth("bob").await.unwrap(),
        0,
        "不该在源 hub 上重复存一份"
    );
}
