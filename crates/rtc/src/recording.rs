//! G09-04 知情同意式录制。
//!
//! 规则很简单，hard 在别处：
//!
//! * **每个参与者都必须同意**；任一人拒绝，状态直接 `Rejected`，存储层从未
//!   被调用，因此不会产生文件。
//! * **与 E2EE 一致**：开启端到端加密时，录制端必须先解出明文再写入存储；
//!   没有密钥的进程（包括 SFU）无法"顺手录一份"。
//! * 录制文件只经 [`RecordingSink`] 交给 G04 的媒体存储；本模块不写磁盘，
//!   也不记录密钥。

use std::collections::{BTreeMap, BTreeSet};

use crate::e2ee::{E2eeError, MediaKeyRing};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingState {
    ConsentPending,
    Active,
    Stopped,
    Rejected,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RecordingError {
    #[error("a recording needs at least one participant")]
    NoParticipants,
    #[error("{0} is not a participant of this recording")]
    NotParticipant(String),
    #[error("recording consent was already decided")]
    AlreadyDecided,
    #[error("recording is not active")]
    NotActive,
    #[error("{0} may not control this recording")]
    NotInitiator(String),
    #[error("encrypted recording requires the media key")]
    E2eeRequired,
    #[error("media decryption failed: {0}")]
    E2ee(E2eeError),
    #[error("recording storage failed: {0}")]
    Storage(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordingEvent {
    Requested {
        call_id: String,
        participants: Vec<String>,
    },
    ConsentRecorded {
        user: String,
        granted: bool,
    },
    Started {
        call_id: String,
    },
    Rejected {
        by: String,
    },
    Stopped {
        call_id: String,
    },
}

/// 发起、同意、停止的状态机。时间由调用方给，便于审计对齐。
#[derive(Clone, Debug)]
pub struct RecordingSession {
    call_id: String,
    initiator: String,
    participants: BTreeSet<String>,
    consent: BTreeMap<String, bool>,
    state: RecordingState,
    e2ee: bool,
    requested_at_ms: u64,
    started_at_ms: Option<u64>,
    stopped_at_ms: Option<u64>,
}

impl RecordingSession {
    pub fn request(
        call_id: impl Into<String>,
        initiator: impl Into<String>,
        participants: impl IntoIterator<Item = String>,
        e2ee: bool,
        now_ms: u64,
    ) -> Result<Self, RecordingError> {
        let participants: BTreeSet<String> =
            participants.into_iter().filter(|p| !p.is_empty()).collect();
        if participants.is_empty() {
            return Err(RecordingError::NoParticipants);
        }
        let initiator = initiator.into();
        if !participants.contains(&initiator) {
            return Err(RecordingError::NotParticipant(initiator));
        }
        let mut consent = BTreeMap::new();
        // 发起本身就是同意；其余人必须显式点同意。
        consent.insert(initiator.clone(), true);
        Ok(Self {
            call_id: call_id.into(),
            initiator,
            participants,
            consent,
            state: RecordingState::ConsentPending,
            e2ee,
            requested_at_ms: now_ms,
            started_at_ms: None,
            stopped_at_ms: None,
        })
    }

    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    pub fn state(&self) -> RecordingState {
        self.state
    }

    pub fn is_active(&self) -> bool {
        self.state == RecordingState::Active
    }

    pub fn is_e2ee(&self) -> bool {
        self.e2ee
    }

    pub fn initiator(&self) -> &str {
        &self.initiator
    }

    pub fn participants(&self) -> &BTreeSet<String> {
        &self.participants
    }

    pub fn consent(&self, user: &str) -> Option<bool> {
        self.consent.get(user).copied()
    }

    pub fn pending_consent(&self) -> Vec<String> {
        self.participants
            .iter()
            .filter(|p| !self.consent.contains_key(*p))
            .cloned()
            .collect()
    }

    /// 记录一次同意。任一人拒绝 → `Rejected`，且不可反悔。
    pub fn consent_to_record(
        &mut self,
        user: &str,
        granted: bool,
        now_ms: u64,
    ) -> Result<RecordingEvent, RecordingError> {
        if !self.participants.contains(user) {
            return Err(RecordingError::NotParticipant(user.to_string()));
        }
        if self.state != RecordingState::ConsentPending {
            return Err(RecordingError::AlreadyDecided);
        }
        self.consent.insert(user.to_string(), granted);
        if !granted {
            self.state = RecordingState::Rejected;
            return Ok(RecordingEvent::Rejected {
                by: user.to_string(),
            });
        }
        if self
            .participants
            .iter()
            .all(|p| self.consent.get(p) == Some(&true))
        {
            self.state = RecordingState::Active;
            self.started_at_ms = Some(now_ms);
            return Ok(RecordingEvent::Started {
                call_id: self.call_id.clone(),
            });
        }
        Ok(RecordingEvent::ConsentRecorded {
            user: user.to_string(),
            granted,
        })
    }

    /// 停止录制。只有发起人能停；已停止时对发起人幂等成功。
    pub fn stop(&mut self, user: &str, now_ms: u64) -> Result<RecordingEvent, RecordingError> {
        if user != self.initiator {
            return Err(RecordingError::NotInitiator(user.to_string()));
        }
        match self.state {
            RecordingState::Active => {
                self.state = RecordingState::Stopped;
                self.stopped_at_ms = Some(now_ms);
                Ok(RecordingEvent::Stopped {
                    call_id: self.call_id.clone(),
                })
            }
            RecordingState::Stopped => Ok(RecordingEvent::Stopped {
                call_id: self.call_id.clone(),
            }),
            _ => Err(RecordingError::NotActive),
        }
    }

    pub fn requested_at_ms(&self) -> u64 {
        self.requested_at_ms
    }

    pub fn started_at_ms(&self) -> Option<u64> {
        self.started_at_ms
    }

    pub fn stopped_at_ms(&self) -> Option<u64> {
        self.stopped_at_ms
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RecordingArtifact {
    pub call_id: String,
    pub bytes: u64,
    pub frames: u64,
    pub e2ee: bool,
}

/// G04 媒体存储的端口。实现方负责落盘/对象存储与保留策略。
pub trait RecordingSink {
    fn begin(&mut self, call_id: &str, e2ee: bool) -> Result<(), String>;
    fn write(&mut self, data: &[u8]) -> Result<(), String>;
    fn finish(&mut self) -> Result<RecordingArtifact, String>;
    fn abort(&mut self);
}

/// 把状态机接到存储端口。只有 `Active` 才允许写；拒绝路径从不 begin。
pub struct RecordingPipeline<S: RecordingSink> {
    session: RecordingSession,
    sink: S,
    artifact: Option<RecordingArtifact>,
    frames: u64,
}

impl<S: RecordingSink> RecordingPipeline<S> {
    pub fn new(session: RecordingSession, sink: S) -> Self {
        Self {
            session,
            sink,
            artifact: None,
            frames: 0,
        }
    }

    pub fn session(&self) -> &RecordingSession {
        &self.session
    }

    pub fn sink(&self) -> &S {
        &self.sink
    }

    pub fn consent(
        &mut self,
        user: &str,
        granted: bool,
        now_ms: u64,
    ) -> Result<RecordingEvent, RecordingError> {
        let event = self.session.consent_to_record(user, granted, now_ms)?;
        match &event {
            RecordingEvent::Started { .. } => {
                self.sink
                    .begin(self.session.call_id(), self.session.is_e2ee())
                    .map_err(RecordingError::Storage)?;
            }
            RecordingEvent::Rejected { .. } => self.sink.abort(),
            _ => {}
        }
        Ok(event)
    }

    /// 写入一帧。E2EE 会话必须先经密钥环解密；SFU 没有密钥，调用会失败。
    pub fn record_frame(
        &mut self,
        ring: Option<&mut MediaKeyRing>,
        sender: &str,
        frame: &[u8],
    ) -> Result<(), RecordingError> {
        if !self.session.is_active() {
            return Err(RecordingError::NotActive);
        }
        let plaintext = if self.session.is_e2ee() {
            let ring = ring.ok_or(RecordingError::E2eeRequired)?;
            ring.open(sender, frame).map_err(RecordingError::E2ee)?
        } else {
            frame.to_vec()
        };
        self.sink
            .write(&plaintext)
            .map_err(RecordingError::Storage)?;
        self.frames += 1;
        Ok(())
    }

    /// 停止并封存文件。重复调用返回同一份 artifact。
    pub fn stop(&mut self, user: &str, now_ms: u64) -> Result<RecordingArtifact, RecordingError> {
        if user != self.session.initiator() {
            return Err(RecordingError::NotInitiator(user.to_string()));
        }
        if let Some(done) = &self.artifact {
            return Ok(done.clone());
        }
        self.session.stop(user, now_ms)?;
        let mut artifact = self.sink.finish().map_err(RecordingError::Storage)?;
        artifact.frames = self.frames;
        artifact.call_id = self.session.call_id().to_string();
        artifact.e2ee = self.session.is_e2ee();
        self.artifact = Some(artifact.clone());
        Ok(artifact)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MemorySink {
        begun: bool,
        aborted: bool,
        finished: usize,
        data: Vec<u8>,
    }

    impl RecordingSink for MemorySink {
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
            self.aborted = true;
            self.data.clear();
        }
    }

    fn session() -> RecordingSession {
        RecordingSession::request(
            "call-1",
            "alice",
            ["alice".to_string(), "bob".to_string()],
            false,
            1_000,
        )
        .unwrap()
    }

    #[test]
    fn starts_only_after_everyone_consents() {
        let mut p = RecordingPipeline::new(session(), MemorySink::default());
        assert_eq!(
            p.record_frame(None, "alice", b"x"),
            Err(RecordingError::NotActive)
        );
        let ev = p.consent("bob", true, 1_100).unwrap();
        assert!(matches!(ev, RecordingEvent::Started { .. }));
        assert_eq!(p.session().state(), RecordingState::Active);
        assert!(p.sink().begun);
        p.record_frame(None, "alice", b"hello").unwrap();
        let artifact = p.stop("alice", 1_200).unwrap();
        assert_eq!(artifact.bytes, 5);
        assert_eq!(artifact.frames, 1);
        assert_eq!(p.sink().finished, 1);
        // 重复 stop 不重复封存
        assert_eq!(p.stop("alice", 1_300).unwrap(), artifact);
        assert_eq!(p.sink().finished, 1);
    }

    #[test]
    fn refusal_produces_no_file_and_no_finish() {
        let mut p = RecordingPipeline::new(session(), MemorySink::default());
        let ev = p.consent("bob", false, 1_100).unwrap();
        assert_eq!(ev, RecordingEvent::Rejected { by: "bob".into() });
        assert_eq!(p.session().state(), RecordingState::Rejected);
        assert!(p.sink().aborted);
        assert!(!p.sink().begun, "拒绝时存储层从未 begin");
        assert_eq!(p.sink().finished, 0);
        assert_eq!(
            p.record_frame(None, "alice", b"x"),
            Err(RecordingError::NotActive)
        );
        assert_eq!(p.stop("alice", 1_200), Err(RecordingError::NotActive));
        assert!(p.sink().data.is_empty());
    }

    #[test]
    fn e2ee_recording_requires_the_media_key() {
        let secret = [3u8; crate::e2ee::KEY_LEN];
        let mut ring =
            MediaKeyRing::new("call-1", 1, secret, ["alice".into(), "bob".into()]).unwrap();
        let sender_ring = MediaKeyRing::new(
            "call-1",
            1,
            secret,
            ["alice".to_string(), "bob".to_string()],
        )
        .unwrap();
        let frame = sender_ring.seal("alice", 1, b"secret audio").unwrap();
        assert!(!frame.windows(12).any(|w| w == b"secret audio"));

        let session = RecordingSession::request(
            "call-1",
            "alice",
            ["alice".to_string(), "bob".to_string()],
            true,
            1_000,
        )
        .unwrap();
        let mut p = RecordingPipeline::new(session, MemorySink::default());
        p.consent("bob", true, 1_100).unwrap();
        assert_eq!(
            p.record_frame(None, "alice", &frame),
            Err(RecordingError::E2eeRequired)
        );
        p.record_frame(Some(&mut ring), "alice", &frame).unwrap();
        assert_eq!(p.sink().data, b"secret audio");
        let artifact = p.stop("alice", 1_200).unwrap();
        assert!(artifact.e2ee);
    }

    #[test]
    fn only_the_initiator_may_stop() {
        let mut p = RecordingPipeline::new(session(), MemorySink::default());
        p.consent("bob", true, 1_100).unwrap();
        assert_eq!(
            p.stop("bob", 1_200),
            Err(RecordingError::NotInitiator("bob".into()))
        );
        assert!(p.session().is_active(), "越权调用不得改变状态");
        p.stop("alice", 1_300).unwrap();
        assert_eq!(p.session().state(), RecordingState::Stopped);
    }

    #[test]
    fn double_consent_and_non_participants_are_refused() {
        let mut s = session();
        s.consent_to_record("bob", true, 1_100).unwrap();
        assert_eq!(
            s.consent_to_record("bob", true, 1_200),
            Err(RecordingError::AlreadyDecided)
        );
        let mut s = session();
        assert_eq!(
            s.consent_to_record("mallory", true, 1_100),
            Err(RecordingError::NotParticipant("mallory".into()))
        );
    }
}
