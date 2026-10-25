//! Resumable, idempotent upload sessions.
//!
//! A session owns one staging file. Chunks must land at the current offset;
//! replaying an already accepted chunk is a no-op when its digest matches.
//! Completing streams the staging file through SHA-256 (fixed buffer, so a
//! 1 GiB upload never becomes a 1 GiB `Vec`) and only then publishes the
//! content-addressed object.

use std::io::{Seek as _, SeekFrom, Write as _};

use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use crate::access::{add_ref_conn, object_by_id_conn, RefSpec};
use crate::db::now_ms;
use crate::error::{MediaError, Result};
use crate::ids;
use crate::model::{category_for, Audience, Purpose, RefType, UploadStatus};
use crate::store::stream_digest_file;
use crate::MediaService;

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct MediaCapabilities {
    pub max_object_bytes: u64,
    pub max_chunk_bytes: u64,
    pub max_user_bytes: u64,
    pub session_ttl_ms: i64,
    pub signed_url_ttl_ms: i64,
    pub max_share_ttl_ms: i64,
    pub max_page_limit: u32,
}

#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct UploadStart {
    pub purpose: Purpose,
    pub name: String,
    pub size: u64,
    pub mime: String,
    #[serde(default)]
    pub digest: String,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct UploadSession {
    pub upload_id: String,
    pub purpose: Purpose,
    pub name: String,
    pub mime: String,
    pub declared_size: u64,
    pub offset: u64,
    pub chunk_size: u64,
    pub status: UploadStatus,
    pub expires_at: i64,
    pub object_id: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct UploadProgress {
    pub upload_id: String,
    pub offset: u64,
    pub declared_size: u64,
    pub complete: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct CompletedUpload {
    pub object_id: String,
    pub file_id: String,
    pub size: u64,
    pub digest: String,
    pub mime: String,
    pub newly_stored: bool,
}

#[derive(Clone, Debug)]
struct UploadRow {
    upload_id: String,
    owner: String,
    purpose: Purpose,
    name: String,
    mime: String,
    declared_size: u64,
    declared_digest: String,
    offset: u64,
    chunk_size: u64,
    status: UploadStatus,
    object_id: Option<String>,
    expires_at: i64,
}

fn purpose_cap(purpose: Purpose, configured: u64) -> u64 {
    let specific = match purpose {
        Purpose::Voice => crate::model::VOICE_MAX_BYTES,
        Purpose::Sticker => crate::model::STICKER_MAX_BYTES,
        _ => configured,
    };
    specific.min(configured)
}

fn row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<UploadRow> {
    Ok(UploadRow {
        upload_id: row.get(0)?,
        owner: row.get(1)?,
        purpose: Purpose::parse(&row.get::<_, String>(2)?).map_err(to_sql)?,
        name: row.get(3)?,
        mime: row.get(4)?,
        declared_size: row.get::<_, i64>(5)?.max(0) as u64,
        declared_digest: row.get(6)?,
        offset: row.get::<_, i64>(7)?.max(0) as u64,
        chunk_size: row.get::<_, i64>(8)?.max(0) as u64,
        status: UploadStatus::parse(&row.get::<_, String>(9)?).map_err(to_sql)?,
        object_id: row.get(10)?,
        expires_at: row.get(11)?,
    })
}

fn to_sql(err: MediaError) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(err))
}

fn session_of(row: UploadRow) -> UploadSession {
    UploadSession {
        upload_id: row.upload_id,
        purpose: row.purpose,
        name: row.name,
        mime: row.mime,
        declared_size: row.declared_size,
        offset: row.offset,
        chunk_size: row.chunk_size,
        status: row.status,
        expires_at: row.expires_at,
        object_id: row.object_id,
    }
}

