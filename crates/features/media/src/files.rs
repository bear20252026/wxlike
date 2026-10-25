//! File-center projection, attachment manifests, revocable shares and GC.
//!
//! Listing is an authorization projection, not a search over names: a row is
//! returned only when the caller owns it or an active reference audience
//! includes them. Deleting a file from the center never destroys an object that
//! a message still references; GC is a separate reference-count step.

use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, OptionalExtension as _, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::access::{add_ref_conn, remove_ref_conn, RefSpec};
use crate::db::now_ms;
use crate::error::{MediaError, Result};
use crate::ids;
use crate::model::{
    Audience, FileItem, Purpose, RefType, DEFAULT_PAGE_LIMIT, IMAGE_MAX_BYTES, MAX_PAGE_LIMIT,
    STICKER_MAX_BYTES, VOICE_MAX_BYTES, VOICE_MAX_MS,
};
use crate::store::stream_digest_file;
use crate::MediaService;

pub const SIGNED_SHARE_PREFIX: &str = "/api/v1/objects";
pub const SHARE_MIN_TTL_MS: i64 = 60 * 1000;
pub const SHARE_TOKEN_BYTES: usize = 32;
const WAVEFORM_BARS: usize = 40;
const WAVEFORM_MAX: u8 = 100;
const FILE_MAX_BYTES: u64 = crate::GIB;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileSort {
    #[default]
    Recent,
    Name,
    Size,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct FileListQuery {
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub sort: Option<FileSort>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct FilePage {
    pub items: Vec<FileItem>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub snapshot_version: i64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Voice,
    Image,
    Sticker,
    File,
    Video,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AttachmentManifest {
    pub kind: AttachmentKind,
    pub media_id: String,
    pub name: String,
    pub size: u64,
    pub digest: String,
    pub mime: String,
    #[serde(default)]
    pub duration_ms: Option<u32>,
    #[serde(default)]
    pub waveform: Option<Vec<u8>>,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct ShareGrant {
    pub share_id: String,
    pub token: String,
    pub url: String,
    pub expires_at: i64,
}

fn file_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileItem> {
    Ok(FileItem {
        file_id: row.get(0)?,
        object_id: row.get(1)?,
        owner: row.get(2)?,
        name: row.get(3)?,
        folder: row.get(4)?,
        category: row.get(5)?,
        size: row.get::<_, i64>(6)?.max(0) as u64,
        digest: row.get(7)?,
        mime: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

const FILE_COLUMNS: &str = "f.file_id, f.object_id, f.owner, f.name, f.folder, f.category,
    f.size, o.digest, o.mime, f.created_at, f.updated_at";

#[derive(Clone, Debug)]
enum CursorKey {
    Int(i64),
    Text(String),
}

struct Cursor {
    key: CursorKey,
    file_id: String,
}

fn encode_cursor(key: &CursorKey, file_id: &str) -> String {
    match key {
        CursorKey::Int(value) => format!("{value}:{file_id}"),
        CursorKey::Text(value) => format!("{value}:{file_id}"),
    }
}

fn decode_cursor(sort: FileSort, raw: &str) -> Result<Cursor> {
    let (key, file_id) = raw
        .rsplit_once(':')
        .ok_or_else(|| MediaError::bad("malformed cursor"))?;
    if file_id.is_empty() {
        return Err(MediaError::bad("malformed cursor"));
    }
    let key = match sort {
        FileSort::Name => CursorKey::Text(key.to_string()),
        _ => CursorKey::Int(
            key.parse()
                .map_err(|_| MediaError::bad("malformed cursor"))?,
        ),
    };
    Ok(Cursor {
        key,
        file_id: file_id.to_string(),
    })
}

impl AttachmentManifest {
    pub fn validate(&self) -> Result<()> {
        ids::validate_object_id(&self.media_id)?;
        ids::validate_name(&self.name)?;
        ids::validate_declared_digest(&self.digest)?;
        if !self.digest.is_empty() && !self.digest.eq_ignore_ascii_case(&self.media_id) {
            return Err(MediaError::bad("digest must equal the content address"));
        }
        if self.size == 0 || self.size > self.max_bytes() {
            return Err(MediaError::PayloadTooLarge);
        }
        self.validate_mime_and_voice()
    }

    fn validate_mime_and_voice(&self) -> Result<()> {
        let prefix = match self.kind {
            AttachmentKind::Image => "image/",
            AttachmentKind::Video => "video/",
            AttachmentKind::Voice => "audio/",
            AttachmentKind::Sticker => "image/",
            AttachmentKind::File => "",
        };
        if !prefix.is_empty() && !self.mime.starts_with(prefix) {
            return Err(MediaError::Unsupported(format!(
                "{} must be {prefix}*",
                self.kind_name()
            )));
        }
        self.validate_voice()
    }

    fn validate_voice(&self) -> Result<()> {
        if self.kind != AttachmentKind::Voice {
            return Ok(());
        }
        let duration = self.duration_ms.unwrap_or_default();
        if duration == 0 || duration > VOICE_MAX_MS {
            return Err(MediaError::bad("voice duration is out of range"));
        }
        let waveform = self.waveform.as_deref().unwrap_or_default();
        if waveform.len() != WAVEFORM_BARS || waveform.iter().any(|bar| *bar > WAVEFORM_MAX) {
            return Err(MediaError::bad("voice waveform must be 40 bars in 0..=100"));
        }
        Ok(())
    }

    fn max_bytes(&self) -> u64 {
        match self.kind {
            AttachmentKind::Voice => VOICE_MAX_BYTES,
            AttachmentKind::Sticker => STICKER_MAX_BYTES,
            AttachmentKind::Image => IMAGE_MAX_BYTES,
            AttachmentKind::Video => FILE_MAX_BYTES,
            AttachmentKind::File => FILE_MAX_BYTES,
        }
    }

    fn kind_name(&self) -> &'static str {
        match self.kind {
            AttachmentKind::Voice => "voice",
            AttachmentKind::Image => "image",
            AttachmentKind::Sticker => "sticker",
            AttachmentKind::File => "file",
            AttachmentKind::Video => "video",
        }
    }

    /// Download integrity: the bytes must hash to the manifest's media id.
    pub fn verify_download(&self, bytes: &[u8]) -> Result<()> {
        let digest = ids::sha256_hex(bytes);
        if digest == self.media_id {
            Ok(())
        } else {
            Err(MediaError::Tampered("downloaded bytes do not match".into()))
        }
    }
}

impl MediaService {
    pub fn list_files(&self, user_id: &str, query: FileListQuery) -> Result<FilePage> {
        let sort = query.sort.unwrap_or_default();
        let limit = query
            .limit
            .unwrap_or(DEFAULT_PAGE_LIMIT)
            .clamp(1, MAX_PAGE_LIMIT);
        let cursor = query
            .cursor
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(|raw| decode_cursor(sort, raw))
            .transpose()?;
        let audiences = self.visible_audiences(user_id);
        let mut args = list_base_args(user_id, &audiences);
        let mut sql = list_sql(sort, &mut args, &query, cursor.as_ref());
        args.push(Value::Integer(i64::from(limit) + 1));
        sql.push_str(" LIMIT ?");
        let items = self.query_files(&sql, args)?;
        Ok(page_from_items(items, limit, sort))
    }

    fn visible_audiences(&self, user_id: &str) -> Vec<String> {
        let mut out = vec![
            Audience::User {
                id: user_id.to_string(),
            }
            .key(),
            Audience::Public.key(),
        ];
        out.extend(
            self.audience
                .visible_conversations(user_id)
                .into_iter()
                .map(|id| Audience::Conversation { id }.key()),
        );
        out.extend(
            self.audience
                .visible_groups(user_id)
                .into_iter()
                .map(|id| Audience::Group { id }.key()),
        );
        out
    }

    fn query_files(&self, sql: &str, args: Vec<Value>) -> Result<Vec<FileItem>> {
        let c = self.db.lock();
        let mut stmt = c.prepare(sql)?;
        let rows = stmt.query_map(params_from_iter(args.iter()), file_from_row)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    fn file_row(&self, user_id: &str, file_id: &str) -> Result<FileItem> {
        let c = self.db.lock();
        let sql = format!(
            "SELECT {FILE_COLUMNS} FROM media_file f
             JOIN media_object o ON o.object_id = f.object_id
             WHERE f.file_id = ?1 AND f.owner = ?2 AND f.trashed_at IS NULL"
        );
        c.query_row(&sql, params![file_id, user_id], file_from_row)
            .optional()?
            .ok_or(MediaError::NotFound)
    }

    pub fn rename_file(&self, user_id: &str, file_id: &str, name: &str) -> Result<FileItem> {
        let name = ids::validate_name(name)?;
        self.update_file_text(user_id, file_id, "name", &name)
    }

    pub fn move_file(&self, user_id: &str, file_id: &str, folder: &str) -> Result<FileItem> {
        let folder = ids::validate_folder(folder)?;
        self.update_file_text(user_id, file_id, "folder", &folder)
    }

    fn update_file_text(
        &self,
        user_id: &str,
        file_id: &str,
        column: &str,
        value: &str,
    ) -> Result<FileItem> {
        let sql = format!(
            "UPDATE media_file SET {column} = ?1, updated_at = ?2
             WHERE file_id = ?3 AND owner = ?4 AND trashed_at IS NULL"
        );
        let changed = {
            let c = self.db.lock();
            c.execute(&sql, params![value, now_ms(), file_id, user_id])?
        };
        if changed == 0 {
            return Err(MediaError::NotFound);
        }
        self.file_row(user_id, file_id)
    }

    /// Move a file to the recycle bin and drop its file reference. Message
    /// references stay, so chat history keeps its attachment.
    pub fn trash_file(&self, user_id: &str, file_id: &str) -> Result<()> {
        let file = self.file_row(user_id, file_id)?;
        let now = now_ms();
        {
            let c = self.db.lock();
            let changed = c.execute(
                "UPDATE media_file SET trashed_at = ?1, updated_at = ?1
                 WHERE file_id = ?2 AND owner = ?3 AND trashed_at IS NULL",
                params![now, file_id, user_id],
            )?;
            if changed == 0 {
                return Err(MediaError::NotFound);
            }
            remove_ref_conn(&c, &file.object_id, RefType::File, file_id, now)?;
        }
        self.gc_object(&file.object_id).map(|_| ())
    }

    pub fn restore_file(&self, user_id: &str, file_id: &str) -> Result<()> {
        let file = self.trashed_file_row(user_id, file_id)?;
        let now = now_ms();
        {
            let c = self.db.lock();
            c.execute(
                "UPDATE media_file SET trashed_at = NULL, updated_at = ?1
                 WHERE file_id = ?2 AND owner = ?3",
                params![now, file_id, user_id],
            )?;
            let audience = Audience::User {
                id: user_id.to_string(),
            };
            let spec = RefSpec {
                ref_type: RefType::File,
                ext_id: file_id,
                owner: user_id,
                audience: &audience,
                purpose: Purpose::File,
            };
            add_ref_conn(&c, &file.object_id, &spec, now)?;
        }
        Ok(())
    }

    /// Hard delete from the recycle bin. Only this removes the file row and the
    /// object reference; GC then frees the blob if nothing else needs it.
    pub fn purge_file(&self, user_id: &str, file_id: &str) -> Result<()> {
        let file = self.trashed_file_row(user_id, file_id)?;
        let now = now_ms();
        {
            let c = self.db.lock();
            let changed = c.execute(
                "DELETE FROM media_file WHERE file_id = ?1 AND owner = ?2 AND trashed_at IS NOT NULL",
                params![file_id, user_id],
            )?;
            if changed == 0 {
                return Err(MediaError::NotFound);
            }
            remove_ref_conn(&c, &file.object_id, RefType::File, file_id, now)?;
        }
        self.gc_object(&file.object_id).map(|_| ())
    }

    fn trashed_file_row(&self, user_id: &str, file_id: &str) -> Result<FileItem> {
        let c = self.db.lock();
        let sql = format!(
            "SELECT {FILE_COLUMNS} FROM media_file f
             JOIN media_object o ON o.object_id = f.object_id
             WHERE f.file_id = ?1 AND f.owner = ?2"
        );
        c.query_row(&sql, params![file_id, user_id], file_from_row)
            .optional()?
            .ok_or(MediaError::NotFound)
    }

    /// Reference-safe physical deletion. The transaction is the last word:
    /// an object is marked deleted only when no active ref, non-trashed file
    /// or live share points at it. The blob is removed after commit.
    pub fn gc_object(&self, object_id: &str) -> Result<bool> {
        let mut c = self.db.lock();
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let live_refs: i64 = tx.query_row(
            "SELECT COUNT(*) FROM media_ref WHERE object_id = ?1 AND deleted_at IS NULL",
            params![object_id],
            |row| row.get(0),
        )?;
        let live_files: i64 = tx.query_row(
            "SELECT COUNT(*) FROM media_file WHERE object_id = ?1",
            params![object_id],
            |row| row.get(0),
        )?;
        let live_shares: i64 = tx.query_row(
            "SELECT COUNT(*) FROM media_share
             WHERE object_id = ?1 AND revoked_at IS NULL AND expires_at > ?2",
            params![object_id, now_ms()],
            |row| row.get(0),
        )?;
        if live_refs + live_files + live_shares > 0 {
            tx.commit()?;
            return Ok(false);
        }
        let changed = tx.execute(
            "UPDATE media_object SET status = 'deleted', deleted_at = ?1
             WHERE object_id = ?2 AND status = 'ready'",
            params![now_ms(), object_id],
        )?;
        tx.commit()?;
        drop(c);
        if changed > 0 {
            self.purge_derived_for_source(object_id)?;
            self.store.delete(object_id)?;
        }
        Ok(changed > 0)
    }

    pub fn create_share(
        &self,
        user_id: &str,
        file_id: &str,
        ttl_ms: i64,
        audience: Option<Audience>,
    ) -> Result<ShareGrant> {
        let file = self.file_row(user_id, file_id)?;
        let ttl = ttl_ms.clamp(SHARE_MIN_TTL_MS, self.config.share_ttl_ms);
        let now = now_ms();
        let token = ids::random_hex(SHARE_TOKEN_BYTES);
        let token_hash = ids::sha256_hex(token.as_bytes());
        let share_id = ids::opaque_id("share");
        let audience = audience.unwrap_or(Audience::Public);
        let expires_at = now + ttl;
        {
            let c = self.db.lock();
            c.execute(
                "INSERT INTO media_share
                    (share_id, token_hash, object_id, owner, audience, expires_at, created_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    share_id,
                    token_hash,
                    file.object_id,
                    user_id,
                    audience.key(),
                    expires_at,
                    now
                ],
            )?;
        }
        Ok(ShareGrant {
            share_id,
            url: format!("{SIGNED_SHARE_PREFIX}/{}?share={token}", file.object_id),
            token,
            expires_at,
        })
    }

    pub fn revoke_share(&self, user_id: &str, share_id: &str) -> Result<()> {
        let changed = {
            let c = self.db.lock();
            c.execute(
                "UPDATE media_share SET revoked_at = ?1
                 WHERE share_id = ?2 AND owner = ?3 AND revoked_at IS NULL",
                params![now_ms(), share_id, user_id],
            )?
        };
        if changed == 0 {
            return Err(MediaError::NotFound);
        }
        Ok(())
    }

    /// Resolve a bearer share token. Expired and revoked tokens are both
    /// `Gone`; an unknown token is `NotFound`.
    pub fn resolve_share(&self, token: &str) -> Result<crate::model::ObjectRecord> {
        let token_hash = ids::sha256_hex(token.as_bytes());
        let row: Option<(String, i64, Option<i64>)> = {
            let c = self.db.lock();
            c.query_row(
                "SELECT object_id, expires_at, revoked_at FROM media_share
                 WHERE token_hash = ?1",
                params![token_hash],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
        };
        let (object_id, expires_at, revoked_at) = row.ok_or(MediaError::NotFound)?;
        if revoked_at.is_some() || expires_at < now_ms() {
            return Err(MediaError::Gone("share is no longer valid".into()));
        }
        self.object(&object_id)
    }

    /// Attach a logical reference (messaging, content, avatar). The caller must
    /// already be allowed to read the object; inserting a ref cannot grant
    /// access to a private object.
    pub fn add_reference(
        &self,
        user_id: &str,
        object_id: &str,
        ref_type: RefType,
        ext_id: &str,
        audience: Audience,
        purpose: Purpose,
    ) -> Result<()> {
        self.authorize_read(user_id, object_id)?;
        let now = now_ms();
        let c = self.db.lock();
        let spec = RefSpec {
            ref_type,
            ext_id,
            owner: user_id,
            audience: &audience,
            purpose,
        };
        add_ref_conn(&c, object_id, &spec, now)
    }

    pub fn remove_reference(&self, object_id: &str, ref_type: RefType, ext_id: &str) -> Result<()> {
        {
            let c = self.db.lock();
            remove_ref_conn(&c, object_id, ref_type, ext_id, now_ms())?;
        }
        self.gc_object(object_id)?;
        Ok(())
    }

    /// Verify a stored object against its content address. The bytes are
    /// streamed through SHA-256 in constant memory; a missing blob is `None`,
    /// a mismatch is `false`, never `Ok(true)` by assumption.
    pub fn verify_object_digest(&self, object_id: &str) -> Result<Option<bool>> {
        let Some(mut source) = self.store.open(object_id)? else {
            return Ok(None);
        };
        let scratch = self
            .store
            .cache_path(&format!("verify-{}", ids::random_hex(6)));
        let computed = {
            let mut target = std::fs::File::create(&scratch)?;
            std::io::copy(&mut source, &mut target)?;
            stream_digest_file(&scratch)?
        };
        std::fs::remove_file(&scratch).ok();
        Ok(Some(computed == object_id))
    }
}

fn list_base_args(user_id: &str, audiences: &[String]) -> Vec<Value> {
    let mut args = vec![Value::Text(user_id.to_string())];
    args.extend(audiences.iter().cloned().map(Value::Text));
    args
}

fn list_sql(
    sort: FileSort,
    args: &mut Vec<Value>,
    query: &FileListQuery,
    cursor: Option<&Cursor>,
) -> String {
    // `args` holds the owner id plus one value per visible audience.
    let markers = vec!["?"; args.len().saturating_sub(1)].join(",");
    let mut sql = format!(
        "SELECT {FILE_COLUMNS} FROM media_file f
         JOIN media_object o ON o.object_id = f.object_id
         WHERE f.trashed_at IS NULL AND o.status = 'ready'
           AND (f.owner = ? OR EXISTS (
                SELECT 1 FROM media_ref r WHERE r.object_id = f.object_id
                  AND r.deleted_at IS NULL AND r.audience IN ({markers})
           ))"
    );
    if let Some(category) = query.category.as_deref().filter(|value| !value.is_empty()) {
        args.push(Value::Text(category.to_string()));
        sql.push_str(" AND f.category = ?");
    }
    if let Some(folder) = query.folder.as_deref().filter(|value| !value.is_empty()) {
        args.push(Value::Text(folder.to_string()));
        sql.push_str(" AND f.folder = ?");
    }
    append_cursor(sort, args, cursor, &mut sql);
    sql
}

fn append_cursor(sort: FileSort, args: &mut Vec<Value>, cursor: Option<&Cursor>, sql: &mut String) {
    match (sort, cursor) {
        (FileSort::Recent, Some(cursor)) => match cursor.key {
            CursorKey::Int(value) => {
                args.push(Value::Integer(value));
                args.push(Value::Integer(value));
                args.push(Value::Text(cursor.file_id.clone()));
                sql.push_str(" AND (f.created_at < ? OR (f.created_at = ? AND f.file_id < ?))");
            }
            CursorKey::Text(_) => {}
        },
        (FileSort::Size, Some(cursor)) => match cursor.key {
            CursorKey::Int(value) => {
                args.push(Value::Integer(value));
                args.push(Value::Integer(value));
                args.push(Value::Text(cursor.file_id.clone()));
                sql.push_str(" AND (f.size < ? OR (f.size = ? AND f.file_id < ?))");
            }
            CursorKey::Text(_) => {}
        },
        (FileSort::Name, Some(cursor)) => match &cursor.key {
            CursorKey::Text(value) => {
                args.push(Value::Text(value.clone()));
                args.push(Value::Text(value.clone()));
                args.push(Value::Text(cursor.file_id.clone()));
                sql.push_str(" AND (f.name > ? COLLATE NOCASE OR (f.name = ? AND f.file_id > ?))");
            }
            CursorKey::Int(_) => {}
        },
        (_, None) => {}
    }
    sql.push_str(match sort {
        FileSort::Recent => " ORDER BY f.created_at DESC, f.file_id DESC",
        FileSort::Name => " ORDER BY f.name COLLATE NOCASE ASC, f.file_id ASC",
        FileSort::Size => " ORDER BY f.size DESC, f.file_id DESC",
    });
}

fn cursor_key_of(item: &FileItem, sort: FileSort) -> CursorKey {
    match sort {
        FileSort::Name => CursorKey::Text(item.name.clone()),
        FileSort::Size => CursorKey::Int(item.size as i64),
        FileSort::Recent => CursorKey::Int(item.created_at),
    }
}

fn page_from_items(mut items: Vec<FileItem>, limit: u32, sort: FileSort) -> FilePage {
    let has_more = items.len() > limit as usize;
    if has_more {
        items.truncate(limit as usize);
    }
    let next_cursor = if has_more {
        items
            .last()
            .map(|item| encode_cursor(&cursor_key_of(item, sort), &item.file_id))
    } else {
        None
    };
    let snapshot_version = items
        .iter()
        .map(|item| item.updated_at)
        .max()
        .unwrap_or_default();
    FilePage {
        items,
        next_cursor,
        has_more,
        snapshot_version,
    }
}
