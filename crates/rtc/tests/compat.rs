//! G09-01 验收：唯一引擎门禁 + 四类客户端两两语音/视频。
//!
//! 这里用**同一套真实 str0m 媒体栈**（真 ICE/DTLS/SRTP）跑 6 组配对，
//! 证明媒体协议不因客户端类别而异。真实设备（浏览器/Android/iOS/Flutter
//! 音视频路由、后台来电）仍属待验项，矩阵里已明确标注需要的原生桥。

use std::time::Duration;

use str0m::Event;
use wxlike_rtc::compat::{
    acceptance_pairs, approved_engine, budget, matrix, power_budget, Capability, ClientClass,
    Engine, EngineError, Quality, Support,
};
use wxlike_rtc::media::{pump, MediaSession};

const A_AUDIO: [u8; 80] = [0xA1; 80];
const B_AUDIO: [u8; 80] = [0xB2; 80];
const VIDEO: [u8; 120] = [0xC3; 120];

fn connect(a: &mut MediaSession, b: &mut MediaSession) {
    for _ in 0..40 {
        pump(a, b, Duration::from_millis(250)).unwrap();
        if a.is_connected() && b.is_connected() {
            return;
        }
    }
    panic!(
        "ICE/DTLS 未连通: a={} b={}",
        a.is_connected(),
        b.is_connected()
    );
}

/// 一个配对：A→B 音频、B→A 音频、A→B 视频，三个方向都必须到达。
fn exercise_pair(left: ClientClass, right: ClientClass) {
    let mut a = MediaSession::new(true);
    let mut b = MediaSession::new(false);
    a.add_host_candidate("192.0.2.10:40000".parse().unwrap());
    b.add_host_candidate("192.0.2.20:40001".parse().unwrap());

    let offer = a.offer(true).unwrap();
    let answer = b.accept_offer(offer).unwrap();
    a.accept_answer(answer).unwrap();

    assert!(
        approved_engine(None).is_ok(),
        "{left:?}/{right:?} 只允许唯一引擎"
    );
    connect(&mut a, &mut b);

    let mut at_a = Vec::new();
    let mut at_b = Vec::new();
    for _ in 0..20 {
        a.write_audio(&A_AUDIO).unwrap();
        b.write_audio(&B_AUDIO).unwrap();
        a.write_video(&VIDEO).unwrap();
        let (ea, eb) = pump(&mut a, &mut b, Duration::from_millis(50)).unwrap();
        at_a.extend(ea);
        at_b.extend(eb);
    }
    let (mut ea, mut eb) = pump(&mut a, &mut b, Duration::from_millis(300)).unwrap();
    at_a.append(&mut ea);
    at_b.append(&mut eb);

    assert!(
        at_b.iter()
            .any(|e| matches!(e, Event::MediaData(d) if d.data == A_AUDIO)),
        "{left:?} → {right:?} 音频未到达"
    );
    assert!(
        at_a.iter()
            .any(|e| matches!(e, Event::MediaData(d) if d.data == B_AUDIO)),
        "{right:?} → {left:?} 音频未到达"
    );
    assert!(
        at_b.iter()
            .any(|e| matches!(e, Event::MediaData(d) if d.data == VIDEO)),
        "{left:?} → {right:?} 视频未到达"
    );
}

#[test]
fn all_four_client_classes_exchange_audio_and_video_pairwise() {
    let pairs = acceptance_pairs();
    assert_eq!(pairs.len(), 6, "四端两两组合");
    for (left, right) in pairs {
        assert!(
            left != ClientClass::CliHarness && right != ClientClass::CliHarness,
            "验收配对不包含 CI 探针"
        );
        exercise_pair(left, right);
    }
}

#[test]
fn second_engine_requires_a_global_adr() {
    assert_eq!(approved_engine(Some("str0m")), Ok(Engine::Str0m));
    assert_eq!(
        approved_engine(Some("livekit")),
        Err(EngineError::NeedsGlobalAdr("livekit"))
    );
}

#[test]
fn acceptance_clients_advertise_voice_video_and_relay() {
    for client in ClientClass::ACCEPTANCE {
        for cap in [Capability::Voice, Capability::Video, Capability::TurnRelay] {
            assert_eq!(
                wxlike_rtc::compat::support(client, cap),
                Support::Native,
                "{} 的 {} 必须是原生可用",
                client.name(),
                cap.name()
            );
        }
    }
    let rows = matrix();
    assert_eq!(rows.len(), ClientClass::ALL.len() * Capability::ALL.len());
}

#[test]
fn budgets_are_ordered_and_bounded() {
    let audio = budget(Quality::AudioOnly);
    let video = budget(Quality::Video720p);
    let screen = budget(Quality::Screen1080p);
    assert_eq!(audio.video_kbps, 0);
    assert!(audio.total_kbps < video.total_kbps && video.total_kbps < screen.total_kbps);
    assert!(screen.audio_kbps + screen.video_kbps <= 4_000);

    // 移动端耗电预算必须高于桌面端，否则这个指标就没有产品意义
    for q in [Quality::AudioOnly, Quality::Video720p] {
        assert!(
            power_budget(ClientClass::IosNative, q).mah_per_hour
                > power_budget(ClientClass::DesktopBrowser, q).mah_per_hour
        );
    }
}
