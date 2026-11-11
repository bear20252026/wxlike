//! ADR-25：移植自 LiveKit 官方参考库的房间/参与者状态机验收。
//!
//! 每条断言旁边注的是上游出处（`livekit/rust-sdks` @ `052f089166b6…`、
//! `livekit/livekit` @ `94edd5da7524…`）。改上游行为等于改这些测试。
//! 登记见 `third_party/reviews/registry.json` 的 `UP-LIVEKIT-RUSTSDK`。

use wxlike_rtc::recording::server_egress_allowed;
use wxlike_rtc::room::{
    plan_participant_events, ConnectionStateTracker, DisconnectReason, MemberAction, MemberCommand,
    MemberError, MemberState, Room, RoomCloseReason, RoomError, SidecarConnectionState,
    SidecarParticipantState, SidecarRoomEvent, SidecarStateEffect,
};
use wxlike_rtc::token::{
    assert_participant_kind_allowed, assert_sidecar_grant_allowed, AccessRequest, ApiKeyPair,
    LiveKitIssuer, MediaScope, TokenError, VideoGrant,
};

const T0: u64 = 1_700_000_000_000;

fn livekit_room() -> Room {
    Room::livekit("rl", "g1", "host", 4, T0).unwrap()
}

fn join(r: &mut Room, user: &str, device: &str, at: u64) {
    let revision = r.revision();
    r.member_command(&MemberCommand {
        actor: user.to_string(),
        action: MemberAction::Join {
            device: device.to_string(),
        },
        expected_revision: Some(revision),
        at_ms: at,
    })
    .expect("join 应当被接受");
}

#[test]
fn sidecar_state_only_moves_forward() {
    // 上游 `livekit.ParticipantInfo_State` 的数值序就是可比较的进度序，
    // `updateState` 的 `state <= oldState` 判定完全依赖它。
    assert!(SidecarParticipantState::Joining < SidecarParticipantState::Joined);
    assert!(SidecarParticipantState::Joined < SidecarParticipantState::Active);
    assert!(SidecarParticipantState::Active < SidecarParticipantState::Disconnected);
    assert_eq!(SidecarParticipantState::Joining.tag(), 0);
    assert_eq!(SidecarParticipantState::Disconnected.tag(), 3);
    assert_eq!(SidecarParticipantState::Active.as_str_name(), "ACTIVE");
    assert_eq!(
        SidecarParticipantState::from_str_name("JOINED"),
        Some(SidecarParticipantState::Joined)
    );
    assert_eq!(SidecarParticipantState::from_str_name("ACTIVE-ish"), None);

    assert_eq!(
        SidecarParticipantState::advance(
            SidecarParticipantState::Active,
            SidecarParticipantState::Joined
        ),
        None,
        "迟到的 JOINED 不能把 ACTIVE 打回去"
    );
    assert_eq!(
        SidecarParticipantState::advance(
            SidecarParticipantState::Active,
            SidecarParticipantState::Active
        ),
        None,
        "重复的 ACTIVE 也不是变更"
    );
    assert_eq!(
        SidecarParticipantState::advance(
            SidecarParticipantState::Joining,
            SidecarParticipantState::Joined
        ),
        Some(SidecarParticipantState::Joining)
    );
    // DISCONNECTED 是终态：之后的 ACTIVE 不能复活一个已结束的会话。
    assert_eq!(
        SidecarParticipantState::advance(
            SidecarParticipantState::Disconnected,
            SidecarParticipantState::Active
        ),
        None
    );
    // 上游 `IsReady()` 的非迁移分支（participant.go:632-641）。
    assert!(!SidecarParticipantState::Joining.is_ready());
    assert!(SidecarParticipantState::Joined.is_ready());
    assert!(SidecarParticipantState::Active.is_ready());
    assert!(!SidecarParticipantState::Disconnected.is_ready());
    assert!(SidecarParticipantState::Disconnected.is_disconnected());
}

