//! HTTP projection of the media domain.
//!
//! The router is intentionally dumb: it authenticates, parses, calls one service
//! method and renders the shared error shape. Owner checks, idempotency and
//! revocation all live in the service, so a second transport cannot bypass them.

use std::collections::HashMap;
use std::io::SeekFrom;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, FromRequestParts, Path, Query, State};
use axum::http::header::{
    ACCEPT_RANGES, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE,
};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse as _, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
use tokio_util::io::ReaderStream;

use crate::access::constant_time_eq;
use crate::backup::{BackupSummary, CacheCandidate, CachePlan, EncryptionMeta, ImportOutcome};
use crate::error::{MediaError, Result as MediaResult};
use crate::files::{FileListQuery, FilePage, ShareGrant};
use crate::ids;
use crate::model::{Audience, Preset};
use crate::platform::{
    PlatformBroker, PlatformCapabilities, PlatformKind, PlatformRequest, PlatformResult,
};
use crate::uploads::{
    CompletedUpload, MediaCapabilities, UploadProgress, UploadSession, UploadStart,
};
use crate::MediaService;

pub const API_PREFIX: &str = "/api/v1";
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
pub const RANGE_PREFIX: &str = "bytes=";
pub const NO_STORE: &str = "private, no-store";

/// Session verification is owned by G02. The composition root injects the real
/// implementation; this crate only requires "bearer header -> user id".
pub trait SessionVerifier: Send + Sync {
    fn verify(&self, headers: &HeaderMap) -> Option<String>;
}

pub struct FnVerifier<F>(pub F);

impl<F> SessionVerifier for FnVerifier<F>
where
    F: Fn(&HeaderMap) -> Option<String> + Send + Sync,
{
    fn verify(&self, headers: &HeaderMap) -> Option<String> {
        (self.0)(headers)
    }
}

/// Development verifier: one static bearer token for one user. Production must
/// inject a real session verifier through [`MediaApp::new`].
pub struct StaticBearer {
    pub token: String,
    pub user_id: String,
}

impl SessionVerifier for StaticBearer {
    fn verify(&self, headers: &HeaderMap) -> Option<String> {
        let raw = headers.get("authorization")?.to_str().ok()?;
        let token = raw.strip_prefix("Bearer ")?;
        if constant_time_eq(token.as_bytes(), self.token.as_bytes()) {
            Some(self.user_id.clone())
        } else {
            None
        }
    }
}

pub struct AppState {
    pub service: Arc<MediaService>,
    pub verifier: Arc<dyn SessionVerifier>,
    pub broker: PlatformBroker,
}

pub struct MediaApp {
    state: Arc<AppState>,
}

impl MediaApp {
    pub fn new(service: Arc<MediaService>, verifier: Arc<dyn SessionVerifier>) -> Self {
        Self {
            state: Arc::new(AppState {
                service,
                verifier,
                broker: PlatformBroker::new(),
            }),
        }
    }

