//! G09-04 端到端加密 + 知情同意录制的联合验收。
//!
//! 链路是真的：两端的 str0m 会话通过真 ICE/DTLS/SRTP 传媒体，只是载荷
//! 在进媒体栈之前已被 ChaCha20-Poly1305 封装。SFU/中继只看到密文；录制端
//! 必须先用自己的密钥解开，才能把明文交给存储端口。

use std::time::Duration;

use str0m::Event;
use wxlike_rtc::e2ee::{MediaKeyRing, KEY_LEN};
use wxlike_rtc::media::{pump, MediaSession};
use wxlike_rtc::recording::{
    RecordingArtifact, RecordingPipeline, RecordingSession, RecordingSink,
};

#[derive(Default)]
struct VecSink {
    begun: bool,
    finished: usize,
    data: Vec<u8>,
}

impl RecordingSink for VecSink {
    fn begin(&mut self, _call_id: &str, _e2ee: bool) -> Result<(), String> {
        self.begun = true;
        Ok(())
    }

    fn write(&mut self, data: &[u8]) -> Result<(), String> {
        self.data.extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> Result<RecordingArtifact, String> {
        self.finished += 1;
        Ok(RecordingArtifact {
            call_id: String::new(),
            bytes: self.data.len() as u64,
            frames: 0,
            e2ee: false,
        })
    }

    fn abort(&mut self) {
        self.data.clear();
    }
}

fn connect() -> (MediaSession, MediaSession) {
    let mut sender = MediaSession::new(true);
    let mut receiver = MediaSession::new(false);
    sender.add_host_candidate("192.0.2.10:42000".parse().unwrap());
    receiver.add_host_candidate("192.0.2.20:42001".parse().unwrap());
    let offer = sender.offer(false).unwrap();
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

fn key_ring() -> MediaKeyRing {
    MediaKeyRing::new(
        "call-1",
        1,
        [7u8; KEY_LEN],
        ["alice".to_string(), "bob".to_string()],
    )
    .unwrap()
}

#[test]
fn encrypted_audio_crosses_the_media_path_and_records_only_plaintext() {
    let (mut alice, mut bob) = connect();
    let alice_ring = key_ring();
    let mut bob_ring = key_ring();

    let plaintext = b"opus frame: secret words";
    let sealed = alice_ring.seal("alice", 1, plaintext).unwrap();
    assert!(
        !sealed.windows(plaintext.len()).any(|w| w == plaintext),
        "封装后不得含明文"
    );

    alice.write_audio(&sealed).unwrap();
    let mut at_bob = Vec::new();
    for _ in 0..20 {
        let (_, ev) = pump(&mut alice, &mut bob, Duration::from_millis(50)).unwrap();
        at_bob.extend(ev);
    }
    let wire = at_bob
        .iter()
        .find_map(|e| match e {
            Event::MediaData(d) if d.data == sealed => Some(d.data.clone()),
            _ => None,
        })
        .expect("密文应原样到达对端（媒体栈不应解析载荷）");

    // 录制端（bob 发起）拿到的是密文，必须用密钥解开再写存储。
    let session = RecordingSession::request(
        "call-1",
        "bob",
        ["alice".to_string(), "bob".to_string()],
        true,
        1_000,
    )
    .unwrap();
    let mut pipeline = RecordingPipeline::new(session, VecSink::default());
    pipeline.consent("alice", true, 1_100).unwrap();
    assert!(pipeline.sink().begun);

    assert_eq!(
        pipeline.record_frame(None, "alice", &wire),
        Err(wxlike_rtc::recording::RecordingError::E2eeRequired)
    );
    pipeline
        .record_frame(Some(&mut bob_ring), "alice", &wire)
        .unwrap();
    assert_eq!(pipeline.sink().data, plaintext);
    assert_ne!(pipeline.sink().data, sealed);

    let artifact = pipeline.stop("bob", 1_200).unwrap();
    assert!(artifact.e2ee);
    assert_eq!(artifact.bytes, plaintext.len() as u64);
    assert_eq!(artifact.frames, 1);
    assert_eq!(pipeline.sink().finished, 1);
}

#[test]
fn rotation_locks_a_removed_member_out_of_new_media() {
    let old = [1u8; KEY_LEN];
    let next = [2u8; KEY_LEN];
    let mut alice = MediaKeyRing::new(
        "call-1",
        1,
        old,
        ["alice".into(), "bob".into(), "carol".into()],
    )
    .unwrap();
    let mut bob = MediaKeyRing::new(
        "call-1",
        1,
        old,
        ["alice".into(), "bob".into(), "carol".into()],
    )
    .unwrap();
    let mut carol = MediaKeyRing::new(
        "call-1",
        1,
        old,
        ["alice".into(), "bob".into(), "carol".into()],
    )
    .unwrap();

    let before = alice.seal("alice", 1, b"before").unwrap();
    assert!(carol.open("alice", &before).is_ok());

    // carol 被移出：剩余成员的密钥环升到 epoch 2。
    alice
        .install_rotation(2, next, ["alice".into(), "bob".into()])
        .unwrap();
    bob.install_rotation(2, next, ["alice".into(), "bob".into()])
        .unwrap();
    let after = alice.seal("alice", 2, b"after").unwrap();
    assert!(bob.open("alice", &after).is_ok(), "新成员仍可解");
    assert!(
        carol.open("alice", &after).is_err(),
        "被移除者不得解出新代次媒体"
    );
}

#[test]
fn recording_without_every_consent_never_reaches_storage() {
    let session = RecordingSession::request(
        "call-2",
        "alice",
        ["alice".to_string(), "bob".to_string()],
        false,
        1_000,
    )
    .unwrap();
    let mut pipeline = RecordingPipeline::new(session, VecSink::default());
    let ev = pipeline.consent("bob", false, 1_100).unwrap();
    assert!(matches!(
        ev,
        wxlike_rtc::recording::RecordingEvent::Rejected { .. }
    ));
    assert!(!pipeline.sink().begun);
    assert!(pipeline.sink().data.is_empty());
    assert_eq!(
        pipeline.record_frame(None, "alice", b"must not be saved"),
        Err(wxlike_rtc::recording::RecordingError::NotActive)
    );
}