#[test]
fn late_sidecar_reports_are_ignored_without_touching_state() {
    let mut r = livekit_room();
    join(&mut r, "bob", "phone", T0 + 1);
    assert_eq!(
        r.sidecar_state("bob", "phone"),
        Some(SidecarParticipantState::Joining),
        "上游 ParticipantImpl 的初始状态就是 JOINING（participant.go:443）"
    );

    let rev_after_active = {
        let a = r
            .report_sidecar_state("bob", "phone", SidecarParticipantState::Active, T0 + 2)
            .unwrap();
        assert!(a.applied);
        assert_eq!(
            a.effects,
            vec![
                SidecarStateEffect::SubscribeExistingTracks,
                SidecarStateEffect::ReportActive
            ],
            "上游 onStateChange 的 ACTIVE 分支：先订阅既有轨道，再上报指标"
        );
        assert!(r.participant("bob").unwrap().holds_media(Some("phone")));
        r.revision()
    };

    // 乱序：迟到的 JOINED。状态与水印都不许动。
    let ignored = r
        .report_sidecar_state("bob", "phone", SidecarParticipantState::Joined, T0 + 3)
        .unwrap();
    assert!(!ignored.applied);
    assert!(ignored.effects.is_empty());
    assert_eq!(ignored.current, SidecarParticipantState::Active);
    assert_eq!(r.revision(), rev_after_active, "被忽略的报告不许推进水印");
    let audit = r.audit_tail(1);
    assert_eq!(
        audit[0].code.as_deref(),
        Some("out_of_order_sidecar_state"),
        "忽略也要留痕，否则排障看不到"
    );

    // 断线：进入 DISCONNECTED，我们的席位落到宽限期而不是立刻移除。
    let d = r
        .report_sidecar_state(
            "bob",
            "phone",
            SidecarParticipantState::Disconnected,
            T0 + 4,
        )
        .unwrap();
    assert!(d.applied);
    assert_eq!(d.effects, vec![SidecarStateEffect::BeginReconnectGrace]);
    assert_eq!(
        r.participant("bob").unwrap().state,
        MemberState::Reconnecting,
        "ADR-25 刻意的偏离：上游立即 RemoveParticipant，我们保留名册到宽限期"
    );
    assert_eq!(d.previous, SidecarParticipantState::Active);
    // 上游只对 ACTIVE→DISCONNECTED 记完整会话（participant.go:2354）。
    assert!(SidecarParticipantState::Disconnected
        .is_clean_disconnect_from(SidecarParticipantState::Active));
    assert!(!SidecarParticipantState::Disconnected
        .is_clean_disconnect_from(SidecarParticipantState::Joining));
    assert_eq!(
        SidecarParticipantState::Joined.to_member_state(),
        MemberState::Joining
    );
    assert_eq!(
        SidecarParticipantState::from_member_state(MemberState::Removed),
        SidecarParticipantState::Disconnected,
        "上游没有 REMOVED 这个状态，只有 DISCONNECTED + 关闭原因"
    );
}

#[test]
fn unknown_sidecar_seat_and_missing_member_are_named_not_folded() {
    let mut r = livekit_room();
    join(&mut r, "bob", "phone", T0 + 1);
    assert_eq!(
        r.report_sidecar_state("bob", "tablet", SidecarParticipantState::Active, T0 + 2),
        Err(MemberError::NoSuchDevice("tablet".to_string()))
    );
    assert_eq!(
        r.report_sidecar_state("mallory", "phone", SidecarParticipantState::Active, T0 + 2),
        Err(MemberError::NotMember)
    );
    assert_eq!(r.sidecar_state("mallory", "phone"), None);
}

#[test]
fn reconnect_resets_the_sidecar_clock_so_rejoin_can_succeed() {
    let mut r = livekit_room();
    join(&mut r, "bob", "phone", T0 + 1);
    r.report_sidecar_state("bob", "phone", SidecarParticipantState::Active, T0 + 2)
        .unwrap();
    r.report_sidecar_state(
        "bob",
        "phone",
        SidecarParticipantState::Disconnected,
        T0 + 3,
    )
    .unwrap();
    // 上游重连是新建一个 ParticipantImpl（状态表从 JOINING 重来）；我们复用
    // 席位，所以 Reconnect 必须显式重置，否则新的 JOINING 会被当乱序丢弃。
    let rev = r.revision();
    r.member_command(&MemberCommand {
        actor: "bob".to_string(),
        action: MemberAction::Reconnect {
            device: "phone".to_string(),
        },
        expected_revision: Some(rev),
        at_ms: T0 + 4,
    })
    .unwrap();
    assert_eq!(
        r.sidecar_state("bob", "phone"),
        Some(SidecarParticipantState::Joining)
    );
    let a = r
        .report_sidecar_state("bob", "phone", SidecarParticipantState::Active, T0 + 5)
        .unwrap();
    assert!(a.applied, "重连后又被当成乱序 = 重连永远成功不了");
    assert_eq!(a.previous, SidecarParticipantState::Joining);
}

