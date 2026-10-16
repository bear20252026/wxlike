//! WP-08a/b media acceptance: real ICE/DTLS/SRTP between two str0m sessions
//! — first over a virtual in-process wire, then over real loopback UDP
//! sockets ("无 TURN 时本地回环媒体连通"), plus the WP-08b video m-line and
//! bandwidth-ladder behavior.

use std::time::Duration;

use str0m::Event;
use wxlike_rtc::media::{pump, BitrateController, LoopbackPair, MediaSession};

#[test]
fn audio_round_trip_over_virtual_wire() {
    let mut caller = MediaSession::new(true);
    let mut callee = MediaSession::new(false);
    caller.add_host_candidate("192.0.2.10:40000".parse().unwrap());
    callee.add_host_candidate("192.0.2.20:40001".parse().unwrap());

    let offer = caller.offer(false).unwrap();
    let answer = callee.accept_offer(offer).unwrap();
    caller.accept_answer(answer).unwrap();

    // bring ICE + DTLS up
    for _ in 0..40 {
        pump(&mut caller, &mut callee, Duration::from_millis(250)).unwrap();
        if caller.is_connected() && callee.is_connected() {
            break;
        }
    }
    assert!(caller.is_connected(), "caller ICE/DTLS up");
    assert!(callee.is_connected(), "callee ICE/DTLS up");

    // exchange audio both ways; both directions must arrive intact.
    // events accumulate across every pump window, not just the last one
    let mut all_a = Vec::new();
    let mut all_b = Vec::new();
    for _ in 0..30 {
        caller.write_audio(&[0xA5; 80]).unwrap();
        callee.write_audio(&[0x5A; 80]).unwrap();
        let (ea, eb) = pump(&mut caller, &mut callee, Duration::from_millis(50)).unwrap();
        all_a.extend(ea);
        all_b.extend(eb);
    }
    let (mut ea, mut eb) = pump(&mut caller, &mut callee, Duration::from_millis(300)).unwrap();
    all_a.append(&mut ea);
    all_b.append(&mut eb);
    let (ev_a, ev_b) = (all_a, all_b);
    let at_caller = ev_a
        .iter()
        .filter(|e| matches!(e, Event::MediaData(d) if d.data == vec![0x5A; 80]))
        .count();
    let at_callee = ev_b
        .iter()
        .filter(|e| matches!(e, Event::MediaData(d) if d.data == vec![0xA5; 80]))
        .count();
    // RTP may retransmit/nack; what matters is both directions delivered
    assert!(at_callee > 0, "callee received caller's audio frames");
    assert!(at_caller > 0, "caller received callee's audio frames");
}

/// The task book's loopback acceptance shape: real OS UDP sockets on
/// 127.0.0.1, datagrams through the kernel, no TURN, no STUN.
#[tokio::test]
async fn media_connects_over_real_loopback_udp() {
    let mut a = MediaSession::new(true);
    let mut b = MediaSession::new(false);

    // candidates BEFORE negotiation: str0m's SDP mode does not trickle
    let pair = LoopbackPair::bind().await.unwrap();
    pair.register(&mut a, &mut b);

    let offer = a.offer(false).unwrap();
    let answer = b.accept_offer(offer).unwrap();
    a.accept_answer(answer).unwrap();

    let (a_to_b, b_to_a) = pair
        .run(&mut a, &mut b, Duration::from_secs(5))
        .await
        .unwrap();
    assert!(
        a.is_connected(),
        "a connected over real UDP ({a_to_b} pkts a->b)"
    );
    assert!(
        b.is_connected(),
        "b connected over real UDP ({b_to_a} pkts b->a)"
    );
    assert!(
        a_to_b > 0 && b_to_a > 0,
        "packets actually crossed the OS loopback"
    );
}

// ---------------------------------------------------------------------------
// WP-08b
// ---------------------------------------------------------------------------

