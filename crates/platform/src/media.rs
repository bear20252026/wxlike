//! **handler 层**（`media`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::error::{ApiError, Result};
use crate::{Platform, PlatformState};
use axum::extract::multipart::{Field, Multipart};
use axum::extract::{DefaultBodyLimit, Path as ApiPath, Query, State};
use axum::http::header;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::domain::media::{
    authorize_download, parse_range, read_field, DownloadQuery, SignedUrl, StoredObject,
    UploadReply, MAX_MEDIA_BYTES, MEDIA_URL_DEFAULT_TTL_SECS, MEDIA_URL_MAX_TTL_SECS,
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

async fn read_text(field: Field<'_>) -> std::result::Result<String, ApiError> {
    Ok(String::from_utf8_lossy(&read_field(field).await?).into_owned())
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