#[test]
fn expired_grace_reclaims_the_seat_through_tick() {
    let mut r = livekit_room().with_reconnect_grace(10_000);
    join(&mut r, "bob", "phone", T0 + 1);
    r.report_sidecar_state("bob", "phone", SidecarParticipantState::Active, T0 + 2)
        .unwrap();
    r.report_sidecar_state(
        "bob",
        "phone",
        SidecarParticipantState::Disconnected,
        T0 + 3,
    )
    .unwrap();
    assert_eq!(r.reconnect_grace_ms(), 10_000);
    // 宽限期内不掉席位：客户端网络抖一下不该被踢出名册。
    // 断线发生在 T0+3，宽限到 T0+10_003，所以 T0+9_000 还在窗口内。
    assert!(r.tick(T0 + 9_000).is_empty());
    assert_eq!(r.len(), 2);
    // 到点即回收。
    assert_eq!(r.tick(T0 + 10_003), vec!["bob".to_string()]);
    assert_eq!(r.len(), 1);
    assert_eq!(r.participant("bob").map(|p| p.user.clone()), None);
}

#[test]
fn room_close_is_idempotent_and_agrees_on_the_reason() {
    let mut r = livekit_room();
    join(&mut r, "bob", "phone", T0 + 1);
    r.report_sidecar_state("bob", "phone", SidecarParticipantState::Active, T0 + 2)
        .unwrap();
    assert!(!r.is_closed());
    assert!(r.close(RoomCloseReason::ApiDelete, T0 + 3));
    assert!(r.is_closed());
    assert_eq!(r.close_reason(), Some(RoomCloseReason::ApiDelete));
    // 上游 `Close` 先 select 已关即 return：重复关闭不产生第二份审计。
    let pending_before = r.pending_audit();
    assert!(
        !r.close(RoomCloseReason::ServerShutdown, T0 + 4),
        "关闭必须幂等"
    );
    assert_eq!(r.close_reason(), Some(RoomCloseReason::ApiDelete));
    assert_eq!(r.pending_audit(), pending_before);
    // 房间侧与参与者侧的原因必须是同一次决定的两个投影。
    assert_eq!(
        RoomCloseReason::ApiDelete.to_disconnect_reason(),
        DisconnectReason::RoomDeleted
    );
    assert_eq!(
        RoomCloseReason::IdleTimeout.to_disconnect_reason(),
        DisconnectReason::RoomClosed
    );
    assert_eq!(
        RoomCloseReason::Superseded.to_disconnect_reason(),
        DisconnectReason::RoomClosed
    );
    assert_eq!(
        RoomCloseReason::ServerShutdown.to_disconnect_reason(),
        DisconnectReason::ServerShutdown
    );
    assert_eq!(
        RoomCloseReason::OpenFailed.to_disconnect_reason(),
        DisconnectReason::UnknownReason
    );
    let audit = r.audit_tail(6);
    assert!(
        audit
            .iter()
            .any(|e| e.detail.as_deref() == Some("ROOM_DELETED")),
        "每个被一起关掉的参与者都要看到参与侧原因"
    );
    assert!(audit
        .iter()
        .any(|e| e.detail.as_deref() == Some("API_DELETE")));
    assert!(!r.participant("bob").unwrap().holds_media(Some("phone")));
}

