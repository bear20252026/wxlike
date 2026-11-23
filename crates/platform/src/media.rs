//! **handler 层**（`media`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::{offload, offload_api};
use crate::error::{ApiError, Result};
use crate::PlatformState;
use axum::extract::multipart::{Field, Multipart};
use axum::extract::{DefaultBodyLimit, Path as ApiPath, Query, State};
use axum::http::header;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::domain::media::{
    authorize_download, parse_range, read_field, DownloadQuery, MediaStore, SignedUrl,
    StoredObject, UploadReply, MAX_MEDIA_BYTES, MEDIA_URL_DEFAULT_TTL_SECS, MEDIA_URL_MAX_TTL_SECS,
};

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
        let actor = authed.user_id.clone();
        let conv = conv.to_string();
        let allowed = offload(&platform, move |p| Ok(p.may_read_conv(&actor, &conv))).await?;
        if !allowed {
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
    let actor = authed.user_id.clone();
    let media_id_for_task = media_id.clone();
    let ttl = q
        .ttl_secs
        .unwrap_or(MEDIA_URL_DEFAULT_TTL_SECS)
        .clamp(1, MEDIA_URL_MAX_TTL_SECS);
    let (url, expires_at) = offload_api(&platform, move |p| {
        if !p.may_read_media(&media_id_for_task, &actor)? {
            return Err(ApiError::not_found("no such media"));
        }
        Ok(p.sign_media_url(&media_id_for_task, ttl.saturating_mul(1_000)))
    })
    .await?;
    Ok(Json(SignedUrl { url, expires_at }))
}

#[derive(serde::Deserialize, Default)]
pub struct UrlQuery {
    #[serde(default)]
    ttl_secs: Option<u64>,
}

async fn download(
    State(platform): State<PlatformState>,
    ApiPath(media_id): ApiPath<String>,
    headers: axum::http::HeaderMap,
    Query(q): Query<DownloadQuery>,
) -> Response {
    // **先授权再碰路径**（issue #14）。
    let want_thumb = q.thumb.unwrap_or(0) == 1;
    let auth_id = media_id.clone();
    let auth_headers = headers.clone();
    let rejected = offload(&platform, move |p| {
        Ok(authorize_download(p, &auth_id, &auth_headers, &q))
    })
    .await;
    match rejected {
        Ok(Some(rejected)) => return rejected,
        Ok(None) => {}
        Err(err) => return ApiError::from(err).into_response(),
    }

    // An unknown or malformed id must be a 404 before any path is touched.
    let store = match offload(&platform, |p| Ok(p.media_store())).await {
        Ok(store) => store,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let object = match resolve(&platform, &store, &media_id, want_thumb).await {
        Ok(Some(o)) => o,
        Ok(None) => return not_found_media(),
        Err(err) => return ApiError::from(err).into_response(),
    };

    let total = object.total;
    let (start, end) = match requested_range(
        headers.get(header::RANGE).and_then(|v| v.to_str().ok()),
        headers.contains_key(header::RANGE),
        total,
    ) {
        Ok(pair) => pair,
        Err(response) => return response,
    };

    // this handler answers with a bare `Response`, so errors are mapped inline.
    //
    // **总是按 Range 取**：`resolve` 只给了长度与类型（issue #14）。
    // 缩略图很小、且存在另一条路径里，单独取。
    let bytes = match load_media_bytes(&store, &media_id, want_thumb, start, end).await {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return not_found_media(),
        Err(err) => return ApiError::from(err).into_response(),
    };

    media_response(object, bytes, start, end, total)
}

/// 解析 `Range` 并为不可满足的范围生成 416。没有 Range 头时退化为完整对象。
fn requested_range(
    raw: Option<&str>,
    has_range: bool,
    total: u64,
) -> std::result::Result<(u64, u64), Response> {
    match parse_range(raw, total) {
        Some(pair) => Ok(pair),
        None if has_range => Err((
            StatusCode::RANGE_NOT_SATISFIABLE,
            [(header::CONTENT_RANGE, format!("bytes */{total}"))],
        )
            .into_response()),
        None => Ok((0, total.saturating_sub(1))),
    }
}

/// 按缩略图/原图分支从媒体仓取一段字节；`None` 统一交给 404。
async fn load_media_bytes(
    store: &std::sync::Arc<dyn MediaStore>,
    media_id: &str,
    want_thumb: bool,
    start: u64,
    end: u64,
) -> Result<Option<Vec<u8>>> {
    if want_thumb {
        store.get_thumb(media_id).await
    } else {
        store.get(media_id, start, end).await
    }
}

fn not_found_media() -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"error":"no such media"}"#,
    )
        .into_response()
}

/// 组装 200/206 响应；`Content-Range` 只在部分响应时出现。
fn media_response(
    object: StoredObject,
    bytes: Vec<u8>,
    start: u64,
    end: u64,
    total: u64,
) -> Response {
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

async fn read_text(field: Field<'_>) -> std::result::Result<String, ApiError> {
    Ok(String::from_utf8_lossy(&read_field(field).await?).into_owned())
}

/// Reads the object (or its thumbnail) through the store.
async fn resolve(
    platform: &PlatformState,
    store: &std::sync::Arc<dyn MediaStore>,
    media_id: &str,
    want_thumb: bool,
) -> Result<Option<StoredObject>> {
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
    let id = media_id.to_string();
    let meta = offload(platform, move |p| p.media_meta(&id)).await?;
    let Some((total, content_type, _has_thumb)) = meta else {
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
