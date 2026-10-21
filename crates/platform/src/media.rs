//! WP-05 media: content-addressed object storage behind a trait, plus HTTP
//! upload/download with `Range`, thumbnails, and hard size/format limits.
//!
//! `media_id` **is** the SHA-256 of the bytes (hex), so storing the same file
//! twice is one write and one row — dedup falls out of the addressing rather
//! than being tracked separately. The id doubles as the lookup key and is
//! validated as 64 hex characters before it is ever joined onto a path, which
//! is what makes `../../etc/passwd` a 404 instead of a directory escape.
//!
//! [`MediaStore`] is the seam: [`LocalDirStore`] serves today, an S3/MinIO
//! implementation would plug in without touching the handlers.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use axum::extract::multipart::{Field, Multipart};
use axum::extract::{DefaultBodyLimit, Path as ApiPath, Query, State};
use axum::http::header;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension as _};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::{Platform, PlatformState};

/// WP-05 A1: uploads larger than this are refused with 413.
pub const MAX_MEDIA_BYTES: usize = 100 * 1024 * 1024;
/// Longest edge of an generated thumbnail.
pub const THUMB_MAX_EDGE: u32 = 256;

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/media", post(upload))
        .route("/api/media/{media_id}", get(download))
        // the cap is per-route so downloads are not limited by it
        .layer(DefaultBodyLimit::max(MAX_MEDIA_BYTES))
}

/// One object as read back from storage.
#[derive(Debug)]
pub struct StoredObject {
    pub bytes: Vec<u8>,
    /// Inclusive first byte of this slice.
    pub start: u64,
    /// Inclusive last byte.
    pub end: u64,
    /// Full object length, for `Content-Range`.
    pub total: u64,
    pub content_type: String,
}

#[async_trait]
pub trait MediaStore: Send + Sync + 'static {
    /// Persist the object. `sha_hex` is the content address and the file name.
    async fn put(&self, sha_hex: &str, bytes: &[u8]) -> Result<()>;
    /// Read `bytes[start..=end]` (inclusive), clamped to the object length.
    /// `None` when the object does not exist.
    async fn get(&self, sha_hex: &str, start: u64, end: u64) -> Result<Option<Vec<u8>>>;
    /// Full object length, or `None` when absent.
    async fn size_of(&self, sha_hex: &str) -> Result<Option<u64>>;

    async fn put_thumb(&self, sha_hex: &str, png: &[u8]) -> Result<()>;
    async fn get_thumb(&self, sha_hex: &str) -> Result<Option<Vec<u8>>>;
}

/// Files under `root/{first2}/{sha}`; thumbnail beside it as `{sha}.thumb.png`.
pub struct LocalDirStore {
    root: PathBuf,
}

impl LocalDirStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The id is validated *before* any path is built: hex-only, exactly 64
    /// chars. This is the traversal defence, not a filter on top of it.
    fn check_id(sha_hex: &str) -> Result<()> {
        if sha_hex.len() != wxlike_core::limits::SHA256_HEX_LEN
            || !sha_hex.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(PlatformError::BadInput("malformed media id"));
        }
        Ok(())
    }

    fn object_path(&self, sha_hex: &str) -> PathBuf {
        self.root.join(&sha_hex[..2]).join(sha_hex)
    }

    fn thumb_path(&self, sha_hex: &str) -> PathBuf {
        self.root
            .join(&sha_hex[..2])
            .join(format!("{sha_hex}.thumb.png"))
    }
}

#[async_trait]
impl MediaStore for LocalDirStore {
    async fn put(&self, sha_hex: &str, bytes: &[u8]) -> Result<()> {
        Self::check_id(sha_hex)?;
        let path = self.object_path(sha_hex);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // content-addressed: an existing file with this name already holds
        // exactly these bytes, so a rewrite would be pure waste
        if path.exists() {
            return Ok(());
        }
        let tmp = path.with_extension("part");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    async fn get(&self, sha_hex: &str, start: u64, end: u64) -> Result<Option<Vec<u8>>> {
        Self::check_id(sha_hex)?;
        let path = self.object_path(sha_hex);
        if !path.exists() {
            return Ok(None);
        }
        let all = std::fs::read(&path)?;
        let total = all.len() as u64;
        let start = start.min(total.saturating_sub(1));
        let end = end.min(total.saturating_sub(1));
        if start > end || total == 0 {
            return Ok(Some(Vec::new()));
        }
        Ok(Some(all[start as usize..=end as usize].to_vec()))
    }

    async fn size_of(&self, sha_hex: &str) -> Result<Option<u64>> {
        Self::check_id(sha_hex)?;
        let path = self.object_path(sha_hex);
        match std::fs::metadata(&path) {
            Ok(m) => Ok(Some(m.len())),
            Err(_) => Ok(None),
        }
    }

    async fn put_thumb(&self, sha_hex: &str, png: &[u8]) -> Result<()> {
        Self::check_id(sha_hex)?;
        let path = self.thumb_path(sha_hex);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, png)?;
        Ok(())
    }

