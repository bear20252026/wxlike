//! 通话房间 HTTP 控制面契约（把 `rtc::room` 接进平台后的行为）。
//!
//! 覆盖：成员资格、开播/重复开播、进出房、静音权限、全员静音（参照 spreed
//! 的 force-mute）、踢人禁入、锁门、录制、移交主持、最后一人离开结束房间、
//! 出口码率分配接口。

use wxlike_platform::Platform;
use wxlike_rtc::room::Track;

struct Fixture {
    p: wxlike_platform::PlatformState,
    alice: String,
    bob: String,
    carol: String,
    dave: String,
    conv: String,
}

/// alice 建群，bob/carol 是成员，dave 是群外用户。
fn fixture() -> Fixture {
    let p = Platform::in_memory().unwrap();
    let alice = p.register("alice", "pw").unwrap();
    let bob = p.register("bob", "pw").unwrap();
    let carol = p.register("carol", "pw").unwrap();
    let dave = p.register("dave", "pw").unwrap();
    let conv = p
        .create_group(&alice, "通话群", &[bob.clone(), carol.clone()])
        .unwrap();
    Fixture {
        p: p.shared(),
        alice,
        bob,
        carol,
        dave,
        conv,
    }
}

#[test]
fn outsider_cannot_start_or_join() {
    let f = fixture();
    assert!(f.p.start_call(&f.conv, &f.dave, 8).is_err());
    assert!(f.p.join_call(&f.conv, &f.dave).is_err());
    // 群外用户也看不到房间状态
    assert!(f.p.call_state(&f.conv, &f.dave).is_err());
}

#[test]
fn start_twice_conflicts_and_state_shows_host() {
    let f = fixture();
    f.p.start_call(&f.conv, &f.alice, 8).unwrap();
    assert!(f.p.start_call(&f.conv, &f.bob, 8).is_err());

    let st = f.p.call_state(&f.conv, &f.bob).unwrap().unwrap();
    assert!(st.live);
    assert_eq!(st.host.as_deref(), Some(f.alice.as_str()));
    assert_eq!(st.participants.len(), 1, "只有房主在房");
    assert_eq!(st.participants[0].role, "host");
}

#[test]
fn join_leave_and_state() {
    let f = fixture();
    f.p.start_call(&f.conv, &f.alice, 8).unwrap();
    f.p.join_call(&f.conv, &f.bob).unwrap();
    // 重复 join 幂等（客户端重连）
    f.p.join_call(&f.conv, &f.bob).unwrap();

    let st = f.p.call_state(&f.conv, &f.carol).unwrap().unwrap();
    assert_eq!(st.participants.len(), 2);

    f.p.leave_call(&f.conv, &f.bob).unwrap();
    let st = f.p.call_state(&f.conv, &f.carol).unwrap().unwrap();
    assert_eq!(st.participants.len(), 1);
}

#[test]
fn member_cannot_mute_others_but_host_can() {
    let f = fixture();
    f.p.start_call(&f.conv, &f.alice, 8).unwrap();
    f.p.join_call(&f.conv, &f.bob).unwrap();
    f.p.join_call(&f.conv, &f.carol).unwrap();

    // 普通成员不能静别人
    assert!(f
        .p
        .mute_in_call(&f.conv, &f.bob, &f.carol, Track::Audio, true)
        .is_err());
    // 自己静自己可以
    f.p.mute_in_call(&f.conv, &f.bob, &f.bob, Track::Audio, true)
        .unwrap();
    // 主持人可以静别人
    f.p.mute_in_call(&f.conv, &f.alice, &f.carol, Track::Video, true)
        .unwrap();

    let st = f.p.call_state(&f.conv, &f.bob).unwrap().unwrap();
    let bob = st.participants.iter().find(|p| p.user == f.bob).unwrap();
    let carol = st.participants.iter().find(|p| p.user == f.carol).unwrap();
    assert!(bob.audio_muted && !bob.video_muted, "音视频独立");
    assert!(carol.video_muted && !carol.audio_muted);
}

#[test]
fn mute_all_requires_moderator_and_affects_everyone() {
    let f = fixture();
    f.p.start_call(&f.conv, &f.alice, 8).unwrap();
    f.p.join_call(&f.conv, &f.bob).unwrap();
    f.p.join_call(&f.conv, &f.carol).unwrap();

    // 普通成员不能全员静音
    assert!(f
        .p
        .mute_all_in_call(&f.conv, &f.bob, Track::Audio, true)
        .is_err());

    let affected =
        f.p.mute_all_in_call(&f.conv, &f.alice, Track::Audio, true)
            .unwrap();
    assert_eq!(affected, 3);
    let st = f.p.call_state(&f.conv, &f.bob).unwrap().unwrap();
    assert!(st.participants.iter().all(|p| p.audio_muted));

    // 解除也只由主持人做
    f.p.mute_all_in_call(&f.conv, &f.alice, Track::Audio, false)
        .unwrap();
    let st = f.p.call_state(&f.conv, &f.bob).unwrap().unwrap();
    assert!(st.participants.iter().all(|p| !p.audio_muted));
}

