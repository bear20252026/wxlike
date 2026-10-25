//! G09-05 直播服务端裁决的对抗测试。
//!
//! 覆盖：token scope 分离、观众计数去重、断网宽限重连、强制停播撤源、
//! 连麦/举报/封禁、播放状态机（不假装在播）。Feed 与归档用真实记录端口。

use wxlike_rtc::live_session::{
    LiveEndReason, LiveError, LiveServer, LiveSideEffects, LiveState, PlaybackState,
    LIVE_TOKEN_TTL_MS, RECONNECT_GRACE_MS,
};
use wxlike_rtc::token::{MediaScope, TokenError, TokenSigner, MIN_SECRET_LEN};

const T0: u64 = 1_000_000;

#[derive(Default)]
struct Effects {
    events: Vec<String>,
}

impl LiveSideEffects for Effects {
    fn feed_live(&mut self, stream: &str, host: &str, _now_ms: u64) {
        self.events.push(format!("feed_live:{stream}:{host}"));
    }

    fn feed_ended(&mut self, stream: &str, _now_ms: u64) {
        self.events.push(format!("feed_ended:{stream}"));
    }

    fn source_revoked(&mut self, stream: &str, _now_ms: u64) {
        self.events.push(format!("source_revoked:{stream}"));
    }

    fn archive_started(&mut self, stream: &str, _now_ms: u64) {
        self.events.push(format!("archive_started:{stream}"));
    }

    fn archive_stopped(&mut self, stream: &str, _now_ms: u64) {
        self.events.push(format!("archive_stopped:{stream}"));
    }
}

fn server() -> LiveServer<Effects> {
    let signer = TokenSigner::new(vec![0x5A; MIN_SECRET_LEN]).unwrap();
    LiveServer::new(signer, Effects::default())
}

fn start(server: &mut LiveServer<Effects>) -> String {
    server
        .start("alice", "live-1", "conv-1", T0)
        .unwrap()
        .publish_token
}

#[test]
fn start_is_idempotent_and_publishes_the_feed_once() {
    let mut s = server();
    let first = s.start("alice", "live-1", "conv-1", T0).unwrap();
    assert!(!first.already_live);
    let token = first.publish_token;
    assert_eq!(
        s.verify_token(&token, T0 + 10, "live-1", MediaScope::LivePublish)
            .unwrap()
            .user,
        "alice"
    );
    let again = s.start("alice", "live-1", "conv-1", T0 + 5).unwrap();
    assert!(again.already_live);
    assert_eq!(
        s.effects().events,
        vec!["feed_live:live-1:alice".to_string()],
        "重复开播不重复写 Feed"
    );
}

#[test]
fn only_the_host_may_start_or_restart_a_stream() {
    let mut s = server();
    start(&mut s);
    assert_eq!(
        s.start("mallory", "live-1", "conv-1", T0 + 1),
        Err(LiveError::Forbidden)
    );
}

#[test]
fn viewer_count_dedupes_users_but_not_devices() {
    let mut s = server();
    start(&mut s);
    let a = s.watch("live-1", "bob", "phone", false, T0 + 10).unwrap();
    assert_eq!((a.viewer_count, a.connections), (1, 1));
    let a2 = s.watch("live-1", "bob", "tablet", false, T0 + 11).unwrap();
    assert_eq!(
        (a2.viewer_count, a2.connections),
        (1, 2),
        "同人多设备不刷人数"
    );
    let b = s.watch("live-1", "carol", "phone", false, T0 + 12).unwrap();
    assert_eq!((b.viewer_count, b.connections), (2, 3));

    assert_eq!(s.leave("live-1", "bob", "phone"), Some(2));
    assert_eq!(
        s.stream("live-1").unwrap().viewer_count(),
        2,
        "bob 还有一台设备在看"
    );
    assert_eq!(s.leave("live-1", "bob", "tablet"), Some(1));
    assert_eq!(s.stream("live-1").unwrap().viewer_count(), 1);
    assert_eq!(s.leave("live-1", "bob", "tablet"), None, "重复退房幂等");
}

