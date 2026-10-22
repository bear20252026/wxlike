//! P3 投递策略（DeliveryPolicy）契约测试：拉黑/禁言/陌生人闸门在 hub 帧级
//! 生效；系统帧（Notice）豁免；无策略时零成本直通。
//!
//! 这里的 StaticPolicy 是 trait 的最小实现（net 层无关系表），真实平台链路
//! 由 `server/tests/policy_integration.rs` 用 Arc<Platform> 端到端覆盖。

use std::collections::HashSet;
use std::time::Duration;

use wxlike_core::{Envelope, MsgBody, Store};
use wxlike_mq::memory::{MemoryMq, MemoryRoutes};
use wxlike_mq::MqRouter;
use wxlike_net::{run_mq_hub_with, Client, DeliveryPolicy, Membership, MqHubOptions};

/// 静态成员表：g-muted = loud/boss/fan。
struct StaticMembership;

#[async_trait::async_trait]
impl Membership for StaticMembership {
    async fn members_of(&self, conv_id: &str) -> anyhow::Result<Option<Vec<String>>> {
        Ok(match conv_id {
            "g-muted" => Some(vec!["loud".into(), "boss".into(), "fan".into()]),
            _ => None,
        })
    }
}

#[derive(Default, Clone)]
struct StaticPolicy {
    blocked: HashSet<(String, String)>,
    muted: HashSet<(String, String)>,
    direct_allowed: HashSet<(String, String)>,
}

impl StaticPolicy {
    fn allow_direct(&mut self, from: &str, to: &str) {
        self.direct_allowed.insert((from.into(), to.into()));
    }
}

#[async_trait::async_trait]
impl DeliveryPolicy for StaticPolicy {
    async fn blocked(&self, from: &str, to: &str) -> bool {
        self.blocked.contains(&(from.into(), to.into()))
    }
    async fn muted(&self, group: &str, user: &str) -> bool {
        self.muted.contains(&(group.into(), user.into()))
    }
    async fn direct_allowed(&self, from: &str, to: &str) -> bool {
        self.direct_allowed.contains(&(from.into(), to.into()))
    }
}