#[test]
fn kick_removes_and_blocks_rejoin() {
    let f = fixture();
    f.p.start_call(&f.conv, &f.alice, 8).unwrap();
    f.p.join_call(&f.conv, &f.bob).unwrap();

    // 成员不能踢人
    assert!(f.p.kick_from_call(&f.conv, &f.bob, &f.alice).is_err());
    // 主持人可以，且不能踢自己
    assert!(f.p.kick_from_call(&f.conv, &f.alice, &f.alice).is_err());
    f.p.kick_from_call(&f.conv, &f.alice, &f.bob).unwrap();

    let st = f.p.call_state(&f.conv, &f.carol).unwrap().unwrap();
    assert_eq!(st.participants.len(), 1);
    // 被踢者不可再进
    assert!(f.p.join_call(&f.conv, &f.bob).is_err());
}

#[test]
fn lock_blocks_new_members() {
    let f = fixture();
    f.p.start_call(&f.conv, &f.alice, 8).unwrap();
    f.p.join_call(&f.conv, &f.bob).unwrap();

    assert!(
        f.p.lock_call(&f.conv, &f.bob, true).is_err(),
        "仅主持人可锁"
    );
    f.p.lock_call(&f.conv, &f.alice, true).unwrap();

    assert!(
        f.p.join_call(&f.conv, &f.carol).is_err(),
        "锁门后新人进不来"
    );
    // 已在房内的人不受影响
    let st = f.p.call_state(&f.conv, &f.bob).unwrap().unwrap();
    assert_eq!(st.participants.len(), 2);
    assert!(st.locked);

    f.p.lock_call(&f.conv, &f.alice, false).unwrap();
    f.p.join_call(&f.conv, &f.carol).unwrap();
}

#[test]
fn recording_is_host_only_and_visible() {
    let f = fixture();
    f.p.start_call(&f.conv, &f.alice, 8).unwrap();
    f.p.join_call(&f.conv, &f.bob).unwrap();

    assert!(f.p.record_call(&f.conv, &f.bob, true).is_err());
    f.p.record_call(&f.conv, &f.alice, true).unwrap();

    let st = f.p.call_state(&f.conv, &f.bob).unwrap().unwrap();
    assert!(st.recording);
}

#[test]
fn transfer_changes_host_and_demotes_previous() {
    let f = fixture();
    f.p.start_call(&f.conv, &f.alice, 8).unwrap();
    f.p.join_call(&f.conv, &f.bob).unwrap();

    f.p.transfer_call(&f.conv, &f.alice, &f.bob).unwrap();
    let st = f.p.call_state(&f.conv, &f.carol).unwrap().unwrap();
    assert_eq!(st.host.as_deref(), Some(f.bob.as_str()));
    // 原主持人已无锁门权
    assert!(f.p.lock_call(&f.conv, &f.alice, true).is_err());
    // 新主持人可以
    f.p.lock_call(&f.conv, &f.bob, true).unwrap();
}

#[test]
fn last_participant_leaving_ends_the_room() {
    let f = fixture();
    f.p.start_call(&f.conv, &f.alice, 8).unwrap();
    assert_eq!(f.p.calls().len(), 1);

    f.p.leave_call(&f.conv, &f.alice).unwrap();
    assert_eq!(f.p.calls().len(), 0, "最后一人离开即结束");
    assert!(f.p.call_state(&f.conv, &f.bob).unwrap().is_none());
}

#[test]
fn hand_raise_is_visible_in_state() {
    let f = fixture();
    f.p.start_call(&f.conv, &f.alice, 8).unwrap();
    f.p.join_call(&f.conv, &f.bob).unwrap();

    f.p.hand_in_call(&f.conv, &f.bob, true).unwrap();
    let st = f.p.call_state(&f.conv, &f.alice).unwrap().unwrap();
    let bob = st.participants.iter().find(|p| p.user == f.bob).unwrap();
    assert!(bob.hand_raised);
}

#[test]
fn egress_endpoint_returns_probe_plan_without_subscriptions() {
    let f = fixture();
    f.p.start_call(&f.conv, &f.alice, 8).unwrap();
    f.p.join_call(&f.conv, &f.bob).unwrap();

    let plan = f.p.call_egress(&f.conv, &f.bob, 1_000_000).unwrap();
    assert!(plan.tracks.is_empty());
    assert_eq!(plan.bwe.0, wxlike_rtc::sfu::NO_TRACK_BWE_CURRENT);
}

