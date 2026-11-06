//! Domain acceptance tests for G04. These use the real SQLite schema, the real
//! filesystem store and the real authorization rules; only the FFmpeg child and
//! the G02/G05 ports are replaced by deterministic fixtures.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use wxlike_media::backup;
use wxlike_media::backup::{
    ChatImportReport, ChatSnapshotMessage, ChatSnapshotPort, KeyEscrowPort,
};
use wxlike_media::files::{AttachmentKind, AttachmentManifest, FileSort};
use wxlike_media::platform;
use wxlike_media::platform::{
    PlatformBroker, PlatformKind, PlatformRequest, PlatformResult, SaveDestination,
};
use wxlike_media::processing::{FixtureRunner, ProbeInfo};
use wxlike_media::*;

struct Port {
    conversations: Vec<(String, String)>,
    groups: Vec<(String, String)>,
}

impl Default for Port {
    fn default() -> Self {
        Self {
            conversations: vec![("u2".into(), "c1".into()), ("u3".into(), "c1".into())],
            groups: vec![("u2".into(), "g1".into())],
        }
    }
}

impl AudiencePort for Port {
    fn may_read_conversation(&self, user_id: &str, conv_id: &str) -> bool {
        self.conversations
            .iter()
            .any(|(user, conv)| user == user_id && conv == conv_id)
    }

    fn may_read_group(&self, user_id: &str, group_id: &str) -> bool {
        self.groups
            .iter()
            .any(|(user, group)| user == user_id && group == group_id)
    }

    fn visible_conversations(&self, user_id: &str) -> Vec<String> {
        self.conversations
            .iter()
            .filter(|(user, _)| user == user_id)
            .map(|(_, conv)| conv.clone())
            .collect()
    }

    fn visible_groups(&self, user_id: &str) -> Vec<String> {
        self.groups
            .iter()
            .filter(|(user, _)| user == user_id)
            .map(|(_, group)| group.clone())
            .collect()
    }
}

fn service_at(root: &Path) -> MediaService {
    MediaService::open(MediaConfig::new(root), None, Arc::new(Port::default())).unwrap()
}

fn chunked_service(root: &Path, chunk: u64) -> MediaService {
    let config = MediaConfig::new(root).with_limits(GIB, chunk);
    MediaService::open(config, None, Arc::new(Port::default())).unwrap()
}

fn upload(
    service: &MediaService,
    owner: &str,
    name: &str,
    mime: &str,
    purpose: Purpose,
    bytes: &[u8],
) -> CompletedUpload {
    let start = service
        .upload_start(
            owner,
            UploadStart {
                purpose,
                name: name.into(),
                size: bytes.len() as u64,
                mime: mime.into(),
                digest: ids::sha256_hex(bytes),
            },
        )
        .unwrap();
    let step = start.chunk_size as usize;
    for (index, chunk) in bytes.chunks(step).enumerate() {
        service
            .upload_chunk(
                owner,
                &start.upload_id,
                (index * step) as u64,
                chunk,
                Some(&ids::sha256_hex(chunk)),
            )
            .unwrap();
    }
    service
        .upload_complete(owner, &start.upload_id, Some(&ids::sha256_hex(bytes)))
        .unwrap()
}

fn png_bytes() -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend([b'x'; 64]);
    bytes
}

#[test]
fn upload_is_resumable_and_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let service = chunked_service(dir.path(), 4);
    let start = service
        .upload_start(
            "u1",
            UploadStart {
                purpose: Purpose::File,
                name: "report.bin".into(),
                size: 10,
                mime: "application/octet-stream".into(),
                digest: String::new(),
            },
        )
        .unwrap();
    assert_eq!(start.chunk_size, 4);
    service
        .upload_chunk(
            "u1",
            &start.upload_id,
            0,
            b"abcd",
            Some(&ids::sha256_hex(b"abcd")),
        )
        .unwrap();
    service
        .upload_chunk("u1", &start.upload_id, 4, b"efgh", None)
        .unwrap();
    let progress = service
        .upload_chunk("u1", &start.upload_id, 4, b"efgh", None)
        .unwrap();
    assert_eq!(progress.offset, 8, "a replayed chunk must not move offset");
    assert!(service
        .upload_chunk("u1", &start.upload_id, 4, b"zzzz", None)
        .is_err());
    service
        .upload_chunk("u1", &start.upload_id, 8, b"ij", None)
        .unwrap();
    let digest = ids::sha256_hex(b"abcdefghij");
    let first = service
        .upload_complete("u1", &start.upload_id, Some(&digest))
        .unwrap();
    assert!(first.newly_stored);
    let second = service
        .upload_complete("u1", &start.upload_id, Some(&digest))
        .unwrap();
    assert!(!second.newly_stored);
    assert_eq!(first.object_id, second.object_id);
    assert!(!dir
        .path()
        .join(".uploads")
        .join(format!("{}.part", start.upload_id))
        .exists());
}

#[test]
fn cancel_removes_staged_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let service = service_at(dir.path());
    let start = service
        .upload_start(
            "u1",
            UploadStart {
                purpose: Purpose::File,
                name: "a.bin".into(),
                size: 4,
                mime: "application/octet-stream".into(),
                digest: String::new(),
            },
        )
        .unwrap();
    service
        .upload_chunk("u1", &start.upload_id, 0, b"data", None)
        .unwrap();
    let staged = dir
        .path()
        .join(".uploads")
        .join(format!("{}.part", start.upload_id));
    assert!(staged.is_file());
    let canceled = service.upload_cancel("u1", &start.upload_id).unwrap();
    assert_eq!(canceled.status, UploadStatus::Canceled);
    assert!(!staged.exists());
    assert!(service
        .upload_complete("u1", &start.upload_id, None)
        .is_err());
}

#[test]
fn content_hash_is_not_a_capability() {
    let dir = tempfile::tempdir().unwrap();
    let service = service_at(dir.path());
    let done = upload(
        &service,
        "u1",
        "secret.txt",
        "text/plain",
        Purpose::File,
        b"private",
    );
    assert!(service.authorize_read("u2", &done.object_id).is_err());
    assert!(service.download_authorize("u2", &done.object_id).is_err());
    assert!(service
        .add_reference(
            "u2",
            &done.object_id,
            RefType::Message,
            "m1",
            Audience::Public,
            Purpose::Attachment,
        )
        .is_err());
    assert!(service.object("not-a-digest").is_err());
}

#[test]
fn signed_urls_expire_and_revocation_is_immediate() {
    let dir = tempfile::tempdir().unwrap();
    let service = service_at(dir.path());
    let done = upload(
        &service,
        "u1",
        "a.txt",
        "text/plain",
        Purpose::File,
        b"hello",
    );
    let access = service.download_authorize("u1", &done.object_id).unwrap();
    service
        .verify_signed(
            &done.object_id,
            access.expires_at,
            access.epoch,
            &access.signature(),
            access.expires_at - 1,
        )
        .unwrap();
    assert!(service
        .verify_signed(
            &done.object_id,
            access.expires_at,
            access.epoch,
            &access.signature(),
            access.expires_at + 1
        )
        .is_err());
    let epoch = service.revoke_object_access("u1", &done.object_id).unwrap();
    assert!(epoch > access.epoch);
    assert!(service
        .verify_signed(
            &done.object_id,
            access.expires_at,
            access.epoch,
            &access.signature(),
            access.expires_at - 1
        )
        .is_err());
}