async fn hub_with(
    policy: StaticPolicy,
) -> (
    std::sync::Arc<Store>,
    std::net::SocketAddr,
    wxlike_net::HubHandle,
) {
    let router = MqRouter::new(std::sync::Arc::new(MemoryMq::new()), MemoryRoutes::new());
    let store = std::sync::Arc::new(Store::open(None).unwrap());
    let hub = run_mq_hub_with(
        "policy-test",
        "127.0.0.1:0".parse().unwrap(),
        router,
        MqHubOptions {
            // 测试用 StaticAuth：token 原样当 user_id（生产走 session 表）。
            // 不注入的话 hub 会 fail closed 拒绝连接——这是 issue #3 的默认值。
            auth: Some(std::sync::Arc::new(wxlike_net::auth::StaticAuth)),
            storage: Some(store.clone()),
            policy: Some(std::sync::Arc::new(policy)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let addr = hub.local_addr().unwrap();
    let handle = hub.handle();
    std::mem::forget(hub);
    (store, addr, handle)
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(300)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blocked_direct_frame_is_dropped_without_trace() {
    let mut policy = StaticPolicy::default();
    policy.blocked.insert(("mallory".into(), "alice".into()));
    policy.allow_direct("bob", "alice"); // bob 过陌生人闸门；mallory 被 blocked 短路
    let (store, addr, _handle) = hub_with(policy).await;

    let _alice = Client::connect(addr, "localhost", "alice").await.unwrap();
    let bob = Client::connect(addr, "localhost", "bob").await.unwrap();
    let mallory = Client::connect(addr, "localhost", "mallory").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // mallory（被 alice 拉黑）直发 → 丢弃
    mallory
        .send(&Envelope::direct(
            "d-alice~mallory",
            "mallory",
            vec!["alice".into()],
            1,
            "骚扰",
        ))
        .await
        .unwrap();
    // bob（未拉黑）直发 → 正常
    bob.send(&Envelope::direct(
        "d-alice~bob",
        "bob",
        vec!["alice".into()],
        1,
        "正常",
    ))
    .await
    .unwrap();

    settle().await;

    // 被拦的帧不落库（无痕迹）
    let rows = store.list_messages("d-alice~mallory").unwrap();
    assert!(rows.is_empty(), "blocked frame must not persist: {rows:?}");
    let rows = store.list_messages("d-alice~bob").unwrap();
    assert_eq!(rows.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stranger_gate_requires_acceptance() {
    let (store, addr, _handle) = hub_with(StaticPolicy::default()).await; // 默认全拒
    let _alice = Client::connect(addr, "localhost", "alice").await.unwrap();
    let stranger = Client::connect(addr, "localhost", "stranger")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // 非好友且未接受 → 丢弃（不变式 9 的投递面）
    stranger
        .send(&Envelope::direct(
            "d-alice~stranger",
            "stranger",
            vec!["alice".into()],
            1,
            "骚扰",
        ))
        .await
        .unwrap();
    settle().await;
    assert!(store.list_messages("d-alice~stranger").unwrap().is_empty());

    // 接受陌生人请求后（策略放行）→ 同一对直发可达
    let mut policy_with_accept = StaticPolicy::default();
    policy_with_accept.allow_direct("stranger", "alice");
    let _ = policy_with_accept; // 策略属 hub；此处以新 hub 表达"状态变化后"
    let (store2, addr2, _handle2) = hub_with(policy_with_accept).await;
    let alice2 = Client::connect(addr2, "localhost", "alice").await.unwrap();
    let stranger2 = Client::connect(addr2, "localhost", "stranger")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    stranger2
        .send(&Envelope::direct(
            "d-alice~stranger",
            "stranger",
            vec!["alice".into()],
            1,
            "已接受",
        ))
        .await
        .unwrap();
    let env = tokio::time::timeout(Duration::from_secs(15), alice2.next_envelope())
        .await
        .expect("accepted stranger delivers")
        .unwrap();
    assert!(matches!(&env.body, MsgBody::Text(t) if t == "已接受"));
    assert_eq!(store2.list_messages("d-alice~stranger").unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn muted_group_member_frames_are_dropped_managers_pass() {
    let mut policy = StaticPolicy::default();
    policy.muted.insert(("g-muted".into(), "loud".into()));
    let (store, _addr, _handle) = hub_with(policy).await;

    // 需要 membership Some 才进群帧分支：hub 带 StaticMembership
    let router = MqRouter::new(std::sync::Arc::new(MemoryMq::new()), MemoryRoutes::new());
    let hub2 = wxlike_net::run_mq_hub_with(
        "policy-muted",
        "127.0.0.1:0".parse().unwrap(),
        router,
        MqHubOptions {
            // 测试用 StaticAuth：token 原样当 user_id（生产走 session 表）。
            // 不注入的话 hub 会 fail closed 拒绝连接——这是 issue #3 的默认值。
            auth: Some(std::sync::Arc::new(wxlike_net::auth::StaticAuth)),
            storage: Some(store.clone()),
            policy: Some(std::sync::Arc::new(StaticPolicyMuted)),
            membership: Some(std::sync::Arc::new(StaticMembership)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let addr2 = hub2.local_addr().unwrap();
    std::mem::forget(hub2);

    let loud = Client::connect(addr2, "localhost", "loud").await.unwrap();
    let boss = Client::connect(addr2, "localhost", "boss").await.unwrap();
    let fan = Client::connect(addr2, "localhost", "fan").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // 被禁言者的群帧：丢弃（不落库、不扇出）
    loud.send(&Envelope::text("g-muted", "loud", 1, "插话"))
        .await
        .unwrap();
    // 管理者同帧形状：正常扇出给其他成员
    boss.send(&Envelope::text("g-muted", "boss", 2, "正常发言"))
        .await
        .unwrap();

    let env = tokio::time::timeout(Duration::from_secs(15), fan.next_envelope())
        .await
        .expect("fan alive")
        .unwrap();
    assert!(
        matches!(&env.body, MsgBody::Text(t) if t == "正常发言"),
        "fan 只收到管理者的帧: {env:?}"
    );
    settle().await;
    let rows = store.list_messages("g-muted").unwrap();
    assert_eq!(rows.len(), 1, "muted frame not persisted: {rows:?}");
    assert_eq!(rows[0].envelope.sender, "boss");
}

/// muted 专用静态策略（与 hub_with 的实例解耦，便于群帧测试精确控制）。
#[derive(Default, Clone)]
struct StaticPolicyMuted;

#[async_trait::async_trait]
impl DeliveryPolicy for StaticPolicyMuted {
    async fn blocked(&self, _from: &str, _to: &str) -> bool {
        false
    }
    async fn muted(&self, group: &str, user: &str) -> bool {
        group == "g-muted" && user == "loud"
    }
    async fn direct_allowed(&self, _from: &str, _to: &str) -> bool {
        true
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_server_can_send_system_notices() {
    let mut policy = StaticPolicy::default();
    policy.blocked.insert(("mallory".into(), "alice".into()));
    policy.allow_direct("bob", "alice"); // bob 过陌生人闸门；mallory 被 blocked 短路
    let (store, addr, handle) = hub_with(policy).await;

    let alice = Client::connect(addr, "localhost", "alice").await.unwrap();
    let mallory = Client::connect(addr, "localhost", "mallory").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // 1) **用户伪造系统帧**必须被拒（issue #13）：以前 Notice 既跳过 send_gate
    //    也跳过关系过滤，等于谁都能往别人界面上写"系统通知"。
    let mut forged = Envelope::with_body(
        "d-alice~mallory",
        "mallory",
        vec!["alice".into()],
        1,
        MsgBody::Notice("你已被管理员移出群聊".into()),
    );
    forged.kind = wxlike_core::MsgKind::System;
    mallory.send(&forged).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(400), alice.next_envelope())
            .await
            .is_err(),
        "用户发的系统通知不能送达"
    );
    assert!(
        store.list_messages("d-alice~mallory").unwrap().is_empty(),
        "伪造的系统帧不该落库"
    );

    // 2) **服务端内部入口**（`HubHandle::publish`）仍然能送系统通知——而且
    //    不受"用户之间的拉黑"影响，否则"撤销拉黑"这类信号自己也送不出去。
    let mut real = Envelope::with_body(
        "d-alice~mallory",
        "mallory",
        vec!["alice".into()],
        2,
        MsgBody::Notice("你们已成为好友".into()),
    );
    real.kind = wxlike_core::MsgKind::System;
    handle.publish(&real).await.unwrap();
    let env = tokio::time::timeout(Duration::from_secs(5), alice.next_envelope())
        .await
        .expect("服务端系统通知必须送达")
        .unwrap();
    assert!(matches!(env.body, MsgBody::Notice(_)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_policy_keeps_everything_delivering() {
    let router = MqRouter::new(std::sync::Arc::new(MemoryMq::new()), MemoryRoutes::new());
    let store = std::sync::Arc::new(Store::open(None).unwrap());
    let hub = run_mq_hub_with(
        "no-policy",
        "127.0.0.1:0".parse().unwrap(),
        router,
        MqHubOptions {
            // 测试用 StaticAuth：token 原样当 user_id（生产走 session 表）。
            // 不注入的话 hub 会 fail closed 拒绝连接——这是 issue #3 的默认值。
            auth: Some(std::sync::Arc::new(wxlike_net::auth::StaticAuth)),
            storage: Some(store.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let addr = hub.local_addr().unwrap();
    std::mem::forget(hub);

    let alice = Client::connect(addr, "localhost", "alice").await.unwrap();
    let stranger = Client::connect(addr, "localhost", "stranger")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    stranger
        .send(&Envelope::direct(
            "d-alice~stranger",
            "stranger",
            vec!["alice".into()],
            1,
            "无策略直发",
        ))
        .await
        .unwrap();
    let env = tokio::time::timeout(Duration::from_secs(15), alice.next_envelope())
        .await
        .expect("policy None = legacy behavior")
        .unwrap();
    assert!(matches!(&env.body, MsgBody::Text(t) if t == "无策略直发"));
}
