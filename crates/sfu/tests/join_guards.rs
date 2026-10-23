//! #58 的入会闸门对抗测试：身份、成员资格、容量、重名冲突。
//!
//! 身份与成员资格在触碰 SDP 之前裁决，所以拒绝用例的 offer 用空对象即可；
//! 需要真正入会的用例才构造 offer/answer。

use std::collections::HashMap;
use std::net::UdpSocket;

use serde_json::json;
use wxlike_platform::{Platform, PlatformState};
use wxlike_rtc::media::MediaSession;
use wxlike_sfu::{SfuJoinError, SfuRoom};

struct Fixture {
    platform: PlatformState,
    conv: String,
    ids: HashMap<&'static str, String>,
    tokens: HashMap<&'static str, String>,
}

/// alice/bob/carol 在一个三人群里；dave 已注册但不是成员。
fn fixture() -> Fixture {
    let p = Platform::in_memory().unwrap();
    let alice = p.register("alice", "pw").unwrap();
    let bob = p.register("bob", "pw").unwrap();
    let carol = p.register("carol", "pw").unwrap();
    let dave = p.register("dave", "pw").unwrap();
    let conv = p
        .create_group(&alice, "sfu-guards", &[bob.clone(), carol.clone()])
        .unwrap();
    let ids = HashMap::from([
        ("alice", alice),
        ("bob", bob),
        ("carol", carol),
        ("dave", dave),
    ]);
    let tokens = ids
        .iter()
        .map(|(name, id)| (*name, p.create_session(id, 3_600_000).unwrap()))
        .collect();
    Fixture {
        platform: p.shared(),
        conv,
        ids,
        tokens,
    }
}

fn room(f: &Fixture, capacity: usize) -> SfuRoom {
    SfuRoom::with_capacity(
        f.platform.clone(),
        &f.conv,
        capacity,
        Some("127.0.0.1".parse().unwrap()),
        0,
    )
}

/// 真实 offer（只有通过闸门的 join 才会走到 SDP 协商）。
fn real_offer() -> serde_json::Value {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let local = socket.local_addr().unwrap();
    let mut session = MediaSession::new(true);
    session.add_host_candidate(local);
    session.offer(false).unwrap()
}

#[test]
fn invalid_and_expired_tokens_are_unauthorized() {
    let f = fixture();
    let mut r = room(&f, 8);

    let err = r.join("not-a-real-token", json!({}), false).unwrap_err();
    assert!(matches!(err, SfuJoinError::Unauthorized), "实际: {err:?}");
    assert_eq!(err.http_status(), 401);

    let expired = f.platform.create_session(&f.ids["alice"], 0).unwrap();
    let err = r.join(&expired, json!({}), false).unwrap_err();
    assert!(matches!(err, SfuJoinError::Unauthorized), "实际: {err:?}");
    assert!(r.is_empty(), "被拒绝的 join 不能留下参与者");
}

#[test]
fn non_member_is_not_found() {
    let f = fixture();
    let mut r = room(&f, 8);

    let err = r.join(&f.tokens["dave"], json!({}), false).unwrap_err();
    assert!(matches!(err, SfuJoinError::NotFound), "实际: {err:?}");
    assert_eq!(err.http_status(), 404);
    assert!(r.is_empty());
}

/// 不存在的房间与"房间存在但我不是成员"必须返回同一个错误——
/// 否则调用方可以靠 404/403 的差异探测房间是否存在。
#[test]
fn unknown_room_looks_exactly_like_a_non_member() {
    let f = fixture();
    let mut bogus = SfuRoom::new(
        f.platform.clone(),
        "g-doesnotexist",
        Some("127.0.0.1".parse().unwrap()),
        0,
    );

    let err = bogus
        .join(&f.tokens["alice"], json!({}), false)
        .unwrap_err();
    assert!(matches!(err, SfuJoinError::NotFound), "实际: {err:?}");
    assert_eq!(err.http_status(), 404);
}

#[test]
fn room_capacity_rejects_the_next_member() {
    let f = fixture();
    let mut r = room(&f, 2);

    r.join(&f.tokens["alice"], real_offer(), false).unwrap();
    r.join(&f.tokens["bob"], real_offer(), false).unwrap();
    assert_eq!(r.len(), 2);

    let err = r.join(&f.tokens["carol"], real_offer(), false).unwrap_err();
    assert!(matches!(err, SfuJoinError::Full), "实际: {err:?}");
    assert_eq!(err.http_status(), 409);
    assert_eq!(r.len(), 2, "超员拒绝不能影响已在房的人");
}

#[test]
fn duplicate_identity_cannot_evict_the_existing_peer() {
    let f = fixture();
    let mut r = room(&f, 8);

    r.join(&f.tokens["alice"], real_offer(), false).unwrap();
    let err = r.join(&f.tokens["alice"], real_offer(), false).unwrap_err();
    assert!(matches!(err, SfuJoinError::PeerConflict), "实际: {err:?}");
    assert_eq!(err.http_status(), 409);
    assert_eq!(r.len(), 1, "不能用同一 id 顶替（踢掉）已有参与者");
    assert_eq!(r.participants(), vec![f.ids["alice"].clone()]);
}
