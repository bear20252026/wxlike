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
    let chat = Arc::new(ChatFixture {
        snapshot: chat_messages(),
        imported: Mutex::new(Vec::new()),
        report: ChatImportReport {
            imported: 2,
            conflicts: 1,
            preserved_local_seq: 1,
        },
    });
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