#[test]
fn watch_tokens_are_bound_to_the_stream_user_device_and_generation() {
    let mut s = server();
    let publish = start(&mut s);
    let ticket = s.watch("live-1", "bob", "phone", false, T0 + 10).unwrap();

    assert!(s
        .verify_token(&ticket.token, T0 + 20, "live-1", MediaScope::LiveWatch)
        .is_ok());
    assert_eq!(
        s.verify_token(&ticket.token, T0 + 20, "live-1", MediaScope::LivePublish),
        Err(LiveError::Token(TokenError::WrongScope))
    );
    assert_eq!(
        s.verify_token(&ticket.token, T0 + 20, "other", MediaScope::LiveWatch),
        Err(LiveError::Token(TokenError::WrongCall))
    );
    assert_eq!(
        s.verify_token(
            &ticket.token,
            T0 + 20 + LIVE_TOKEN_TTL_MS,
            "live-1",
            MediaScope::LiveWatch
        ),
        Err(LiveError::Token(TokenError::Expired))
    );
    // 通话 token 不得进直播间：scope 检查在签名之前就拒绝 Call。
    assert_eq!(
        s.verify_token(&ticket.token, T0 + 20, "live-1", MediaScope::Call),
        Err(LiveError::Forbidden)
    );
    // 主播的推流 token 不能拿来观看
    assert_eq!(
        s.verify_token(&publish, T0 + 20, "live-1", MediaScope::LiveWatch),
        Err(LiveError::Token(TokenError::WrongScope))
    );
}

#[test]
fn leaving_and_banning_revoke_watch_tokens() {
    let mut s = server();
    start(&mut s);
    let ticket = s.watch("live-1", "bob", "phone", false, T0 + 10).unwrap();
    s.leave("live-1", "bob", "phone");
    assert_eq!(
        s.verify_token(&ticket.token, T0 + 20, "live-1", MediaScope::LiveWatch),
        Err(LiveError::NotWatching)
    );

    let ticket = s.watch("live-1", "bob", "phone", false, T0 + 30).unwrap();
    assert_eq!(s.ban("alice", "live-1", "bob", T0 + 40).unwrap(), 1);
    assert_eq!(
        s.verify_token(&ticket.token, T0 + 50, "live-1", MediaScope::LiveWatch),
        Err(LiveError::Banned)
    );
    assert_eq!(
        s.watch("live-1", "bob", "phone", false, T0 + 60),
        Err(LiveError::Banned)
    );
    assert_eq!(
        s.ban("bob", "live-1", "alice", T0 + 60),
        Err(LiveError::Forbidden)
    );
}

#[test]
fn cohost_flow_requires_request_approval_and_stays_revocable() {
    let mut s = server();
    start(&mut s);
    s.watch("live-1", "bob", "phone", false, T0 + 10).unwrap();

    assert_eq!(
        s.approve_cohost("alice", "live-1", "bob", "phone", T0 + 20),
        Err(LiveError::Forbidden),
        "未申请不能直接连麦"
    );
    assert!(s.request_cohost("live-1", "bob", T0 + 20).unwrap());
    assert!(
        !s.request_cohost("live-1", "bob", T0 + 21).unwrap(),
        "重复申请幂等"
    );

    assert_eq!(
        s.approve_cohost("bob", "live-1", "bob", "phone", T0 + 30),
        Err(LiveError::Forbidden),
        "只有主播能批准"
    );
    let token = s
        .approve_cohost("alice", "live-1", "bob", "phone", T0 + 30)
        .unwrap();
    assert_eq!(
        s.verify_token(&token, T0 + 40, "live-1", MediaScope::LiveCohost)
            .unwrap()
            .user,
        "bob"
    );

    assert!(s.revoke_cohost("alice", "live-1", "bob").unwrap());
    assert_eq!(
        s.verify_token(&token, T0 + 50, "live-1", MediaScope::LiveCohost),
        Err(LiveError::Forbidden),
        "取消连麦后旧令牌立即失效"
    );
}

