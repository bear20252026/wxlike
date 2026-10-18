//! 通话房间契约：名册、管控动作、权限边界、主持人移交。

use wxlike_rtc::room::{Role, Room, RoomError, Track};

const T0: u64 = 1_700_000_000_000;

fn room() -> Room {
    Room::new("r1", "g1", "host", 4, T0).unwrap()
}

#[test]
fn join_and_roster() {
    let mut r = room();
    r.join("bob", T0 + 1).unwrap();
    r.join("carol", T0 + 2).unwrap();
    assert_eq!(r.len(), 3);
    assert_eq!(r.host(), "host");
    assert_eq!(r.participant("host").unwrap().role, Role::Host);
    assert_eq!(r.participant("bob").unwrap().role, Role::Member);
    assert_eq!(r.roster().len(), 3);

    assert_eq!(r.join("bob", T0 + 3).unwrap_err(), RoomError::AlreadyJoined);
}

#[test]
fn capacity_is_enforced() {
    let mut r = Room::new("r2", "g1", "host", 2, T0).unwrap();
    r.join("bob", T0 + 1).unwrap();
    assert_eq!(r.join("carol", T0 + 2).unwrap_err(), RoomError::Full(2));
    assert_eq!(r.len(), 2);
}

#[test]
fn locked_room_refuses_members() {
    let mut r = room();
    r.join("bob", T0 + 1).unwrap();
    r.set_locked("host", true).unwrap();
    assert!(r.is_locked());
    assert_eq!(r.join("carol", T0 + 2).unwrap_err(), RoomError::Locked);

    // 主持人仍可解除
    r.set_locked("host", false).unwrap();
    r.join("carol", T0 + 3).unwrap();
}

#[test]
fn only_host_can_lock_or_record() {
    let mut r = room();
    r.join("bob", T0 + 1).unwrap();
    r.set_role("host", "bob", Role::Moderator).unwrap();

    assert_eq!(r.set_locked("bob", true).unwrap_err(), RoomError::Forbidden);
    assert_eq!(
        r.set_recording("bob", true).unwrap_err(),
        RoomError::Forbidden
    );

    r.set_recording("host", true).unwrap();
    assert!(r.is_recording());
    r.set_recording("host", false).unwrap();
    assert!(!r.is_recording());
}

#[test]
fn mute_self_is_allowed_but_muting_others_needs_role() {
    let mut r = room();
    r.join("bob", T0 + 1).unwrap();
    r.join("carol", T0 + 2).unwrap();

    // 本人可静自己
    assert!(r.set_mute("bob", "bob", Track::Audio, true).unwrap());
    assert!(r.participant("bob").unwrap().audio_muted);
    // 视频独立
    assert!(!r.participant("bob").unwrap().video_muted);

    // 普通成员不能静别人
    assert_eq!(
        r.set_mute("bob", "carol", Track::Audio, true).unwrap_err(),
        RoomError::Forbidden
    );

    // 主持人可以
    r.set_mute("host", "carol", Track::Video, true).unwrap();
    assert!(r.participant("carol").unwrap().video_muted);
}

#[test]
fn kick_removes_and_blocks_rejoin() {
    let mut r = room();
    r.join("bob", T0 + 1).unwrap();
    r.join("carol", T0 + 2).unwrap();

    // 成员不能踢人
    assert_eq!(r.kick("bob", "carol").unwrap_err(), RoomError::Forbidden);

    r.kick("host", "carol").unwrap();
    assert!(r.participant("carol").is_none());
    assert_eq!(r.len(), 2);
    // 被踢者不可再进
    assert_eq!(r.join("carol", T0 + 3).unwrap_err(), RoomError::Banned);

    // 主持人不可被踢
    r.set_role("host", "bob", Role::Moderator).unwrap();
    assert_eq!(
        r.kick("bob", "host").unwrap_err(),
        RoomError::CannotKickHost
    );
}

#[test]
fn moderator_cannot_kick_moderator() {
    let mut r = room();
    r.join("bob", T0 + 1).unwrap();
    r.join("carol", T0 + 2).unwrap();
    r.set_role("host", "bob", Role::Moderator).unwrap();
    r.set_role("host", "carol", Role::Moderator).unwrap();
    assert_eq!(r.kick("bob", "carol").unwrap_err(), RoomError::Forbidden);
    // 主持人可以踢管理员
    r.kick("host", "carol").unwrap();
    assert!(r.participant("carol").is_none());
}

#[test]
fn host_leave_transfers_to_moderator_first() {
    let mut r = room();
    r.join("bob", T0 + 1).unwrap();
    r.join("carol", T0 + 2).unwrap();
    r.join("dave", T0 + 3).unwrap();
    r.set_role("host", "carol", Role::Moderator).unwrap();

    let out = r.leave("host").unwrap();
    assert!(!out.ended);
    assert_eq!(out.new_host.as_deref(), Some("carol"));
    assert_eq!(r.host(), "carol");
    assert_eq!(r.participant("carol").unwrap().role, Role::Host);
}

#[test]
fn host_leave_without_moderator_falls_back_to_earliest_member() {
    let mut r = room();
    r.join("bob", T0 + 5).unwrap();
    r.join("carol", T0 + 1).unwrap(); // 更早加入但 id 排序在后
    let out = r.leave("host").unwrap();
    assert_eq!(out.new_host.as_deref(), Some("carol"));
    assert_eq!(r.host(), "carol");
}

#[test]
fn last_participant_leaving_ends_the_room() {
    let mut r = Room::new("r3", "g1", "host", 4, T0).unwrap();
    let out = r.leave("host").unwrap();
    assert!(out.ended);
    assert_eq!(out.new_host, None);
    assert!(r.is_empty());
}

#[test]
fn transfer_host_demotes_previous_host() {
    let mut r = room();
    r.join("bob", T0 + 1).unwrap();
    r.transfer_host("host", "bob").unwrap();
    assert_eq!(r.host(), "bob");
    assert_eq!(r.participant("bob").unwrap().role, Role::Host);
    assert_eq!(r.participant("host").unwrap().role, Role::Moderator);
    // 原主持人已无锁定权
    assert_eq!(
        r.set_locked("host", true).unwrap_err(),
        RoomError::Forbidden
    );
}

#[test]
fn hands_raise_in_join_order() {
    let mut r = room();
    r.join("bob", T0 + 1).unwrap();
    r.join("carol", T0 + 2).unwrap();
    r.raise_hand("carol", true).unwrap();
    r.raise_hand("bob", true).unwrap();
    let queue: Vec<&str> = r.hands_raised().iter().map(|p| p.user.as_str()).collect();
    assert_eq!(queue, vec!["bob", "carol"]);
    r.raise_hand("bob", false).unwrap();
    assert_eq!(r.hands_raised().len(), 1);
}

#[test]
fn bad_capacity_is_refused() {
    assert_eq!(
        Room::new("r", "", "h", 1, T0).unwrap_err(),
        RoomError::BadCapacity
    );
    assert_eq!(
        Room::new("r", "", "h", 1001, T0).unwrap_err(),
        RoomError::BadCapacity
    );
}