#[test]
fn group_audience_is_checked_by_the_port() {
    let dir = tempfile::tempdir().unwrap();
    let service = service_at(dir.path());
    let done = upload(
        &service,
        "u1",
        "pic.png",
        "image/png",
        Purpose::Attachment,
        &png_bytes(),
    );
    service
        .add_reference(
            "u1",
            &done.object_id,
            RefType::Message,
            "m-1",
            Audience::Group { id: "g1".into() },
            Purpose::Attachment,
        )
        .unwrap();
    service.authorize_read("u2", &done.object_id).unwrap();
    assert!(service.authorize_read("u3", &done.object_id).is_err());
    assert!(!service.authorize_read("outside", &done.object_id).is_ok());
}

#[test]
fn large_upload_uses_bounded_chunks_and_matching_digest() {
    let dir = tempfile::tempdir().unwrap();
    let service = chunked_service(dir.path(), 1024 * 1024);
    let bytes: Vec<u8> = (0..3 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let done = upload(
        &service,
        "u1",
        "big.bin",
        "application/octet-stream",
        Purpose::File,
        &bytes,
    );
    assert_eq!(done.size, bytes.len() as u64);
    assert_eq!(service.capabilities().max_object_bytes, GIB);
    assert_eq!(service.capabilities().max_chunk_bytes, 1024 * 1024);
    assert_eq!(
        service.verify_object_digest(&done.object_id).unwrap(),
        Some(true)
    );
}

#[test]
fn file_center_projects_lists_and_pages() {
    let dir = tempfile::tempdir().unwrap();
    let service = service_at(dir.path());
    upload(&service, "u1", "b.txt", "text/plain", Purpose::File, b"b");
    upload(&service, "u1", "a.txt", "text/plain", Purpose::File, b"a");
    upload(&service, "u1", "c.txt", "text/plain", Purpose::File, b"c");
    let first = service
        .list_files(
            "u1",
            FileListQuery {
                sort: Some(FileSort::Name),
                limit: Some(2),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(first.items.len(), 2);
    assert_eq!(first.items[0].name, "a.txt");
    assert!(first.has_more);
    assert!(first.snapshot_version > 0);
    let second = service
        .list_files(
            "u1",
            FileListQuery {
                sort: Some(FileSort::Name),
                limit: Some(2),
                cursor: first.next_cursor,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].name, "c.txt");
    let images = service
        .list_files(
            "u1",
            FileListQuery {
                category: Some("image".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(images.items.is_empty());
}

#[test]
fn file_center_does_not_leak_other_users_names() {
    let dir = tempfile::tempdir().unwrap();
    let service = service_at(dir.path());
    upload(
        &service,
        "u1",
        "private-doc.txt",
        "text/plain",
        Purpose::File,
        b"x",
    );
    let page = service.list_files("u2", FileListQuery::default()).unwrap();
    assert!(page.items.iter().all(|item| item.name != "private-doc.txt"));
}

#[test]
fn trash_keeps_history_refs_and_purge_collects() {
    let dir = tempfile::tempdir().unwrap();
    let service = service_at(dir.path());
    let done = upload(
        &service,
        "u1",
        "note.txt",
        "text/plain",
        Purpose::File,
        b"note",
    );
    service
        .add_reference(
            "u1",
            &done.object_id,
            RefType::Message,
            "m-9",
            Audience::Conversation { id: "c1".into() },
            Purpose::Attachment,
        )
        .unwrap();
    service
        .rename_file("u1", &done.file_id, "renamed.txt")
        .unwrap();
    service.move_file("u1", &done.file_id, "2026/docs").unwrap();
    service.trash_file("u1", &done.file_id).unwrap();
    service.authorize_read("u2", &done.object_id).unwrap();
    assert!(service.open_object(&done.object_id).unwrap().is_some());
    service.restore_file("u1", &done.file_id).unwrap();
    let page = service.list_files("u1", FileListQuery::default()).unwrap();
    assert_eq!(page.items.len(), 1);
    service.trash_file("u1", &done.file_id).unwrap();
    service.purge_file("u1", &done.file_id).unwrap();
    service.authorize_read("u2", &done.object_id).unwrap();
    service
        .remove_reference(&done.object_id, RefType::Message, "m-9")
        .unwrap();
    assert!(service.authorize_read("u2", &done.object_id).is_err());
    assert!(service.open_object(&done.object_id).unwrap().is_none());
}

#[test]
fn shares_are_revocable_bearer_references() {
    let dir = tempfile::tempdir().unwrap();
    let service = service_at(dir.path());
    let done = upload(
        &service,
        "u1",
        "shared.txt",
        "text/plain",
        Purpose::File,
        b"shared",
    );
    let grant = service
        .create_share("u1", &done.file_id, 60_000, None)
        .unwrap();
    assert!(grant.url.contains("share="));
    assert_eq!(
        service.resolve_share(&grant.token).unwrap().object_id,
        done.object_id
    );
    service.revoke_share("u1", &grant.share_id).unwrap();
    assert!(service.resolve_share(&grant.token).is_err());
    assert!(service.resolve_share("bogus-token").is_err());
}

#[test]
fn attachment_manifest_enforces_type_and_length_limits() {
    let bytes = b"voice";
    let digest = ids::sha256_hex(bytes);
    let mut manifest = AttachmentManifest {
        kind: AttachmentKind::Voice,
        media_id: digest.clone(),
        name: "voice.ogg".into(),
        size: bytes.len() as u64,
        digest: digest.clone(),
        mime: "audio/ogg".into(),
        duration_ms: Some(1200),
        waveform: Some(vec![10u8; 40]),
    };
    manifest.validate().unwrap();
    manifest.verify_download(bytes).unwrap();
    manifest.waveform = Some(vec![10u8; 39]);
    assert!(manifest.validate().is_err());
    manifest.waveform = Some(vec![10u8; 40]);
    manifest.mime = "video/mp4".into();
    assert!(manifest.validate().is_err());
    let mut image = AttachmentManifest {
        kind: AttachmentKind::Image,
        media_id: digest.clone(),
        name: "a.png".into(),
        size: 21 * 1024 * 1024,
        digest: digest.clone(),
        mime: "image/png".into(),
        duration_ms: None,
        waveform: None,
    };
    assert!(image.validate().is_err());
    image.size = 1024;
    image.mime = "image/png".into();
    image.validate().unwrap();
    assert!(image.verify_download(b"different").is_err());
}

fn fixture_probe() -> ProbeInfo {
    ProbeInfo {
        duration_ms: 5_000,
        width: 640,
        height: 360,
        frames: 150,
        has_video: true,
        has_audio: true,
        format: "fixture".into(),
    }
}

#[test]
fn transcode_jobs_are_idempotent_and_publish_derived_objects() {
    let dir = tempfile::tempdir().unwrap();
    let service = service_at(dir.path()).with_runner(Arc::new(FixtureRunner::new(fixture_probe())));
    let done = upload(
        &service,
        "u1",
        "clip.png",
        "image/png",
        Purpose::Video,
        &png_bytes(),
    );
    let first = service
        .submit_job("u1", &done.object_id, Preset::Thumbnail)
        .unwrap();
    let again = service
        .submit_job("u1", &done.object_id, Preset::Thumbnail)
        .unwrap();
    assert_eq!(first.job_id, again.job_id);
    assert_eq!(service.run_due_jobs(10).unwrap(), 1);
    let view = service.job_status("u1", &first.job_id).unwrap();
    assert_eq!(view.state, JobState::Succeeded);
    assert_eq!(view.progress, 100);
    assert_eq!(view.outputs.len(), 1);
    let artifact = &view.outputs[0];
    assert_eq!(artifact.kind, "thumbnail");
    assert!(service.authorize_read("u1", &artifact.object_id).is_ok());
    assert_eq!(service.run_due_jobs(10).unwrap(), 0);
    assert!(service.job_status("u2", &first.job_id).is_err());
}

#[test]
fn budget_failures_are_terminal_and_cancellation_sticks() {
    let dir = tempfile::tempdir().unwrap();
    let mut probe = fixture_probe();
    probe.duration_ms = processing::MAX_DURATION_MS + 1;
    let service = service_at(dir.path()).with_runner(Arc::new(FixtureRunner::new(probe)));
    let done = upload(
        &service,
        "u1",
        "long.png",
        "image/png",
        Purpose::Video,
        &png_bytes(),
    );
    let job = service
        .submit_job("u1", &done.object_id, Preset::Thumbnail)
        .unwrap();
    service.run_due_jobs(10).unwrap();
    let failed = service.job_status("u1", &job.job_id).unwrap();
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(failed.attempts, 1);
    assert_eq!(service.run_due_jobs(10).unwrap(), 0);

    let canceled = service
        .submit_job("u1", &done.object_id, Preset::Playback720)
        .unwrap();
    let canceled = service.cancel_job("u1", &canceled.job_id).unwrap();
    assert_eq!(canceled.state, JobState::Canceled);
    assert_eq!(service.run_due_jobs(10).unwrap(), 0);
}

struct ChatFixture {
    snapshot: Vec<ChatSnapshotMessage>,
    imported: Mutex<Vec<ChatSnapshotMessage>>,
    report: ChatImportReport,
    /// Fault injection for the G05 snapshot port: #122's rollback semantics
    /// can only be exercised against an import that really fails halfway —
    /// this is the vendor-fault fixture the card explicitly allows.
    fail: std::sync::atomic::AtomicBool,
    /// 聊天记录游标被交接的次数；落盘失败时必须仍是 0。
    import_calls: std::sync::atomic::AtomicUsize,
}

impl ChatFixture {
    fn new(snapshot: Vec<ChatSnapshotMessage>, report: ChatImportReport) -> Self {
        Self {
            snapshot,
            imported: Mutex::new(Vec::new()),
            report,
            fail: std::sync::atomic::AtomicBool::new(false),
            import_calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.import_calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl ChatSnapshotPort for ChatFixture {
    fn snapshot(&self, _user_id: &str) -> Result<Vec<ChatSnapshotMessage>> {
        Ok(self.snapshot.clone())
    }

    fn import(
        &self,
        _user_id: &str,
        _device_id: &str,
        messages: &[ChatSnapshotMessage],
    ) -> Result<ChatImportReport> {
        self.import_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(MediaError::Unsupported("snapshot port is down".into()));
        }
        *self.imported.lock().unwrap() = messages.to_vec();
        Ok(self.report.clone())
    }
}

struct KeyFixture {
    seen: Mutex<Vec<String>>,
}

impl KeyEscrowPort for KeyFixture {
    fn restore_from_reference(&self, _user_id: &str, _package: &Path, key_ref: &str) -> Result<()> {
        self.seen.lock().unwrap().push(key_ref.to_string());
        Ok(())
    }
}

fn chat_messages() -> Vec<ChatSnapshotMessage> {
    vec![
        ChatSnapshotMessage {
            message_id: "m-1".into(),
            conv_id: "c1".into(),
            seq: 41,
            sender: "u1".into(),
            sent_at_ms: 1,
            body_ref: "hello".into(),
        },
        ChatSnapshotMessage {
            message_id: "m-2".into(),
            conv_id: "c1".into(),
            seq: 42,
            sender: "u1".into(),
            sent_at_ms: 2,
            body_ref: "world".into(),
        },
    ]
}

#[test]
fn backup_roundtrip_preserves_ids_and_rejects_tampering() {
    let source_dir = tempfile::tempdir().unwrap();
    let chat = Arc::new(ChatFixture::new(
        chat_messages(),
        ChatImportReport {
            imported: 2,
            conflicts: 1,
            preserved_local_seq: 1,
        },
    ));
    let keys = Arc::new(KeyFixture {
        seen: Mutex::new(Vec::new()),
    });
    let source = service_at(source_dir.path())
        .with_chat_port(chat.clone())
        .with_key_port(keys.clone());
    let first = upload(
        &source,
        "u1",
        "a.png",
        "image/png",
        Purpose::File,
        &png_bytes(),
    );
    upload(
        &source,
        "u1",
        "b.txt",
        "text/plain",
        Purpose::File,
        b"backup me",
    );
    let summary = source
        .export_backup("u1", "device-a", EncryptionMeta::client_side("kms-7"))
        .unwrap();
    assert_eq!(summary.entries, 2);
    let package = source.backup_package("u1", &summary.backup_id).unwrap();
    let verified = source.verify_backup(&package).unwrap();
    assert_eq!(verified.entries, 2);
    source
        .restore_backup_keys("u1", &summary.backup_id, &package)
        .unwrap();
    assert_eq!(keys.seen.lock().unwrap().as_slice(), ["kms-7".to_string()]);

    let target_dir = tempfile::tempdir().unwrap();
    let target = service_at(target_dir.path()).with_chat_port(chat.clone());
    let outcome = target.import_backup("u1", "device-b", &package).unwrap();
    assert_eq!(outcome.imported, 2);
    assert_eq!(outcome.state, "completed");
    assert_eq!(outcome.chat_imported, 2);
    assert_eq!(outcome.chat_conflicts, 1);
    let imported = chat.imported.lock().unwrap().clone();
    assert_eq!(imported[0].seq, 41);
    assert_eq!(imported[1].message_id, "m-2");
    let restored = target.authorize_read("u1", &first.object_id).unwrap();
    assert_eq!(restored.object.object_id, first.object_id);
    let page = target.list_files("u1", FileListQuery::default()).unwrap();
    assert_eq!(page.items.len(), 2);
    let rerun = target.import_backup("u1", "device-b", &package).unwrap();
    assert_eq!(rerun.skipped, 2);
    assert_eq!(rerun.imported, 0);

    let corrupt = target_dir.path().join("corrupt.wxbk");
    let mut bytes = std::fs::read(&package).unwrap();
    let middle = bytes.len() - 40;
    bytes[middle] ^= 0xFF;
    std::fs::write(&corrupt, bytes).unwrap();
    assert!(target
        .verify_backup(&corrupt)
        .is_err_and(|err| matches!(err, MediaError::Tampered(_))));
}

#[test]
fn cache_plan_protects_unique_objects_and_keeps_history_downloadable() {
    let dir = tempfile::tempdir().unwrap();
    let service = service_at(dir.path());
    let done = upload(
        &service,
        "u1",
        "keep.txt",
        "text/plain",
        Purpose::File,
        b"keep",
    );
    let cache_dir = dir.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    std::fs::write(cache_dir.join("download-1"), b"cached bytes").unwrap();
    let plan = service
        .plan_cache(
            "u1",
            0,
            vec![
                backup::CacheCandidate {
                    key: "download-1".into(),
                    object_id: Some(done.object_id.clone()),
                    bytes: 12,
                    kind: backup::CacheKind::Downloaded,
                },
                backup::CacheCandidate {
                    key: "unsent-unique".into(),
                    object_id: None,
                    bytes: 999,
                    kind: backup::CacheKind::Unknown,
                },
            ],
        )
        .unwrap();
    assert_eq!(plan.protected.len(), 1);
    assert_eq!(plan.protected[0].key, "unsent-unique");
    assert!(plan.entries.iter().any(|entry| entry.recoverable));
    let freed = service.execute_cache_plan(&plan).unwrap();
    assert!(freed > 0);
    assert!(!cache_dir.join("download-1").exists());
    assert!(service.authorize_read("u1", &done.object_id).is_ok());
    assert!(service
        .verify_object_digest(&done.object_id)
        .unwrap()
        .unwrap());
}

#[test]
fn platform_fixture_is_shared_across_bridges() {
    let fixture = r#"{"request_id":"r1","user_gesture":true,"capability":"save_download",
        "suggested_name":"report.pdf","bytes":10}"#;
    let request: PlatformRequest = serde_json::from_str(fixture).unwrap();
    let broker = PlatformBroker::new();
    let web = broker.authorize(PlatformKind::Web, "u1", &request).unwrap();
    assert!(matches!(
        web,
        PlatformResult::Saved {
            destination: SaveDestination::BrowserDownload { .. }
        }
    ));
    let desktop = broker
        .authorize(PlatformKind::Desktop, "u1", &request)
        .unwrap();
    assert!(matches!(
        desktop,
        PlatformResult::Saved {
            destination: SaveDestination::Dialog { .. }
        }
    ));
    let silent = PlatformRequest {
        user_gesture: false,
        ..request
    };
    assert!(matches!(
        broker.authorize(PlatformKind::Web, "u1", &silent).unwrap(),
        PlatformResult::PermissionDenied { .. }
    ));
    assert!(broker
        .authorize(
            PlatformKind::Web,
            "u1",
            &PlatformRequest {
                request_id: "r2".into(),
                user_gesture: true,
                kind: platform::PlatformRequestKind::SaveDownload {
                    suggested_name: "../escape.pdf".into(),
                    bytes: 1,
                },
            }
        )
        .is_err());
}

#[test]
fn session_ttl_expiry_closes_the_upload() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = MediaConfig::new(dir.path());
    config.session_ttl_ms = -1;
    let service = MediaService::open(config, None, Arc::new(Port::default())).unwrap();
    let start = service
        .upload_start(
            "u1",
            UploadStart {
                purpose: Purpose::File,
                name: "late.bin".into(),
                size: 4,
                mime: "application/octet-stream".into(),
                digest: String::new(),
            },
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(2));
    let status = service.upload_status("u1", &start.upload_id).unwrap();
    assert_eq!(status.status, UploadStatus::Expired);
    assert!(service
        .upload_chunk("u1", &start.upload_id, 0, b"data", None)
        .is_err());
}

/// 挂载路由（#295）之后，"提交转码"必须真的有人在执行——这条测的就是执行者。
///
/// 为什么单独一条：`run_due_jobs` 此前的调用方**只有测试**，所以"作业会做完"
/// 一直是靠测试自己调用它来成立的，运行中的进程里没人调。这条改成
/// "提交 → 交给 worker → 不主动调 run_due_jobs"，把它和上面那些用例区分开。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submitted_jobs_finish_without_the_test_calling_run_due_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let service =
        Arc::new(service_at(dir.path()).with_runner(Arc::new(FixtureRunner::new(fixture_probe()))));
    let done = upload(
        &service,
        "u1",
        "clip.png",
        "image/png",
        Purpose::Video,
        &png_bytes(),
    );
    let job = service
        .submit_job("u1", &done.object_id, Preset::Thumbnail)
        .unwrap();
    assert_eq!(
        service.job_status("u1", &job.job_id).unwrap().state,
        JobState::Pending,
        "提交后只入队，不该有东西替测试执行它"
    );

    let worker = Arc::clone(&service).spawn_job_worker(Duration::from_millis(5), 4);
    // 有界轮询而不是"睡一个够长的时间"：轮询次数用完仍不是 Succeeded 就红，
    // 不会因为 CI 慢就假失败，也不会因为本机快就假通过。
    let mut final_state = service.job_status("u1", &job.job_id).unwrap().state;
    for _ in 0..400 {
        let state = service.job_status("u1", &job.job_id).unwrap().state;
        if state == JobState::Succeeded {
            final_state = state;
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    worker.abort();
    assert_eq!(
        final_state,
        JobState::Succeeded,
        "worker 起了就必须把作业做完；没做完说明循环没跑或跑在错的地方"
    );
    assert_eq!(service.job_status("u1", &job.job_id).unwrap().progress, 100);
}

/// 导出计划必须与真实产品一致：同一查询序列、同一预算判定，外加
/// “ready 行背后字节还在”的完整性预检（#122 导出计划 + 完整性校验）。
#[test]
fn export_plan_matches_the_package_and_flags_missing_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let chat = Arc::new(ChatFixture::new(
        chat_messages(),
        ChatImportReport {
            imported: 2,
            conflicts: 0,
            preserved_local_seq: 0,
        },
    ));
    let service = service_at(dir.path()).with_chat_port(chat);
    let a = upload(
        &service,
        "u1",
        "a.bin",
        "application/octet-stream",
        Purpose::File,
        b"one",
    );
    let b = upload(
        &service,
        "u1",
        "b.bin",
        "application/octet-stream",
        Purpose::File,
        b"two!",
    );
    let plan = service
        .plan_backup_export("u1", "device-a", EncryptionMeta::client_side("kms-1"))
        .unwrap();
    assert_eq!(plan.entries.len(), 2);
    assert!(plan.within_budget && plan.publishable);
    assert_eq!(plan.chat_messages, 2);
    assert_eq!(plan.payload_bytes, a.size + b.size);
    assert!(plan.entries.iter().all(|entry| entry.present));
    let summary = service
        .export_backup("u1", "device-a", EncryptionMeta::client_side("kms-1"))
        .unwrap();
    assert_eq!(summary.total_bytes, plan.payload_bytes);
    assert_eq!(summary.entries as usize, plan.entries.len());
    let delta = (summary.package_bytes as i64 - plan.estimated_package_bytes as i64).abs();
    assert!(delta < 64, "计划估算与真实包尺寸必须接近: {delta}");

    // 完整性预检：盘上少字节，计划立刻判不可发布。
    std::fs::remove_file(dir.path().join(&a.object_id[..2]).join(&a.object_id)).unwrap();
    let plan = service
        .plan_backup_export("u1", "device-a", EncryptionMeta::plaintext())
        .unwrap();
    assert!(!plan.publishable);
    assert_eq!(plan.missing_objects, vec![a.object_id.clone()]);

    // 空库用户的计划是平凡可发布的空包。
    let empty = service
        .plan_backup_export("u9", "device-z", EncryptionMeta::plaintext())
        .unwrap();
    assert!(empty.entries.is_empty() && empty.publishable);
}

/// #122 恢复语义：先落盘（blob 内容校验 + fsync + 元数据同事务）再切游标
/// （G05 快照端口）。快照端口挂掉 = 游标未动 = 可回滚；回滚后重跑必须从零
/// 导入成功；completed 之后拒绝回滚（那是数据丢失，不是错误恢复）。
#[test]
fn failed_chat_import_lands_then_rolls_back_and_reimports() {
    let source_dir = tempfile::tempdir().unwrap();
    let source_chat = Arc::new(ChatFixture::new(
        chat_messages(),
        ChatImportReport::default(),
    ));
    let source = service_at(source_dir.path()).with_chat_port(source_chat);
    let a = upload(
        &source,
        "u1",
        "a.bin",
        "application/octet-stream",
        Purpose::File,
        b"alpha bytes",
    );
    let b = upload(
        &source,
        "u1",
        "b.bin",
        "application/octet-stream",
        Purpose::File,
        b"beta bytes!",
    );
    let summary = source
        .export_backup("u1", "device-a", EncryptionMeta::plaintext())
        .unwrap();
    let package = source.backup_package("u1", &summary.backup_id).unwrap();

    let target_dir = tempfile::tempdir().unwrap();
    let flaky = Arc::new(ChatFixture::new(Vec::new(), ChatImportReport::default()));
    flaky.fail.store(true, std::sync::atomic::Ordering::SeqCst);
    let target = service_at(target_dir.path()).with_chat_port(flaky.clone());
    let err = target
        .import_backup("u1", "device-b", &package)
        .unwrap_err();
    assert!(matches!(err, MediaError::Unsupported(_)));

    // 失败发生在“切游标”阶段：落盘半区已完整可读可下载。
    for id in [&a.object_id, &b.object_id] {
        let path = target_dir.path().join(&id[..2]).join(id);
        assert!(path.is_file(), "blob {id} 必须已落盘");
        assert!(target.authorize_read("u1", id).is_ok());
    }
    assert_eq!(
        target
            .list_files("u1", FileListQuery::default())
            .unwrap()
            .items
            .len(),
        2
    );

    let verified = target.verify_backup(&package).unwrap();
    let outcome = target
        .rollback_backup_import("u1", &verified.backup_id)
        .unwrap();
    assert_eq!(outcome.state, "rolled_back");
    assert_eq!(outcome.reset_entries, 2);
    assert_eq!(outcome.removed_objects, 2);
    assert_eq!(outcome.kept_objects, 0);
    for id in [&a.object_id, &b.object_id] {
        assert!(
            !target_dir.path().join(&id[..2]).join(id).exists(),
            "回滚后 blob {id} 必须被回收"
        );
        assert!(target.authorize_read("u1", id).is_err());
    }
    assert!(target
        .list_files("u1", FileListQuery::default())
        .unwrap()
        .items
        .is_empty());

    // 端口恢复 → 重跑 = 从零导入（游标已复位），不是残缺续跑；
    // 交给 G05 端口的消息必须保持包内原始 seq/message_id（不许本地重编号）。
    flaky.fail.store(false, std::sync::atomic::Ordering::SeqCst);
    let rerun = target.import_backup("u1", "device-b", &package).unwrap();
    assert_eq!(rerun.imported, 2);
    assert_eq!(rerun.skipped, 0);
    {
        let handed_over = flaky.imported.lock().unwrap();
        assert_eq!(handed_over.len(), 2);
        assert_eq!(handed_over[0].seq, 41);
        assert_eq!(handed_over[1].message_id, "m-2");
    }
    assert_eq!(rerun.state, "completed");

    // completed 拒绝回滚；他人/未知 id 一律 404 形状，不泄存在性。
    let err = target
        .rollback_backup_import("u1", &rerun.backup_id)
        .unwrap_err();
    assert!(matches!(err, MediaError::Conflict(_)));
    for id in [&a.object_id, &b.object_id] {
        assert!(target_dir.path().join(&id[..2]).join(id).is_file());
    }
    assert!(matches!(
        target
            .rollback_backup_import("u9", &rerun.backup_id)
            .unwrap_err(),
        MediaError::NotFound
    ));
    assert!(matches!(
        target.rollback_backup_import("u1", "bk_nope").unwrap_err(),
        MediaError::NotFound
    ));
    assert!(matches!(
        target.rollback_backup_import("u1", "").unwrap_err(),
        MediaError::BadInput(_)
    ));
}

/// 去重内容在回滚时必须保留：目标机上本地原件先引用着同一内容地址，
/// 导入只该加 restore 引用；回滚撤掉 restore 的文件行/引用后，
/// 对象因仍有本地引用而保留（kept_objects），原件照常可下载。
#[test]
fn rollback_keeps_content_the_local_library_still_needs() {
    let source_dir = tempfile::tempdir().unwrap();
    let source_chat = Arc::new(ChatFixture::new(Vec::new(), ChatImportReport::default()));
    let source = service_at(source_dir.path()).with_chat_port(source_chat);
    let payload = b"shared between both devices";
    let original = upload(
        &source,
        "u1",
        "shared.bin",
        "application/octet-stream",
        Purpose::File,
        payload,
    );
    let summary = source
        .export_backup("u1", "device-a", EncryptionMeta::plaintext())
        .unwrap();
    let package = source.backup_package("u1", &summary.backup_id).unwrap();

    let target_dir = tempfile::tempdir().unwrap();
    // 目标机先有同一内容的本地原件（dedup 命中），再导入同一备份包。
    let flaky = Arc::new(ChatFixture::new(Vec::new(), ChatImportReport::default()));
    let target = service_at(target_dir.path()).with_chat_port(flaky.clone());
    let local = upload(
        &target,
        "u1",
        "local-copy.bin",
        "application/octet-stream",
        Purpose::File,
        payload,
    );
    assert_eq!(local.object_id, original.object_id);
    flaky.fail.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(target.import_backup("u1", "device-b", &package).is_err());

    let outcome = target
        .rollback_backup_import("u1", &summary.backup_id)
        .unwrap();
    assert_eq!(outcome.reset_entries, 1);
    assert_eq!(outcome.removed_objects, 0);
    assert_eq!(outcome.kept_objects, 1);
    assert_eq!(outcome.removed_files, 1);
    // 本地原件完好，restore 的文件行已消失（列表回到 1 条）。
    let page = target.list_files("u1", FileListQuery::default()).unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].file_id, local.file_id);
    assert!(target.authorize_read("u1", &original.object_id).is_ok());
    assert!(target
        .verify_object_digest(&original.object_id)
        .unwrap()
        .unwrap());
}

/// 并发：同一备份包同时被两个导入者落地、又赶上第三个线程申请回滚时，
/// 状态机守卫必须保证账实一致——要么 completed（引用双份齐全），要么被
/// 回滚守卫挡住报错，绝不允许半套 restore 引用存活、也绝不双删对象。
#[test]
fn concurrent_imports_of_one_package_converge() {
    let source_dir = tempfile::tempdir().unwrap();
    let source_chat = Arc::new(ChatFixture::new(Vec::new(), ChatImportReport::default()));
    let source = service_at(source_dir.path()).with_chat_port(source_chat);
    upload(
        &source,
        "u1",
        "p.bin",
        "application/octet-stream",
        Purpose::File,
        b"parallel one",
    );
    upload(
        &source,
        "u1",
        "q.bin",
        "application/octet-stream",
        Purpose::File,
        b"parallel two",
    );
    let summary = source
        .export_backup("u1", "device-a", EncryptionMeta::plaintext())
        .unwrap();
    let package = source.backup_package("u1", &summary.backup_id).unwrap();

    let target_dir = tempfile::tempdir().unwrap();
    let chat = Arc::new(ChatFixture::new(Vec::new(), ChatImportReport::default()));
    let target = Arc::new(service_at(target_dir.path()).with_chat_port(chat));
    let package = Arc::new(package);
    let (imports, rollback) = std::thread::scope(|scope| {
        // 第三个线程赌一把回滚：抢到 importing/failed 就真回滚；抢不到必须
        // 干脆地拒绝（Conflict），早于任何 register 落地时是 NotFound。
        let rollback = scope.spawn(|| {
            target
                .rollback_backup_import("u1", &summary.backup_id)
                .map(|outcome| outcome.state)
        });
        let mut handles = Vec::new();
        for _ in 0..2 {
            let target = Arc::clone(&target);
            let package = Arc::clone(&package);
            handles.push(scope.spawn(move || target.import_backup("u1", "device-b", &package)));
        }
        let imports = handles.into_iter().map(|h| h.join()).collect::<Vec<_>>();
        (imports, rollback.join())
    });
    for outcome in imports.iter().filter_map(|res| res.as_ref().ok()) {
        match outcome {
            Ok(ok) => assert_eq!(ok.state, "completed"),
            Err(err) => assert!(
                matches!(err, MediaError::Conflict(_)),
                "并发只允许状态冲突类失败: {err:?}"
            ),
        }
    }
    if let Some(state) = rollback.as_ref().ok().and_then(|res| res.as_ref().ok()) {
        assert_eq!(state, "rolled_back");
    } else if let Some(err) = rollback.as_ref().ok().and_then(|res| res.as_ref().err()) {
        assert!(
            matches!(err, MediaError::Conflict(_) | MediaError::NotFound),
            "抢不到窗口的回滚必须干脆拒绝: {err:?}"
        );
    }
    // 不论交错顺序如何，收尾必须是完整一致的状态：把回滚后的局面重导一遍，
    // 第三次顺序导入总能得到 2 个可下载对象、2 条文件行。
    let final_outcome = target.import_backup("u1", "device-b", &package).unwrap();
    assert_eq!(final_outcome.state, "completed");
    let page = target.list_files("u1", FileListQuery::default()).unwrap();
    assert_eq!(page.items.len(), 2, "restore 文件行不许双份也不许残缺");
    let ids: Vec<&str> = page
        .items
        .iter()
        .map(|item| item.object_id.as_str())
        .collect();
    assert_eq!(
        ids.len(),
        page.items
            .iter()
            .map(|i| &i.object_id)
            .collect::<std::collections::HashSet<_>>()
            .len()
    );
    for item in &page.items {
        assert!(target.authorize_read("u1", &item.object_id).is_ok());
    }
}

/// 磁盘故障注入（#122「先落盘再切游标」的证伪面）：
/// `fail_put_at = N` 让第 N 次 `put_file` 失败；`land_then_error` 表示失败
/// 发生在字节已经写完并 rename 到位之后，即「落盘中途断电」那一格。
/// 其余方法一律原样委托，保证除注入点外行为不变。
#[derive(Default)]
struct Faults {
    puts: std::sync::atomic::AtomicUsize,
    fail_put_at: std::sync::atomic::AtomicUsize,
    land_then_error: std::sync::atomic::AtomicBool,
}

impl Faults {
    /// 让「下一次」`put_file` 失败（计数器已含此前的上传落盘）。
    fn fail_next_put(&self, land_then_error: bool) {
        let done = self.puts.load(std::sync::atomic::Ordering::SeqCst);
        self.fail_at(done + 1, land_then_error);
    }

    /// 让第 nth 次 `put_file` 失败（1 起）。
    fn fail_at(&self, nth: usize, land_then_error: bool) {
        self.fail_put_at
            .store(nth, std::sync::atomic::Ordering::SeqCst);
        self.land_then_error
            .store(land_then_error, std::sync::atomic::Ordering::SeqCst);
    }

    fn heal(&self) {
        self.fail_put_at
            .store(0, std::sync::atomic::Ordering::SeqCst);
    }
}

struct FaultStore {
    inner: LocalObjectStore,
    faults: Arc<Faults>,
}

impl ObjectStore for FaultStore {
    fn put_file(&self, object_id: &str, source: &Path) -> Result<u64> {
        let nth = self
            .faults
            .puts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        if self
            .faults
            .fail_put_at
            .load(std::sync::atomic::Ordering::SeqCst)
            == nth
        {
            if self
                .faults
                .land_then_error
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                // 字节真的落盘了，只是"落盘成功"这个回执没跑回来。
                self.inner.put_file(object_id, source)?;
            }
            return Err(MediaError::Io(std::io::Error::other(
                "injected disk failure",
            )));
        }
        self.inner.put_file(object_id, source)
    }

    fn put_bytes(&self, object_id: &str, bytes: &[u8]) -> Result<u64> {
        self.inner.put_bytes(object_id, bytes)
    }

    fn open(&self, object_id: &str) -> Result<Option<std::fs::File>> {
        self.inner.open(object_id)
    }

    fn read_range(&self, object_id: &str, start: u64, len: usize) -> Result<Option<Vec<u8>>> {
        self.inner.read_range(object_id, start, len)
    }

    fn size(&self, object_id: &str) -> Result<Option<u64>> {
        self.inner.size(object_id)
    }

    fn delete(&self, object_id: &str) -> Result<bool> {
        self.inner.delete(object_id)
    }

    fn upload_path(&self, upload_id: &str) -> std::path::PathBuf {
        self.inner.upload_path(upload_id)
    }

    fn work_path(&self, job_id: &str) -> std::path::PathBuf {
        self.inner.work_path(job_id)
    }

    fn cache_path(&self, key: &str) -> std::path::PathBuf {
        self.inner.cache_path(key)
    }
}

/// 盘上库 + 故障存储：游标是否前进必须由测试另开一条连接去查，
/// 不能只听被测代码自己报告。
fn faulty_service_at(root: &Path, faults: Arc<Faults>) -> MediaService {
    let objects = root.join("objects");
    let store = Arc::new(FaultStore {
        inner: LocalObjectStore::new(&objects).unwrap(),
        faults,
    });
    let db = root.join("media.db");
    MediaService::open(
        MediaConfig::new(&objects),
        db.to_str(),
        Arc::new(Port::default()),
    )
    .unwrap()
    .with_store(store)
}

/// 恢复现场快照：对象目录里到底有哪些字节，加上四张账表的行数与逐条游标。
/// 「回到恢复前状态」只有能被这份快照判定才算数。
#[derive(Debug)]
struct Scene {
    blobs: Vec<(String, u64)>,
    objects: i64,
    live_refs: i64,
    files: i64,
    cursors: Vec<(i64, i64, String)>,
    state: String,
}

fn scene(root: &Path) -> Scene {
    let db = root.join("media.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.busy_timeout(Duration::from_secs(10)).unwrap();
    let base = root.join("objects");
    let mut blobs = Vec::new();
    if let Ok(shards) = std::fs::read_dir(&base) {
        for shard in shards.flatten() {
            let Ok(files) = std::fs::read_dir(shard.path()) else {
                continue;
            };
            for file in files.flatten() {
                let name = file.file_name().to_string_lossy().to_string();
                if name.len() == 64 && name.chars().all(|c| c.is_ascii_hexdigit()) {
                    let bytes = file.metadata().map(|meta| meta.len()).unwrap_or(0);
                    blobs.push((
                        format!("{}/{}", shard.file_name().to_string_lossy(), name),
                        bytes,
                    ));
                }
            }
        }
    }
    blobs.sort();
    let objects: i64 = conn
        .query_row("SELECT COUNT(*) FROM media_object", [], |row| row.get(0))
        .unwrap();
    let live_refs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM media_ref WHERE deleted_at IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let files: i64 = conn
        .query_row("SELECT COUNT(*) FROM media_file", [], |row| row.get(0))
        .unwrap();
    let cursors = {
        let mut stmt = conn
            .prepare(
                "SELECT entry_idx, imported, object_id FROM media_backup_entry ORDER BY entry_idx",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap();
        rows.collect::<std::result::Result<_, _>>().unwrap()
    };
    let state = conn
        .query_row(
            "SELECT state FROM media_backup ORDER BY created_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or_default();
    Scene {
        blobs,
        objects,
        live_refs,
        files,
        cursors,
        state,
    }
}

fn two_entry_package(source_dir: &Path) -> (String, std::path::PathBuf, Vec<String>) {
    let source_chat = Arc::new(ChatFixture::new(
        chat_messages(),
        ChatImportReport::default(),
    ));
    let source = service_at(source_dir).with_chat_port(source_chat);
    let first = upload(
        &source,
        "u1",
        "a.bin",
        "application/octet-stream",
        Purpose::File,
        b"the first payload lands",
    );
    let second = upload(
        &source,
        "u1",
        "b.bin",
        "application/octet-stream",
        Purpose::File,
        b"the second payload dies",
    );
    let summary = source
        .export_backup("u1", "device-a", EncryptionMeta::plaintext())
        .unwrap();
    let package = source.backup_package("u1", &summary.backup_id).unwrap();
    (
        summary.backup_id,
        package,
        vec![first.object_id, second.object_id],
    )
}

/// (a) 落盘阶段真的失败（磁盘报错，一条字节都没写进去）时：
/// 游标绝不能跑到字节前面，聊天记录游标一次都不许被交接，
/// 状态必须是可回滚的 `failed`；盘修好后重跑 = 续跑而不是重导。
#[test]
fn disk_failure_while_landing_keeps_every_cursor_behind_the_bytes() {
    let source_dir = tempfile::tempdir().unwrap();
    let (backup_id, package, ids) = two_entry_package(source_dir.path());

    let target_dir = tempfile::tempdir().unwrap();
    let chat = Arc::new(ChatFixture::new(Vec::new(), ChatImportReport::default()));
    let faults = Arc::new(Faults::default());
    // 第二条落盘时报错：第一条正常落盘，落盘半途断掉。
    faults.fail_at(2, false);
    let target =
        faulty_service_at(target_dir.path(), Arc::clone(&faults)).with_chat_port(chat.clone());

    let err = target
        .import_backup("u1", "device-b", &package)
        .unwrap_err();
    assert!(
        matches!(err, MediaError::Io(_)),
        "磁盘故障必须如实报存储错误: {err:?}"
    );

    let after = scene(target_dir.path());
    assert_eq!(after.state, "failed", "落盘失败 = 可回滚的 failed");
    assert_eq!(after.blobs.len(), 1, "只落了 1 个 blob: {after:?}");
    // 核心不变式：imported=1 的条目，字节必须已经在盘上；反之亦然。
    let landed = after
        .cursors
        .iter()
        .filter(|(_, imported, _)| *imported == 1)
        .collect::<Vec<_>>();
    assert_eq!(landed.len(), 1, "恰好一条游标前进: {after:?}");
    for (_, imported, object_id) in &after.cursors {
        let present = after
            .blobs
            .iter()
            .any(|(path, _)| path.ends_with(object_id.as_str()));
        assert_eq!(
            *imported == 1,
            present,
            "游标(imported={imported}) 必须与盘上字节一致: {object_id}"
        );
        if *imported == 1 {
            assert!(target.authorize_read("u1", object_id).is_ok());
        } else {
            assert!(matches!(
                target.authorize_read("u1", object_id),
                Err(MediaError::NotFound)
            ));
        }
    }
    // 游标没走完 = 聊天真相一次都没交接（不许半途切游标）。
    assert_eq!(chat.calls(), 0, "落盘失败时聊天记录游标不得前进");
    assert_eq!(after.files, 1, "落了盘的条目才有文件行: {after:?}");

    // 磁盘修好 → 续跑：已游标条目 skip，未落盘条目补导，聊天只交接一次。
    faults.heal();
    let rerun = target.import_backup("u1", "device-b", &package).unwrap();
    assert_eq!(rerun.backup_id, backup_id);
    assert_eq!((rerun.skipped, rerun.imported), (1, 1));
    assert_eq!(rerun.state, "completed");
    assert_eq!(chat.calls(), 1);
    assert_eq!(chat.imported.lock().unwrap().len(), 2);
    let after = scene(target_dir.path());
    assert_eq!(after.blobs.len(), 2);
    assert!(after.cursors.iter().all(|(_, imported, _)| *imported == 1));
    assert!(ids.iter().all(|id| target.authorize_read("u1", id).is_ok()));
}

/// (b) 「落盘后、切游标前」断电那一格：字节已 rename 到位但回执丢了。
/// 游标必须仍是 0（账实不一致时以实为准，宁可重落），聊天真相不许交接；
/// 回滚必须把这份孤儿字节清掉，让现场逐项回到恢复前 —— 这才是「能回到恢复前」。
#[test]
fn crash_after_publish_before_cursor_is_rerolled_back_to_the_pre_restore_scene() {
    let source_dir = tempfile::tempdir().unwrap();
    let (backup_id, package, ids) = two_entry_package(source_dir.path());

    let target_dir = tempfile::tempdir().unwrap();
    let chat = Arc::new(ChatFixture::new(Vec::new(), ChatImportReport::default()));
    let faults = Arc::new(Faults::default());
    let target =
        faulty_service_at(target_dir.path(), Arc::clone(&faults)).with_chat_port(chat.clone());
    let before = scene(target_dir.path());
    assert!(before.blobs.is_empty() && before.objects == 0 && before.files == 0);

    // 第一条落盘：字节写完、rename 到位，然后断电（回执失败）。
    faults.fail_next_put(true);
    let err = target
        .import_backup("u1", "device-b", &package)
        .unwrap_err();
    assert!(matches!(err, MediaError::Io(_)), "{err:?}");

    let crashed = scene(target_dir.path());
    assert_eq!(crashed.state, "failed");
    assert_eq!(crashed.blobs.len(), 1, "孤儿字节确实在盘上");
    assert!(
        crashed
            .cursors
            .iter()
            .all(|(_, imported, _)| *imported == 0),
        "落盘中途断电不得让任何游标前进: {crashed:?}"
    );
    assert_eq!(crashed.objects, 0, "元数据事务没跑: {crashed:?}");
    assert_eq!(crashed.files, 0);
    assert_eq!(chat.calls(), 0, "游标没落齐 = 聊天真相不许交接");

    let outcome = target.rollback_backup_import("u1", &backup_id).unwrap();
    assert_eq!(outcome.state, "rolled_back");
    assert_eq!(outcome.reset_entries, 0, "没有游标可复位");
    assert_eq!(outcome.removed_objects, 1, "孤儿字节必须被回收");
    assert_eq!(outcome.kept_objects, 0);
    assert_eq!(outcome.removed_files, 0);

    // 逐项回到恢复前：字节、对象账、引用、文件行、游标全部复原。
    let after = scene(target_dir.path());
    assert_eq!(after.blobs, before.blobs, "回滚后盘上一字节不剩");
    assert_eq!(after.objects, before.objects);
    assert_eq!(after.live_refs, before.live_refs);
    assert_eq!(after.files, before.files);
    assert!(after.cursors.iter().all(|(_, imported, _)| *imported == 0));
    assert_eq!(after.state, "rolled_back");
    for id in &ids {
        assert!(matches!(
            target.authorize_read("u1", id),
            Err(MediaError::NotFound)
        ));
    }

    // 回滚不是终点：修好磁盘后必须能从零导入成功。
    faults.heal();
    let rerun = target.import_backup("u1", "device-b", &package).unwrap();
    assert_eq!(rerun.imported, 2, "回滚后重跑必须从零导入");
    assert_eq!(rerun.skipped, 0);
    assert_eq!(rerun.state, "completed");
    assert_eq!(chat.calls(), 1);
    let final_scene = scene(target_dir.path());
    assert_eq!(final_scene.blobs.len(), 2);
    assert_eq!(final_scene.files, 2);
    assert!(final_scene
        .cursors
        .iter()
        .all(|(_, imported, _)| *imported == 1));
    for id in &ids {
        assert!(target.authorize_read("u1", id).is_ok());
    }
}

/// 回滚不许误伤本地已有的同内容原件：崩溃残留的清扫只针对「账上无人引用」的
/// 地址，用户自己上传过的原件（有 `media_object` 行 + 引用 + 文件行）必须
/// 原样保留，即使它和孤儿字节同在一次失败的导入里。
#[test]
fn rollback_of_a_crashed_landing_keeps_a_local_original_of_the_same_content() {
    let source_dir = tempfile::tempdir().unwrap();
    let source_chat = Arc::new(ChatFixture::new(Vec::new(), ChatImportReport::default()));
    let source = service_at(source_dir.path()).with_chat_port(source_chat);
    let shared = upload(
        &source,
        "u1",
        "shared.bin",
        "application/octet-stream",
        Purpose::File,
        b"content the target already owns",
    );
    let unique = upload(
        &source,
        "u1",
        "unique.bin",
        "application/octet-stream",
        Purpose::File,
        b"content only this package carries",
    );
    let summary = source
        .export_backup("u1", "device-a", EncryptionMeta::plaintext())
        .unwrap();
    let package = source.backup_package("u1", &summary.backup_id).unwrap();

    let target_dir = tempfile::tempdir().unwrap();
    let chat = Arc::new(ChatFixture::new(Vec::new(), ChatImportReport::default()));
    let faults = Arc::new(Faults::default());
    let target =
        faulty_service_at(target_dir.path(), Arc::clone(&faults)).with_chat_port(chat.clone());
    let local = upload(
        &target,
        "u1",
        "local.bin",
        "application/octet-stream",
        Purpose::File,
        b"content the target already owns",
    );
    assert_eq!(local.object_id, shared.object_id);
    let objects = target_dir.path().join("objects");

    // dedup 命中不发 put_file，所以注入落在唯一内容那条上：字节到位、回执丢失。
    faults.fail_next_put(true);
    let err = target
        .import_backup("u1", "device-b", &package)
        .unwrap_err();
    assert!(matches!(err, MediaError::Io(_)), "{err:?}");
    assert_eq!(chat.calls(), 0, "落盘没跑完就不许切聊天游标");

    let outcome = target
        .rollback_backup_import("u1", &summary.backup_id)
        .unwrap();
    assert_eq!(outcome.state, "rolled_back");
    assert_eq!(outcome.removed_objects, 1, "只该回收那份无人引用的孤儿字节");

    // 本地原件：字节、对象账、文件行、可读性全部完好。
    assert!(objects
        .join(&local.object_id[..2])
        .join(&local.object_id)
        .is_file());
    assert!(!objects
        .join(&unique.object_id[..2])
        .join(&unique.object_id)
        .is_file());
    assert!(target.authorize_read("u1", &local.object_id).is_ok());
    assert!(target
        .verify_object_digest(&local.object_id)
        .unwrap()
        .unwrap());
    let page = target.list_files("u1", FileListQuery::default()).unwrap();
    assert_eq!(page.items.len(), 1, "本地文件行不能少，restore 行不能留");
    assert_eq!(page.items[0].file_id, local.file_id);
    let after = scene(target_dir.path());
    assert!(after.cursors.iter().all(|(_, imported, _)| *imported == 0));

    // 修好磁盘后重跑：唯一内容真落盘，同内容仍走 dedup。
    faults.heal();
    let rerun = target.import_backup("u1", "device-b", &package).unwrap();
    assert_eq!(rerun.backup_id, summary.backup_id);
    assert_eq!(rerun.state, "completed");
    assert_eq!(rerun.imported + rerun.deduped, 2);
    assert_eq!(rerun.skipped, 0);
    assert!(objects
        .join(&unique.object_id[..2])
        .join(&unique.object_id)
        .is_file());
    assert_eq!(chat.calls(), 1);
}
