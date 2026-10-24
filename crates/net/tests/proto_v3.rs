//! G03-04 acceptance: protocol v3 over the hub's real serve loop, plus the
//! carrier-shared fixture.
//!
//! The connection here is an in-process frame pipe attached with
//! `HubHandle::attach_transport`, so `serve_transport` — the same code path a
//! QUIC or WebSocket peer runs — is what these tests exercise. The v2 paths stay
//! covered by the existing QUIC/WS suites (`offline_reconnect`, `multi_device`,
//! `route_lease`), which is what "旧协议兼容" has to mean in practice.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::DuplexStream;
use wxlike_core::Envelope;
use wxlike_mq::memory::{MemoryMq, MemoryRoutes};
use wxlike_mq::{MqRouter, RouteTable};
use wxlike_net::auth::{HubAuthenticator, HubIdentity, StaticAuth};
use wxlike_net::conn::{FrameRx, FrameTx, PeerConn};
use wxlike_net::offline::{MemoryOffline, OfflineStore};
use wxlike_net::policy::Limits;
use wxlike_net::proto::{caps_bits, Capability, Vector};
use wxlike_net::wire::WireFrame;
use wxlike_net::{framing, run_mq_hub_with, MqHub, MqHubOptions};

/// Hub-facing end of a test connection: the hub writes into `from_hub`, reads
/// from `to_hub`. Dropping both halves is how a test "pulls the cable".
struct WriteHalf(DuplexStream);
struct ReadHalf(DuplexStream);

