//! G09-02 服务端呼叫裁决的对抗测试。
//!
//! 这里不模拟媒体：只验证"谁有权接听、谁忙线、令牌绑定谁、结束后能否复活"。
//! 成员资格用真实集合实现（G02 的成员表尚未合入，接入点由
//! `integration-requests/G09/G09-02.yaml` 指定），通话记录用真 SQLite 落库。

use std::collections::HashSet;

use wxlike_rtc::call_session::{
    CallError, CallServer, CallState, DeviceRef, EndReason, Membership, CONNECT_TIMEOUT_MS,
    RING_TIMEOUT_MS,
};
use wxlike_rtc::call_store::{CallRecordSink, MemoryCallRecords, SqliteCallRecords};
use wxlike_rtc::token::{MediaScope, TokenSigner, MIN_SECRET_LEN};

const T0: u64 = 1_000_000;

#[derive(Clone)]
struct Roster {
    members: HashSet<(String, String)>,
}

impl Roster {
    fn new(conv: &str, users: &[&str]) -> Self {
        Self {
            members: users
                .iter()
                .map(|u| (conv.to_string(), u.to_string()))
                .collect(),
        }
    }

    fn remove(&mut self, conv: &str, user: &str) {
        self.members.remove(&(conv.to_string(), user.to_string()));
    }
}

impl Membership for Roster {
    fn member_allowed(&self, conv_id: &str, user: &str) -> bool {
        self.members
            .contains(&(conv_id.to_string(), user.to_string()))
    }
}

fn signer() -> TokenSigner {
    TokenSigner::new(vec![0x42; MIN_SECRET_LEN]).unwrap()
}

fn server(roster: Roster) -> CallServer<Roster, MemoryCallRecords> {
    CallServer::new(signer(), roster, MemoryCallRecords::default())
}

fn caller() -> DeviceRef {
    DeviceRef::new("alice", "laptop")
}

fn invite<R: CallRecordSink>(server: &mut CallServer<Roster, R>) -> String {
    server
        .invite(
            caller(),
            "g-1",
            "bob",
            vec!["phone".into(), "tablet".into()],
            true,
            T0,
        )
        .unwrap()
        .call_id
}

fn answered() -> (CallServer<Roster, MemoryCallRecords>, String) {
    let mut s = server(Roster::new("g-1", &["alice", "bob"]));
    let call_id = invite(&mut s);
    s.accept(&call_id, "bob", "tablet", T0 + 10).unwrap();
    s.media_connected(&call_id, "bob").unwrap();
    (s, call_id)
}

#[test]
fn first_device_wins_the_answer_race() {
    let mut s = server(Roster::new("g-1", &["alice", "bob"]));
    let call_id = invite(&mut s);

    let winner = s.accept(&call_id, "bob", "tablet", T0 + 10).unwrap();
    let claims = s.verify_media_token(&winner.token, T0 + 20).unwrap();
    assert_eq!(claims.user, "bob");
    assert_eq!(claims.device, "tablet");
    assert_eq!(claims.scope, MediaScope::Call);
    assert_eq!(claims.call, call_id);

    let err = s.accept(&call_id, "bob", "phone", T0 + 11).unwrap_err();
    assert_eq!(err, CallError::AnsweredElsewhere("tablet".into()));
    assert_eq!(
        s.session(&call_id).unwrap().answered_by().unwrap().device,
        "tablet"
    );
    assert_eq!(s.session(&call_id).unwrap().state(), CallState::Accepted);
}

#[test]
fn same_device_retry_is_idempotent() {
    let mut s = server(Roster::new("g-1", &["alice", "bob"]));
    let call_id = invite(&mut s);
    let first = s.accept(&call_id, "bob", "tablet", T0 + 10).unwrap();
    let again = s.accept(&call_id, "bob", "tablet", T0 + 12).unwrap();
    assert_eq!(first.event, again.event);
    assert_ne!(first.token, again.token, "a fresh token is minted per join");
}

#[test]
fn busy_line_is_refused_before_a_second_call_exists() {
    let mut s = server(Roster::new("g-1", &["alice", "bob", "carol"]));
    let call_id = invite(&mut s);
    s.accept(&call_id, "bob", "phone", T0 + 5).unwrap();

    let err = s
        .invite(
            DeviceRef::new("carol", "laptop"),
            "g-1",
            "bob",
            vec!["tablet".into()],
            false,
            T0 + 20,
        )
        .unwrap_err();
    assert_eq!(err, CallError::Busy(call_id.clone()));
    assert_eq!(s.len(), 1, "忙线不能产生第二个房间");

    let err = s
        .invite(
            caller(),
            "g-1",
            "carol",
            vec!["phone".into()],
            false,
            T0 + 21,
        )
        .unwrap_err();
    assert!(matches!(err, CallError::Busy(_)), "主叫自己也在通话中");
}

#[test]
fn uninvited_users_and_devices_are_rejected() {
    let mut s = server(Roster::new("g-1", &["alice", "bob", "carol"]));
    let call_id = invite(&mut s);

    assert_eq!(
        s.accept(&call_id, "carol", "phone", T0 + 1).unwrap_err(),
        CallError::NotInvited
    );
    assert_eq!(
        s.accept(&call_id, "bob", "watch", T0 + 2).unwrap_err(),
        CallError::DeviceNotInvited
    );
    assert_eq!(
        s.accept(&call_id, "alice", "laptop", T0 + 3).unwrap_err(),
        CallError::NotInvited
    );
    assert_eq!(
        s.rejoin(&call_id, "carol", "phone", T0 + 4).unwrap_err(),
        CallError::NotInvited
    );
    assert_eq!(s.session(&call_id).unwrap().state(), CallState::Ringing);
}