    async fn get_thumb(&self, sha_hex: &str) -> Result<Option<Vec<u8>>> {
        Self::check_id(sha_hex)?;
        match std::fs::read(self.thumb_path(sha_hex)) {
            Ok(b) => Ok(Some(b)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }
}

#[derive(Serialize)]
pub struct UploadReply {
    pub media_id: String,
    pub size: u64,
    pub content_type: String,
    /// False when the object was already present (dedup hit).
    pub newly_stored: bool,
    pub has_thumb: bool,
}

#[derive(serde::Deserialize, Default)]
pub struct DownloadQuery {
    #[serde(default)]
    thumb: Option<u8>,
}

/// PNG / JPEG / GIF / WEBP by magic bytes: the declared part is client input,
/// so it never decides what we serve.
fn sniff(bytes: &[u8]) -> &'static str {
    let head = |n: usize| bytes.get(..n).unwrap_or(&[]);
    if head(8) == b"\x89PNG\r\n\x1a\n" {
        "image/png"
    } else if head(3) == b"\xff\xd8\xff" {
        "image/jpeg"
    } else if head(6) == b"GIF87a" || head(6) == b"GIF89a" {
        "image/gif"
    } else if head(4) == b"RIFF" && bytes.get(8..12) == Some(b"WEBP") {
        "image/webp"
    } else {
        "application/octet-stream"
    }
}

fn is_image(content_type: &str) -> bool {
    content_type.starts_with("image/")
}

/// Longest edge scaled down to [`THUMB_MAX_EDGE`]; `None` for anything the
/// decoder cannot read (a corrupt image is an opaque object, not an error).
fn make_thumb(bytes: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory(bytes).ok()?;
    let (w, h) = (img.width(), img.height());
    let scaled = if w.max(h) <= THUMB_MAX_EDGE {
        // already small: re-encode as-is so the thumb route still works
        img
    } else {
        img.resize(
            THUMB_MAX_EDGE,
            THUMB_MAX_EDGE,
            image::imageops::FilterType::Triangle,
        )
    };
    let mut out = Vec::new();
    scaled
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .ok()?;
    Some(out)
}

/// Parse `Range: bytes=a-b` / `bytes=a-` / `bytes=-suffix`.
/// `None` means "whole object". An unsatisfiable or unparseable range is
/// reported so the handler can answer 416.
fn parse_range(value: Option<&str>, total: u64) -> Option<(u64, u64)> {
    let spec = value?.strip_prefix("bytes=")?;
    if total == 0 {
        return Some((0, 0));
    }
    let last = total - 1;
    let (first_s, last_s) = spec.split_once('-')?;
    if first_s.is_empty() {
        // suffix form: last N bytes
        let n: u64 = last_s.trim().parse().ok()?;
        if n == 0 {
            return None;
        }
        let start = last.saturating_sub(n - 1);
        return Some((start, last));
    }
    let start: u64 = first_s.trim().parse().ok()?;
    if start > last {
        return None;
    }
    let end = if last_s.trim().is_empty() {
        last
    } else {
        let e: u64 = last_s.trim().parse().ok()?;
        e.min(last)
    };
    Some((start, end))
}

impl Platform {
    /// Stores one object, dedup by content address. Validates the client's
    /// declared digest when it supplied one (WP-05 A3).
    pub async fn store_media(
        &self,
        bytes: Vec<u8>,
        declared_sha: Option<&str>,
        uploader: &str,
    ) -> Result<UploadReply> {
        if bytes.is_empty() {
            return Err(PlatformError::BadInput("empty upload"));
        }
        if bytes.len() > self.options().max_media_bytes {
            return Err(PlatformError::PayloadTooLarge);
        }
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let sha_hex = hex::encode(hasher.finalize());

        if let Some(declared) = declared_sha {
            let declared = declared.trim().to_ascii_lowercase();
            if declared != sha_hex {
                return Err(PlatformError::BadInput("sha256 does not match content"));
            }
        }

        let store = self.media_store();
        let existed = store.size_of(&sha_hex).await?.is_some();
        store.put(&sha_hex, &bytes).await?;

        let content_type = sniff(&bytes).to_string();
        let thumb = if is_image(&content_type) {
            make_thumb(&bytes)
        } else {
            None
        };
        let has_thumb = match &thumb {
            Some(png) => {
                store.put_thumb(&sha_hex, png).await?;
                true
            }
            None => false,
        };

        let size = bytes.len() as u64;
        let c = self.db().lock();
        c.execute(
            "INSERT INTO media(id, size, content_type, uploader, has_thumb, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET size = excluded.size",
            params![
                sha_hex,
                size as i64,
                content_type,
                uploader,
                has_thumb as i64,
                wxlike_core::types::now_ms() as i64
            ],
        )?;
        drop(c);

        Ok(UploadReply {
            media_id: sha_hex,
            size,
            content_type,
            newly_stored: !existed,
            has_thumb,
        })
    }

