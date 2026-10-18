//! P4 发送闸门（`DeliveryPolicy::send_gate`）在 hub 帧级的契约：
//! 被拒/被限速的帧**不落库、不暂存、不扇出**；默认实现放行（P3 兼容）。

use std::sync::Arc;
use std::time::Duration;

use wxlike_core::{Envelope, Store};
use wxlike_mq::memory::{MemoryMq, MemoryRoutes};
use wxlike_mq::MqRouter;
use wxlike_net::{run_mq_hub_with, Client, DeliveryPolicy, MqHubOptions, SendGate};

/// 按内容裁决的策略：含 "blocked" 拒、含 "slow" 限速、其余放行。
struct GatePolicy {
    allow_direct: bool,
}

#[async_trait::async_trait]
impl DeliveryPolicy for GatePolicy {
    async fn blocked(&self, _from: &str, _to: &str) -> bool {
        false
    }
    async fn muted(&self, _group: &str, _user: &str) -> bool {
        false
    }
    async fn direct_allowed(&self, _from: &str, _to: &str) -> bool {
        self.allow_direct
    }
    async fn send_gate(&self, _from: &str, _conv: &str, text: &str) -> SendGate {
        if text.contains("blocked") {
            SendGate::Reject {
                reason: "policy test".into(),
            }
        } else if text.contains("slow") {
            SendGate::Throttle {
                retry_after_ms: 500,
            }
        } else {
            SendGate::Allow
        }
    }
}

/// 不覆盖 send_gate 的策略：走默认实现，必须放行。
struct DefaultGatePolicy;

#[async_trait::async_trait]
impl DeliveryPolicy for DefaultGatePolicy {
    async fn blocked(&self, _from: &str, _to: &str) -> bool {
        false
    }
    async fn muted(&self, _group: &str, _user: &str) -> bool {
        false
    }
    async fn direct_allowed(&self, _from: &str, _to: &str) -> bool {
        true
    }
}

async fn hub_with(policy: Arc<dyn DeliveryPolicy>) -> (Arc<Store>, std::net::SocketAddr) {
    let router = MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new());
    let store = Arc::new(Store::open(None).unwrap());
    let hub = run_mq_hub_with(
        "gate-test",
        "127.0.0.1:0".parse().unwrap(),
        router,
        MqHubOptions {
            storage: Some(store.clone()),
            policy: Some(policy),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let addr = hub.local_addr().unwrap();
    std::mem::forget(hub);
    (store, addr)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejected_and_throttled_frames_leave_no_trace() {
    let (store, addr) = hub_with(Arc::new(GatePolicy { allow_direct: true })).await;
    let _alice = Client::connect(addr, "localhost", "alice").await.unwrap();
    let bob = Client::connect(addr, "localhost", "bob").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    bob.send(&Envelope::direct(
        "d-alice~bob",
        "bob",
        vec!["alice".into()],
        1,
        "this is blocked content",
    ))
    .await
    .unwrap();
    bob.send(&Envelope::direct(
        "d-alice~bob",
        "bob",
        vec!["alice".into()],
        2,
        "please slow down",
    ))
    .await
    .unwrap();
    bob.send(&Envelope::direct(
        "d-alice~bob",
        "bob",
        vec!["alice".into()],
        3,
        "normal message",
    ))
    .await
    .unwrap();

    tokio::time::sleep(Duration::from_millis(400)).await;

    let rows = store.list_messages("d-alice~bob").unwrap();
    assert_eq!(rows.len(), 1, "只有放行的那条落库: {rows:?}");
    let text = match &rows[0].envelope.body {
        wxlike_core::MsgBody::Text(t) => t.clone(),
        other => panic!("unexpected body {other:?}"),
    };
    assert_eq!(text, "normal message");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn default_gate_allows_everything() {
    let (store, addr) = hub_with(Arc::new(DefaultGatePolicy)).await;
    let _alice = Client::connect(addr, "localhost", "alice").await.unwrap();
    let bob = Client::connect(addr, "localhost", "bob").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    bob.send(&Envelope::direct(
        "d-alice~bob",
        "bob",
        vec!["alice".into()],
        1,
        "this is blocked content",
    ))
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let rows = store.list_messages("d-alice~bob").unwrap();
    assert_eq!(rows.len(), 1, "默认实现必须放行（P3 行为不变）");
}