#[test]
fn video_m_line_is_negotiated_both_sides() {
    let mut caller = MediaSession::new(true);
    let mut callee = MediaSession::new(false);
    caller.add_host_candidate("192.0.2.10:40010".parse().unwrap());
    callee.add_host_candidate("192.0.2.20:40011".parse().unwrap());

    let offer = caller.offer(true).unwrap();
    assert!(caller.has_video_mline(), "caller added video");
    let answer = callee.accept_offer(offer).unwrap();
    caller.accept_answer(answer).unwrap();

    // and a video frame crosses like audio does
    for _ in 0..40 {
        pump(&mut caller, &mut callee, Duration::from_millis(250)).unwrap();
        if caller.is_connected() && callee.is_connected() {
            break;
        }
    }
    assert!(caller.is_connected() && callee.is_connected());
    assert!(callee.has_video_mline(), "callee answered with video");
    assert!(caller.audio_mid().is_some() && callee.audio_mid().is_some());
    assert!(caller.video_mid().is_some() && callee.video_mid().is_some());
    let mut all_a = Vec::new();
    let mut all_b = Vec::new();
    for _ in 0..20 {
        caller.write_video(&[0xCC; 120]).unwrap();
        let (ea, eb) = pump(&mut caller, &mut callee, Duration::from_millis(50)).unwrap();
        all_a.extend(ea);
        all_b.extend(eb);
    }
    let (mut ea, mut eb) = pump(&mut caller, &mut callee, Duration::from_millis(300)).unwrap();
    all_a.append(&mut ea);
    all_b.append(&mut eb);
    let (ev_a, ev_b) = (all_a, all_b);
    let video_at_callee = ev_b
        .iter()
        .filter(|e| matches!(e, Event::MediaData(d) if d.data == vec![0xCC; 120]))
        .count();
    let video_at_caller = ev_a
        .iter()
        .filter(|e| matches!(e, Event::MediaData(d) if d.data == vec![0xCC; 120]))
        .count();
    assert!(video_at_callee > 0, "video frames arrived at callee");
    assert_eq!(video_at_caller, 0, "no echo back to the sender");
}

#[test]
fn audio_only_call_has_no_video_m_line() {
    let mut caller = MediaSession::new(true);
    let mut callee = MediaSession::new(false);
    caller.add_host_candidate("192.0.2.10:40020".parse().unwrap());
    callee.add_host_candidate("192.0.2.20:40021".parse().unwrap());
    let offer = caller.offer(false).unwrap();
    let answer = callee.accept_offer(offer).unwrap();
    caller.accept_answer(answer).unwrap();
    assert!(!caller.has_video_mline());
    assert!(!callee.has_video_mline());
}

#[test]
fn bandwidth_ladder_degrades_hard_and_climbs_cautiously() {
    let mut ctl = BitrateController::new(150, 2500);
    assert_eq!(ctl.target_kbps(), 2500);

    // path collapses to 800 kbps: target drops just under the estimate
    assert_eq!(ctl.observe(800), Some(750));
    // further collapse hits the floor and stays there
    assert_eq!(ctl.observe(50), Some(150));
    assert_eq!(ctl.observe(10), None, "cannot go below the floor");

    // recovery needs 50% headroom and climbs by quarters
    let mut recovering = BitrateController::new(150, 2400);
    // degrade first: (400-50) rounded down to a 50-step
    assert_eq!(recovering.observe(400), Some(350));
    // 530 > 350*1.5 -> climb by a quarter
    assert_eq!(recovering.observe(530), Some(350 + 350 / 4));
    // 600 has no headroom over 437*1.5
    assert_eq!(recovering.observe(600), None, "no headroom, no change");
    // huge headroom climbs again, later capped by the ceiling
    assert_eq!(recovering.observe(10_000), Some(437 + 437 / 4));
}

#[test]
fn bandwidth_ladder_respects_ceiling() {
    // start below the ceiling so climbing is observable
    let mut ctl = BitrateController::new(100, 1000);
    assert_eq!(ctl.observe(100), Some(100), "degrade to the floor");
    assert_eq!(ctl.observe(2000), Some(125), "climb by a quarter");
    // keep offering huge headroom: quarters up, then pinned at the ceiling
    let mut last = 125;
    while let Some(next) = ctl.observe(10_000) {
        assert!(next > last, "monotonic climb");
        last = next;
    }
    assert_eq!(ctl.target_kbps(), 1000, "the climb stops at the ceiling");
}