    pub fn media_meta(&self, media_id: &str) -> Result<Option<(u64, String, bool)>> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT size, content_type, has_thumb FROM media WHERE id = ?1",
            params![media_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)? as u64,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)? != 0,
                ))
            },
        )
        .optional()?)
    }

    /// The object store this platform writes through.
    pub fn media_store(&self) -> std::sync::Arc<dyn MediaStore> {
        self.media
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Swap the backing store (tests use a temp dir; MinIO/S3 later).
    pub fn set_media_store(&self, store: std::sync::Arc<dyn MediaStore>) {
        let mut guard = self
            .media
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = store;
    }
}

async fn upload(
    State(platform): State<PlatformState>,
    authed: Authed,
    mut form: Multipart,
) -> std::result::Result<(StatusCode, Json<UploadReply>), ApiError> {
    let mut data: Option<Vec<u8>> = None;
    let mut declared: Option<String> = None;

    while let Some(field) = form.next_field().await.map_err(|err| {
        // a body over the limit surfaces here rather than at the layer
        if format!("{err}").contains("length") || format!("{err}").contains("limit") {
            ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "media too large")
        } else {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("malformed multipart: {err}"),
            )
        }
    })? {
        match field.name().unwrap_or_default() {
            "file" | "bytes" | "data" => data = Some(read_field(field).await?),
            "sha256" | "sha" => declared = Some(read_text(field).await?),
            other => {
                return Err(ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("unexpected field '{other}'"),
                ))
            }
        }
    }

    let bytes =
        data.ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "missing 'file' field"))?;
    let reply = platform
        .store_media(bytes, declared.as_deref(), &authed.user_id)
        .await
        .map_err(ApiError::from)?;
    Ok((StatusCode::OK, Json(reply)))
}

async fn read_field(field: Field<'_>) -> std::result::Result<Vec<u8>, ApiError> {
    field.bytes().await.map(|b| b.to_vec()).map_err(|err| {
        let text = format!("{err}");
        if text.contains("length") || text.contains("limit") {
            ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "media too large")
        } else {
            ApiError::new(StatusCode::BAD_REQUEST, format!("unreadable field: {err}"))
        }
    })
}

async fn read_text(field: Field<'_>) -> std::result::Result<String, ApiError> {
    Ok(String::from_utf8_lossy(&read_field(field).await?).into_owned())
}

async fn download(
    State(platform): State<PlatformState>,
    ApiPath(media_id): ApiPath<String>,
    headers: axum::http::HeaderMap,
    Query(q): Query<DownloadQuery>,
) -> Response {
    // An unknown or malformed id must be a 404 before any path is touched.
    let want_thumb = q.thumb.unwrap_or(0) == 1;
    let object = match resolve(&platform, &media_id, want_thumb).await {
        Ok(Some(o)) => o,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "application/json")],
                r#"{"error":"no such media"}"#,
            )
                .into_response()
        }
        Err(err) => return ApiError::from(err).into_response(),
    };

    let total = object.total;
    let range = parse_range(
        headers.get(header::RANGE).and_then(|v| v.to_str().ok()),
        total,
    );
    let (start, end) = match range {
        Some(pair) => pair,
        None if headers.contains_key(header::RANGE) => {
            return (
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{total}"))],
            )
                .into_response();
        }
        None => (0, total.saturating_sub(1)),
    };

    // this handler answers with a bare `Response`, so errors are mapped inline
    let bytes = if start == 0 && end + 1 == total {
        object.bytes
    } else {
        match platform.media_store().get(&media_id, start, end).await {
            Ok(Some(b)) => b,
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(err) => return ApiError::from(err).into_response(),
        }
    };

    let partial = start != 0 || end + 1 != total;
    let status = if partial {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, object.content_type)
        .header(header::CONTENT_LENGTH, bytes.len())
        .header(header::ACCEPT_RANGES, "bytes");
    if partial {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{total}"),
        );
    }
    builder
        .body(axum::body::Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Reads the object (or its thumbnail) through the store.
async fn resolve(
    platform: &Platform,
    media_id: &str,
    want_thumb: bool,
) -> Result<Option<StoredObject>> {
    let store = platform.media_store();
    if want_thumb {
        // the thumbnail exists only for decoded images; the row records that
        let png = store.get_thumb(media_id).await?;
        let Some(bytes) = png else {
            return Ok(None);
        };
        let total = bytes.len() as u64;
        return Ok(Some(StoredObject {
            end: total.saturating_sub(1),
            bytes,
            start: 0,
            total,
            content_type: "image/png".to_string(),
        }));
    }
    match crate::validate::id(media_id) {
        Ok(_) => {}
        // not a valid id: no filesystem lookup at all
        Err(_) => return Ok(None),
    }
    let Some(meta) = platform.media_meta(media_id)? else {
        return Ok(None);
    };
    let bytes = match store.get(media_id, 0, meta.0.saturating_sub(1)).await? {
        Some(b) => b,
        None => return Ok(None),
    };
    Ok(Some(StoredObject {
        bytes,
        start: 0,
        end: meta.0.saturating_sub(1),
        total: meta.0,
        content_type: meta.1,
    }))
}

/// Where media files live by default (`WXLIKE_MEDIA_DIR`).
pub fn default_media_dir() -> PathBuf {
    std::env::var("WXLIKE_MEDIA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new("data").join("media"))
}