#[test]
fn reports_are_deduped_per_reporter() {
    let mut s = server();
    start(&mut s);
    s.watch("live-1", "bob", "phone", false, T0 + 10).unwrap();
    s.watch("live-1", "carol", "phone", false, T0 + 11).unwrap();
    assert_eq!(s.report("live-1", "bob", "spam", T0 + 20).unwrap(), 1);
    assert_eq!(s.report("live-1", "bob", "spam again", T0 + 21).unwrap(), 1);
    assert_eq!(s.report("live-1", "carol", "abuse", T0 + 22).unwrap(), 2);
    assert_eq!(
        s.report("live-1", "mallory", "not watching", T0 + 23),
        Err(LiveError::NotWatching)
    );
}

#[test]
fn stop_revokes_the_source_immediately_and_is_idempotent() {
    let mut s = server();
    start(&mut s);
    s.watch("live-1", "bob", "phone", false, T0 + 10).unwrap();
    s.watch("live-1", "carol", "phone", false, T0 + 11).unwrap();
    s.set_archive("alice", "live-1", true, T0 + 12).unwrap();

    let out = s.stop("alice", "live-1", T0 + 20).unwrap();
    assert!(!out.already_ended);
    let mut disconnected = out.disconnected;
    disconnected.sort();
    assert_eq!(disconnected, vec!["bob".to_string(), "carol".to_string()]);
    assert_eq!(s.stream("live-1").unwrap().viewer_count(), 0);
    assert_eq!(
        s.stream("live-1").unwrap().state(),
        &LiveState::Ended(LiveEndReason::HostStopped)
    );

    let events = &s.effects().events;
    let revoke = events
        .iter()
        .position(|e| e == "source_revoked:live-1")
        .unwrap();
    let ended = events
        .iter()
        .position(|e| e == "feed_ended:live-1")
        .unwrap();
    assert!(revoke < ended, "撤源必须先于 Feed 收尾: {events:?}");
    assert!(events.contains(&"archive_stopped:live-1".to_string()));

    let again = s.stop("alice", "live-1", T0 + 30).unwrap();
    assert!(again.already_ended);
    assert_eq!(
        s.effects()
            .events
            .iter()
            .filter(|e| *e == "source_revoked:live-1")
            .count(),
        1,
        "重复停播不重复撤源"
    );
}

#[test]
fn non_host_cannot_stop_but_moderation_can_force_stop() {
    let mut s = server();
    start(&mut s);
    assert_eq!(s.stop("bob", "live-1", T0 + 10), Err(LiveError::Forbidden));
    let out = s.force_stop("live-1", T0 + 20).unwrap();
    assert!(out.ended);
    assert_eq!(
        s.stream("live-1").unwrap().state().end_reason(),
        Some(LiveEndReason::Forced)
    );
}

#[test]
fn host_reconnect_rolls_the_generation_and_old_tokens_die() {
    let mut s = server();
    let old_publish = start(&mut s);
    let watch = s.watch("live-1", "bob", "phone", false, T0 + 10).unwrap();

    s.host_offline("alice", "live-1", T0 + 20).unwrap();
    assert!(matches!(
        s.stream("live-1").unwrap().state(),
        LiveState::Reconnecting { .. }
    ));
    assert!(s.tick(T0 + 20 + RECONNECT_GRACE_MS - 1).is_empty());

    let resumed = s.host_reconnect("alice", "live-1", T0 + 40).unwrap();
    assert!(s.stream("live-1").unwrap().state().is_live());
    assert_eq!(
        s.verify_token(&old_publish, T0 + 50, "live-1", MediaScope::LivePublish),
        Err(LiveError::Token(TokenError::StaleGeneration))
    );
    assert!(s
        .verify_token(
            &resumed.publish_token,
            T0 + 50,
            "live-1",
            MediaScope::LivePublish
        )
        .is_ok());
    assert_eq!(
        s.verify_token(&watch.token, T0 + 50, "live-1", MediaScope::LiveWatch),
        Err(LiveError::Token(TokenError::StaleGeneration)),
        "旧观看令牌随代次失效，客户端重新 watch"
    );
}

