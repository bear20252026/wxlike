//! WP-04 A2 + A4 at the hub: group messages are delivered by the member table,
//! across hubs, with @mentions marked only for the people named, and a
//! non-member's frame dropped before it is ever stored.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use wxlike_core::types::{custom_kind, MsgBody, MsgKind};
use wxlike_core::{Envelope, Store};
use wxlike_mq::memory::{MemoryMq, MemoryRoutes};
use wxlike_mq::MqRouter;
use wxlike_net::{run_mq_hub_with, Client, Membership, MqHubOptions};

/// Test double for the platform's group table.
struct Stub {
    groups: RwLock<HashMap<String, Vec<String>>>,
}

impl Stub {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            groups: RwLock::new(HashMap::new()),
        })
    }

    async fn set(&self, group: &str, members: &[&str]) {
        self.groups.write().await.insert(
            group.to_string(),
            members.iter().map(|m| m.to_string()).collect(),
        );
    }
}

#[async_trait::async_trait]
impl Membership for Stub {
    async fn members_of(&self, conv_id: &str) -> anyhow::Result<Option<Vec<String>>> {
        Ok(self.groups.read().await.get(conv_id).cloned())
    }
}

async fn drain(client: &Client, within: Duration) -> Vec<Envelope> {
    let mut out = Vec::new();
    while let Ok(Ok(env)) = tokio::time::timeout(within, client.next_envelope()).await {
        out.push(env);
    }
    out
}

