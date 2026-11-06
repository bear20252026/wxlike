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
//! Import is resumable **and** rollback-able, with a strict two-phase order:
//! 1. *land on disk first* — every payload blob is content-checked, fsynced and
//!    published, then its `media_object` row, retention ref, file row and
//!    per-entry import cursor flip together in one immediate transaction;
//! 2. *then move the cursor* — the chat snapshot (message seq / history truth)
//!    is handed to the injected snapshot port only after all blobs landed, and
//!    the backup row moves `importing -> chat_committed -> completed`.
//!
//! A failure before the chat cursor moved leaves state `failed`;
//! [`MediaService::rollback_backup_import`] then undoes the landing (restore
//! refs/files dropped, objects freed only when nothing else references them,
//! per-entry cursors reset) so the same package can be re-imported cleanly.
//! A completed restore is never rolled back: undoing history that the user
//! already accepted would be data loss, not error recovery.
//!
//! Import keeps each entry's historical object id; blobs already present are
//! deduplicated rather than rewritten, and a re-run resumes at the first
//! unimported entry instead of letting a restore overwrite local truth.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use rusqlite::{params, OptionalExtension as _, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::access::{add_ref_conn, object_by_id_conn, remove_ref_conn, RefSpec};
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
/// Upper bound on how many missing objects one export plan reports; the scan
/// itself is bounded by [`BACKUP_MAX_ENTRIES`].
pub const PLAN_MAX_MISSING_REPORTED: usize = 64;

/// Import/backup lifecycle states stored in `media_backup.state`. Kept as
/// strings because the column predates this enum; the constants are the
/// single naming authority so a rollback guard and a writer can never drift.
pub const STATE_READY: &str = "ready";
pub const STATE_IMPORTING: &str = "importing";
pub const STATE_CHAT_COMMITTED: &str = "chat_committed";
pub const STATE_COMPLETED: &str = "completed";
pub const STATE_FAILED: &str = "failed";
pub const STATE_ROLLING_BACK: &str = "rolling_back";
pub const STATE_ROLLED_BACK: &str = "rolled_back";

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

/// One row of an export plan: what the package would contain and whether the
/// bytes are actually there to be packed.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BackupPlanEntry {
    pub object_id: String,
    pub size: u64,
    pub mime: String,
    pub name: String,
    pub category: String,
    /// False when the ready `media_object` row has no (or a wrong-length) blob
    /// on the store — export would fail on this entry, so a plan with any
    /// missing object is not publishable.
    pub present: bool,
}

/// Dry-run answer to "what would a backup cost and can it succeed?". No files
/// are written and no `media_backup` row is created; the numbers come from the
/// same queries [`MediaService::export_backup`] uses, so plan and product
/// cannot disagree.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct BackupPlan {
    pub owner: String,
    pub device_id: String,
    pub created_at: i64,
    pub entries: Vec<BackupPlanEntry>,
    pub payload_bytes: u64,
    pub estimated_package_bytes: u64,
    pub chat_messages: u32,
    pub missing_objects: Vec<String>,
    pub within_budget: bool,
    pub publishable: bool,
    pub encryption: EncryptionMeta,
}

