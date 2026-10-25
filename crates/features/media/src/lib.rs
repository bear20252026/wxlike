//! G04 media domain: uploads, object authorization, file center, isolated
//! transcoding, backup/restore and client cache budgets.
//!
//! The crate owns the **business rules**. HTTP DTOs live in [`http`], SQLite in
//! [`db`], and the external seams are injected:
//! * [`AudiencePort`] resolves conversation/group membership (G02/G03);
//! * [`ChatSnapshotPort`] / [`KeyEscrowPort`] own chat history and keys (G02/G05);
//! * [`processing::TransformRunner`] executes FFmpeg or a test fixture.
//!
//! Content addresses are never capabilities. Every read goes through
//! [`access`], which checks owner, active reference audience or a revocable
//! share, and short-lived signed URLs are bound to a per-object access epoch.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

pub mod access;
pub mod backup;
pub mod db;
pub mod error;
pub mod files;
pub mod http;
pub mod ids;
pub mod model;
pub mod platform;
pub mod processing;
pub mod store;
pub mod uploads;

pub use access::{AccessGrant, AudiencePort, SignedAccess};
pub use backup::{
    BackupEntry, BackupManifest, BackupSummary, CacheCandidate, CacheEntry, CacheKind, CachePlan,
    ChatImportReport, ChatSnapshotMessage, ChatSnapshotPort, EncryptionMeta, EncryptionMode,
    ImportOutcome, KeyEscrowPort,
};
pub use db::MediaDb;
pub use error::{MediaError, Result};
pub use files::{
    AttachmentKind, AttachmentManifest, FileListQuery, FilePage, FileSort, ShareGrant,
};
pub use model::{
    Artifact, Audience, FileItem, JobState, ObjectRecord, Preset, Purpose, RefType, UploadStatus,
};
pub use platform::{
    CacheBudget, PlatformBroker, PlatformCapabilities, PlatformKind, PlatformRequest,
    PlatformRequestKind, PlatformResult,
};
pub use processing::{
    Budget, FfmpegRunner, FixtureRunner, JobView, ProbeInfo, TransformOutput, TransformRequest,
    TransformRunner,
};
pub use store::{LocalObjectStore, ObjectStore};
pub use uploads::{CompletedUpload, MediaCapabilities, UploadProgress, UploadSession, UploadStart};

pub const GIB: u64 = 1024 * MIB;
pub const MIB: u64 = 1024 * 1024;
pub const DEFAULT_MAX_OBJECT_BYTES: u64 = GIB;
pub const DEFAULT_MAX_CHUNK_BYTES: u64 = 8 * MIB;
pub const DEFAULT_MAX_USER_BYTES: u64 = 5 * GIB;
pub const DEFAULT_SESSION_TTL_MS: i64 = 24 * 3600 * 1000;
pub const DEFAULT_SIGNED_URL_TTL_MS: i64 = 5 * 60 * 1000;
pub const DEFAULT_SHARE_TTL_MS: i64 = 7 * 24 * 3600 * 1000;
pub const DEFAULT_JOB_STALE_MS: i64 = 30 * 60 * 1000;
pub const DEFAULT_MAX_JOB_ATTEMPTS: u32 = 3;
pub const SIGNING_KEY_BYTES: usize = 32;

#[derive(Clone, Debug)]
pub struct MediaConfig {
    pub object_root: PathBuf,
    pub max_object_bytes: u64,
    pub max_chunk_bytes: u64,
    pub max_user_bytes: u64,
    pub session_ttl_ms: i64,
    pub signed_url_ttl_ms: i64,
    pub share_ttl_ms: i64,
    pub job_stale_ms: i64,
    pub max_job_attempts: u32,
    pub signing_key: Vec<u8>,
}