impl MediaService {
    pub fn upload_start(&self, owner_id: &str, req: UploadStart) -> Result<UploadSession> {
        let name = ids::validate_name(&req.name)?;
        let mime = ids::validate_mime(&req.mime)?;
        ids::validate_declared_digest(&req.digest)?;
        let cap = purpose_cap(req.purpose, self.config.max_object_bytes);
        if req.size == 0 || req.size > cap {
            return Err(MediaError::PayloadTooLarge);
        }
        if req.size > self.config.max_user_bytes {
            return Err(MediaError::QuotaExceeded);
        }
        self.check_upload_quota(owner_id, req.size)?;
        let now = now_ms();
        let upload_id = ids::opaque_id("up");
        let chunk_size = self.config.max_chunk_bytes.max(1);
        let expires_at = now + self.config.session_ttl_ms;
        {
            let c = self.db.lock();
            c.execute(
                "INSERT INTO media_upload
                 (upload_id, owner, purpose, name, mime, declared_size, declared_digest,
                  offset, chunk_size, status, object_id, expires_at, created_at, updated_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, 'open', NULL, ?9, ?10, ?10)",
                params![
                    upload_id,
                    owner_id,
                    req.purpose.as_str(),
                    name,
                    mime,
                    req.size as i64,
                    req.digest.to_ascii_lowercase(),
                    chunk_size as i64,
                    expires_at,
                    now
                ],
            )?;
        }
        Ok(UploadSession {
            upload_id,
            purpose: req.purpose,
            name,
            mime,
            declared_size: req.size,
            offset: 0,
            chunk_size,
            status: UploadStatus::Open,
            expires_at,
            object_id: None,
        })
    }

    fn check_upload_quota(&self, owner_id: &str, incoming: u64) -> Result<()> {
        let used: i64 = {
            let c = self.db.lock();
            c.query_row(
                "SELECT
                   (SELECT COALESCE(SUM(size), 0) FROM media_object
                    WHERE owner = ?1 AND status = 'ready')
                 + (SELECT COALESCE(SUM(declared_size), 0) FROM media_upload
                    WHERE owner = ?1 AND status = 'open')",
                params![owner_id],
                |row| row.get(0),
            )?
        };
        let total = (used.max(0) as u64).saturating_add(incoming);
        if total > self.config.max_user_bytes {
            return Err(MediaError::QuotaExceeded);
        }
        Ok(())
    }

    fn upload_row(&self, owner_id: &str, upload_id: &str) -> Result<UploadRow> {
        let row = {
            let c = self.db.lock();
            c.query_row(
                "SELECT upload_id, owner, purpose, name, mime, declared_size, declared_digest,
                        offset, chunk_size, status, object_id, expires_at
                 FROM media_upload WHERE upload_id = ?1",
                params![upload_id],
                row_from_sql,
            )
            .optional()?
        };
        match row {
            Some(row) if row.owner == owner_id => Ok(row),
            _ => Err(MediaError::NotFound),
        }
    }

    pub fn upload_status(&self, owner_id: &str, upload_id: &str) -> Result<UploadSession> {
        let mut row = self.upload_row(owner_id, upload_id)?;
        self.expire_if_needed(&mut row, now_ms())?;
        Ok(session_of(row))
    }

    fn expire_if_needed(&self, row: &mut UploadRow, now: i64) -> Result<()> {
        if row.status == UploadStatus::Open && row.expires_at < now {
            self.mark_closed(&row.upload_id, UploadStatus::Expired, now)?;
            row.status = UploadStatus::Expired;
        }
        Ok(())
    }

    fn mark_closed(&self, upload_id: &str, status: UploadStatus, now: i64) -> Result<()> {
        {
            let c = self.db.lock();
            c.execute(
                "UPDATE media_upload SET status = ?1, updated_at = ?2 WHERE upload_id = ?3",
                params![status.as_str(), now, upload_id],
            )?;
        }
        std::fs::remove_file(self.store.upload_path(upload_id)).ok();
        Ok(())
    }

    pub fn upload_chunk(
        &self,
        owner_id: &str,
        upload_id: &str,
        offset: u64,
        bytes: &[u8],
        declared_digest: Option<&str>,
    ) -> Result<UploadProgress> {
        let mut row = self.upload_row(owner_id, upload_id)?;
        let now = now_ms();
        self.expire_if_needed(&mut row, now)?;
        if row.status != UploadStatus::Open {
            return Err(MediaError::Gone("upload is no longer open".into()));
        }
        let digest = ids::sha256_hex(bytes);
        if let Some(declared) = declared_digest.filter(|value| !value.is_empty()) {
            if !declared.eq_ignore_ascii_case(&digest) {
                return Err(MediaError::Tampered("chunk digest mismatch".into()));
            }
        }
        if let Some(progress) = self.replay_chunk(&row, offset, bytes.len() as u64, &digest)? {
            return Ok(progress);
        }
        self.validate_chunk_span(&row, offset, bytes.len() as u64)?;
        self.write_chunk(&row.upload_id, offset, bytes)?;
        self.record_chunk(&row, offset, bytes.len() as u64, &digest, now)?;
        Ok(UploadProgress {
            upload_id: row.upload_id,
            offset: offset + bytes.len() as u64,
            declared_size: row.declared_size,
            complete: false,
        })
    }