/// Result of undoing a failed restore. Only the media half is ours: the chat
/// cursor never moved (rollback is refused once it did), so G02/G05 history
/// needs no compensating write.
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct RollbackOutcome {
    pub backup_id: String,
    pub state: String,
    pub reset_entries: u32,
    /// Blobs freed because the restore was their last reference.
    pub removed_objects: u32,
    /// Content kept because something outside this restore still needs it
    /// (deduplicated against a pre-existing copy).
    pub kept_objects: u32,
    pub removed_files: u32,
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
    /// ownership again. Runs the exact query sequence prepared by
    /// [`Self::plan_backup_export`], so a plan that said `publishable` lands a
    /// package with the same entry set and payload size.
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
            state: STATE_READY.into(),
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

    /// Dry-run of [`Self::export_backup`]: entry set, payload/estimate sizes,
    /// chat snapshot count and an integrity pre-check (every ready row must
    /// still have its blob on the store). Writes nothing.
    pub fn plan_backup_export(
        &self,
        user_id: &str,
        device_id: &str,
        encryption: EncryptionMeta,
    ) -> Result<BackupPlan> {
        if encryption.key_ref.len() > ids::MAX_NAME_CHARS {
            return Err(MediaError::bad("key reference is too long"));
        }
        let entries = self.owner_entries(user_id)?;
        let (planned, missing_objects) = self.plan_entries(&entries);
        let payload_bytes: u64 = planned.iter().map(|entry| entry.size).sum();
        let chat_snapshot = self.chat.snapshot(user_id)?;
        let chat_messages = chat_snapshot.len() as u32;
        let created_at = now_ms();
        let manifest = BackupManifest {
            format: BACKUP_FORMAT.into(),
            version: BACKUP_VERSION,
            // The export stamps a fresh id here; the placeholder keeps the
            // serialization shape (and therefore the size estimate) identical.
            backup_id: String::new(),
            created_at,
            device_id: device_id.to_string(),
            encryption: encryption.clone(),
            chat_messages: chat_snapshot,
            entries,
        };
        let manifest_bytes = serde_json::to_vec(&manifest)?.len() as u64;
        let estimated_package_bytes =
            BACKUP_HEADER_LEN as u64 + manifest_bytes + payload_bytes + BACKUP_TRAILER_LEN as u64;
        let within_budget = payload_bytes <= BACKUP_MAX_BYTES;
        let publishable = within_budget && missing_objects.is_empty();
        Ok(BackupPlan {
            owner: user_id.to_string(),
            device_id: device_id.to_string(),
            created_at,
            entries: planned,
            payload_bytes,
            estimated_package_bytes,
            chat_messages,
            missing_objects,
            within_budget,
            publishable,
            encryption,
        })
    }

    /// Integrity pre-check for a plan: a row counts as present only when the
    /// store still holds a blob of exactly the recorded size.
    fn plan_entries(&self, entries: &[BackupEntry]) -> (Vec<BackupPlanEntry>, Vec<String>) {
        let mut missing_objects = Vec::new();
        let planned = entries
            .iter()
            .map(|entry| {
                let present = self
                    .store
                    .size(&entry.object_id)
                    .ok()
                    .flatten()
                    .is_some_and(|size| size == entry.size);
                if !present && missing_objects.len() < PLAN_MAX_MISSING_REPORTED {
                    missing_objects.push(entry.object_id.clone());
                }
                BackupPlanEntry {
                    object_id: entry.object_id.clone(),
                    size: entry.size,
                    mime: entry.mime.clone(),
                    name: entry.name.clone(),
                    category: entry.category.clone(),
                    present,
                }
            })
            .collect();
        (planned, missing_objects)
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

    /// Verify then import, in two phases: every payload blob lands durably
    /// first (content address checked, fsynced, published, and its metadata +
    /// per-entry cursor flipped in one transaction), and only afterwards the
    /// chat cursor moves through the snapshot port. Any failure before the
    /// chat commit marks the backup `failed` so
    /// [`Self::rollback_backup_import`] can undo it; re-running after an
    /// interruption resumes at the first unimported entry.
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
            state: STATE_IMPORTING.into(),
            ..Default::default()
        };
        let landed = self.run_import(
            user_id,
            device_id,
            &manifest,
            &parsed.payloads,
            &mut outcome,
        );
        cleanup_payloads(&parsed.payloads);
        if let Err(err) = landed {
            self.mark_import_failed(&manifest.backup_id);
            return Err(err);
        }
        outcome.state = STATE_COMPLETED.into();
        Ok(outcome)
    }

    fn run_import(
        &self,
        user_id: &str,
        device_id: &str,
        manifest: &BackupManifest,
        payloads: &[PathBuf],
        outcome: &mut ImportOutcome,
    ) -> Result<()> {
        for (entry, payload) in manifest.entries.iter().zip(payloads.iter()) {
            self.import_entry(user_id, &manifest.backup_id, entry, payload, outcome)?;
        }
        self.import_chat(user_id, device_id, manifest, outcome)?;
        self.finish_import(&manifest.backup_id, outcome)
    }

    fn register_import(
        &self,
        user_id: &str,
        manifest: &BackupManifest,
        digest: &str,
    ) -> Result<()> {
        let manifest_json = serde_json::to_string(manifest)?;
        let now = now_ms();
        let mut c = self.db.lock();
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing_owner: Option<String> = tx
            .query_row(
                "SELECT owner FROM media_backup WHERE backup_id = ?1",
                params![manifest.backup_id],
                |row| row.get(0),
            )
            .optional()?;
        if existing_owner.is_some_and(|owner| owner != user_id) {
            return Err(MediaError::Forbidden);
        }
        tx.execute(
            "INSERT INTO media_backup
                (backup_id, owner, state, manifest, package_path, package_digest,
                 created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, '', ?5, ?6, ?6)
             ON CONFLICT(backup_id) DO UPDATE SET state = ?3, updated_at = ?6",
            params![
                manifest.backup_id,
                user_id,
                STATE_IMPORTING,
                manifest_json,
                digest,
                now
            ],
        )?;
        for entry in &manifest.entries {
            tx.execute(
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
        tx.commit()?;
        Ok(())
    }

    /// Land one entry durably **before** moving its cursor: the blob is
    /// content-checked and published (fsynced inside the store), then object
    /// row, retention ref, file row and `imported = 1` commit as one
    /// transaction guarded by the backup still being in `importing`.
    fn import_entry(
        &self,
        user_id: &str,
        backup_id: &str,
        entry: &BackupEntry,
        payload: &Path,
        outcome: &mut ImportOutcome,
    ) -> Result<()> {
        let (imported, state) = self.entry_cursor(backup_id, entry.index)?;
        if state != STATE_IMPORTING {
            return Err(MediaError::conflict(format!(
                "import {backup_id} is no longer in state {state}"
            )));
        }
        if imported != 0 {
            outcome.skipped += 1;
            std::fs::remove_file(payload).ok();
            return Ok(());
        }
        // "already present" means *usable*: a ready row **and** its bytes on
        // the store. A soft-deleted row (e.g. left by an earlier rollback of
        // this backup) is re-landed, not counted as dedup.
        let existed = self.entry_content_present(&entry.object_id, entry.size)?;
        if !existed {
            self.land_entry_blob(entry, payload)?;
        }
        std::fs::remove_file(payload).ok();
        self.commit_entry_cursor(user_id, backup_id, entry)?;
        if existed {
            outcome.deduped += 1;
        } else {
            outcome.imported += 1;
        }
        Ok(())
    }

    fn entry_cursor(&self, backup_id: &str, index: u32) -> Result<(i64, String)> {
        let c = self.db.lock();
        Ok(c.query_row(
            "SELECT e.imported, b.state
             FROM media_backup_entry e
             JOIN media_backup b ON b.backup_id = e.backup_id
             WHERE e.backup_id = ?1 AND e.entry_idx = ?2",
            params![backup_id, i64::from(index)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?)
    }

    fn entry_content_present(&self, object_id: &str, size: u64) -> Result<bool> {
        let row = object_by_id_conn(&self.db.lock(), object_id)?;
        Ok(row.is_some_and(|object| object.ready())
            && self
                .store
                .size(object_id)?
                .is_some_and(|stored| stored == size))
    }

    fn land_entry_blob(&self, entry: &BackupEntry, payload: &Path) -> Result<()> {
        let digest = crate::store::stream_digest_file(payload)?;
        if digest != entry.object_id {
            std::fs::remove_file(payload).ok();
            return Err(MediaError::Tampered("entry digest mismatch".into()));
        }
        self.store.put_file(&entry.object_id, payload)?;
        Ok(())
    }

    fn commit_entry_cursor(
        &self,
        user_id: &str,
        backup_id: &str,
        entry: &BackupEntry,
    ) -> Result<()> {
        let mut c = self.db.lock();
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let guarded: String = tx.query_row(
            "SELECT state FROM media_backup WHERE backup_id = ?1",
            params![backup_id],
            |row| row.get(0),
        )?;
        if guarded != STATE_IMPORTING {
            return Err(MediaError::conflict(format!(
                "import {backup_id} changed state under it"
            )));
        }
        let now = now_ms();
        restored_object_conn(&tx, user_id, backup_id, entry, now)?;
        tx.execute(
            "UPDATE media_backup_entry SET imported = 1
             WHERE backup_id = ?1 AND entry_idx = ?2",
            params![backup_id, i64::from(entry.index)],
        )?;
        tx.commit()?;
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
        // The chat cursor only flips after every blob has landed; recording
        // that fact is what makes a later failure non-rollback-able by design.
        let c = self.db.lock();
        c.execute(
            "UPDATE media_backup SET state = ?1, updated_at = ?2
             WHERE backup_id = ?3 AND state = ?4",
            params![
                STATE_CHAT_COMMITTED,
                now_ms(),
                manifest.backup_id,
                STATE_IMPORTING
            ],
        )?;
        Ok(())
    }

    fn finish_import(&self, backup_id: &str, outcome: &mut ImportOutcome) -> Result<()> {
        {
            let c = self.db.lock();
            let changed = c.execute(
                "UPDATE media_backup SET state = ?1, updated_at = ?2
                 WHERE backup_id = ?3 AND state = ?4",
                params![STATE_COMPLETED, now_ms(), backup_id, STATE_CHAT_COMMITTED],
            )?;
            if changed == 0 {
                return Err(MediaError::conflict(format!(
                    "import {backup_id} lost its state race"
                )));
            }
        }
        outcome.state = STATE_COMPLETED.into();
        Ok(())
    }

    /// Best-effort bookkeeping after a failed restore: demote `importing` to
    /// `failed` so a rollback is allowed. A `chat_committed` row is **not**
    /// demoted — the cursor already moved, recovery there is a resume, not a
    /// rollback.
    fn mark_import_failed(&self, backup_id: &str) {
        let c = self.db.lock();
        let _ = c.execute(
            "UPDATE media_backup SET state = ?1, updated_at = ?2
             WHERE backup_id = ?3 AND state = ?4",
            params![STATE_FAILED, now_ms(), backup_id, STATE_IMPORTING],
        );
    }

    /// Undo a restore that failed before the chat cursor moved. Drops the
    /// synthetic file rows and refs this import created, frees objects only
    /// when nothing else references them (deduplicated content survives), and
    /// resets the per-entry cursors so a re-import starts from scratch.
    pub fn rollback_backup_import(
        &self,
        user_id: &str,
        backup_id: &str,
    ) -> Result<RollbackOutcome> {
        if backup_id.is_empty() || backup_id.len() > ids::MAX_NAME_CHARS {
            return Err(MediaError::bad("backup id is out of range"));
        }
        let (affected, removed_files, reset_entries) =
            self.rollback_restore_landing(user_id, backup_id)?;
        let mut outcome = RollbackOutcome {
            backup_id: backup_id.to_string(),
            state: STATE_ROLLED_BACK.into(),
            removed_files,
            reset_entries,
            ..Default::default()
        };
        for object_id in affected {
            if self.gc_object(&object_id)? {
                outcome.removed_objects += 1;
            } else {
                outcome.kept_objects += 1;
            }
        }
        Ok(outcome)
    }

    /// Transactional half of [`Self::rollback_backup_import`]: claim the row
    /// (only `importing`/`failed` are rollback-able — a completed restore is
    /// accepted history, and `chat_committed` means the cursor already moved),
    /// undo every landed entry's metadata and flip the row to `rolled_back`.
    /// Physical freeing happens outside, via ref-counted GC.
    fn rollback_restore_landing(
        &self,
        user_id: &str,
        backup_id: &str,
    ) -> Result<(Vec<String>, u32, u32)> {
        let mut c = self.db.lock();
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: Option<(String, String)> = tx
            .query_row(
                "SELECT owner, state FROM media_backup WHERE backup_id = ?1",
                params![backup_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (owner, state) = row.ok_or(MediaError::NotFound)?;
        if owner != user_id {
            return Err(MediaError::NotFound);
        }
        let claimed = tx.execute(
            "UPDATE media_backup SET state = ?1, updated_at = ?2
             WHERE backup_id = ?3 AND state IN (?4, ?5)",
            params![
                STATE_ROLLING_BACK,
                now_ms(),
                backup_id,
                STATE_IMPORTING,
                STATE_FAILED
            ],
        )?;
        if claimed == 0 {
            return Err(MediaError::conflict(format!(
                "backup {state} is not rollback-able (resume it instead)"
            )));
        }
        let (landed, removed_files) = undo_landed_entries(&tx, user_id, backup_id)?;
        tx.execute(
            "UPDATE media_backup SET state = ?1, updated_at = ?2
             WHERE backup_id = ?3 AND state = ?4",
            params![STATE_ROLLED_BACK, now_ms(), backup_id, STATE_ROLLING_BACK],
        )?;
        tx.commit()?;
        let reset_entries = landed.len() as u32;
        Ok((landed, removed_files, reset_entries))
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

/// Publish the metadata half of one restored entry. Runs inside the caller's
/// transaction so the object row, its retention ref, the synthetic file row
/// and the per-entry cursor commit (or roll back) together. Historical object
/// ids are kept verbatim; a live `ready` row is left completely untouched
/// (that is the dedup path), while a row previously *deleted by a rollback of
/// this same backup* is revived — otherwise re-importing after a rollback
/// would publish a `ready` cursor over soft-deleted garbage.
fn restored_object_conn(
    c: &rusqlite::Connection,
    user_id: &str,
    backup_id: &str,
    entry: &BackupEntry,
    now: i64,
) -> Result<()> {
    let file_id = format!("restore_{backup_id}_{}", entry.index);
    let name = if entry.name.is_empty() {
        format!("restored-{}", entry.index)
    } else {
        entry.name.clone()
    };
    c.execute(
        "INSERT INTO media_object
            (object_id, owner, size, digest, mime, status, access_epoch, created_at)
         VALUES(?1, ?2, ?3, ?1, ?4, 'ready', 0, ?5)
         ON CONFLICT(object_id) DO UPDATE
             SET owner = excluded.owner, size = excluded.size, mime = excluded.mime,
                 status = 'ready', deleted_at = NULL
             WHERE media_object.status = 'deleted'",
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
    add_ref_conn(c, &entry.object_id, &spec, now)?;
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

/// Undo the per-entry metadata half of a rollback: for every landed entry,
/// drop the synthetic restore ref + file row and reset the `imported` cursor.
/// Runs inside the caller's transaction. Returns the object ids touched (for
/// the caller's ref-counted GC) and how many file rows were removed.
fn undo_landed_entries(
    tx: &rusqlite::Connection,
    user_id: &str,
    backup_id: &str,
) -> Result<(Vec<String>, u32)> {
    let landed: Vec<(i64, String)> = {
        let mut stmt = tx.prepare(
            "SELECT entry_idx, object_id FROM media_backup_entry
             WHERE backup_id = ?1 AND imported = 1 ORDER BY entry_idx",
        )?;
        let rows = stmt.query_map(params![backup_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let mut removed_files = 0u32;
    let now = now_ms();
    for (index, object_id) in &landed {
        let file_id = format!("restore_{backup_id}_{index}");
        remove_ref_conn(tx, object_id, RefType::File, &file_id, now)?;
        removed_files += tx.execute(
            "DELETE FROM media_file WHERE file_id = ?1 AND owner = ?2",
            params![file_id, user_id],
        )? as u32;
        tx.execute(
            "UPDATE media_backup_entry SET imported = 0
             WHERE backup_id = ?1 AND entry_idx = ?2",
            params![backup_id, index],
        )?;
    }
    Ok((
        landed.into_iter().map(|(_, id)| id).collect(),
        removed_files,
    ))
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