#[test]
fn closed_room_refuses_commands_mints_and_sidecar_reports() {
    let mut r = livekit_room();
    join(&mut r, "bob", "phone", T0 + 1);
    r.close(RoomCloseReason::IdleTimeout, T0 + 2);
    let rev = r.revision();
    assert_eq!(
        r.member_command(&MemberCommand {
            actor: "bob".to_string(),
            action: MemberAction::Leave {
                device: "phone".to_string()
            },
            expected_revision: Some(rev),
            at_ms: T0 + 3,
        }),
        Err(MemberError::RoomClosed(RoomCloseReason::IdleTimeout))
    );
    assert_eq!(
        r.report_sidecar_state("bob", "phone", SidecarParticipantState::Active, T0 + 3),
        Err(MemberError::RoomClosed(RoomCloseReason::IdleTimeout))
    );
    assert_eq!(
        r.prepare_access("bob", "phone", "c1", T0 + 3),
        Err(MemberError::RoomClosed(RoomCloseReason::IdleTimeout))
    );
    // 匿名 actor 的命令也先撞关闭闸（不是先报 BadInput 再放行）。
    assert_eq!(
        r.member_command(&MemberCommand {
            actor: String::new(),
            action: MemberAction::Hand { raised: true },
            expected_revision: None,
            at_ms: T0 + 4,
        }),
        Err(MemberError::RoomClosed(RoomCloseReason::IdleTimeout))
    );
    // 结论码稳定、可反解，前端与运维不用猜。
    assert_eq!(
        MemberError::RoomClosed(RoomCloseReason::IdleTimeout).code(),
        wxlike_rtc::room::MemberErrorCode::RoomClosed
    );
    assert_eq!(
        MemberError::OutOfOrderSidecarState {
            current: SidecarParticipantState::Active,
            next: SidecarParticipantState::Joined,
        }
        .code(),
        wxlike_rtc::room::MemberErrorCode::OutOfOrderSidecarState
    );
    // ADR-25：房间关了也不许借此打开服务端 Egress。
    assert_eq!(
        r.request_server_egress("host", T0 + 5),
        Err(wxlike_rtc::room::EgressError::Disabled)
    );
    assert!(!server_egress_allowed());
}

#[test]
fn close_if_empty_prefers_departure_timeout_after_someone_left() {
    // 上游 `CloseIfEmpty` 只在"没有任何独立成员在场"时才计时；所以这里把
    // 留在房里的人都标成依附（等价于"只剩 recorder/agent 连接"）。
    let mut r = livekit_room().with_room_timeouts(300, 20);
    join(&mut r, "bob", "phone", T0 + 1);
    assert!(r.mark_dependent("host", true) && r.mark_dependent("bob", true));
    assert_eq!(r.room_timeouts(), (300, 20));
    // 还没人离开过：走 empty_timeout（300 秒）。
    assert_eq!(r.close_if_empty(T0 + 100_000), None);
    assert_eq!(
        r.close_if_empty(T0 + 301_000),
        Some(RoomCloseReason::IdleTimeout)
    );
    // 已经关过的房间不再关第二次。
    assert_eq!(r.close_if_empty(T0 + 900_000), None);

    // 有人离开过：从 last_left 起算、用 departure_timeout（20 秒）。
    let mut r2 = livekit_room().with_room_timeouts(300, 20);
    join(&mut r2, "bob", "phone", T0 + 1);
    assert!(r2.mark_dependent("host", true));
    let rev = r2.revision();
    r2.member_command(&MemberCommand {
        actor: "bob".to_string(),
        action: MemberAction::Leave {
            device: "phone".to_string(),
        },
        expected_revision: Some(rev),
        at_ms: T0 + 1_000,
    })
    .unwrap();
    assert_eq!(r2.close_if_empty(T0 + 1_000 + 19_000), None);
    assert_eq!(
        r2.close_if_empty(T0 + 1_000 + 20_000),
        Some(RoomCloseReason::IdleTimeout)
    );
    let audit = r2.audit_tail(3);
    assert!(audit
        .iter()
        .any(|e| e.detail.as_deref() == Some("departure_timeout")));
}