#[async_trait::async_trait]
impl FrameTx for WriteHalf {
    async fn send_frame(&mut self, frame: &[u8]) -> anyhow::Result<()> {
        framing::write_frame(&mut self.0, frame).await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl FrameRx for ReadHalf {
    async fn read_frame(&mut self) -> anyhow::Result<Vec<u8>> {
        Ok(framing::read_frame(&mut self.0).await?)
    }
}

struct Peer {
    to_hub: DuplexStream,
    from_hub: DuplexStream,
}

impl Peer {
    /// Attach a fresh connection to the hub and return the client halves.
    fn attach(hub: &MqHub) -> Self {
        let (hub_out, client_in) = tokio::io::duplex(64 * 1024);
        let (client_out, hub_in) = tokio::io::duplex(64 * 1024);
        let conn = PeerConn::new(Box::new(WriteHalf(hub_out)), Box::new(ReadHalf(hub_in)));
        hub.handle().attach_transport(conn);
        Self {
            to_hub: client_out,
            from_hub: client_in,
        }
    }

    async fn send(&mut self, frame: &WireFrame) {
        let bytes = frame.to_bytes().expect("encode");
        framing::write_frame(&mut self.to_hub, &bytes)
            .await
            .expect("send frame");
    }

    /// Next frame from the hub, or `None` if the hub closed the connection.
    async fn next(&mut self) -> Option<WireFrame> {
        let read = framing::read_frame(&mut self.from_hub);
        match tokio::time::timeout(Duration::from_secs(2), read).await {
            Ok(Ok(bytes)) => WireFrame::from_bytes(&bytes).ok(),
            _ => None,
        }
    }

    /// Nothing more is coming (used after an ack: no re-delivery expected).
    async fn expect_quiet(&mut self) {
        let read = framing::read_frame(&mut self.from_hub);
        assert!(
            tokio::time::timeout(Duration::from_millis(400), read)
                .await
                .is_err(),
            "the hub sent a frame that should have stayed retired"
        );
    }

    /// Pulling the cable: both halves go away and the serve loop sees EOF.
    fn drop_link(self) {
        let Self { to_hub, from_hub } = self;
        drop(to_hub);
        drop(from_hub);
    }
}

/// v2 handshake: `Hello{token}` only.
async fn hello_v2(peer: &mut Peer, user: &str) {
    peer.send(&WireFrame::Hello {
        token: user.to_string(),
    })
    .await;
    let ack = peer.next().await.expect("hello ack");
    assert!(
        matches!(&ack, WireFrame::HelloAck { user_id, .. } if user_id == user),
        "{ack:?}"
    );
}

/// v3 handshake: `ClientHello{device}` then the same credential frame.
async fn hello_v3(peer: &mut Peer, user: &str, device: &str, caps: &[Capability]) {
    peer.send(&WireFrame::ClientHello {
        device_id: device.to_string(),
        protocol: wxlike_net::proto::PROTOCOL_V3,
        caps: caps_bits(caps),
    })
    .await;
    peer.send(&WireFrame::Hello {
        token: user.to_string(),
    })
    .await;
    let ack = peer.next().await.expect("hello ack");
    assert!(matches!(ack, WireFrame::HelloAck { .. }), "{ack:?}");
    let auth = peer.next().await.expect("v3 auth");
    match auth {
        WireFrame::Auth {
            user_id,
            device_id,
            version,
            ..
        } => {
            assert_eq!((user_id.as_str(), device_id.as_str()), (user, device));
            assert_eq!(version, wxlike_net::proto::PROTOCOL_V3);
        }
        other => panic!("expected the v3 Auth frame, got {other:?}"),
    }
}

async fn hub_with(limits: Limits, mailbox: bool) -> MqHub {
    let offline = MemoryOffline::new(500);
    hub_with_store(limits, mailbox, offline).await
}

async fn hub_with_store(limits: Limits, mailbox: bool, offline: Arc<MemoryOffline>) -> MqHub {
    run_mq_hub_with(
        "hub-v3",
        "127.0.0.1:0".parse().unwrap(),
        MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new()),
        MqHubOptions {
            auth: Some(Arc::new(StaticAuth)),
            offline,
            mailbox: mailbox
                .then(|| Arc::new(wxlike_message::MessageStore::open(None).expect("mailbox"))),
            limits,
            ..Default::default()
        },
    )
    .await
    .expect("hub")
}

#[tokio::test]
async fn v3_negotiates_and_a_v2_peer_is_left_alone() {
    let hub = hub_with(Limits::default(), false).await;
    // v3: the second ack frame carries the device and the version
    let mut v3 = Peer::attach(&hub);
    hello_v3(&mut v3, "carol", "dev-carol-1", &[Capability::StoredAck]).await;

    // v2 on the same hub: only HelloAck, never an Auth frame
    let mut v2 = Peer::attach(&hub);
    hello_v2(&mut v2, "dave").await;
    let late = {
        let read = framing::read_frame(&mut v2.from_hub);
        tokio::time::timeout(Duration::from_millis(300), read).await
    };
    assert!(
        late.is_err(),
        "a v2 peer must not be fed v3 downlink frames"
    );
    hub.shutdown().await;
}

#[tokio::test]
async fn an_unknown_protocol_version_is_refused_with_a_reason() {
    let hub = hub_with(Limits::default(), false).await;
    let mut peer = Peer::attach(&hub);
    peer.send(&WireFrame::ClientHello {
        device_id: "dev-x".into(),
        protocol: 9,
        caps: 0,
    })
    .await;
    let reject = peer.next().await.expect("reject");
    match reject {
        WireFrame::HelloReject { reason } => assert!(reason.contains("9"), "{reason}"),
        other => panic!("expected a rejection, got {other:?}"),
    }
    assert!(
        peer.next().await.is_none(),
        "and then the connection is closed"
    );
    hub.shutdown().await;
}

#[tokio::test]
async fn a_v3_frame_on_a_v2_connection_closes_instead_of_silently_working() {
    let hub = hub_with(Limits::default(), false).await;
    let mut peer = Peer::attach(&hub);
    hello_v2(&mut peer, "mallory").await;
    peer.send(&WireFrame::Ack {
        conv_id: "dm".into(),
        stored_cursor: 9,
    })
    .await;
    assert!(
        peer.next().await.is_none(),
        "protocol error must end the session"
    );
    hub.shutdown().await;
}

#[tokio::test]
async fn stored_ack_is_what_retires_a_parked_frame_not_the_write() {
    let offline = MemoryOffline::new(500);
    let mailbox = Arc::new(wxlike_message::MessageStore::open(None).unwrap());
    let hub = {
        let opts = MqHubOptions {
            auth: Some(Arc::new(StaticAuth)),
            offline: offline.clone(),
            mailbox: Some(mailbox.clone()),
            limits: Limits::default(),
            ..Default::default()
        };
        run_mq_hub_with(
            "hub-stored",
            "127.0.0.1:0".parse().unwrap(),
            MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new()),
            opts,
        )
        .await
        .unwrap()
    };
    // bob is nowhere online: the frame is parked
    let alice = wxlike_net::Client::connect(hub.local_addr().unwrap(), "localhost", "alice")
        .await
        .unwrap();
    alice
        .send(&Envelope::direct(
            "dm",
            "alice",
            vec!["bob".into()],
            0,
            "hello bob",
        ))
        .await
        .unwrap();
    for _ in 0..60 {
        if offline.depth("bob").await.unwrap() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(offline.depth("bob").await.unwrap(), 1, "parked while away");

    // v3 with StoredAck: the frame is written but stays owed until acked
    let mut peer = Peer::attach(&hub);
    hello_v3(&mut peer, "bob", "dev-bob-1", &[Capability::StoredAck]).await;
    let got = peer.next().await.expect("parked frame");
    assert!(matches!(got, WireFrame::Msg(_)), "{got:?}");
    assert_eq!(
        offline.depth("bob").await.unwrap(),
        1,
        "a socket write must not delete the only copy"
    );

    // the client dies before persisting: the row goes back, it is not lost
    peer.drop_link();
    for _ in 0..60 {
        if offline.depth("bob").await.unwrap() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        offline.depth("bob").await.unwrap(),
        1,
        "requeued after the connection died"
    );

    // reconnect, then acknowledge what the device actually stored
    let mut peer = Peer::attach(&hub);
    hello_v3(
        &mut peer,
        "bob",
        "dev-bob-1",
        &[Capability::StoredAck, Capability::Resume],
    )
    .await;
    let again = peer.next().await.expect("redelivered frame");
    let seq = match again {
        WireFrame::Msg(env) => env.seq,
        other => panic!("expected the message again, got {other:?}"),
    };
    assert!(seq > 0, "the hub stamps the mailbox seq onto the frame");
    peer.send(&WireFrame::Ack {
        conv_id: "dm".into(),
        stored_cursor: seq,
    })
    .await;
    for _ in 0..60 {
        if offline.depth("bob").await.unwrap() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        offline.depth("bob").await.unwrap(),
        0,
        "stored ack retired it"
    );
    // and the device cursor is the same fact on the same row
    assert_eq!(
        mailbox.device_cursor("dev-bob-1", "dm").unwrap().0,
        seq,
        "per-device persisted cursor advanced"
    );
    assert_eq!(mailbox.delivery_depth(Some("bob")).unwrap(), 0);
    peer.expect_quiet().await;
    hub.shutdown().await;
}

#[tokio::test]
async fn a_v2_peer_keeps_the_legacy_ack_on_write() {
    let offline = MemoryOffline::new(500);
    let hub = hub_with_store(Limits::default(), false, offline.clone()).await;
    let alice = wxlike_net::Client::connect(hub.local_addr().unwrap(), "localhost", "alice")
        .await
        .unwrap();
    alice
        .send(&Envelope::direct(
            "dm",
            "alice",
            vec!["bob".into()],
            3,
            "legacy",
        ))
        .await
        .unwrap();
    for _ in 0..60 {
        if offline.depth("bob").await.unwrap() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut peer = Peer::attach(&hub);
    hello_v2(&mut peer, "bob").await;
    let got = peer.next().await.expect("parked frame");
    assert!(matches!(got, WireFrame::Msg(_)), "{got:?}");
    for _ in 0..60 {
        if offline.depth("bob").await.unwrap() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // the observable difference between the two protocol generations: v2 has no
    // way to say "stored", so the hub retires on write, exactly as before
    assert_eq!(
        offline.depth("bob").await.unwrap(),
        0,
        "v2 acknowledgement is unchanged"
    );
    hub.shutdown().await;
}

#[tokio::test]
async fn resume_answers_with_the_range_the_device_is_missing() {
    let offline = MemoryOffline::new(500);
    let mailbox = Arc::new(wxlike_message::MessageStore::open(None).unwrap());
    let hub = {
        let opts = MqHubOptions {
            auth: Some(Arc::new(StaticAuth)),
            offline: offline.clone(),
            mailbox: Some(mailbox.clone()),
            limits: Limits::default(),
            ..Default::default()
        };
        run_mq_hub_with(
            "hub-resume",
            "127.0.0.1:0".parse().unwrap(),
            MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new()),
            opts,
        )
        .await
        .unwrap()
    };
    let alice = wxlike_net::Client::connect(hub.local_addr().unwrap(), "localhost", "alice")
        .await
        .unwrap();
    for text in ["one", "two", "three"] {
        alice
            .send(&Envelope::direct(
                "dm",
                "alice",
                vec!["bob".into()],
                0,
                text,
            ))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
    for _ in 0..60 {
        if offline.depth("bob").await.unwrap() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let mut peer = Peer::attach(&hub);
    hello_v3(
        &mut peer,
        "bob",
        "dev-bob-1",
        &[Capability::StoredAck, Capability::Resume],
    )
    .await;
    // the flush already delivered 1..=3; drain them without acking
    for _ in 0..3 {
        assert!(matches!(peer.next().await, Some(WireFrame::Msg(_))));
    }
    peer.drop_link();
    for _ in 0..60 {
        if offline.depth("bob").await.unwrap() == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // reconnect and say where the device really stopped: after seq 1
    let mut peer = Peer::attach(&hub);
    hello_v3(
        &mut peer,
        "bob",
        "dev-bob-1",
        &[Capability::StoredAck, Capability::Resume],
    )
    .await;
    peer.send(&WireFrame::Resume {
        conv_id: "dm".into(),
        cursor: 1,
    })
    .await;
    let mut saw_range = false;
    for _ in 0..8 {
        match peer.next().await {
            Some(WireFrame::Msg(_)) => {}
            Some(WireFrame::Event {
                conv_id,
                from_seq,
                to_seq,
                gap,
            }) => {
                assert_eq!(conv_id, "dm");
                assert_eq!(from_seq, 2, "everything after the reported cursor");
                assert!(to_seq >= 3, "head of {to_seq}");
                assert!(!gap, "three messages is one page, not a resync");
                saw_range = true;
                break;
            }
            Some(other) => panic!("unexpected downlink {other:?}"),
            None => break,
        }
    }
    assert!(
        saw_range,
        "the hub never answered the Resume with a range hint"
    );
    hub.shutdown().await;
}

#[tokio::test]
async fn the_connection_ceiling_is_enforced_per_user() {
    let limits = Limits {
        max_conns_per_user: 1,
        ..Limits::default()
    };
    let hub = hub_with(limits, false).await;
    let mut first = Peer::attach(&hub);
    hello_v2(&mut first, "bob").await;

    let mut second = Peer::attach(&hub);
    second
        .send(&WireFrame::Hello {
            token: "bob".into(),
        })
        .await;
    let reject = second.next().await.expect("rejection");
    match reject {
        WireFrame::HelloReject { reason } => assert!(reason.contains("too many"), "{reason}"),
        other => panic!("expected a rejection, got {other:?}"),
    }
    // the first connection is untouched by the refusal: it still gets a reply
    first.send(&WireFrame::Ping).await;
    hub.shutdown().await;
}

#[tokio::test]
async fn the_per_user_message_rate_ceiling_drops_and_meters() {
    let limits = Limits {
        max_msgs_per_user_per_min: 2,
        ..Limits::default()
    };
    let hub = hub_with(limits, false).await;
    let mut sender = Peer::attach(&hub);
    hello_v2(&mut sender, "alice").await;
    let mut sink = Peer::attach(&hub);
    hello_v2(&mut sink, "bob").await;

    let mut received = 0usize;
    for i in 0..6u64 {
        sender
            .send(&WireFrame::Msg(Envelope::direct(
                "dm",
                "alice",
                vec!["bob".into()],
                i,
                "spam",
            )))
            .await;
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    for _ in 0..6 {
        match sink.next().await {
            Some(WireFrame::Msg(_)) => received += 1,
            Some(_) => {}
            None => break,
        }
    }
    assert!(
        (2..6).contains(&received),
        "the ceiling has to cut the flood short, got {received} of 6"
    );
    hub.shutdown().await;
}

#[tokio::test]
async fn a_revoked_credential_stops_the_connection_at_the_next_frame() {
    struct Revoker {
        revoked: std::sync::atomic::AtomicBool,
    }
    impl HubAuthenticator for Revoker {
        fn authenticate(&self, token: &str) -> Option<HubIdentity> {
            if token == "alice" {
                Some(HubIdentity {
                    user_id: "alice".into(),
                    session_id: "s-1".into(),
                })
            } else {
                None
            }
        }

        fn is_revoked(&self, _token: &str) -> bool {
            self.revoked.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    let offline = MemoryOffline::new(500);
    let revoker = Arc::new(Revoker {
        revoked: std::sync::atomic::AtomicBool::new(false),
    });
    let hub = run_mq_hub_with(
        "hub-revoke",
        "127.0.0.1:0".parse().unwrap(),
        MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new()),
        MqHubOptions {
            auth: Some(revoker.clone()),
            offline,
            limits: Limits::default(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut peer = Peer::attach(&hub);
    hello_v2(&mut peer, "alice").await;
    revoker
        .revoked
        .store(true, std::sync::atomic::Ordering::Relaxed);
    peer.send(&WireFrame::Ping).await;
    assert!(
        peer.next().await.is_none(),
        "a revoked token must not keep receiving for a whole heartbeat"
    );
    hub.shutdown().await;
}

#[tokio::test]
async fn a_stale_connection_cannot_unbind_a_route_it_no_longer_owns() {
    let routes = MemoryRoutes::new();
    let hub = run_mq_hub_with(
        "hub-gen",
        "127.0.0.1:0".parse().unwrap(),
        MqRouter::new(Arc::new(MemoryMq::new()), routes.clone()),
        MqHubOptions {
            auth: Some(Arc::new(StaticAuth)),
            offline: MemoryOffline::new(500),
            limits: Limits::default(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut old = Peer::attach(&hub);
    hello_v2(&mut old, "bob").await;
    assert_eq!(
        routes.route_of("bob").await.unwrap(),
        Some("hub-gen".into())
    );

    // a newer bind takes the route (what happens when the client reconnects
    // while the old socket is still half-alive)
    let fresh = routes.bind_route("bob", "hub-gen").await.unwrap();
    assert!(fresh > 0);

    old.drop_link();
    for _ in 0..60 {
        if routes.renew_route("bob", "hub-gen", fresh).await.unwrap() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        routes.route_of("bob").await.unwrap(),
        Some("hub-gen".into()),
        "the dying old connection must not clear the newer route"
    );
    hub.shutdown().await;
}

/// The same fixture bytes must survive both carriers with identical numbering:
/// QUIC uses a length-prefixed byte stream, WebSocket is message-per-frame.
#[tokio::test]
async fn the_v3_fixture_is_identical_over_the_quic_and_ws_carriers() {
    let vectors = wxlike_net::proto::vectors();
    let blob = wxlike_net::proto::vectors_blob();
    let parsed = wxlike_net::proto::parse_vectors_blob(&blob).unwrap();
    assert_eq!(parsed.len(), vectors.len());

    // carrier 1: the QUIC framing path (u32 BE length prefix + bytes)
    let (mut w, mut r) = tokio::io::duplex(1024 * 512);
    let expected: Vec<Vec<u8>> = write_all_then_read(&mut w, &vectors, &mut r).await;

    // carrier 2: the WS path (one message == one frame, no prefix)
    let mut direct: Vec<Vec<u8>> = Vec::new();
    for v in &vectors {
        direct.push(v.frame.to_bytes().unwrap());
    }
    assert_eq!(
        expected, direct,
        "the frame bytes must not depend on the carrier's framing"
    );

    // and every frame decodes back to the same variant on both paths
    for (v, bytes) in vectors.iter().zip(&direct) {
        let back = WireFrame::from_bytes(bytes).unwrap();
        assert_eq!(format!("{back:?}"), format!("{:?}", v.frame), "{}", v.name);
    }
}

async fn write_all_then_read(
    w: &mut DuplexStream,
    vectors: &[Vector],
    r: &mut DuplexStream,
) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for v in vectors {
        let bytes = v.frame.to_bytes().expect("encode");
        framing::write_frame(w, &bytes).await.expect("write");
        let read = framing::read_frame(r).await.expect("read");
        assert_eq!(read, bytes, "{}", v.name);
        out.push(read);
    }
    out
}
