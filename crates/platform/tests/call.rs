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