#[test]
fn dependent_participants_do_not_keep_the_room_alive() {
    // 上游 `CloseIfEmpty` 会跳过 `IsDependent()` 的参与者；否则一个录制器
    // 会话会让房间永远关不掉（participant.go:615-622 + room.go:771-775）。
    let mut r = livekit_room().with_room_timeouts(0, 0);
    join(&mut r, "bob", "phone", T0 + 1);
    assert!(r.mark_dependent("bob", true));
    assert!(r.is_dependent("bob"));
    // host 还是独立成员，所以房间不空。
    assert_eq!(r.close_if_empty(T0 + 5_000), None);
    assert!(r.mark_dependent("host", true));
    assert_eq!(
        r.close_if_empty(T0 + 5_000),
        Some(RoomCloseReason::IdleTimeout)
    );
    // 不存在的人不能标记依附。
    let mut r2 = livekit_room();
    assert!(!r2.mark_dependent("mallory", true));
    assert!(
        !r2.is_dependent("mallory"),
        "认不出来的人按独立处理：宁可晚关，不可错关"
    );
}

#[test]
fn participant_event_order_matches_the_upstream_client() {
    // 1. 不认识 + DISCONNECTED：上游直接 Ignore，什么都不发。
    assert!(
        plan_participant_events(false, false, SidecarParticipantState::Disconnected, "bob")
            .is_empty()
    );
    // 2. 已认识、非 Active → Active：只发边沿一条 ParticipantActive。
    assert_eq!(
        plan_participant_events(true, false, SidecarParticipantState::Active, "bob"),
        vec![
            SidecarRoomEvent::ParticipantActive("bob".to_string()),
            SidecarRoomEvent::ParticipantsUpdated(vec!["bob".to_string()]),
        ]
    );
    // 3. 已经 Active 又报一次：不重复发 ParticipantActive（边沿而非电平）。
    assert_eq!(
        plan_participant_events(true, true, SidecarParticipantState::Active, "bob"),
        vec![SidecarRoomEvent::ParticipantsUpdated(vec![
            "bob".to_string()
        ])]
    );
    // 4. 新人且一进来就是 ACTIVE：Connected 之后紧跟一条 Active。
    assert_eq!(
        plan_participant_events(false, false, SidecarParticipantState::Active, "bob"),
        vec![
            SidecarRoomEvent::ParticipantConnected("bob".to_string()),
            SidecarRoomEvent::ParticipantActive("bob".to_string()),
            SidecarRoomEvent::ParticipantsUpdated(vec!["bob".to_string()]),
        ]
    );
    // 5. 新人还在 JOINING：只有 Connected + 一批更新。
    assert_eq!(
        plan_participant_events(false, false, SidecarParticipantState::Joining, "bob"),
        vec![
            SidecarRoomEvent::ParticipantConnected("bob".to_string()),
            SidecarRoomEvent::ParticipantsUpdated(vec!["bob".to_string()]),
        ]
    );
    // 6. 已认识的人断线：只发 Disconnected。上游那一支没有把参与者 push 进
    //    `participants`，所以批尾的 `ParticipantsUpdated` 也不发。
    assert_eq!(
        plan_participant_events(true, true, SidecarParticipantState::Disconnected, "bob"),
        vec![SidecarRoomEvent::ParticipantDisconnected(
            "bob".to_string(),
            DisconnectReason::UnknownReason
        )]
    );
}

#[test]
fn connection_state_tracker_latches_the_terminal_disconnect() {
    // 上游 `update_connection_state`：同值不发事件；DISCONNECTED 之后一律锁死。
    let mut t = ConnectionStateTracker::new();
    assert_eq!(t.state(), SidecarConnectionState::Disconnected);
    assert!(
        !t.update(SidecarConnectionState::Disconnected),
        "同值不发事件"
    );
    assert!(t.update(SidecarConnectionState::Connected));
    assert!(!t.update(SidecarConnectionState::Connected));
    assert!(t.update(SidecarConnectionState::Reconnecting));
    assert!(
        t.update(SidecarConnectionState::Connected),
        "重连成功仍可上报"
    );
    assert!(t.update(SidecarConnectionState::Disconnected));
    assert!(t.is_disconnected());
    // 终态锁存：断线后不能再"复活"，那等于两份房间事实并存。
    assert!(!t.update(SidecarConnectionState::Connected));
    assert_eq!(t.state(), SidecarConnectionState::Disconnected);
}