async fn spawn_hub(
    hub_id: &str,
    mq: Arc<MemoryMq>,
    routes: Arc<wxlike_mq::memory::MemoryRoutes>,
    membership: Arc<Stub>,
) -> wxlike_net::MqHub {
    run_mq_hub_with(
        hub_id,
        "127.0.0.1:0".parse().unwrap(),
        MqRouter::new(mq, routes),
        MqHubOptions {
            // 测试用 StaticAuth：token 原样当 user_id（生产走 session 表）。
            // 不注入的话 hub 会 fail closed 拒绝连接——这是 issue #3 的默认值。
            auth: Some(std::sync::Arc::new(wxlike_net::auth::StaticAuth)),
            membership: Some(membership),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

/// A2: alice on hub-a, bob and carol on hub-b, one group. alice's message has
/// to reach both peers and must not echo back to her.
#[tokio::test]
async fn three_users_across_two_hubs() {
    let routes = MemoryRoutes::new();
    let mq: Arc<MemoryMq> = Arc::new(MemoryMq::new());
    let membership = Stub::new();
    membership.set("g1", &["alice", "bob", "carol"]).await;

    let hub_a = spawn_hub("hub-a", mq.clone(), routes.clone(), membership.clone()).await;
    let hub_b = spawn_hub("hub-b", mq.clone(), routes.clone(), membership).await;
    let addr_a = hub_a.local_addr().unwrap();
    let addr_b = hub_b.local_addr().unwrap();

    let alice = Client::connect(addr_a, "localhost", "alice").await.unwrap();
    let bob = Client::connect(addr_b, "localhost", "bob").await.unwrap();
    let carol = Client::connect(addr_b, "localhost", "carol").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // group message: conv_id is the group, `to` is empty (membership decides)
    alice
        .send(&Envelope::text("g1", "alice", 1, "hello group"))
        .await
        .unwrap();

    for (who, client) in [("bob", &bob), ("carol", &carol)] {
        let got = tokio::time::timeout(Duration::from_secs(5), client.next_envelope())
            .await
            .unwrap_or_else(|_| panic!("{who} should get the group message"))
            .unwrap();
        assert_eq!(got.sender, "alice");
        assert_eq!(got.conv_id, "g1");
        assert_eq!(got.body, MsgBody::Text("hello group".into()));
        assert_eq!(got.to, vec![who.to_string()], "delivered as a direct frame");
    }

    // the sender gets nothing back
    assert!(
        tokio::time::timeout(Duration::from_millis(700), alice.next_envelope())
            .await
            .is_err(),
        "no echo to the sender"
    );
}

/// A4: someone outside the group must not be able to post into it, and the
/// frame must not reach storage.
#[tokio::test]
async fn outsider_message_is_dropped_before_storing() {
    let store = Arc::new(Store::open(None).unwrap());
    let membership = Stub::new();
    membership.set("g2", &["alice", "bob"]).await;

    let hub = run_mq_hub_with(
        "hub-1",
        "127.0.0.1:0".parse().unwrap(),
        MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new()),
        MqHubOptions {
            // 测试用 StaticAuth：token 原样当 user_id（生产走 session 表）。
            // 不注入的话 hub 会 fail closed 拒绝连接——这是 issue #3 的默认值。
            auth: Some(std::sync::Arc::new(wxlike_net::auth::StaticAuth)),
            storage: Some(store.clone()),
            membership: Some(membership),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let addr = hub.local_addr().unwrap();

    let alice = Client::connect(addr, "localhost", "alice").await.unwrap();
    let mallory = Client::connect(addr, "localhost", "mallory").await.unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;

    mallory
        .send(&Envelope::text("g2", "mallory", 1, "injected"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert!(
        drain(&alice, Duration::from_millis(500)).await.is_empty(),
        "nothing delivered to members"
    );
    assert!(
        store.list_messages("g2").unwrap().is_empty(),
        "the rejected frame was not stored either"
    );

    // a real member's message does land
    alice
        .send(&Envelope::text("g2", "alice", 2, "legitimate"))
        .await
        .unwrap();
    let got = drain(&mallory, Duration::from_millis(900)).await;
    assert!(got.is_empty(), "mallory is not a member, still nothing");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let rows = store.list_messages("g2").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].envelope.sender, "alice");
}

/// An offline member is parked and catches up on connect.
#[tokio::test]
async fn offline_member_is_parked_then_flushed() {
    let membership = Stub::new();
    membership.set("g3", &["alice", "bob", "carol"]).await;

    let hub = run_mq_hub_with(
        "hub-1",
        "127.0.0.1:0".parse().unwrap(),
        MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new()),
        MqHubOptions {
            // 测试用 StaticAuth：token 原样当 user_id（生产走 session 表）。
            // 不注入的话 hub 会 fail closed 拒绝连接——这是 issue #3 的默认值。
            auth: Some(std::sync::Arc::new(wxlike_net::auth::StaticAuth)),
            membership: Some(membership),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let addr = hub.local_addr().unwrap();

    let alice = Client::connect(addr, "localhost", "alice").await.unwrap();
    let bob = Client::connect(addr, "localhost", "bob").await.unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;

    // carol has not connected at all
    alice
        .send(&Envelope::text("g3", "alice", 1, "for the group"))
        .await
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), bob.next_envelope())
        .await
        .expect("online member gets it live")
        .unwrap();
    assert_eq!(got.conv_id, "g3");

    let carol = Client::connect(addr, "localhost", "carol").await.unwrap();
    let parked = tokio::time::timeout(Duration::from_secs(5), carol.next_envelope())
        .await
        .expect("offline member receives what was parked")
        .unwrap();
    assert_eq!(parked.body, MsgBody::Text("for the group".into()));
    assert_eq!(parked.sender, "alice");
}

/// A1: only the people named by an @mention see the marker; everyone else in
/// the group gets the plain text.
#[tokio::test]
async fn mention_marker_reaches_only_the_named_members() {
    let membership = Stub::new();
    membership.set("g4", &["alice", "bob", "carol"]).await;

    let hub = run_mq_hub_with(
        "hub-1",
        "127.0.0.1:0".parse().unwrap(),
        MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new()),
        MqHubOptions {
            // 测试用 StaticAuth：token 原样当 user_id（生产走 session 表）。
            // 不注入的话 hub 会 fail closed 拒绝连接——这是 issue #3 的默认值。
            auth: Some(std::sync::Arc::new(wxlike_net::auth::StaticAuth)),
            membership: Some(membership),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let addr = hub.local_addr().unwrap();
    let alice = Client::connect(addr, "localhost", "alice").await.unwrap();
    let bob = Client::connect(addr, "localhost", "bob").await.unwrap();
    let carol = Client::connect(addr, "localhost", "carol").await.unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;

    alice
        .send(&Envelope::with_body(
            "g4",
            "alice",
            vec![],
            1,
            MsgBody::Custom {
                kind: custom_kind::MENTION.to_string(),
                payload: serde_json::json!({
                    "text": "standup in 5",
                    "user_ids": ["bob"],
                }),
            },
        ))
        .await
        .unwrap();

    let to_bob = drain(&bob, Duration::from_millis(900)).await;
    assert_eq!(to_bob.len(), 1);
    match &to_bob[0].body {
        MsgBody::Custom { kind, payload } => {
            assert_eq!(kind, "mention", "named member keeps the marker");
            assert_eq!(payload["text"], "standup in 5");
        }
        other => panic!("bob should get the marked form, got {other:?}"),
    }

    let to_carol = drain(&carol, Duration::from_millis(900)).await;
    assert_eq!(to_carol.len(), 1);
    assert_eq!(
        to_carol[0].body,
        MsgBody::Text("standup in 5".into()),
        "unnamed member gets plain text"
    );
    assert_eq!(
        to_carol[0].msg_id, to_bob[0].msg_id,
        "same underlying message"
    );

    // a group text that is not a mention goes to everyone unchanged
    alice
        .send(&Envelope::text("g4", "alice", 2, "plain group text"))
        .await
        .unwrap();
    for client in [&bob, &carol] {
        let got = drain(client, Duration::from_millis(700)).await;
        assert!(
            got.iter()
                .any(|e| e.body == MsgBody::Text("plain group text".into())),
            "everyone in the group sees ordinary messages"
        );
    }
    assert!(
        drain(&alice, Duration::from_millis(400)).await.is_empty(),
        "no echo"
    );
}

/// Group ops still travel as system envelopes through the same fan-out.
#[tokio::test]
async fn system_envelope_follows_the_member_table() {
    let membership = Stub::new();
    membership.set("g5", &["alice", "bob", "carol"]).await;
    let hub = run_mq_hub_with(
        "hub-1",
        "127.0.0.1:0".parse().unwrap(),
        MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new()),
        MqHubOptions {
            // 测试用 StaticAuth：token 原样当 user_id（生产走 session 表）。
            // 不注入的话 hub 会 fail closed 拒绝连接——这是 issue #3 的默认值。
            auth: Some(std::sync::Arc::new(wxlike_net::auth::StaticAuth)),
            membership: Some(membership),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let addr = hub.local_addr().unwrap();
    let bob = Client::connect(addr, "localhost", "bob").await.unwrap();
    let carol = Client::connect(addr, "localhost", "carol").await.unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;

    // what the platform emits when someone creates/joins a group
    hub.handle()
        .publish(&Envelope {
            kind: MsgKind::System,
            body: MsgBody::Notice("alice 加入了群聊".into()),
            ..Envelope::with_body(
                "g5",
                "alice",
                vec!["bob".into(), "carol".into()],
                0,
                MsgBody::Notice("alice 加入了群聊".into()),
            )
        })
        .await
        .unwrap();

    for client in [&bob, &carol] {
        let got = drain(client, Duration::from_millis(800)).await;
        assert_eq!(got.len(), 1, "one notification each");
        assert_eq!(got[0].kind, MsgKind::System);
        assert!(matches!(&got[0].body, MsgBody::Notice(t) if t.contains("alice")));
    }
}
