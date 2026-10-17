//! WP-08a end to end: call signaling rides the **real QUIC hub** (invite /
//! accept / hangup as `rtc-signal` envelopes), and the offline-peer path
//! fails immediately using the hub's route table as the presence source (A3).
//! Media connectivity itself is proven in `media_loopback.rs`.

use std::sync::Arc;
use std::time::Duration;

use wxlike_core::{Envelope, MsgBody};
use wxlike_mq::MqRouter;
use wxlike_net::Client;
use wxlike_rtc::call::{Action, CallManager};
use wxlike_rtc::signal::{Signal, SignalContent};
use wxlike_rtc::state::{End, Phase};

async fn hub() -> anyhow::Result<wxlike_net::MqHub> {
    let router = MqRouter::new(
        Arc::new(wxlike_mq::memory::MemoryMq::new()),
        wxlike_mq::memory::MemoryRoutes::new(),
    );
    wxlike_net::run_mq_hub("rtc-hub", "127.0.0.1:0".parse()?, router).await
}

/// Sends every action's signal over the QUIC client.
async fn flush_actions(
    actions: &[Action],
    client: &Client,
    mgr_seq: &mut u64,
) -> anyhow::Result<()> {
    for action in actions {
        if let Action::Send(signal) = action {
            *mgr_seq += 1;
            let env = signal.to_envelope(*mgr_seq)?;
            client.send(&env).await?;
        }
    }
    Ok(())
}

/// Reads envelopes from a client and feeds them to the manager. Keeps
/// polling until the line is quiet for a full quiet_budget (hub routing is
/// async, so a single 50 ms read races the network).
async fn drain_signals(client: &Client, mgr: &mut CallManager) -> anyhow::Result<Vec<Action>> {
    let mut all = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut quiet = 0;
    while quiet < 5 && std::time::Instant::now() < deadline {
        if let Some(env) = try_next(client).await? {
            quiet = 0;
            if let Some(signal) = Signal::from_envelope(&env) {
                all.extend(mgr.handle_signal(signal));
            }
        } else {
            quiet += 1;
        }
    }
    Ok(all)
}

async fn try_next(client: &Client) -> anyhow::Result<Option<Envelope>> {
    match tokio::time::timeout(Duration::from_millis(50), client.next_envelope()).await {
        Ok(Ok(env)) => Ok(Some(env)),
        Ok(Err(err)) => Err(err),
        Err(_) => Ok(None),
    }
}

#[tokio::test]
async fn full_call_over_real_hub_with_media_negotiation() {
    let hub = hub().await.unwrap();
    let addr = hub.local_addr().unwrap();
    let alice = Client::connect(addr, "localhost", "alice").await.unwrap();
    let bob = Client::connect(addr, "localhost", "bob").await.unwrap();
    while hub.handle().online_users().await < 2 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let mut alice_mgr = CallManager::new("alice");
    let mut bob_mgr = CallManager::new("bob");
    let mut a_seq = 0u64;
    let mut b_seq = 0u64;

    // 1. alice calls bob; presence comes from the hub's route table
    let (call_id, actions) = alice_mgr.start("bob", false, true);
    flush_actions(&actions, &alice, &mut a_seq).await.unwrap();
    assert_eq!(alice_mgr.phase_of(&call_id), Some(Phase::Ringing));

    // 2. bob receives the invite (real QUIC delivery), then answers; his
    //    Accept carries the SDP answer for the offer that rode the Invite
    let _inbound = drain_signals(&bob, &mut bob_mgr).await.unwrap();
    let bob_id = match bob_mgr.live_call_with("alice") {
        Some(id) => id,
        None => panic!("invite never rang at bob: {:?}", bob_mgr.phase_of(&call_id)),
    };
    assert_eq!(bob_mgr.phase_of(&bob_id), Some(Phase::Ringing));
    let actions = bob_mgr.accept(&bob_id);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::Send(Signal {
                content: SignalContent::Accept { .. },
                ..
            })
        )),
        "accept must answer the invite with SDP, got {actions:?}"
    );
    flush_actions(&actions, &bob, &mut b_seq).await.unwrap();
    assert_eq!(bob_mgr.phase_of(&bob_id), Some(Phase::Active));
    assert!(
        bob_mgr.media_mut(&bob_id).is_some(),
        "callee owns a media session for the call"
    );

    // 3. alice receives the accept; both sides Active, both hold media
    let actions = drain_signals(&alice, &mut alice_mgr).await.unwrap();
    flush_actions(&actions, &alice, &mut a_seq).await.unwrap();
    assert_eq!(alice_mgr.phase_of(&call_id), Some(Phase::Active));
    assert!(alice_mgr.media_mut(&call_id).is_some());

    // 4. bob hangs up; alice sees RemoteHangup on an active call -> HungUp
    let actions = bob_mgr.hangup(&bob_id);
    flush_actions(&actions, &bob, &mut b_seq).await.unwrap();
    assert_eq!(bob_mgr.phase_of(&bob_id), Some(Phase::Ended(End::HungUp)));
    drain_signals(&alice, &mut alice_mgr).await.unwrap();
    assert_eq!(
        alice_mgr.phase_of(&call_id),
        Some(Phase::Ended(End::HungUp))
    );

    drop(alice);
    drop(bob);
    drop(hub);
}