#[test]
fn disconnect_reason_names_round_trip_and_tags_match_proto() {
    // tag 与上游 proto 的编号必须一致（审计与遥测按数字对表）。
    let table = [
        (DisconnectReason::UnknownReason, 0, "UNKNOWN_REASON"),
        (DisconnectReason::ClientInitiated, 1, "CLIENT_INITIATED"),
        (DisconnectReason::DuplicateIdentity, 2, "DUPLICATE_IDENTITY"),
        (DisconnectReason::ServerShutdown, 3, "SERVER_SHUTDOWN"),
        (
            DisconnectReason::ParticipantRemoved,
            4,
            "PARTICIPANT_REMOVED",
        ),
        (DisconnectReason::RoomDeleted, 5, "ROOM_DELETED"),
        (DisconnectReason::StateMismatch, 6, "STATE_MISMATCH"),
        (DisconnectReason::JoinFailure, 7, "JOIN_FAILURE"),
        (DisconnectReason::Migration, 8, "MIGRATION"),
        (DisconnectReason::SignalClose, 9, "SIGNAL_CLOSE"),
        (DisconnectReason::RoomClosed, 10, "ROOM_CLOSED"),
        (DisconnectReason::UserUnavailable, 11, "USER_UNAVAILABLE"),
        (DisconnectReason::UserRejected, 12, "USER_REJECTED"),
        (DisconnectReason::SipTrunkFailure, 13, "SIP_TRUNK_FAILURE"),
        (
            DisconnectReason::ConnectionTimeout,
            14,
            "CONNECTION_TIMEOUT",
        ),
        (DisconnectReason::MediaFailure, 15, "MEDIA_FAILURE"),
        (DisconnectReason::AgentError, 16, "AGENT_ERROR"),
    ];
    for (reason, tag, name) in table {
        assert_eq!(reason.tag(), tag);
        assert_eq!(reason.as_str_name(), name);
        assert_eq!(DisconnectReason::from_str_name(name), Some(reason));
    }
    assert_eq!(DisconnectReason::from_str_name("NOT_A_REASON"), None);
    for name in [
        "UNKNOWN",
        "API_DELETE",
        "IDLE_TIMEOUT",
        "SERVER_SHUTDOWN",
        "SUPERSEDED",
        "OPEN_FAILED",
    ] {
        let reason = RoomCloseReason::from_str_name(name).unwrap();
        assert_eq!(reason.as_str_name(), name);
    }
    assert_eq!(RoomCloseReason::from_str_name("NOPE"), None);
    assert_eq!(RoomCloseReason::default(), RoomCloseReason::Unknown);
}

#[test]
fn interleaved_join_and_late_report_cannot_oversell_a_seat() {
    // 并发裁决：两个设备抢只剩一个空位的房间。
    let mut r = Room::livekit("rf", "g1", "host", 2, T0).unwrap();
    let rev = r.revision();
    r.member_command(&MemberCommand {
        actor: "bob".to_string(),
        action: MemberAction::Join {
            device: "phone".to_string(),
        },
        expected_revision: Some(rev),
        at_ms: T0 + 1,
    })
    .expect("第一个加入者拿到最后一席");
    let b = r.member_command(&MemberCommand {
        actor: "carol".to_string(),
        action: MemberAction::Join {
            device: "laptop".to_string(),
        },
        // carol 看到的还是加入前的水印：命令既过期又晚了一步。
        expected_revision: Some(rev),
        at_ms: T0 + 2,
    });
    assert!(
        matches!(b, Err(MemberError::StaleRevision { .. })),
        "旧水印的命令先被 CAS 拦下：{b:?}"
    );
    // 用最新水印重试，才是真的满员。
    let c = r.member_command(&MemberCommand {
        actor: "carol".to_string(),
        action: MemberAction::Join {
            device: "laptop".to_string(),
        },
        expected_revision: Some(r.revision()),
        at_ms: T0 + 3,
    });
    assert_eq!(c, Err(MemberError::Full(2)));
    assert_eq!(r.len(), 2, "容量没有被超卖");

    let report = r.report_sidecar_state("carol", "laptop", SidecarParticipantState::Active, T0 + 4);
    assert_eq!(
        report,
        Err(MemberError::NotMember),
        "被拒的加入连名册都不该有，更不该留下席位"
    );
    assert!(r.participant("carol").is_none());
}

