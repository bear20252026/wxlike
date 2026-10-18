//! 出口码率分配契约。
//!
//! 前三项是从 atm0s-media-server（MIT）的 `egress.rs` 单测**移植**过来的
//! 等价用例（同样的输入、同样的期望值），用来证明重写没有改变算法语义；
//! 其余是我们在房间层新增的用例。

use wxlike_rtc::room::Room;
use wxlike_rtc::sfu::{
    EgressBitrateAllocator, Output, DEFAULT_BITRATE_BPS, DEFAULT_MAX_EGRESS_BPS,
    NO_TRACK_BWE_CURRENT, NO_TRACK_BWE_DESIRED,
};

const MAX_BW: u64 = 2_500_000;

#[test]
fn no_source_uses_probe_bitrates() {
    // 移植自 atm0s `no_source`
    let mut a = EgressBitrateAllocator::new(MAX_BW);
    a.set_egress_estimate(200_000);
    a.on_tick();
    assert_eq!(
        a.pop_output(),
        Some(Output::BweConfig(
            NO_TRACK_BWE_CURRENT,
            NO_TRACK_BWE_DESIRED
        ))
    );
    assert_eq!(a.pop_output(), None);
}

#[test]
fn single_source_gets_default_then_caps() {
    // 移植自 atm0s `single_source`
    let mut a = EgressBitrateAllocator::new(MAX_BW);
    a.set_video_track(0, 1);
    a.on_tick();
    assert_eq!(
        a.pop_output(),
        Some(Output::Track(
            0,
            wxlike_rtc::sfu::Action::SetBitrate(DEFAULT_BITRATE_BPS)
        ))
    );
    assert_eq!(
        a.pop_output(),
        Some(Output::BweConfig(
            DEFAULT_BITRATE_BPS,
            DEFAULT_BITRATE_BPS * 6 / 5
        ))
    );
    assert_eq!(a.pop_output(), None);

    // 估算超过上限 → 封顶
    a.set_egress_estimate(MAX_BW + 200_000);
    a.on_tick();
    assert_eq!(
        a.pop_output(),
        Some(Output::Track(
            0,
            wxlike_rtc::sfu::Action::SetBitrate(MAX_BW)
        ))
    );
    assert_eq!(a.pop_output(), Some(Output::BweConfig(MAX_BW, MAX_BW)));
    assert_eq!(a.pop_output(), None);
}

#[test]
fn multi_source_splits_by_weight() {
    // 移植自 atm0s `multi_source`：1:3 的权重分 800k → 200k / 600k
    let mut a = EgressBitrateAllocator::new(MAX_BW);
    a.set_video_track(0, 1);
    a.set_video_track(1, 3);
    a.on_tick();
    assert_eq!(
        a.pop_output(),
        Some(Output::Track(
            0,
            wxlike_rtc::sfu::Action::SetBitrate(DEFAULT_BITRATE_BPS / 4)
        ))
    );
    assert_eq!(
        a.pop_output(),
        Some(Output::Track(
            1,
            wxlike_rtc::sfu::Action::SetBitrate(DEFAULT_BITRATE_BPS * 3 / 4)
        ))
    );
    assert_eq!(
        a.pop_output(),
        Some(Output::BweConfig(
            DEFAULT_BITRATE_BPS,
            DEFAULT_BITRATE_BPS * 6 / 5
        ))
    );
    assert_eq!(a.pop_output(), None);
}

#[test]
fn recompute_only_when_something_changed() {
    let mut a = EgressBitrateAllocator::new(MAX_BW);
    a.set_video_track(0, 1);
    a.on_tick();
    while a.pop_output().is_some() {}
    // 没有变化 → 第二次 tick 不产出（避免每帧刷配置）
    a.on_tick();
    assert_eq!(a.pop_output(), None);
}

#[test]
fn zero_weights_produce_no_track_output() {
    let mut a = EgressBitrateAllocator::new(MAX_BW);
    a.set_video_track(0, 0);
    a.on_tick();
    // 权重和为 0：不分配码率，只给 BWE
    assert_eq!(
        a.pop_output(),
        Some(Output::BweConfig(
            DEFAULT_BITRATE_BPS,
            DEFAULT_BITRATE_BPS * 6 / 5
        ))
    );
    assert_eq!(a.pop_output(), None);
}

// ---------------------------------------------------------------------------
// 房间层集成
// ---------------------------------------------------------------------------

fn room() -> Room {
    let mut r = Room::new("r1", "g1", "host", 8, 1_700_000_000_000).unwrap();
    r.join("bob", 1_700_000_000_001).unwrap();
    r
}

#[test]
fn viewer_subscriptions_split_its_egress() {
    let mut r = room();
    r.subscribe_video("bob", 7, 3).unwrap(); // 主讲
    r.subscribe_video("bob", 9, 1).unwrap(); // 普通
    let plan = r.allocate_egress("bob", 2_400_000).unwrap();
    assert_eq!(plan.tracks, vec![(7, 1_800_000), (9, 600_000)]);
    assert_eq!(plan.bwe, (2_400_000, 2_500_000));
}

#[test]
fn unsubscribing_reallocates_remaining_tracks() {
    let mut r = room();
    r.subscribe_video("bob", 7, 1).unwrap();
    r.subscribe_video("bob", 9, 1).unwrap();
    r.allocate_egress("bob", 800_000).unwrap();

    r.unsubscribe_video("bob", 7).unwrap();
    let plan = r.allocate_egress("bob", 800_000).unwrap();
    assert_eq!(plan.tracks, vec![(9, 800_000)], "退订后整份预算给剩下的轨");
}

#[test]
fn viewer_without_subscriptions_gets_probe_bwe() {
    let mut r = room();
    let plan = r.allocate_egress("bob", 1_000_000).unwrap();
    assert!(plan.tracks.is_empty());
    assert_eq!(plan.bwe, (NO_TRACK_BWE_CURRENT, NO_TRACK_BWE_DESIRED));
}

#[test]
fn leaving_or_kicked_viewer_loses_its_allocator() {
    let mut r = room();
    r.subscribe_video("bob", 7, 1).unwrap();
    r.leave("bob").unwrap();
    assert_eq!(
        r.allocate_egress("bob", 800_000).unwrap_err(),
        wxlike_rtc::room::RoomError::NotMember
    );

    let mut r2 = room();
    r2.join("carol", 1_700_000_000_002).unwrap();
    r2.subscribe_video("carol", 7, 1).unwrap();
    r2.kick("host", "carol").unwrap();
    assert_eq!(
        r2.allocate_egress("carol", 800_000).unwrap_err(),
        wxlike_rtc::room::RoomError::NotMember
    );
}

#[test]
fn default_budget_is_the_documented_cap() {
    let mut r = room();
    r.subscribe_video("bob", 1, 1).unwrap();
    // 估算远超默认上限 → 按默认上限分配
    let plan = r.allocate_egress("bob", 99_000_000).unwrap();
    assert_eq!(plan.tracks, vec![(1, DEFAULT_MAX_EGRESS_BPS)]);
}
