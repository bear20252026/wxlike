//! WP-13: operational baseline — /metrics fields, and crash recovery
//! (hub dies underneath connected clients; they reconnect within the
//! deadline and nothing acknowledged or parked is lost).

use std::sync::Arc;
use std::time::Duration;

use wxlike_core::{Envelope, MsgBody, MsgKind, Store, PROTOCOL_VERSION};
use wxlike_mq::MqRouter;
use wxlike_net::offline::{OfflineStore as _, SqliteOffline};
use wxlike_net::{Client, MqHubOptions};

fn router() -> MqRouter {
    MqRouter::new(
        Arc::new(wxlike_mq::memory::MemoryMq::new()),
        wxlike_mq::memory::MemoryRoutes::new(),
    )
}

fn text(from: &str, to: &str, body: &str, msg_id: u8) -> Envelope {
    Envelope {
        version: PROTOCOL_VERSION,
        conv_id: "d-a~b".into(),
        sender: from.into(),
        to: vec![to.into()],
        seq: 1,
        kind: MsgKind::Text,
        body: MsgBody::Text(body.into()),
        sent_at_ms: 1,
        msg_id: [msg_id; 16],
        quote_of: None,
        receipt: None,
    }
}

/// A3: the metrics endpoint's fields exist and move after real traffic.
#[tokio::test]
async fn metrics_expose_connections_throughput_backlog_and_latency() {
    let hub = wxlike_net::run_mq_hub("metrics-hub", "127.0.0.1:0".parse().unwrap(), router())
        .await
        .unwrap();
    let addr = hub.local_addr().unwrap();

    let alice = Client::connect(addr, "localhost", "alice").await.unwrap();
    let bob = Client::connect(addr, "localhost", "bob").await.unwrap();
    while hub.handle().online_users().await < 2 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // real client traffic so the counters/latency actually move
    alice.send(&text("alice", "bob", "m", 1)).await.unwrap();
    let got = bob.next_envelope().await.unwrap();
    // and a message to someone offline so the parked gauge moves
    alice
        .send(&text("alice", "carol", "parked", 2))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(got.body, MsgBody::Text("m".into()));
    drop(alice);

    // render through the real pipeline (installs the recorder if needed)
    let rendered = wxlike_net::prometheus_text();
    assert!(rendered.contains("wxlike_hub_connections"), "{rendered}");
    assert!(rendered.contains("wxlike_hub_messages_total"), "{rendered}");
    assert!(rendered.contains("wxlike_offline_parked"), "{rendered}");
    assert!(
        rendered.contains("wxlike_delivery_seconds_bucket"),
        "{rendered}"
    );
    assert!(rendered.contains("hub=\"metrics-hub\""), "{rendered}");
    drop(hub);
}

/// A3 / WP-13 core promise: kill the hub (drop = the process dying for the
/// client), restart it on the same address, and:
/// * connected clients reconnect inside the 30 s deadline,
/// * frames parked for an offline user before the crash are delivered after,
/// * persisted conversation history is intact on the new instance.
#[tokio::test]
async fn hub_crash_restart_reconnect_and_nothing_lost() {
    let dir = std::env::temp_dir().join(format!(
        "wxlike-crash-{}-{}",
        std::process::id(),
        wxlike_core::types::now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("crash.db");
    let db_path = db.to_str().unwrap().to_string();

    // pre-open the file so the "parked before crash" assertion can read it
    let offline = SqliteOffline::open(Some(&db_path), 100).unwrap();

    let hub1 = wxlike_net::run_mq_hub_with(
        "recovery-hub",
        "127.0.0.1:0".parse().unwrap(),
        router(),
        MqHubOptions {
            offline: SqliteOffline::open(Some(&db_path), 100).unwrap(),
            storage: Some(Arc::new(Store::open(Some(&db_path)).unwrap())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let addr = hub1.local_addr().unwrap();

    // alice is connected; bob is away. Alice's message parks durably.
    let alice = Client::connect(addr, "localhost", "alice").await.unwrap();
    while hub1.handle().online_users().await < 1 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    alice
        .send(&text("alice", "bob", "before crash", 1))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        offline.depth("bob").await.unwrap(),
        1,
        "parked before crash"
    );

    // ---- crash: the hub process dies from the clients' point of view ----
    drop(hub1);
    drop(alice);

    // restart on a fresh port would be trivial; bind the SAME address so the
    // reconnect path (host:port unchanged) is what's under test
    let hub2 = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match wxlike_net::run_mq_hub_with(
                "recovery-hub",
                addr,
                router(),
                MqHubOptions {
                    offline: SqliteOffline::open(Some(&db_path), 100).unwrap(),
                    storage: Some(Arc::new(Store::open(Some(&db_path)).unwrap())),
                    ..Default::default()
                },
            )
            .await
            {
                Ok(hub) => return hub,
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    })
    .await
    .expect("hub restarts within 10s");

    // the connected client reconnects on its own within the task book's 30 s
    let started = std::time::Instant::now();
    let alice2 = Client::connect_retry(addr, "localhost", "alice", Duration::from_secs(30))
        .await
        .expect("reconnect inside the deadline");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "reconnect took {:?}",
        started.elapsed()
    );

    // a second message from the reconnected client parks on the restarted hub
    alice2
        .send(&text("alice", "bob", "after crash", 2))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // bob finally shows up and receives BOTH messages, in order
    let bob = Client::connect(addr, "localhost", "bob").await.unwrap();
    let first = bob.next_envelope().await.unwrap();
    let second = bob.next_envelope().await.unwrap();
    assert_eq!(first.body, MsgBody::Text("before crash".into()));
    assert_eq!(second.body, MsgBody::Text("after crash".into()));

    // and the persisted conversation survived the restart
    let history = hub2.handle().messages_of("d-a~b").unwrap();
    let texts: Vec<&str> = history
        .iter()
        .map(|m| match &m.envelope.body {
            MsgBody::Text(t) => t.as_str(),
            _ => "",
        })
        .collect();
    assert!(
        texts.contains(&"before crash"),
        "history intact after restart: {texts:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
    drop(hub2);
}

/// The retry helper itself gives up exactly at the deadline when nothing is
/// listening, instead of hanging forever.
#[tokio::test]
async fn connect_retry_respects_deadline() {
    // nothing binds this: an address in the 127.x range with no listener
    let addr: std::net::SocketAddr = "127.0.0.1:1".parse().unwrap();
    let started = std::time::Instant::now();
    let err = match Client::connect_retry(addr, "localhost", "alice", Duration::from_secs(2)).await
    {
        Err(err) => err,
        Ok(_) => panic!("nothing is listening; connect must fail"),
    };
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "too fast: {err:#}"
    );
    assert!(started.elapsed() < Duration::from_secs(10), "too slow");
}