#[test]
fn missing_the_grace_window_ends_the_stream_and_releases_viewers() {
    let mut s = server();
    start(&mut s);
    s.watch("live-1", "bob", "phone", false, T0 + 10).unwrap();
    s.host_offline("alice", "live-1", T0 + 20).unwrap();

    assert_eq!(s.tick(T0 + 20 + RECONNECT_GRACE_MS), vec!["live-1"]);
    let stream = s.stream("live-1").unwrap();
    assert_eq!(stream.state().end_reason(), Some(LiveEndReason::HostLost));
    assert_eq!(stream.viewer_count(), 0, "超时后必须释放观看资源");
    assert!(s
        .effects()
        .events
        .contains(&"source_revoked:live-1".to_string()));
    assert_eq!(
        s.watch("live-1", "carol", "phone", false, T0 + 100_000),
        Err(LiveError::Ended)
    );
}

#[test]
fn playback_never_pretends_to_play_under_weak_network_or_autoplay_block() {
    let mut s = server();
    start(&mut s);
    let ticket = s.watch("live-1", "bob", "phone", true, T0 + 10).unwrap();
    assert_eq!(
        s.playback("live-1", "bob", "phone").unwrap(),
        PlaybackState::AwaitingGesture,
        "浏览器未获手势时不能算在播"
    );

    assert_eq!(
        s.set_playback("live-1", "bob", "phone", PlaybackState::Playing, T0 + 20),
        Err(LiveError::BadPlayback(PlaybackState::AwaitingGesture))
    );
    s.set_playback("live-1", "bob", "phone", PlaybackState::Buffering, T0 + 21)
        .unwrap();
    s.set_playback("live-1", "bob", "phone", PlaybackState::Playing, T0 + 22)
        .unwrap();
    assert!(s.playback("live-1", "bob", "phone").unwrap().is_playing());

    let stalled = s
        .set_playback(
            "live-1",
            "bob",
            "phone",
            PlaybackState::Stalled { since_ms: 0 },
            T0 + 30,
        )
        .unwrap();
    assert_eq!(stalled, PlaybackState::Stalled { since_ms: T0 + 30 });
    assert!(!stalled.is_playing(), "弱网下必须显示恢复态而非假播放");

    s.set_playback("live-1", "bob", "phone", PlaybackState::Buffering, T0 + 31)
        .unwrap();
    s.set_playback("live-1", "bob", "phone", PlaybackState::Playing, T0 + 32)
        .unwrap();
    assert!(s.playback("live-1", "bob", "phone").unwrap().is_playing());

    assert!(s
        .verify_token(&ticket.token, T0 + 40, "live-1", MediaScope::LiveWatch)
        .is_ok());
}

#[test]
fn archive_is_host_only_and_toggles_idempotently() {
    let mut s = server();
    start(&mut s);
    assert_eq!(
        s.set_archive("bob", "live-1", true, T0 + 10),
        Err(LiveError::Forbidden)
    );
    assert!(s.set_archive("alice", "live-1", true, T0 + 20).unwrap());
    assert!(s.set_archive("alice", "live-1", true, T0 + 21).unwrap());
    assert_eq!(
        s.effects()
            .events
            .iter()
            .filter(|e| *e == "archive_started:live-1")
            .count(),
        1
    );
    assert!(!s.set_archive("alice", "live-1", false, T0 + 30).unwrap());
    assert!(!s.stream("live-1").unwrap().is_archiving());
}