    pub fn router(self) -> Router {
        let chunk_limit = MAX_BODY_BYTES;
        Router::new()
            .route("/api/v1/media/capabilities", get(capabilities))
            .route("/api/v1/uploads", post(start_upload))
            .route("/api/v1/uploads/{id}", get(upload_status))
            .route("/api/v1/uploads/{id}/chunks", put(upload_chunk))
            .route("/api/v1/uploads/{id}/complete", post(complete_upload))
            .route("/api/v1/uploads/{id}/cancel", post(cancel_upload))
            .route("/api/v1/objects/{id}/access", post(authorize_download))
            .route("/api/v1/objects/{id}/revoke", post(revoke_access))
            .route("/api/v1/objects/{id}", get(download))
            .route("/api/v1/files", get(list_files))
            .route("/api/v1/files/{id}", patch(patch_file).delete(trash_file))
            .route("/api/v1/files/{id}/restore", post(restore_file))
            .route("/api/v1/files/{id}/purge", delete(purge_file))
            .route("/api/v1/files/{id}/shares", post(create_share))
            .route("/api/v1/shares/{id}", delete(revoke_share))
            .route("/api/v1/jobs", post(submit_job))
            .route("/api/v1/jobs/{id}", get(job_status))
            .route("/api/v1/jobs/{id}/cancel", post(cancel_job))
            .route("/api/v1/backups", post(export_backup))
            .route("/api/v1/backups/{id}/package", get(download_backup))
            .route("/api/v1/backups/verify", post(verify_backup))
            .route("/api/v1/backups/import", post(import_backup))
            .route("/api/v1/backups/{id}/keys", post(restore_backup_keys))
            .route("/api/v1/cache/plan", post(cache_plan))
            .route("/api/v1/cache/execute", post(cache_execute))
            .route("/api/v1/platform/capabilities", get(platform_capabilities))
            .route("/api/v1/platform/requests", post(platform_request))
            .route("/api/v1/platform/audio/indicator", get(audio_indicator))
            .layer(DefaultBodyLimit::max(chunk_limit))
            .with_state(self.state)
    }
}

pub struct Principal(pub String);

impl FromRequestParts<Arc<AppState>> for Principal {
    type Rejection = wxlike_api::ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> std::result::Result<Self, Self::Rejection> {
        state
            .verifier
            .verify(&parts.headers)
            .map(Principal)
            .ok_or_else(|| MediaError::Unauthorized.into_api())
    }
}

fn map_err(err: MediaError) -> wxlike_api::ApiError {
    err.into_api()
}

fn map<T>(result: MediaResult<T>) -> std::result::Result<Json<T>, wxlike_api::ApiError> {
    result.map(Json).map_err(map_err)
}

async fn capabilities(
    State(state): State<Arc<AppState>>,
    _auth: Principal,
) -> std::result::Result<Json<MediaCapabilities>, wxlike_api::ApiError> {
    Ok(Json(state.service.capabilities()))
}

async fn start_upload(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    body: Json<UploadStart>,
) -> std::result::Result<(StatusCode, Json<UploadSession>), wxlike_api::ApiError> {
    let session = state
        .service
        .upload_start(&auth.0, body.0)
        .map_err(map_err)?;
    Ok((StatusCode::CREATED, Json(session)))
}

async fn upload_status(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    Path(id): Path<String>,
) -> std::result::Result<Json<UploadSession>, wxlike_api::ApiError> {
    map(state.service.upload_status(&auth.0, &id))
}

#[derive(Deserialize)]
struct ChunkQuery {
    offset: u64,
}