#[test]
fn legacy_leave_does_not_arm_the_departure_timer_by_stealth() {
    // 冷备/旧路由用的 `Room::leave` 没有时刻参数，所以它**不**推动 departure
    // 计时器；这种房间走的是 empty_timeout。这条测试钉住那个差异，免得以后
    // 有人以为两条路径等价（切路由的需求在 integration-requests/G09 里）。
    let mut r = Room::livekit("rg", "g1", "host", 4, T0)
        .unwrap()
        .with_room_timeouts(300, 20);
    r.join("bob", T0 + 1).unwrap();
    assert!(r.mark_dependent("host", true) && r.mark_dependent("bob", true));
    r.leave("bob").unwrap();
    // departure_timeout 是 20 秒：如果旧路径也推动了离席计时器，这里就该关了。
    assert_eq!(r.close_if_empty(T0 + 25_000), None);
    assert_eq!(
        r.close_if_empty(T0 + 301_000),
        Some(RoomCloseReason::IdleTimeout)
    );
}

#[test]
fn egress_gate_is_independent_of_the_room_lifecycle() {
    // ADR-25 的录制闸与房间生命周期无关：开房、关房都签不出 recorder 令牌。
    let pair = ApiKeyPair::new("APIg09", vec![0x21u8; 32]).unwrap();
    let issuer = LiveKitIssuer::new(pair);
    let req = AccessRequest {
        room: "wxlike-call-g1-rl".to_string(),
        identity: "bob".to_string(),
        device: "phone".to_string(),
        call: "c1".to_string(),
        scope: MediaScope::Call,
        generation: 1,
        engine: "livekit".to_string(),
        ..Default::default()
    };
    assert!(issuer.issue(&req, T0).is_ok());
    for grant in [
        VideoGrant {
            recorder: true,
            ..Default::default()
        },
        VideoGrant {
            hidden: true,
            ..Default::default()
        },
    ] {
        assert_eq!(
            assert_sidecar_grant_allowed(&grant),
            Err(TokenError::ServerEgressGrantForbidden)
        );
    }
    // 房间侧：egress 会话即使被标成"依附"也换不来一个可签的 kind。
    assert_eq!(
        assert_participant_kind_allowed("egress"),
        Err(TokenError::ParticipantKindForbidden("egress".to_string()))
    );
    let mut r = livekit_room();
    join(&mut r, "bob", "phone", T0 + 1);
    assert!(r.prepare_access("bob", "phone", "c1", T0 + 2).is_ok());
    assert_eq!(
        r.request_server_egress("host", T0 + 3),
        Err(wxlike_rtc::room::EgressError::Disabled)
    );
    r.close(RoomCloseReason::ApiDelete, T0 + 4);
    assert_eq!(
        r.request_server_egress("host", T0 + 5),
        Err(wxlike_rtc::room::EgressError::Disabled)
    );
    assert!(!server_egress_allowed(), "ADR-25 之后不得有自动录制开关");
}

#[test]
fn prepare_access_still_refuses_a_non_livekit_room() {
    // 冷备房间（str0m）不能签 LiveKit 令牌，反过来也一样：引擎事实只有一份。
    let mut cold = Room::new("rc", "g1", "host", 4, T0).unwrap();
    let rev = cold.revision();
    cold.member_command(&MemberCommand {
        actor: "bob".to_string(),
        action: MemberAction::Join {
            device: "phone".to_string(),
        },
        expected_revision: Some(rev),
        at_ms: T0 + 1,
    })
    .unwrap();
    assert_eq!(
        cold.prepare_access("bob", "phone", "c1", T0 + 2),
        Err(MemberError::WrongEngine("str0m", "livekit"))
    );
    assert_eq!(cold.engine(), wxlike_rtc::room::MediaEngine::Str0m);
    // 冷备房间的出口码率分配仍在本地算（LiveKit 房间由 sidecar 自己估）。
    assert!(cold.egress_is_local());
    // 旧路由还在用 RoomError，关闭原因没有对应的变体（不能加：
    // crates/platform/src/call.rs 对该枚举做穷尽匹配，超出本组写权限）。
    assert_eq!(cold.join("host", T0 + 3), Err(RoomError::AlreadyJoined));
}
