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

use std::io::{Read as _, Seek as _};
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

/// 定长比较：签名不对时不要因为"第几个字节不同"泄露时间差。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/media", post(upload))
        .route("/api/media/{media_id}", get(download))
        // 给"带不了 Authorization 的消费者"（浏览器 <img>、分享页）签一条
        // 短期链接。签名只对这一个 id + 过期时间有效。
        .route("/api/media/{media_id}/url", get(signed_url))
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
        // **只读请求的那一段**（issue #14）。以前是 `fs::read` 整个文件再切片：
        // 一个 1 KiB 的 Range 会为一个 100 MiB 的对象分配 100 MiB，并发几个
        // 就能把服务端内存打满。这里 seek 到位、按长度读。
        let mut file = std::fs::File::open(&path)?;
        let total = file.metadata()?.len();
        if total == 0 {
            return Ok(Some(Vec::new()));
        }
        let start = start.min(total - 1);
        let end = end.min(total - 1);
        if start > end {
            return Ok(Some(Vec::new()));
        }
        let len = usize::try_from(end - start + 1).unwrap_or(0);
        let mut buf = vec![0u8; len];
        file.seek(std::io::SeekFrom::Start(start))?;
        file.read_exact(&mut buf)?;
        Ok(Some(buf))
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
    /// 签名 URL 的过期时间（ms，epoch）。与 `sig` 成对出现。
    #[serde(default)]
    exp: Option<u64>,
    /// `hex(HMAC-SHA256(key, "<media_id>:<exp>"))`
    #[serde(default)]
    sig: Option<String>,
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
        conv_id: Option<&str>,
        public: bool,
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
            "INSERT INTO media(id, size, content_type, uploader, conv_id, public, has_thumb, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET size = excluded.size,
                -- 一旦公开就不回收：头像被别的用户引用着
                public = MAX(media.public, excluded.public),
                -- 同一份字节可能被不同会话引用：保留第一次的归属，
                -- 空归属则补上（内容寻址下，归属是引用关系而不是字节属性）
                conv_id = CASE WHEN media.conv_id = '' THEN excluded.conv_id
                               ELSE media.conv_id END",
            params![
                sha_hex,
                size as i64,
                content_type,
                uploader,
                conv_id.unwrap_or_default(),
                public as i64,
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

    /// 这个用户能不能读这个会话（直聊看 id 两半，群看成员表）。
    ///
    /// 与 `server::authorize_conv` 同一条规则，放在这里是因为媒体下载也要用；
    /// 两份实现迟早会漂移，所以 server 那边改成调它。
    pub fn may_read_conv(&self, user_id: &str, conv_id: &str) -> bool {
        if let Some(rest) = conv_id.strip_prefix("d-") {
            let mut parts = rest.split('~');
            let (Some(a), Some(b), None) = (parts.next(), parts.next(), parts.next()) else {
                return false;
            };
            return a == user_id || b == user_id;
        }
        self.is_member(conv_id, user_id).unwrap_or(false)
    }

    /// 媒体归属：`(uploader, conv_id)`。`None` = 没这条记录。
    fn media_owner(&self, media_id: &str) -> Result<Option<(String, String, bool)>> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT uploader, conv_id, public FROM media WHERE id = ?1",
            params![media_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)? != 0,
                ))
            },
        )
        .optional()?)
    }

    /// 这个用户能不能下载这条媒体（issue #14）。
    ///
    /// **内容地址不是权限**：`media_id` 是 SHA-256，知道文件就能算出来，
    /// 日志/转发也会泄露它。所以判据只有两条：
    /// 1. 上传者本人；
    /// 2. 媒体挂着的会话的成员（没挂会话 = 只有上传者）。
    pub fn may_read_media(&self, media_id: &str, user_id: &str) -> Result<bool> {
        let Some((uploader, conv_id, public)) = self.media_owner(media_id)? else {
            return Ok(false);
        };
        // 公开资源（头像）：登录用户都能看，但**仍要登录**——匿名枚举不做
        if public {
            return Ok(true);
        }
        if uploader == user_id {
            return Ok(true);
        }
        if conv_id.is_empty() {
            return Ok(false);
        }
        Ok(self.may_read_conv(user_id, &conv_id))
    }

    /// 签一条短期下载链接（给带不了 `Authorization` 的消费者）。
    pub fn sign_media_url(&self, media_id: &str, ttl_ms: u64) -> (String, u64) {
        let exp = wxlike_core::types::now_ms() + ttl_ms;
        let sig = self.media_sig(media_id, exp);
        (format!("/api/media/{media_id}?exp={exp}&sig={sig}"), exp)
    }

    /// 校验签名。过期或签名不对都返回 `false`（不区分，别给探测者信息）。
    pub fn verify_media_sig(&self, media_id: &str, exp: u64, sig: &str) -> bool {
        if exp < wxlike_core::types::now_ms() {
            return false;
        }
        constant_time_eq(sig.as_bytes(), self.media_sig(media_id, exp).as_bytes())
    }

    fn media_sig(&self, media_id: &str, exp: u64) -> String {
        use hmac::Mac as _;
        let mut mac =
            hmac::Hmac::<Sha256>::new_from_slice(self.options().media_signing_key.as_bytes())
                .expect("hmac accepts any key length");
        mac.update(format!("{media_id}:{exp}").as_bytes());
        hex::encode(mac.finalize().into_bytes())
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
    // 上传时声明归属会话（issue #14）：下载时据此判成员。不声明 = 只有自己能取。
    let mut conv: Option<String> = None;
    // `visibility=public`：头像这类"所有登录用户都要能看"的资源
    let mut public = false;

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
            "conv" | "conv_id" => conv = Some(read_text(field).await?),
            "visibility" => public = read_text(field).await?.trim() == "public",
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
    let conv = conv.map(|c| c.trim().to_string()).filter(|c| !c.is_empty());
    if let Some(conv) = conv.as_deref() {
        // 不能把附件挂到别人的会话上——那等于给会话成员投递任意内容
        if !platform.may_read_conv(&authed.user_id, conv) {
            return Err(ApiError::forbidden("not your conversation"));
        }
    }
    let reply = platform
        .store_media(
            bytes,
            declared.as_deref(),
            &authed.user_id,
            conv.as_deref(),
            public,
        )
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

/// `GET /api/media/{id}/url`：给当前用户签一条短期下载链接。
///
/// 权限判定与直接下载完全一样（上传者或会话成员），只是把授权换成"签名"，
/// 这样浏览器端可以把链接塞进 `<img src>`。
async fn signed_url(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiPath(media_id): ApiPath<String>,
    Query(q): Query<UrlQuery>,
) -> std::result::Result<Json<SignedUrl>, ApiError> {
    if !platform
        .may_read_media(&media_id, &authed.user_id)
        .map_err(ApiError::from)?
    {
        return Err(ApiError::not_found("no such media"));
    }
    let ttl = q
        .ttl_secs
        .unwrap_or(MEDIA_URL_DEFAULT_TTL_SECS)
        .clamp(1, MEDIA_URL_MAX_TTL_SECS);
    let (url, expires_at) = platform.sign_media_url(&media_id, ttl.saturating_mul(1_000));
    Ok(Json(SignedUrl { url, expires_at }))
}

#[derive(serde::Deserialize, Default)]
pub struct UrlQuery {
    #[serde(default)]
    ttl_secs: Option<u64>,
}

#[derive(Serialize)]
pub struct SignedUrl {
    pub url: String,
    /// 过期时间（ms，epoch）。
    pub expires_at: u64,
}

/// 签名链接默认有效期：5 分钟。够浏览器把一张图画出来，不够被转发收藏。
pub const MEDIA_URL_DEFAULT_TTL_SECS: u64 = 5 * 60;
/// 上限 1 小时：再长就接近"永久公开链接"了。
pub const MEDIA_URL_MAX_TTL_SECS: u64 = 60 * 60;

/// 下载授权：签名 URL 或"登录 + 归属"。返回 `Some(Response)` = 已经决定拒绝。
///
/// 两条凭据二选一：
/// 1. **短期签名**——浏览器 `<img>` 带不了 `Authorization`；
/// 2. **会话令牌 + 归属**——上传者本人，或媒体所属会话的成员。
///
/// 内容地址（SHA-256）不是权限：知道文件就能算出 id，日志与转发也会泄露它。
fn authorize_download(
    platform: &Platform,
    media_id: &str,
    headers: &axum::http::HeaderMap,
    q: &DownloadQuery,
) -> Option<Response> {
    let signed = match (q.exp, q.sig.as_deref()) {
        (Some(exp), Some(sig)) => platform.verify_media_sig(media_id, exp, sig),
        _ => false,
    };
    if signed {
        return None;
    }
    let caller =
        Authed::bearer_from(headers).and_then(|token| platform.lookup_token(token).ok().flatten());
    let Some(caller) = caller else {
        return Some(
            ApiError::new(StatusCode::UNAUTHORIZED, "需要登录或有效的签名链接").into_response(),
        );
    };
    match platform.may_read_media(media_id, &caller) {
        Ok(true) => None,
        // 别人的附件一律 404：403 等于确认"这个 id 存在"
        Ok(false) => Some(StatusCode::NOT_FOUND.into_response()),
        Err(err) => Some(ApiError::from(err).into_response()),
    }
}

async fn download(
    State(platform): State<PlatformState>,
    ApiPath(media_id): ApiPath<String>,
    headers: axum::http::HeaderMap,
    Query(q): Query<DownloadQuery>,
) -> Response {
    // **先授权再碰路径**（issue #14）。
    if let Some(rejected) = authorize_download(&platform, &media_id, &headers, &q) {
        return rejected;
    }

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

    // this handler answers with a bare `Response`, so errors are mapped inline.
    //
    // **总是按 Range 取**：`resolve` 只给了长度与类型（issue #14）。
    // 缩略图很小、且存在另一条路径里，单独取。
    let bytes = if want_thumb {
        match platform.media_store().get_thumb(&media_id).await {
            Ok(Some(b)) => b,
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(err) => return ApiError::from(err).into_response(),
        }
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
    let Some((total, content_type, _has_thumb)) = platform.media_meta(media_id)? else {
        return Ok(None);
    };
    // **不再读整个对象**（issue #14）：`resolve` 只需要长度与类型，
    // 真正要发的字节由 `download` 按 Range 取。以前这里无条件
    // `get(0, total-1)`，于是"取 1 KiB"也先把 100 MiB 读进内存。
    Ok(Some(StoredObject {
        bytes: Vec::new(),
        start: 0,
        end: total.saturating_sub(1),
        total,
        content_type,
    }))
}

/// Where media files live by default (`WXLIKE_MEDIA_DIR`).
pub fn default_media_dir() -> PathBuf {
    std::env::var("WXLIKE_MEDIA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new("data").join("media"))
}