#[test]
fn hangup_is_idempotent_and_writes_one_record() {
    let (mut s, call_id) = answered();
    let first = s.hangup(&call_id, "alice", T0 + 100).unwrap();
    let second = s.hangup(&call_id, "alice", T0 + 101).unwrap();
    let third = s.hangup(&call_id, "bob", T0 + 102).unwrap();
    assert!(first.ended && !first.already_ended);
    assert!(second.already_ended && third.already_ended);
    assert_eq!(s.session(&call_id).unwrap().state(), CallState::Ended);

    assert_eq!(s.records().rows().len(), 1, "重复挂断只能落一条");
    let record = s.records().rows().first().unwrap();
    assert_eq!(record.reason, EndReason::HungUp);
    assert_eq!(record.answer_device.as_deref(), Some("tablet"));
    assert_eq!(record.answered_at_ms, Some(T0 + 10));
    assert_eq!(record.ended_at_ms, T0 + 100);
}

#[test]
fn late_signals_cannot_resurrect_an_ended_call() {
    let mut s = server(Roster::new("g-1", &["alice", "bob"]));
    let call_id = invite(&mut s);
    s.cancel(&call_id, "alice", T0 + 5).unwrap();

    assert_eq!(
        s.accept(&call_id, "bob", "phone", T0 + 6).unwrap_err(),
        CallError::AlreadyEnded
    );
    assert_eq!(
        s.rejoin(&call_id, "alice", "laptop", T0 + 7).unwrap_err(),
        CallError::AlreadyEnded
    );
    let record = s.records().get(&call_id).unwrap().unwrap();
    assert_eq!(record.reason, EndReason::Cancelled);
    assert_eq!(record.answer_device, None);
}

#[test]
fn ring_and_connect_timeouts_have_distinct_reasons() {
    let mut s = server(Roster::new("g-1", &["alice", "bob"]));
    let call_id = invite(&mut s);
    assert!(!s.tick(T0 + RING_TIMEOUT_MS).is_empty());
    assert_eq!(s.session(&call_id).unwrap().state(), CallState::Ended);
    assert_eq!(
        s.records().get(&call_id).unwrap().unwrap().reason,
        EndReason::Timeout
    );

    let mut s = server(Roster::new("g-1", &["alice", "bob"]));
    let call_id = invite(&mut s);
    s.accept(&call_id, "bob", "phone", T0 + 10).unwrap();
    s.begin_connecting(&call_id, "alice").unwrap();
    assert!(!s.tick(T0 + 10 + CONNECT_TIMEOUT_MS).is_empty());
    assert_eq!(
        s.records().get(&call_id).unwrap().unwrap().reason,
        EndReason::Failed
    );
}

#[test]
fn revocation_kills_the_call_and_every_old_token() {
    let mut roster = Roster::new("g-1", &["alice", "bob"]);
    let mut s = CallServer::new(signer(), roster.clone(), MemoryCallRecords::default());
    let call_id = invite(&mut s);
    let token = s.accept(&call_id, "bob", "tablet", T0 + 5).unwrap().token;
    assert!(s.verify_media_token(&token, T0 + 6).is_ok());

    // 退群/封禁后，服务端成员表先变，再撤销在途通话。
    roster.remove("g-1", "bob");
    *s.membership_mut() = roster;
    let events = s.revoke_member("g-1", "bob", T0 + 7);
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0],
        wxlike_rtc::call_session::CallEvent::Ended {
            reason: EndReason::AuthorizationRevoked,
            ..
        }
    ));
    assert_eq!(
        s.records().get(&call_id).unwrap().unwrap().reason,
        EndReason::AuthorizationRevoked
    );
    assert_eq!(
        s.verify_media_token(&token, T0 + 8).unwrap_err(),
        CallError::AlreadyEnded
    );
    assert_eq!(
        s.rejoin(&call_id, "bob", "tablet", T0 + 9).unwrap_err(),
        CallError::AlreadyEnded
    );
}

#[test]
fn token_is_bound_to_the_winning_device() {
    let mut s = server(Roster::new("g-1", &["alice", "bob"]));
    let call_id = invite(&mut s);
    let _ = s.accept(&call_id, "bob", "tablet", T0 + 1).unwrap();

    // 同一签名器签出的令牌，只要 device 不是赢家，服务端就拒绝。
    let forged = s
        .signer()
        .issue_for(
            &call_id,
            "bob",
            "phone",
            MediaScope::Call,
            1,
            T0 + 2,
            60_000,
        )
        .unwrap();
    assert_eq!(
        s.verify_media_token(&forged, T0 + 3).unwrap_err(),
        CallError::Token(wxlike_rtc::token::TokenError::WrongCall)
    );
}

#[test]
fn sqlite_records_are_real_and_queryable() {
    let records = SqliteCallRecords::in_memory().unwrap();
    let mut s = CallServer::new(signer(), Roster::new("g-1", &["alice", "bob"]), records);
    let call_id = invite(&mut s);
    s.accept(&call_id, "bob", "phone", T0 + 5).unwrap();
    s.media_connected(&call_id, "alice").unwrap();
    s.hangup(&call_id, "bob", T0 + 500).unwrap();

    let stored = s.records().get(&call_id).unwrap().unwrap();
    assert_eq!(stored.reason, EndReason::HungUp);
    assert!(stored.video);
    let recent = s.records().recent("g-1", 10).unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].call_id, call_id);
}
