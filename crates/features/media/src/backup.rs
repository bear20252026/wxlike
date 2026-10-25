//! Backup containers, resumable restore and cache-capacity planning.
//!
//! Wire format (little endian):
//!
//! ```text
//! "WXBK" | version u16 | manifest_len u32 | manifest JSON | payload... | sha256(all previous)
//! ```
//!
//! The trailer digest detects tampering; every payload object is additionally
//! checked against its content address on import, so a truncated or reordered
//! package cannot publish a wrong blob. Encryption metadata only carries a
//! `key_ref` into G02 — private keys never enter the package.
//!
//! Import is resumable: each entry flips `media_backup_entry.imported` in its
//! own transaction, and chat history is handed to the injected snapshot port,
//! which decides conflicts instead of letting a restore overwrite local truth.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::access::{add_ref_conn, RefSpec};
use crate::db::now_ms;
use crate::error::{MediaError, Result};
use crate::ids;
use crate::model::{Audience, Purpose, RefType};
use crate::store::STREAM_BUFFER_BYTES;
use crate::{MediaService, GIB, MIB};

pub const BACKUP_MAGIC: &[u8; 4] = b"WXBK";
pub const BACKUP_VERSION: u16 = 1;
pub const BACKUP_HEADER_LEN: usize = 10;
pub const BACKUP_TRAILER_LEN: usize = 32;
pub const BACKUP_MAX_BYTES: u64 = 64 * GIB;
pub const BACKUP_MAX_ENTRIES: usize = 100_000;
pub const BACKUP_PACKAGE_NAME: &str = "package.wxbk";
pub const BACKUP_FORMAT: &str = "wxlike-backup";
pub const CACHE_KEY_CHARS: usize = 128;
pub const CACHE_REASON_SERVER_COPY: &str = "server copy is ready and re-downloadable";
pub const CACHE_REASON_UNIQUE: &str = "no server copy; deleting would lose the only bytes";
pub const CACHE_REASON_STAGING: &str = "upload is still in flight";

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EncryptionMode {
    None,
    ClientSide,
}

/// Key material is **not** part of this struct by construction. `key_ref` is an
/// opaque pointer the G02 escrow port resolves during restore.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct EncryptionMeta {
    pub mode: EncryptionMode,
    pub key_ref: String,
}

impl EncryptionMeta {
    pub fn plaintext() -> Self {
        Self {
            mode: EncryptionMode::None,
            key_ref: String::new(),
        }
    }