impl MediaConfig {
    /// A development-ready configuration. Production **must** replace the
    /// generated signing key (`WXLIKE_MEDIA_SECRET`) so every instance agrees.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            object_root: root.into(),
            max_object_bytes: DEFAULT_MAX_OBJECT_BYTES,
            max_chunk_bytes: DEFAULT_MAX_CHUNK_BYTES,
            max_user_bytes: DEFAULT_MAX_USER_BYTES,
            session_ttl_ms: DEFAULT_SESSION_TTL_MS,
            signed_url_ttl_ms: DEFAULT_SIGNED_URL_TTL_MS,
            share_ttl_ms: DEFAULT_SHARE_TTL_MS,
            job_stale_ms: DEFAULT_JOB_STALE_MS,
            max_job_attempts: DEFAULT_MAX_JOB_ATTEMPTS,
            signing_key: ids::random_bytes(SIGNING_KEY_BYTES),
        }
    }

    pub fn with_signing_key(mut self, key: impl AsRef<[u8]>) -> Self {
        self.signing_key = key.as_ref().to_vec();
        self
    }

    pub fn with_limits(mut self, max_object: u64, max_chunk: u64) -> Self {
        self.max_object_bytes = max_object;
        self.max_chunk_bytes = max_chunk;
        self
    }

    pub fn capabilities(&self) -> MediaCapabilities {
        MediaCapabilities {
            max_object_bytes: self.max_object_bytes,
            max_chunk_bytes: self.max_chunk_bytes,
            max_user_bytes: self.max_user_bytes,
            session_ttl_ms: self.session_ttl_ms,
            signed_url_ttl_ms: self.signed_url_ttl_ms,
            max_share_ttl_ms: self.share_ttl_ms,
            max_page_limit: model::MAX_PAGE_LIMIT,
        }
    }
}

pub struct MediaService {
    pub(crate) db: MediaDb,
    pub(crate) store: Arc<dyn ObjectStore>,
    pub(crate) audience: Arc<dyn AudiencePort>,
    pub(crate) chat: Arc<dyn ChatSnapshotPort>,
    pub(crate) keys: Arc<dyn KeyEscrowPort>,
    pub(crate) runner: Arc<dyn processing::TransformRunner>,
    pub(crate) cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
    pub(crate) config: MediaConfig,
}

impl MediaService {
    pub fn open(
        config: MediaConfig,
        db_path: Option<&str>,
        audience: Arc<dyn AudiencePort>,
    ) -> Result<Self> {
        let store = Arc::new(LocalObjectStore::new(&config.object_root)?);
        Ok(Self {
            db: MediaDb::open(db_path)?,
            store,
            audience,
            chat: Arc::new(backup::NoChatSnapshot),
            keys: Arc::new(backup::NoKeyEscrow),
            runner: Arc::new(processing::FfmpegRunner::from_env()),
            cancels: Mutex::new(HashMap::new()),
            config,
        })
    }

    pub fn in_memory(root: impl Into<PathBuf>, audience: Arc<dyn AudiencePort>) -> Result<Self> {
        Self::open(MediaConfig::new(root), None, audience)
    }

    pub fn with_store(mut self, store: Arc<dyn ObjectStore>) -> Self {
        self.store = store;
        self
    }

    pub fn with_chat_port(mut self, chat: Arc<dyn ChatSnapshotPort>) -> Self {
        self.chat = chat;
        self
    }

    pub fn with_key_port(mut self, keys: Arc<dyn KeyEscrowPort>) -> Self {
        self.keys = keys;
        self
    }

    pub fn with_runner(mut self, runner: Arc<dyn processing::TransformRunner>) -> Self {
        self.runner = runner;
        self
    }

    pub fn capabilities(&self) -> MediaCapabilities {
        self.config.capabilities()
    }

    pub fn config(&self) -> &MediaConfig {
        &self.config
    }

    /// Open a stored object for streaming. Authorization happens in [`access`];
    /// this is the file handle the HTTP layer turns into a bounded stream.
    pub fn open_object(&self, object_id: &str) -> Result<Option<std::fs::File>> {
        ids::validate_object_id(object_id)?;
        self.store.open(object_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DenyAll;

    impl AudiencePort for DenyAll {
        fn may_read_conversation(&self, _: &str, _: &str) -> bool {
            false
        }
        fn may_read_group(&self, _: &str, _: &str) -> bool {
            false
        }
    }

    #[test]
    fn capabilities_publish_the_one_gib_promise() {
        let dir = std::env::temp_dir().join(format!("wxlike-media-cfg-{}", ids::random_hex(4)));
        let cfg = MediaConfig::new(&dir);
        let caps = cfg.capabilities();
        assert_eq!(caps.max_object_bytes, GIB);
        assert_eq!(caps.max_chunk_bytes, 8 * MIB);
        assert!(caps.max_chunk_bytes < caps.max_object_bytes);
        assert_eq!(cfg.signing_key.len(), SIGNING_KEY_BYTES);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn service_opens_with_the_proposal_schema() {
        let dir = std::env::temp_dir().join(format!("wxlike-media-svc-{}", ids::random_hex(4)));
        let service = MediaService::in_memory(&dir, Arc::new(DenyAll)).unwrap();
        let c = service.db.lock();
        let count: i64 = c
            .query_row("SELECT COUNT(*) FROM media_object", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
        drop(c);
        std::fs::remove_dir_all(dir).ok();
    }
}