/// A3: 对离线用户发起呼叫 → 立即 Failed。Presence comes from the hub's real
/// route table (no mock): carol never connected.
#[tokio::test]
async fn calling_an_offline_peer_fails_immediately_over_hub() {
    let hub = hub().await.unwrap();
    let addr = hub.local_addr().unwrap();
    let alice = Client::connect(addr, "localhost", "alice").await.unwrap();
    while hub.handle().online_users().await < 1 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let mut mgr = CallManager::new("alice");
    let carol_online = hub.handle().is_online("carol").await;
    assert!(!carol_online, "carol is offline per the route table");
    let started = std::time::Instant::now();
    let (call_id, actions) = mgr.start("carol", false, carol_online);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "offline peers fail immediately, not after the ring timeout"
    );
    assert_eq!(mgr.phase_of(&call_id), Some(Phase::Ended(End::Failed)));
    assert!(
        !actions.iter().any(|a| matches!(a, Action::Send(_))),
        "nothing is sent to an offline peer"
    );
    drop(alice);
    drop(hub);
}

/// 信令确实走 QUIC 信封通道：invite 的 body 是 Custom("rtc-signal")。
#[tokio::test]
async fn signaling_envelopes_are_rtc_custom_kind() {
    let signal = Signal::new("c1", "alice", "bob", SignalContent::Hangup);
    let env = signal.to_envelope(1).unwrap();
    match &env.body {
        MsgBody::Custom { kind, .. } => assert_eq!(kind, "rtc-signal"),
        other => panic!("signaling must ride Custom envelopes, got {other:?}"),
    }
}

/// Glare over the real hub: simultaneous invites converge to one call.
#[tokio::test]
async fn glare_over_hub_converges() {
    let hub = hub().await.unwrap();
    let addr = hub.local_addr().unwrap();
    let alice = Client::connect(addr, "localhost", "alice").await.unwrap();
    let bob = Client::connect(addr, "localhost", "bob").await.unwrap();
    while hub.handle().online_users().await < 2 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let mut a = CallManager::new("alice");
    let mut b = CallManager::new("bob");
    let mut a_seq = 0u64;
    let mut b_seq = 0u64;

    // both invite at once
    let (a_call, a_acts) = a.start("bob", false, true);
    let (b_call, b_acts) = b.start("alice", false, true);
    flush_actions(&a_acts, &alice, &mut a_seq).await.unwrap();
    flush_actions(&b_acts, &bob, &mut b_seq).await.unwrap();

    // each receives the other's invite while ringing
    let a_acts = drain_signals(&alice, &mut a).await.unwrap();
    flush_actions(&a_acts, &alice, &mut a_seq).await.unwrap();
    let b_acts = drain_signals(&bob, &mut b).await.unwrap();
    flush_actions(&b_acts, &bob, &mut b_seq).await.unwrap();

    // exactly one call survives as the live call between them; the loser's
    // own call is cancelled and it rings on the winner's call
    let live = a
        .live_call_with("bob")
        .or_else(|| b.live_call_with("alice"));
    assert!(live.is_some(), "one call between alice and bob survives");
    let winner_is_alice = a.live_call_with("bob").is_some_and(|id| id == a_call);
    if winner_is_alice {
        assert_eq!(
            b.phase_of(&a_call),
            Some(Phase::Ringing),
            "bob yields his own call and rings on alice's"
        );
        assert_eq!(
            b.phase_of(&b_call),
            Some(Phase::Ended(End::Cancelled)),
            "bob's own invite lost the arbitration"
        );
        assert_eq!(a.phase_of(&a_call), Some(Phase::Ringing));
    } else {
        assert_eq!(a.phase_of(&b_call), Some(Phase::Ringing));
        assert_eq!(a.phase_of(&a_call), Some(Phase::Ended(End::Cancelled)));
        assert_eq!(b.phase_of(&b_call), Some(Phase::Ringing));
    }

    drop(alice);
    drop(bob);
    drop(hub);
}