    fn replay_chunk(
        &self,
        row: &UploadRow,
        offset: u64,
        len: u64,
        digest: &str,
    ) -> Result<Option<UploadProgress>> {
        if offset >= row.offset {
            return Ok(None);
        }
        let existing: Option<(i64, String)> = {
            let c = self.db.lock();
            c.query_row(
                "SELECT len, digest FROM media_upload_chunk
                 WHERE upload_id = ?1 AND chunk_off = ?2",
                params![row.upload_id, offset as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
        };
        match existing {
            Some((stored_len, stored_digest))
                if stored_len.max(0) as u64 == len && stored_digest == digest =>
            {
                Ok(Some(UploadProgress {
                    upload_id: row.upload_id.clone(),
                    offset: row.offset,
                    declared_size: row.declared_size,
                    complete: false,
                }))
            }
            _ => Err(MediaError::conflict(format!(
                "offset {offset} was already accepted with different bytes"
            ))),
        }
    }

    fn validate_chunk_span(&self, row: &UploadRow, offset: u64, len: u64) -> Result<()> {
        if len == 0 {
            return Err(MediaError::bad("chunk must not be empty"));
        }
        if len > row.chunk_size {
            return Err(MediaError::PayloadTooLarge);
        }
        if offset != row.offset {
            return Err(MediaError::conflict(format!(
                "expected offset {}, got {offset}",
                row.offset
            )));
        }
        if offset.saturating_add(len) > row.declared_size {
            return Err(MediaError::bad("chunk exceeds declared size"));
        }
        Ok(())
    }

    fn write_chunk(&self, upload_id: &str, offset: u64, bytes: &[u8]) -> Result<()> {
        let path = self.store.upload_path(upload_id);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(bytes)?;
        file.flush()?;
        Ok(())
    }

    fn record_chunk(
        &self,
        row: &UploadRow,
        offset: u64,
        len: u64,
        digest: &str,
        now: i64,
    ) -> Result<()> {
        let c = self.db.lock();
        let inserted = c.execute(
            "INSERT OR IGNORE INTO media_upload_chunk
             (upload_id, chunk_off, len, digest, received_at) VALUES(?1, ?2, ?3, ?4, ?5)",
            params![row.upload_id, offset as i64, len as i64, digest, now],
        )?;
        if inserted == 0 {
            return Err(MediaError::conflict("chunk already recorded"));
        }
        let moved = c.execute(
            "UPDATE media_upload SET offset = ?1, updated_at = ?2
             WHERE upload_id = ?3 AND offset = ?4 AND status = 'open'",
            params![(offset + len) as i64, now, row.upload_id, row.offset as i64],
        )?;
        if moved == 0 {
            return Err(MediaError::conflict("upload offset moved concurrently"));
        }
        Ok(())
    }

    pub fn upload_cancel(&self, owner_id: &str, upload_id: &str) -> Result<UploadSession> {
        let mut row = self.upload_row(owner_id, upload_id)?;
        let now = now_ms();
        if row.status == UploadStatus::Open {
            self.mark_closed(upload_id, UploadStatus::Canceled, now)?;
            row.status = UploadStatus::Canceled;
        }
        Ok(session_of(row))
    }

    pub fn upload_complete(
        &self,
        owner_id: &str,
        upload_id: &str,
        declared_digest: Option<&str>,
    ) -> Result<CompletedUpload> {
        let mut row = self.upload_row(owner_id, upload_id)?;
        self.expire_if_needed(&mut row, now_ms())?;
        if row.status == UploadStatus::Completed {
            return self.completed_upload(&row);
        }
        if row.status != UploadStatus::Open {
            return Err(MediaError::Gone("upload is no longer open".into()));
        }
        if row.offset != row.declared_size {
            return Err(MediaError::conflict(format!(
                "upload incomplete: {} of {} bytes",
                row.offset, row.declared_size
            )));
        }
        let digest = self.effective_digest(&row, declared_digest)?;
        let temp = self.store.upload_path(upload_id);
        if !temp.exists() {
            return Err(MediaError::Gone("staged bytes are missing".into()));
        }
        let computed = stream_digest_file(&temp)?;
        if !computed.eq_ignore_ascii_case(&digest) {
            return Err(MediaError::Tampered("completed digest mismatch".into()));
        }
        let size = std::fs::metadata(&temp)?.len();
        if size != row.declared_size {
            return Err(MediaError::Tampered("staged size mismatch".into()));
        }
        let existed = self.store.size(&computed)?.is_some() || self.object_exists(&computed)?;
        self.store.put_file(&computed, &temp)?;
        let file_id = self.finish_upload(&row, &computed, size)?;
        std::fs::remove_file(&temp).ok();
        {
            let c = self.db.lock();
            c.execute(
                "DELETE FROM media_upload_chunk WHERE upload_id = ?1",
                params![upload_id],
            )?;
        }
        Ok(CompletedUpload {
            object_id: computed.clone(),
            file_id,
            size,
            digest: computed,
            mime: row.mime,
            newly_stored: !existed,
        })
    }

    fn effective_digest(&self, row: &UploadRow, supplied: Option<&str>) -> Result<String> {
        let value = supplied
            .filter(|value| !value.is_empty())
            .unwrap_or(&row.declared_digest)
            .to_ascii_lowercase();
        if !ids::is_sha256_hex(&value) {
            return Err(MediaError::bad("a sha256 digest is required to complete"));
        }
        Ok(value)
    }

    pub(crate) fn object_exists(&self, object_id: &str) -> Result<bool> {
        Ok(object_by_id_conn(&self.db.lock(), object_id)?.is_some())
    }

    fn finish_upload(&self, row: &UploadRow, object_id: &str, size: u64) -> Result<String> {
        let now = now_ms();
        let file_id = format!("file_{}", row.upload_id);
        let category = category_for(row.purpose, &row.mime);
        let c = self.db.lock();
        c.execute(
            "INSERT INTO media_object
                (object_id, owner, size, digest, mime, status, access_epoch, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, 'ready', 0, ?6)
             ON CONFLICT(object_id) DO NOTHING",
            params![object_id, row.owner, size as i64, object_id, row.mime, now],
        )?;
        let audience = Audience::User {
            id: row.owner.clone(),
        };
        let spec = RefSpec {
            ref_type: RefType::File,
            ext_id: &file_id,
            owner: &row.owner,
            audience: &audience,
            purpose: row.purpose,
        };
        add_ref_conn(&c, object_id, &spec, now)?;
        c.execute(
            "INSERT OR IGNORE INTO media_file
                (file_id, object_id, owner, name, folder, category, size, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, '', ?5, ?6, ?7, ?7)",
            params![
                file_id,
                object_id,
                row.owner,
                row.name,
                category,
                size as i64,
                now
            ],
        )?;
        c.execute(
            "UPDATE media_upload SET status = 'completed', object_id = ?1, updated_at = ?2
             WHERE upload_id = ?3",
            params![object_id, now, row.upload_id],
        )?;
        Ok(file_id)
    }

    fn completed_upload(&self, row: &UploadRow) -> Result<CompletedUpload> {
        let object_id = row
            .object_id
            .clone()
            .ok_or_else(|| MediaError::internal("completed upload lacks object"))?;
        let object = self.object(&object_id)?;
        Ok(CompletedUpload {
            object_id,
            file_id: format!("file_{}", row.upload_id),
            size: object.size,
            digest: object.digest,
            mime: object.mime,
            newly_stored: false,
        })
    }

    /// Expire open sessions and delete their staging files. Returns how many
    /// sessions were closed; call from a periodic sweeper.
    pub fn cleanup_expired_uploads(&self, now: i64) -> Result<usize> {
        let ids: Vec<String> = {
            let c = self.db.lock();
            let mut stmt = c.prepare(
                "SELECT upload_id FROM media_upload
                 WHERE status = 'open' AND expires_at < ?1",
            )?;
            let rows = stmt.query_map(params![now], |r| r.get(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        for id in &ids {
            self.mark_closed(id, UploadStatus::Expired, now)?;
        }
        Ok(ids.len())
    }
}
