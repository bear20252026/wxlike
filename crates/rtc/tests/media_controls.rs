//! G09-03 媒体控制面验收：静音真的不发包、切换摄像头后旧源帧被丢弃、
//! ICE 看门狗按阈值降级并重启，且重启有次数上限。

use std::time::Duration;

use str0m::Event;
use wxlike_rtc::media::{pump, IceHealth, IceMonitor, MediaSession};

fn connect() -> (MediaSession, MediaSession) {
    let mut sender = MediaSession::new(true);
    let mut receiver = MediaSession::new(false);
    sender.add_host_candidate("192.0.2.10:41000".parse().unwrap());
    receiver.add_host_candidate("192.0.2.20:41001".parse().unwrap());
    let offer = sender.offer(true).unwrap();
    let answer = receiver.accept_offer(offer).unwrap();
    sender.accept_answer(answer).unwrap();
    for _ in 0..40 {
        pump(&mut sender, &mut receiver, Duration::from_millis(250)).unwrap();
        if sender.is_connected() && receiver.is_connected() {
            return (sender, receiver);
        }
    }
    panic!("ICE/DTLS 未连通");
}

fn received(events: &[Event], byte: u8) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, Event::MediaData(d) if d.data.first() == Some(&byte)))
        .count()
}

#[test]
fn muted_audio_produces_no_packets_at_all() {
    let (mut sender, mut receiver) = connect();
    sender.set_audio_muted(true);
    let mut at_receiver = Vec::new();
    for _ in 0..20 {
        sender.write_audio(&[0xE1; 80]).unwrap();
        let (_, ev) = pump(&mut sender, &mut receiver, Duration::from_millis(50)).unwrap();
        at_receiver.extend(ev);
    }
    assert_eq!(
        received(&at_receiver, 0xE1),
        0,
        "静音期间远端必须一包都收不到"
    );

    sender.set_audio_muted(false);
    at_receiver.clear();
    for _ in 0..20 {
        sender.write_audio(&[0xE2; 80]).unwrap();
        let (_, ev) = pump(&mut sender, &mut receiver, Duration::from_millis(50)).unwrap();
        at_receiver.extend(ev);
    }
    assert!(received(&at_receiver, 0xE2) > 0, "解除静音后恢复发送");
}

#[test]
fn video_source_switch_drops_frames_from_the_old_camera() {
    let (mut sender, mut receiver) = connect();
    sender.select_video_source("front").unwrap();
    assert!(sender.write_video_from("front", &[0xF1; 120]).unwrap());

    sender.select_video_source("back").unwrap();
    assert!(
        !sender.write_video_from("front", &[0xF2; 120]).unwrap(),
        "切换后旧摄像头仍在出帧，必须被丢弃"
    );
    assert!(sender.write_video_from("back", &[0xF3; 120]).unwrap());

    let mut at_receiver = Vec::new();
    for _ in 0..20 {
        sender.write_video_from("back", &[0xF3; 120]).unwrap();
        let (_, ev) = pump(&mut sender, &mut receiver, Duration::from_millis(50)).unwrap();
        at_receiver.extend(ev);
    }
    assert!(received(&at_receiver, 0xF3) > 0, "新摄像头画面必须到达");
    assert_eq!(received(&at_receiver, 0xF2), 0, "旧源帧不得泄漏");
}

#[test]
fn camera_switch_can_restore_a_previous_source() {
    let mut sender = MediaSession::new(true);
    sender.select_video_source("front").unwrap();
    sender.select_video_source("back").unwrap();
    sender.select_video_source("front").unwrap();
    assert_eq!(sender.video_source(), Some("front"));
    assert!(sender.select_video_source("  ").is_err(), "空源名必须拒绝");
}

#[test]
fn ice_watchdog_degrades_restarts_and_gives_up() {
    let mut monitor = IceMonitor::default_policy();
    monitor.observe(1_000);
    assert_eq!(monitor.health(1_500), IceHealth::Healthy);
    assert_eq!(
        monitor.health(3_000),
        IceHealth::Degraded { silent_ms: 2_000 }
    );
    assert_eq!(
        monitor.health(6_000),
        IceHealth::RestartDue { silent_ms: 5_000 }
    );
    assert_eq!(monitor.begin_restart(6_000), Some(1));
    assert_eq!(monitor.generation(), 1);

    // 冷却期内即使仍然静默也不重复重启
    monitor.observe(6_000);
    assert_eq!(
        monitor.health(11_000),
        IceHealth::Degraded { silent_ms: 5_000 }
    );
    assert_eq!(monitor.begin_restart(11_000), None);

    // 冷却结束后可再重启，但总次数封顶
    monitor.observe(16_000);
    assert_eq!(monitor.begin_restart(22_000), Some(2));
    monitor.observe(33_000);
    assert_eq!(monitor.begin_restart(39_000), Some(3));
    monitor.observe(39_000);
    assert_eq!(monitor.health(50_000), IceHealth::Exhausted);
    assert_eq!(monitor.begin_restart(50_000), None);
}

#[test]
fn ice_restart_produces_a_fresh_offer() {
    let (mut sender, _receiver) = connect();
    assert!(sender.is_connected());
    let offer = sender.restart_ice().unwrap();
    assert_eq!(offer.get("type").and_then(|v| v.as_str()), Some("offer"));
    assert!(!sender.is_connected(), "重启后需重新等 Connected 事件");
}