async fn upload_chunk(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    Path(id): Path<String>,
    Query(query): Query<ChunkQuery>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> std::result::Result<Json<UploadProgress>, wxlike_api::ApiError> {
    let declared = headers
        .get("x-chunk-sha256")
        .and_then(|value| value.to_str().ok());
    map(state
        .service
        .upload_chunk(&auth.0, &id, query.offset, &body, declared))
}

#[derive(Deserialize)]
struct CompleteBody {
    #[serde(default)]
    digest: Option<String>,
}

async fn complete_upload(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    Path(id): Path<String>,
    body: Json<CompleteBody>,
) -> std::result::Result<Json<CompletedUpload>, wxlike_api::ApiError> {
    map(state
        .service
        .upload_complete(&auth.0, &id, body.0.digest.as_deref()))
}

async fn cancel_upload(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    Path(id): Path<String>,
) -> std::result::Result<Json<UploadSession>, wxlike_api::ApiError> {
    map(state.service.upload_cancel(&auth.0, &id))
}

async fn authorize_download(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    Path(id): Path<String>,
) -> std::result::Result<Json<crate::access::SignedAccess>, wxlike_api::ApiError> {
    map(state.service.download_authorize(&auth.0, &id))
}

#[derive(Serialize)]
struct EpochView {
    epoch: i64,
}

async fn revoke_access(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    Path(id): Path<String>,
) -> std::result::Result<Json<EpochView>, wxlike_api::ApiError> {
    let epoch = state
        .service
        .revoke_object_access(&auth.0, &id)
        .map_err(map_err)?;
    Ok(Json(EpochView { epoch }))
}

#[derive(Deserialize, Default)]
struct DownloadQuery {
    exp: Option<i64>,
    epoch: Option<i64>,
    sig: Option<String>,
    share: Option<String>,
}

async fn download(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<DownloadQuery>,
    headers: HeaderMap,
) -> std::result::Result<Response, wxlike_api::ApiError> {
    ids::validate_object_id(&id).map_err(map_err)?;
    let object = resolve_download(&state, &id, &query)?;
    let range = parse_range(
        headers.get(RANGE).and_then(|v| v.to_str().ok()),
        object.size,
    );
    stream_object(&state, &object.object_id, object.size, &object.mime, range)
        .await
        .map_err(map_err)
}

fn resolve_download(
    state: &AppState,
    object_id: &str,
    query: &DownloadQuery,
) -> std::result::Result<crate::model::ObjectRecord, wxlike_api::ApiError> {
    if let Some(token) = query.share.as_deref().filter(|value| !value.is_empty()) {
        return state.service.resolve_share(token).map_err(map_err);
    }
    let (Some(exp), Some(epoch), Some(sig)) = (query.exp, query.epoch, query.sig.as_deref()) else {
        return Err(MediaError::Unauthorized.into_api());
    };
    state
        .service
        .verify_signed(object_id, exp, epoch, sig, crate::db::now_ms())
        .map_err(map_err)
}

#[derive(Clone, Copy)]
struct ByteRange {
    start: u64,
    len: u64,
}

fn parse_range(raw: Option<&str>, size: u64) -> Option<ByteRange> {
    let spec = raw?.strip_prefix(RANGE_PREFIX)?;
    let (start, end) = spec.split_once('-')?;
    let start: u64 = start.parse().ok()?;
    if start >= size {
        return Some(ByteRange { start, len: 0 });
    }
    let end: u64 = if end.is_empty() {
        size.saturating_sub(1)
    } else {
        end.parse::<u64>().ok()?.min(size.saturating_sub(1))
    };
    if end < start {
        return None;
    }
    Some(ByteRange {
        start,
        len: end - start + 1,
    })
}

async fn stream_object(
    state: &AppState,
    object_id: &str,
    total: u64,
    mime: &str,
    range: Option<ByteRange>,
) -> MediaResult<Response> {
    let file = state
        .service
        .open_object(object_id)?
        .ok_or(MediaError::NotFound)?;
    let mut file = tokio::fs::File::from_std(file);
    let (status, start, len) = match range {
        Some(range) if range.len == 0 => (StatusCode::RANGE_NOT_SATISFIABLE, 0, 0),
        Some(range) => (StatusCode::PARTIAL_CONTENT, range.start, range.len),
        None => (StatusCode::OK, 0, total),
    };
    if start > 0 {
        file.seek(SeekFrom::Start(start)).await?;
    }
    let reader = file.take(len);
    let stream = ReaderStream::new(reader);
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(mime)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(CONTENT_LENGTH, HeaderValue::from(len));
    headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static(NO_STORE));
    if status == StatusCode::PARTIAL_CONTENT {
        let end = start + len.saturating_sub(1);
        let value = format!("bytes {start}-{end}/{total}");
        if let Ok(header) = HeaderValue::from_str(&value) {
            headers.insert(CONTENT_RANGE, header);
        }
    } else if status == StatusCode::RANGE_NOT_SATISFIABLE {
        let value = format!("bytes */{total}");
        if let Ok(header) = HeaderValue::from_str(&value) {
            headers.insert(CONTENT_RANGE, header);
        }
    }
    Ok(response)
}

