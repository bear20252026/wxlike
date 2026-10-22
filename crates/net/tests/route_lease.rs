//! 路由租约续租（issue #20 / A18）。
//!
//! 路由表以前是"用户 → hub"的永久映射：hub 进程崩了没法执行 unbind，
//! 于是**所有**曾经连过的用户永远路由到那台死掉的机器上，消息发进黑洞。
//!
//! 现在带租约的路由表（`RedisRoutes::with_lease`）会给每条路由设 TTL，
//! hub 每隔 1/3 租约续一次；续租停止 = 路由过期 = 调用方重路由或落离线队列。
//!
//! 这条用例不需要 Redis：它验的是 **hub 那一侧的循环真的在跑、真的只给
//! 还连着的用户续**。Redis 侧的行为由 `--ignored` 的真机用例覆盖
//! （`crates/mq/tests/streams_redis.rs`，需要 `REDIS_URL`）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use wxlike_core::Envelope;
use wxlike_mq::memory::{MemoryMq, MemoryRoutes};
use wxlike_mq::RouteTable;
use wxlike_net::{run_mq_hub_with, Client, MqHubOptions};

/// 包一层 `MemoryRoutes`，只为了数续租次数。
struct CountingRoutes {
    inner: Arc<MemoryRoutes>,
    renewals: AtomicUsize,
    last_user: std::sync::Mutex<String>,
}

impl CountingRoutes {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: MemoryRoutes::new(),
            renewals: AtomicUsize::new(0),
            last_user: std::sync::Mutex::new(String::new()),
        })
    }
}

#[async_trait]
impl RouteTable for CountingRoutes {
    async fn bind_user(&self, user: &str, hub_id: &str) -> anyhow::Result<()> {
        self.inner.bind_user(user, hub_id).await
    }

    async fn unbind_user(&self, user: &str, hub_id: &str) -> anyhow::Result<()> {
        self.inner.unbind_user(user, hub_id).await
    }

    async fn route_of(&self, user: &str) -> anyhow::Result<Option<String>> {
        self.inner.route_of(user).await
    }

    async fn register_hub(&self, hub_id: &str) -> anyhow::Result<()> {
        self.inner.register_hub(hub_id).await
    }

    async fn unregister_hub(&self, hub_id: &str) -> anyhow::Result<()> {
        self.inner.unregister_hub(hub_id).await
    }

    async fn list_hubs(&self) -> anyhow::Result<Vec<String>> {
        self.inner.list_hubs().await
    }

    async fn renew_user(&self, user: &str, hub_id: &str) -> anyhow::Result<()> {
        self.renewals.fetch_add(1, Ordering::SeqCst);
        *self.last_user.lock().unwrap_or_else(|e| e.into_inner()) = user.to_string();
        self.inner.bind_user(user, hub_id).await
    }

    /// 100ms 租约 → 续租周期约 33ms，测试不用等太久。
    fn lease_ttl_ms(&self) -> u64 {
        100
    }
}

/// hub 必须给**还连着的**用户续租；断开之后就停。
#[tokio::test]
async fn the_hub_renews_routes_only_while_the_user_is_connected() {
    let routes = CountingRoutes::new();
    let hub = run_mq_hub_with(
        "lease-hub",
        "127.0.0.1:0".parse().unwrap(),
        wxlike_mq::MqRouter::new(Arc::new(MemoryMq::new()), routes.clone()),
        MqHubOptions {
            auth: Some(Arc::new(wxlike_net::auth::StaticAuth)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let addr = hub.local_addr().unwrap();
    std::mem::forget(hub);

    let alice = Client::connect(addr, "localhost", "alice").await.unwrap();
    // 等绑定 + 至少一个续租周期
    tokio::time::sleep(Duration::from_millis(250)).await;

    let first = routes.renewals.load(Ordering::SeqCst);
    assert!(first > 0, "连着的时候必须续租，否则路由会过期");
    assert_eq!(
        routes
            .last_user
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_str(),
        "alice",
        "续租的必须是本 hub 上那个用户"
    );

    // 断开之后不再有新的续租
    drop(alice);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let after = routes.renewals.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        routes.renewals.load(Ordering::SeqCst),
        after,
        "断开之后还在续租，等于给一个不在线的用户续命"
    );
}

/// 没有租约的路由表（内存表）不该起续租循环——`lease_ttl_ms() == 0` 时
/// hub 连定时器都不建。这里用普通 `MemoryRoutes` 跑一遍，确认行为不变。
#[tokio::test]
async fn a_routeless_lease_hub_still_delivers() {
    let hub = run_mq_hub_with(
        "no-lease-hub",
        "127.0.0.1:0".parse().unwrap(),
        wxlike_mq::MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new()),
        MqHubOptions {
            auth: Some(Arc::new(wxlike_net::auth::StaticAuth)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let addr = hub.local_addr().unwrap();
    std::mem::forget(hub);

    let alice = Client::connect(addr, "localhost", "alice").await.unwrap();
    let bob = Client::connect(addr, "localhost", "bob").await.unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;

    let msg = Envelope::direct("d-alice~bob", "alice", vec!["bob".into()], 1, "still works");
    alice.send(&msg).await.unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), bob.next_envelope())
        .await
        .expect("没有租约的表照常投递")
        .unwrap();
    assert_eq!(got.msg_id, msg.msg_id);
}