#[test]
fn capacity_is_enforced() {
    let f = fixture();
    f.p.start_call(&f.conv, &f.alice, 2).unwrap();
    f.p.join_call(&f.conv, &f.bob).unwrap();
    assert!(f.p.join_call(&f.conv, &f.carol).is_err(), "满员拒绝");
}

// -- 通话信令中继（offer/answer/ICE）：媒体面协商的前置 ----------------------

use wxlike_rtc::signal::{Signal, SignalContent};

fn invite(call_id: &str, from: &str, to: &str) -> Signal {
    Signal::new(
        call_id,
        from,
        to,
        SignalContent::Invite {
            sdp: serde_json::json!({"type": "offer", "sdp": "v=0"}),
            video: true,
        },
    )
}

#[test]
fn signals_relay_between_participants() {
    let f = fixture();
    f.p.start_call(&f.conv, &f.alice, 8).unwrap();
    f.p.join_call(&f.conv, &f.bob).unwrap();

    // alice 发 offer 给 bob
    let dropped =
        f.p.post_call_signal(&f.conv, &f.alice, invite("c1", &f.alice, &f.bob))
            .unwrap();
    assert_eq!(dropped, 0);

    let got = f.p.take_call_signals(&f.conv, &f.bob).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].call_id, "c1");
    assert_eq!(got[0].from, f.alice);

    // 读即清：再取一次为空
    assert!(f.p.take_call_signals(&f.conv, &f.bob).unwrap().is_empty());
    // alice 自己队列里没有东西
    assert!(f.p.take_call_signals(&f.conv, &f.alice).unwrap().is_empty());
}

#[test]
fn forged_sender_is_overwritten() {
    let f = fixture();
    f.p.start_call(&f.conv, &f.alice, 8).unwrap();
    f.p.join_call(&f.conv, &f.bob).unwrap();

    // bob 谎称自己是 alice
    let mut forged = invite("c1", &f.alice, &f.alice);
    forged.from = f.alice.clone();
    f.p.post_call_signal(&f.conv, &f.bob, forged).unwrap();
    let got = f.p.take_call_signals(&f.conv, &f.alice).unwrap();
    assert_eq!(got[0].from, f.bob, "from 必须被改写成真实发送者");
}

#[test]
fn signals_rejected_for_outsiders_and_absent_recipients() {
    let f = fixture();
    f.p.start_call(&f.conv, &f.alice, 8).unwrap();
    f.p.join_call(&f.conv, &f.bob).unwrap();

    // 收件人不在房里
    assert!(f
        .p
        .post_call_signal(&f.conv, &f.alice, invite("c1", &f.alice, &f.carol))
        .is_err());
    // 群外用户发不了
    assert!(f
        .p
        .post_call_signal(&f.conv, &f.dave, invite("c1", &f.dave, &f.alice))
        .is_err());
    // 非成员取不到（会先被成员判定挡住）
    assert!(f.p.take_call_signals(&f.conv, &f.dave).is_err());
}

#[test]
fn signal_queue_is_bounded_and_drops_oldest() {
    let f = fixture();
    f.p.start_call(&f.conv, &f.alice, 8).unwrap();
    f.p.join_call(&f.conv, &f.bob).unwrap();

    let mut dropped = 0;
    for i in 0..70 {
        let mut sig = invite(&format!("c{i}"), &f.alice, &f.bob);
        sig.content = SignalContent::Candidate {
            sdp_mid: "0".into(),
            sdp_mline_index: 0,
            candidate: format!("cand-{i}"),
        };
        dropped += f.p.post_call_signal(&f.conv, &f.alice, sig).unwrap();
    }
    assert_eq!(dropped, 6, "70 条塞进 64 的队列，丢 6 条最旧");
    let got = f.p.take_call_signals(&f.conv, &f.bob).unwrap();
    assert_eq!(got.len(), 64);
    // 留下的是最新的那批
    match &got[0].content {
        SignalContent::Candidate { candidate, .. } => assert_eq!(candidate, "cand-6"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn leaving_clears_pending_signals() {
    let f = fixture();
    f.p.start_call(&f.conv, &f.alice, 8).unwrap();
    f.p.join_call(&f.conv, &f.bob).unwrap();
    f.p.post_call_signal(&f.conv, &f.alice, invite("c1", &f.alice, &f.bob))
        .unwrap();

    f.p.leave_call(&f.conv, &f.bob).unwrap();
    f.p.join_call(&f.conv, &f.bob).unwrap();
    assert!(
        f.p.take_call_signals(&f.conv, &f.bob).unwrap().is_empty(),
        "离房即丢弃积压，避免重连时读到陈旧 offer"
    );
}