    pub fn client_side(key_ref: impl Into<String>) -> Self {
        Self {
            mode: EncryptionMode::ClientSide,
            key_ref: key_ref.into(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BackupEntry {
    pub index: u32,
    pub object_id: String,
    pub size: u64,
    pub mime: String,
    pub name: String,
    pub category: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ChatSnapshotMessage {
    pub message_id: String,
    pub conv_id: String,
    pub seq: i64,
    pub sender: String,
    pub sent_at_ms: i64,
    pub body_ref: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BackupManifest {
    pub format: String,
    pub version: u16,
    pub backup_id: String,
    pub created_at: i64,
    pub device_id: String,
    pub encryption: EncryptionMeta,
    pub entries: Vec<BackupEntry>,
    pub chat_messages: Vec<ChatSnapshotMessage>,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct BackupSummary {
    pub backup_id: String,
    pub state: String,
    pub created_at: i64,
    pub entries: u32,
    pub total_bytes: u64,
    pub package_digest: String,
    pub package_bytes: u64,
    pub encryption: EncryptionMeta,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ChatImportReport {
    pub imported: u32,
    pub conflicts: u32,
    pub preserved_local_seq: u32,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportOutcome {
    pub backup_id: String,
    pub imported: u32,
    pub deduped: u32,
    pub skipped: u32,
    pub chat_imported: u32,
    pub chat_conflicts: u32,
    pub state: String,
}

/// Chat data is owned by G02/G05. A restore must never renumber message ids or
/// seq values; the port reports conflicts instead of overwriting newer truth.
pub trait ChatSnapshotPort: Send + Sync {
    fn snapshot(&self, _user_id: &str) -> Result<Vec<ChatSnapshotMessage>> {
        Ok(Vec::new())
    }

    fn import(
        &self,
        _user_id: &str,
        _device_id: &str,
        _messages: &[ChatSnapshotMessage],
    ) -> Result<ChatImportReport> {
        Ok(ChatImportReport::default())
    }
}

/// Private-key restore belongs to G02 as well. Implementations must unwrap the
/// key from escrow; this crate only passes the package path and `key_ref`.
pub trait KeyEscrowPort: Send + Sync {
    fn restore_from_reference(&self, user_id: &str, package: &Path, key_ref: &str) -> Result<()>;
}

pub struct NoChatSnapshot;
impl ChatSnapshotPort for NoChatSnapshot {}

pub struct NoKeyEscrow;
impl KeyEscrowPort for NoKeyEscrow {
    fn restore_from_reference(&self, _: &str, _: &Path, _: &str) -> Result<()> {
        Err(MediaError::Unsupported(
            "key escrow port is not configured".into(),
        ))
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheKind {
    Derived,
    Downloaded,
    UploadStaging,
    Unknown,
}

#[derive(Deserialize, Clone, Debug)]
pub struct CacheCandidate {
    pub key: String,
    #[serde(default)]
    pub object_id: Option<String>,
    pub bytes: u64,
    #[serde(default = "default_cache_kind")]
    pub kind: CacheKind,
}

fn default_cache_kind() -> CacheKind {
    CacheKind::Downloaded
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CacheEntry {
    pub key: String,
    pub object_id: Option<String>,
    pub bytes: u64,
    pub kind: CacheKind,
    pub recoverable: bool,
    pub reason: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CachePlan {
    pub budget_bytes: u64,
    pub total_bytes: u64,
    pub reclaim_bytes: u64,
    pub can_meet_budget: bool,
    pub entries: Vec<CacheEntry>,
    pub protected: Vec<CacheEntry>,
}

#[derive(Clone, Debug)]
struct StoredBackup {
    backup_id: String,
}

impl MediaService {
    fn backup_work_dir(&self, backup_id: &str) -> PathBuf {
        self.store.work_path(backup_id)
    }

    fn backup_package_path(&self, backup_id: &str) -> PathBuf {
        self.backup_work_dir(backup_id).join(BACKUP_PACKAGE_NAME)
    }

    fn owner_entries(&self, user_id: &str) -> Result<Vec<BackupEntry>> {
        let c = self.db.lock();
        let mut stmt = c.prepare(
            "SELECT o.object_id, o.size, o.mime,
                    COALESCE(f.name, ''), COALESCE(f.category, 'file')
             FROM media_object o
             LEFT JOIN media_file f ON f.object_id = o.object_id AND f.owner = o.owner
             WHERE o.owner = ?1 AND o.status = 'ready'
             ORDER BY o.created_at, o.object_id
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![user_id, BACKUP_MAX_ENTRIES as i64], |row| {
            Ok(BackupEntry {
                index: 0,
                object_id: row.get(0)?,
                size: row.get::<_, i64>(1)?.max(0) as u64,
                mime: row.get(2)?,
                name: row.get(3)?,
                category: row.get(4)?,
            })
        })?;
        let mut entries = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        for (index, entry) in entries.iter_mut().enumerate() {
            entry.index = index as u32;
        }
        Ok(entries)
    }

    /// Export everything the user still owns. The package is a private work
    /// file; callers download it through [`Self::backup_package`] which checks
    /// ownership again.
    pub fn export_backup(
        &self,
        user_id: &str,
        device_id: &str,
        encryption: EncryptionMeta,
    ) -> Result<BackupSummary> {
        if encryption.key_ref.len() > ids::MAX_NAME_CHARS {
            return Err(MediaError::bad("key reference is too long"));
        }
        let entries = self.owner_entries(user_id)?;
        let total_bytes: u64 = entries.iter().map(|entry| entry.size).sum();
        if total_bytes > BACKUP_MAX_BYTES {
            return Err(MediaError::BudgetExceeded("backup payload".into()));
        }
        let backup_id = ids::opaque_id("bk");
        let manifest = BackupManifest {
            format: BACKUP_FORMAT.into(),
            version: BACKUP_VERSION,
            backup_id: backup_id.clone(),
            created_at: now_ms(),
            device_id: device_id.to_string(),
            encryption: encryption.clone(),
            entries: entries.clone(),
            chat_messages: self.chat.snapshot(user_id)?,
        };
        let dir = self.backup_work_dir(&backup_id);
        std::fs::create_dir_all(&dir)?;
        let package = self.backup_package_path(&backup_id);
        let (package_digest, package_bytes) = self.write_container(&package, &manifest)?;
        let summary = BackupSummary {
            backup_id: backup_id.clone(),
            state: "ready".into(),
            created_at: manifest.created_at,
            entries: entries.len() as u32,
            total_bytes,
            package_digest: package_digest.clone(),
            package_bytes,
            encryption,
        };
        let manifest_json = serde_json::to_string(&manifest)?;
        {
            let c = self.db.lock();
            c.execute(
                "INSERT INTO media_backup
                    (backup_id, owner, state, manifest, package_path, package_digest,
                     created_at, updated_at)
                 VALUES(?1, ?2, 'ready', ?3, ?4, ?5, ?6, ?6)",
                params![
                    backup_id,
                    user_id,
                    manifest_json,
                    package.to_string_lossy(),
                    package_digest,
                    manifest.created_at
                ],
            )?;
        }
        Ok(summary)
    }

    fn write_container(&self, package: &Path, manifest: &BackupManifest) -> Result<(String, u64)> {
        let manifest_bytes = serde_json::to_vec(manifest)?;
        if manifest_bytes.len() > u32::MAX as usize {
            return Err(MediaError::PayloadTooLarge);
        }
        let mut file = std::fs::File::create(package)?;
        let mut hasher = Sha256::new();
        let mut written = 0u64;
        write_hashed(&mut file, &mut hasher, BACKUP_MAGIC)?;
        write_hashed(&mut file, &mut hasher, &BACKUP_VERSION.to_le_bytes())?;
        write_hashed(
            &mut file,
            &mut hasher,
            &(manifest_bytes.len() as u32).to_le_bytes(),
        )?;
        write_hashed(&mut file, &mut hasher, &manifest_bytes)?;
        for entry in &manifest.entries {
            written += self.copy_object_into(&mut file, &mut hasher, entry)?;
            if written > BACKUP_MAX_BYTES {
                return Err(MediaError::BudgetExceeded("backup payload".into()));
            }
        }
        let trailer = hasher.finalize();
        file.write_all(&trailer)?;
        file.flush()?;
        drop(file);
        let digest = crate::store::stream_digest_file(package)?;
        let bytes = std::fs::metadata(package)?.len();
        Ok((digest, bytes))
    }

    fn copy_object_into(
        &self,
        file: &mut std::fs::File,
        hasher: &mut Sha256,
        entry: &BackupEntry,
    ) -> Result<u64> {
        let Some(mut source) = self.store.open(&entry.object_id)? else {
            return Err(MediaError::NotFound);
        };
        let mut buf = vec![0u8; STREAM_BUFFER_BYTES];
        let mut copied = 0u64;
        loop {
            let read = source.read(&mut buf)?;
            if read == 0 {
                break;
            }
            write_hashed(file, hasher, &buf[..read])?;
            copied += read as u64;
        }
        if copied != entry.size {
            return Err(MediaError::Tampered(format!(
                "object {} changed while exporting",
                entry.object_id
            )));
        }
        Ok(copied)
    }

    pub fn backup_package(&self, user_id: &str, backup_id: &str) -> Result<PathBuf> {
        let stored = self.stored_backup(backup_id)?.ok_or(MediaError::NotFound)?;
        let owned: i64 = {
            let c = self.db.lock();
            c.query_row(
                "SELECT COUNT(*) FROM media_backup WHERE backup_id = ?1 AND owner = ?2",
                params![backup_id, user_id],
                |row| row.get(0),
            )?
        };
        if owned == 0 {
            return Err(MediaError::NotFound);
        }
        let path = self.backup_package_path(&stored.backup_id);
        if !path.is_file() {
            return Err(MediaError::Gone("backup package was cleaned up".into()));
        }
        Ok(path)
    }

    fn stored_backup(&self, backup_id: &str) -> Result<Option<StoredBackup>> {
        Ok(self
            .db
            .lock()
            .query_row(
                "SELECT backup_id FROM media_backup WHERE backup_id = ?1",
                params![backup_id],
                |row| {
                    Ok(StoredBackup {
                        backup_id: row.get(0)?,
                    })
                },
            )
            .optional()?)
    }

    /// Read and fully verify a package without touching stored objects.
    pub fn verify_backup(&self, package: &Path) -> Result<BackupSummary> {
        let parsed = self.read_container(package, false)?;
        let manifest = parsed.manifest;
        let total_bytes: u64 = manifest.entries.iter().map(|entry| entry.size).sum();
        Ok(BackupSummary {
            backup_id: manifest.backup_id,
            state: "verified".into(),
            created_at: manifest.created_at,
            entries: manifest.entries.len() as u32,
            total_bytes,
            package_digest: parsed.package_digest,
            package_bytes: parsed.package_bytes,
            encryption: manifest.encryption,
        })
    }

    /// Verify then import. Every entry keeps its historical object id; blobs
    /// already present are deduplicated rather than rewritten. Re-running after
    /// an interruption resumes at the first unimported entry.
    pub fn import_backup(
        &self,
        user_id: &str,
        device_id: &str,
        package: &Path,
    ) -> Result<ImportOutcome> {
        let parsed = self.read_container(package, true)?;
        let manifest = parsed.manifest;
        self.register_import(user_id, &manifest, &parsed.package_digest)?;
        let mut outcome = ImportOutcome {
            backup_id: manifest.backup_id.clone(),
            state: "importing".into(),
            ..Default::default()
        };
        for (entry, payload) in manifest.entries.iter().zip(parsed.payloads.iter()) {
            self.import_entry(user_id, &manifest.backup_id, entry, payload, &mut outcome)?;
        }
        self.import_chat(user_id, device_id, &manifest, &mut outcome)?;
        self.finish_import(&manifest.backup_id, &mut outcome)?;
        Ok(outcome)
    }

    fn register_import(
        &self,
        user_id: &str,
        manifest: &BackupManifest,
        digest: &str,
    ) -> Result<()> {
        let manifest_json = serde_json::to_string(manifest)?;
        let now = now_ms();
        {
            let c = self.db.lock();
            c.execute(
                "INSERT INTO media_backup
                    (backup_id, owner, state, manifest, package_path, package_digest,
                     created_at, updated_at)
                 VALUES(?1, ?2, 'importing', ?3, '', ?4, ?5, ?5)
                 ON CONFLICT(backup_id) DO UPDATE SET state = 'importing', updated_at = ?5",
                params![manifest.backup_id, user_id, manifest_json, digest, now],
            )?;
            for entry in &manifest.entries {
                c.execute(
                    "INSERT OR IGNORE INTO media_backup_entry
                        (backup_id, entry_idx, object_id, digest, size, imported)
                     VALUES(?1, ?2, ?3, ?3, ?4, 0)",
                    params![
                        manifest.backup_id,
                        i64::from(entry.index),
                        entry.object_id,
                        entry.size as i64
                    ],
                )?;
            }
        }
        Ok(())
    }

    fn import_entry(
        &self,
        user_id: &str,
        backup_id: &str,
        entry: &BackupEntry,
        payload: &Path,
        outcome: &mut ImportOutcome,
    ) -> Result<()> {
        let imported: i64 = {
            let c = self.db.lock();
            c.query_row(
                "SELECT imported FROM media_backup_entry WHERE backup_id = ?1 AND entry_idx = ?2",
                params![backup_id, i64::from(entry.index)],
                |row| row.get(0),
            )?
        };
        if imported != 0 {
            outcome.skipped += 1;
            return Ok(());
        }
        let existed = self.object_exists(&entry.object_id)?;
        if !existed {
            let digest = crate::store::stream_digest_file(payload)?;
            if digest != entry.object_id {
                std::fs::remove_file(payload).ok();
                return Err(MediaError::Tampered("entry digest mismatch".into()));
            }
            self.store.put_file(&entry.object_id, payload)?;
        }
        std::fs::remove_file(payload).ok();
        self.restored_object(user_id, backup_id, entry)?;
        {
            let c = self.db.lock();
            c.execute(
                "UPDATE media_backup_entry SET imported = 1
                 WHERE backup_id = ?1 AND entry_idx = ?2",
                params![backup_id, i64::from(entry.index)],
            )?;
        }
        if existed {
            outcome.deduped += 1;
        } else {
            outcome.imported += 1;
        }
        Ok(())
    }

    fn import_chat(
        &self,
        user_id: &str,
        device_id: &str,
        manifest: &BackupManifest,
        outcome: &mut ImportOutcome,
    ) -> Result<()> {
        let report = self
            .chat
            .import(user_id, device_id, &manifest.chat_messages)?;
        outcome.chat_imported = report.imported;
        outcome.chat_conflicts = report.conflicts;
        Ok(())
    }

    fn finish_import(&self, backup_id: &str, outcome: &mut ImportOutcome) -> Result<()> {
        {
            let c = self.db.lock();
            c.execute(
                "UPDATE media_backup SET state = 'completed', updated_at = ?1
                 WHERE backup_id = ?2",
                params![now_ms(), backup_id],
            )?;
        }
        outcome.state = "completed".into();
        Ok(())
    }

    /// Restore keys through the G02 escrow port. The package never contains key
    /// material, only the reference recorded in its manifest.
    pub fn restore_backup_keys(
        &self,
        user_id: &str,
        backup_id: &str,
        package: &Path,
    ) -> Result<String> {
        let manifest = self.read_manifest_only(package)?;
        if manifest.backup_id != backup_id {
            return Err(MediaError::bad("package does not match backup id"));
        }
        if manifest.encryption.key_ref.is_empty() {
            return Err(MediaError::Unsupported("backup has no escrowed key".into()));
        }
        self.keys
            .restore_from_reference(user_id, package, &manifest.encryption.key_ref)?;
        Ok(manifest.encryption.key_ref)
    }

    /// Copy an uploaded backup package into a private work file. The object is
    /// authorized as the caller's own upload, so a foreign object id cannot be
    /// passed to the importer.
    pub fn stage_backup_object(&self, user_id: &str, object_id: &str) -> Result<PathBuf> {
        let grant = self.authorize_read(user_id, object_id)?;
        if grant.object.owner != user_id {
            return Err(MediaError::Forbidden);
        }
        let path = self
            .store
            .cache_path(&format!("stage-{}", ids::random_hex(8)));
        let Some(mut source) = self.store.open(&grant.object.object_id)? else {
            return Err(MediaError::NotFound);
        };
        let mut target = std::fs::File::create(&path)?;
        std::io::copy(&mut source, &mut target)?;
        target.flush()?;
        Ok(path)
    }

    pub fn verify_backup_object(&self, user_id: &str, object_id: &str) -> Result<BackupSummary> {
        let path = self.stage_backup_object(user_id, object_id)?;
        let result = self.verify_backup(&path);
        std::fs::remove_file(&path).ok();
        result
    }

    pub fn import_backup_object(
        &self,
        user_id: &str,
        device_id: &str,
        object_id: &str,
    ) -> Result<ImportOutcome> {
        let path = self.stage_backup_object(user_id, object_id)?;
        let result = self.import_backup(user_id, device_id, &path);
        std::fs::remove_file(&path).ok();
        result
    }

    fn read_manifest_only(&self, package: &Path) -> Result<BackupManifest> {
        Ok(self.read_container(package, false)?.manifest)
    }

    fn restored_object(&self, user_id: &str, backup_id: &str, entry: &BackupEntry) -> Result<()> {
        let now = now_ms();
        let file_id = format!("restore_{backup_id}_{}", entry.index);
        let name = if entry.name.is_empty() {
            format!("restored-{}", entry.index)
        } else {
            entry.name.clone()
        };
        let c = self.db.lock();
        c.execute(
            "INSERT INTO media_object
                (object_id, owner, size, digest, mime, status, access_epoch, created_at)
             VALUES(?1, ?2, ?3, ?1, ?4, 'ready', 0, ?5)
             ON CONFLICT(object_id) DO NOTHING",
            params![entry.object_id, user_id, entry.size as i64, entry.mime, now],
        )?;
        let audience = Audience::User {
            id: user_id.to_string(),
        };
        let spec = RefSpec {
            ref_type: RefType::File,
            ext_id: &file_id,
            owner: user_id,
            audience: &audience,
            purpose: Purpose::File,
        };
        add_ref_conn(&c, &entry.object_id, &spec, now)?;
        c.execute(
            "INSERT OR IGNORE INTO media_file
                (file_id, object_id, owner, name, folder, category, size, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, '', ?5, ?6, ?7, ?7)",
            params![
                file_id,
                entry.object_id,
                user_id,
                name,
                entry.category,
                entry.size as i64,
                now
            ],
        )?;
        Ok(())
    }

    fn read_container(&self, package: &Path, keep_payloads: bool) -> Result<ParsedContainer> {
        let meta = std::fs::metadata(package)?;
        if meta.len() > BACKUP_MAX_BYTES + MIB {
            return Err(MediaError::PayloadTooLarge);
        }
        let mut file = std::fs::File::open(package)?;
        let mut hasher = Sha256::new();
        let manifest = read_header(&mut file, &mut hasher)?;
        validate_manifest(&manifest)?;
        let payloads =
            self.read_payloads(&mut file, &mut hasher, &manifest.entries, keep_payloads)?;
        verify_trailer(&mut file, hasher, &payloads)?;
        let package_digest = crate::store::stream_digest_file(package)?;
        Ok(ParsedContainer {
            manifest,
            payloads,
            package_digest,
            package_bytes: std::fs::metadata(package)?.len(),
        })
    }

    fn read_payloads(
        &self,
        file: &mut std::fs::File,
        hasher: &mut Sha256,
        entries: &[BackupEntry],
        keep: bool,
    ) -> Result<Vec<PathBuf>> {
        let mut payloads = Vec::new();
        let mut buf = vec![0u8; STREAM_BUFFER_BYTES];
        for entry in entries {
            let temp = self
                .store
                .cache_path(&format!("verify-{}", ids::random_hex(8)));
            let mut out = keep.then(|| std::fs::File::create(&temp)).transpose()?;
            let digest = copy_hashed(file, hasher, entry.size, &mut buf, out.as_mut())?;
            if digest != entry.object_id {
                std::fs::remove_file(&temp).ok();
                cleanup_payloads(&payloads);
                return Err(MediaError::Tampered(format!(
                    "entry {} digest mismatch",
                    entry.index
                )));
            }
            if let Some(out) = out.as_mut() {
                out.flush()?;
                payloads.push(temp);
            } else {
                std::fs::remove_file(&temp).ok();
            }
        }
        Ok(payloads)
    }

    /// Plan a cache sweep. A candidate is recoverable only when the server still
    /// holds a ready copy; anything unique stays protected, so an unsent
    /// attachment is never deleted by a budget decision.
    pub fn plan_cache(
        &self,
        _user_id: &str,
        budget_bytes: u64,
        candidates: Vec<CacheCandidate>,
    ) -> Result<CachePlan> {
        let mut entries = Vec::new();
        let mut protected = Vec::new();
        for candidate in candidates {
            let entry = self.classify_cache_candidate(candidate)?;
            if entry.recoverable {
                entries.push(entry);
            } else {
                protected.push(entry);
            }
        }
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.bytes));
        let total_bytes: u64 = entries
            .iter()
            .chain(protected.iter())
            .map(|e| e.bytes)
            .sum();
        let needed = total_bytes.saturating_sub(budget_bytes);
        let mut reclaim_bytes = 0u64;
        for entry in &mut entries {
            if reclaim_bytes >= needed {
                entry.recoverable = false;
                entry.reason = "not needed for the budget".into();
            } else {
                reclaim_bytes += entry.bytes;
            }
        }
        Ok(CachePlan {
            budget_bytes,
            total_bytes,
            reclaim_bytes,
            can_meet_budget: total_bytes.saturating_sub(reclaim_bytes) <= budget_bytes,
            entries,
            protected,
        })
    }

    fn classify_cache_candidate(&self, candidate: CacheCandidate) -> Result<CacheEntry> {
        let key_valid = validate_cache_key(&candidate.key);
        let server_ready = match candidate.object_id.as_deref() {
            Some(id) if ids::is_sha256_hex(id) => self
                .object(id)
                .map(|object| object.ready() && self.store.size(id).ok().flatten().is_some())
                .unwrap_or(false),
            _ => false,
        };
        let (recoverable, reason) = if !key_valid {
            (false, CACHE_REASON_UNIQUE)
        } else if candidate.kind == CacheKind::UploadStaging {
            (false, CACHE_REASON_STAGING)
        } else if server_ready {
            (true, CACHE_REASON_SERVER_COPY)
        } else {
            (false, CACHE_REASON_UNIQUE)
        };
        Ok(CacheEntry {
            key: candidate.key,
            object_id: candidate.object_id,
            bytes: candidate.bytes,
            kind: candidate.kind,
            recoverable,
            reason: reason.into(),
        })
    }

    /// Delete only entries the plan marked recoverable, re-checking the server
    /// copy each time. Returns freed bytes.
    pub fn execute_cache_plan(&self, plan: &CachePlan) -> Result<u64> {
        let mut freed = 0u64;
        for entry in &plan.entries {
            if !entry.recoverable {
                continue;
            }
            if let Some(id) = entry.object_id.as_deref() {
                let ready = self.object(id).map(|o| o.ready()).unwrap_or(false)
                    && self.store.size(id).ok().flatten().is_some();
                if !ready {
                    continue;
                }
            }
            let path = self.store.cache_path(&entry.key);
            if let Ok(meta) = std::fs::metadata(&path) {
                if std::fs::remove_file(&path).is_ok() {
                    freed += meta.len();
                }
            }
        }
        Ok(freed)
    }
}

fn read_header(file: &mut std::fs::File, hasher: &mut Sha256) -> Result<BackupManifest> {
    let mut magic = [0u8; 4];
    read_hashed(file, hasher, &mut magic)?;
    if &magic != BACKUP_MAGIC {
        return Err(MediaError::Tampered("bad backup magic".into()));
    }
    let version = read_version(file, hasher)?;
    if version != BACKUP_VERSION {
        return Err(MediaError::Unsupported(format!("backup version {version}")));
    }
    let manifest_len = read_u32_hashed(file, hasher)?;
    let mut manifest_bytes = vec![0u8; manifest_len as usize];
    read_hashed(file, hasher, &mut manifest_bytes)?;
    Ok(serde_json::from_slice(&manifest_bytes)?)
}

fn copy_hashed(
    file: &mut std::fs::File,
    hasher: &mut Sha256,
    size: u64,
    buf: &mut [u8],
    mut out: Option<&mut std::fs::File>,
) -> Result<String> {
    let mut entry_hasher = Sha256::new();
    let mut remaining = size;
    while remaining > 0 {
        let want = (STREAM_BUFFER_BYTES as u64).min(remaining) as usize;
        read_hashed(file, hasher, &mut buf[..want])?;
        entry_hasher.update(&buf[..want]);
        if let Some(out) = out.as_deref_mut() {
            out.write_all(&buf[..want])?;
        }
        remaining -= want as u64;
    }
    Ok(hex::encode(entry_hasher.finalize()))
}

fn verify_trailer(file: &mut std::fs::File, hasher: Sha256, payloads: &[PathBuf]) -> Result<()> {
    let expected = hasher.finalize();
    let mut trailer = [0u8; BACKUP_TRAILER_LEN];
    file.read_exact(&mut trailer)?;
    if trailer != expected.as_slice() {
        cleanup_payloads(payloads);
        return Err(MediaError::Tampered("package trailer mismatch".into()));
    }
    Ok(())
}

fn cleanup_payloads(payloads: &[PathBuf]) {
    for path in payloads {
        std::fs::remove_file(path).ok();
    }
}

struct ParsedContainer {
    manifest: BackupManifest,
    payloads: Vec<PathBuf>,
    package_digest: String,
    package_bytes: u64,
}

fn validate_manifest(manifest: &BackupManifest) -> Result<()> {
    if manifest.format != BACKUP_FORMAT || manifest.version != BACKUP_VERSION {
        return Err(MediaError::Unsupported("backup format mismatch".into()));
    }
    if manifest.entries.len() > BACKUP_MAX_ENTRIES {
        return Err(MediaError::PayloadTooLarge);
    }
    for (position, entry) in manifest.entries.iter().enumerate() {
        if entry.index as usize != position {
            return Err(MediaError::Tampered("entry order mismatch".into()));
        }
        ids::validate_object_id(&entry.object_id)?;
        if entry.size == 0 || entry.size > BACKUP_MAX_BYTES {
            return Err(MediaError::Tampered("entry size is out of range".into()));
        }
    }
    Ok(())
}

fn validate_cache_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= CACHE_KEY_CHARS
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        && !key.contains("..")
        && !key.starts_with('.')
}

fn write_hashed(file: &mut std::fs::File, hasher: &mut Sha256, bytes: &[u8]) -> Result<()> {
    file.write_all(bytes)?;
    hasher.update(bytes);
    Ok(())
}

fn read_hashed(file: &mut std::fs::File, hasher: &mut Sha256, bytes: &mut [u8]) -> Result<()> {
    file.read_exact(bytes)?;
    hasher.update(&*bytes);
    Ok(())
}

fn read_version(file: &mut std::fs::File, hasher: &mut Sha256) -> Result<u16> {
    let mut raw = [0u8; 2];
    read_hashed(file, hasher, &mut raw)?;
    Ok(u16::from_le_bytes(raw))
}

fn read_u32_hashed(file: &mut std::fs::File, hasher: &mut Sha256) -> Result<u32> {
    let mut raw = [0u8; 4];
    read_hashed(file, hasher, &mut raw)?;
    Ok(u32::from_le_bytes(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keys_reject_path_shapes() {
        assert!(validate_cache_key("download_1"));
        assert!(!validate_cache_key("../escape"));
        assert!(!validate_cache_key("a/b"));
        assert!(!validate_cache_key(""));
    }

    #[test]
    fn manifest_rejects_reordered_entries() {
        let manifest = BackupManifest {
            format: BACKUP_FORMAT.into(),
            version: BACKUP_VERSION,
            backup_id: "bk_x".into(),
            created_at: 1,
            device_id: "dev".into(),
            encryption: EncryptionMeta::plaintext(),
            entries: vec![BackupEntry {
                index: 3,
                object_id: "a".repeat(64),
                size: 1,
                mime: "text/plain".into(),
                name: "a".into(),
                category: "file".into(),
            }],
            chat_messages: Vec::new(),
        };
        assert!(validate_manifest(&manifest).is_err());
    }
}