async fn list_files(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    Query(query): Query<FileListQuery>,
) -> std::result::Result<Json<FilePage>, wxlike_api::ApiError> {
    map(state.service.list_files(&auth.0, query))
}

#[derive(Deserialize)]
struct FilePatch {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    folder: Option<String>,
}

async fn patch_file(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    Path(id): Path<String>,
    body: Json<FilePatch>,
) -> std::result::Result<Json<crate::model::FileItem>, wxlike_api::ApiError> {
    let item = match (body.0.name, body.0.folder) {
        (Some(name), _) => state.service.rename_file(&auth.0, &id, &name),
        (None, Some(folder)) => state.service.move_file(&auth.0, &id, &folder),
        (None, None) => Err(MediaError::bad("name or folder is required")),
    };
    map(item)
}

async fn trash_file(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, wxlike_api::ApiError> {
    state.service.trash_file(&auth.0, &id).map_err(map_err)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn restore_file(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, wxlike_api::ApiError> {
    state.service.restore_file(&auth.0, &id).map_err(map_err)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn purge_file(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, wxlike_api::ApiError> {
    state.service.purge_file(&auth.0, &id).map_err(map_err)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ShareBody {
    #[serde(default)]
    ttl_ms: Option<i64>,
    #[serde(default)]
    audience: Option<Audience>,
}

async fn create_share(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    Path(id): Path<String>,
    body: Json<ShareBody>,
) -> std::result::Result<(StatusCode, Json<ShareGrant>), wxlike_api::ApiError> {
    let ttl = body.0.ttl_ms.unwrap_or(state.service.config().share_ttl_ms);
    let grant = state
        .service
        .create_share(&auth.0, &id, ttl, body.0.audience)
        .map_err(map_err)?;
    Ok((StatusCode::CREATED, Json(grant)))
}

async fn revoke_share(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, wxlike_api::ApiError> {
    state.service.revoke_share(&auth.0, &id).map_err(map_err)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct JobBody {
    object_id: String,
    preset: Preset,
}

async fn submit_job(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    body: Json<JobBody>,
) -> std::result::Result<(StatusCode, Json<crate::processing::JobView>), wxlike_api::ApiError> {
    let job = state
        .service
        .submit_job(&auth.0, &body.0.object_id, body.0.preset)
        .map_err(map_err)?;
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn job_status(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    Path(id): Path<String>,
) -> std::result::Result<Json<crate::processing::JobView>, wxlike_api::ApiError> {
    map(state.service.job_status(&auth.0, &id))
}

async fn cancel_job(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    Path(id): Path<String>,
) -> std::result::Result<Json<crate::processing::JobView>, wxlike_api::ApiError> {
    map(state.service.cancel_job(&auth.0, &id))
}

#[derive(Deserialize)]
struct ExportBody {
    device_id: String,
    #[serde(default)]
    encryption: Option<EncryptionMeta>,
}

async fn export_backup(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    body: Json<ExportBody>,
) -> std::result::Result<(StatusCode, Json<BackupSummary>), wxlike_api::ApiError> {
    let encryption = body.0.encryption.unwrap_or_else(EncryptionMeta::plaintext);
    let summary = state
        .service
        .export_backup(&auth.0, &body.0.device_id, encryption)
        .map_err(map_err)?;
    Ok((StatusCode::CREATED, Json(summary)))
}

async fn download_backup(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    Path(id): Path<String>,
) -> std::result::Result<Response, wxlike_api::ApiError> {
    let path = state
        .service
        .backup_package(&auth.0, &id)
        .map_err(map_err)?;
    let file = std::fs::File::open(path).map_err(|err| MediaError::Io(err).into_api())?;
    let stream = ReaderStream::new(tokio::fs::File::from_std(file));
    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static(NO_STORE));
    Ok(response.into_response())
}

#[derive(Deserialize)]
struct BackupObjectBody {
    object_id: String,
}

async fn verify_backup(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    body: Json<BackupObjectBody>,
) -> std::result::Result<Json<BackupSummary>, wxlike_api::ApiError> {
    map(state
        .service
        .verify_backup_object(&auth.0, &body.0.object_id))
}

#[derive(Deserialize)]
struct ImportBody {
    device_id: String,
    object_id: String,
}

async fn import_backup(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    body: Json<ImportBody>,
) -> std::result::Result<Json<ImportOutcome>, wxlike_api::ApiError> {
    map(state
        .service
        .import_backup_object(&auth.0, &body.0.device_id, &body.0.object_id))
}

#[derive(Deserialize)]
struct KeysBody {
    object_id: String,
}

async fn restore_backup_keys(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    Path(id): Path<String>,
    body: Json<KeysBody>,
) -> std::result::Result<Json<KeyRestore>, wxlike_api::ApiError> {
    let path = state
        .service
        .stage_backup_object(&auth.0, &body.0.object_id)
        .map_err(map_err)?;
    let result = state
        .service
        .restore_backup_keys(&auth.0, &id, &path)
        .map_err(map_err);
    std::fs::remove_file(&path).ok();
    result.map(|key_ref| Json(KeyRestore { key_ref }))
}

#[derive(Serialize)]
struct KeyRestore {
    key_ref: String,
}

#[derive(Deserialize)]
struct CachePlanBody {
    budget_bytes: u64,
    candidates: Vec<CacheCandidate>,
}

async fn cache_plan(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    body: Json<CachePlanBody>,
) -> std::result::Result<Json<CachePlan>, wxlike_api::ApiError> {
    map(state
        .service
        .plan_cache(&auth.0, body.0.budget_bytes, body.0.candidates))
}

#[derive(Serialize)]
struct CacheExecute {
    freed_bytes: u64,
}

async fn cache_execute(
    State(state): State<Arc<AppState>>,
    _auth: Principal,
    body: Json<CachePlan>,
) -> std::result::Result<Json<CacheExecute>, wxlike_api::ApiError> {
    map(state
        .service
        .execute_cache_plan(&body.0)
        .map(|freed_bytes| CacheExecute { freed_bytes }))
}

#[derive(Deserialize)]
struct PlatformQuery {
    platform: String,
}

async fn platform_capabilities(
    State(_state): State<Arc<AppState>>,
    _auth: Principal,
    Query(query): Query<PlatformQuery>,
) -> std::result::Result<Json<PlatformCapabilities>, wxlike_api::ApiError> {
    let kind = PlatformKind::parse(&query.platform).map_err(map_err)?;
    Ok(Json(kind.capabilities()))
}

#[derive(Deserialize)]
struct PlatformBody {
    platform: String,
    request: PlatformRequest,
}

async fn platform_request(
    State(state): State<Arc<AppState>>,
    auth: Principal,
    body: Json<PlatformBody>,
) -> std::result::Result<Json<PlatformResult>, wxlike_api::ApiError> {
    let kind = PlatformKind::parse(&body.0.platform).map_err(map_err)?;
    let result = state
        .broker
        .authorize(kind, &auth.0, &body.0.request)
        .map_err(map_err)?;
    Ok(Json(result))
}

#[derive(Serialize)]
struct Indicator {
    active: bool,
}

async fn audio_indicator(State(state): State<Arc<AppState>>, _auth: Principal) -> Json<Indicator> {
    Json(Indicator {
        active: state.broker.indicator_active(),
    })
}

/// Fixture map shared by the HTTP tests: token -> user id.
pub fn fixture_verifier(tokens: &[(&str, &str)]) -> Arc<dyn SessionVerifier> {
    let map: HashMap<String, String> = tokens
        .iter()
        .map(|(token, user)| ((*token).to_string(), (*user).to_string()))
        .collect();
    Arc::new(FnVerifier(move |headers: &HeaderMap| {
        let raw = headers.get("authorization")?.to_str().ok()?;
        let token = raw.strip_prefix("Bearer ")?;
        map.get(token).cloned()
    }))
}
