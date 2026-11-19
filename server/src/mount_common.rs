//! 域挂载的公共前置：会话鉴权 / JSON 解析 / 稳定错误形状。
//!
//! commerce/discovery/ai 三个挂载原本各自抄了一份同样的 actor_of /
//! err_response / parse_json——质量闸门的跨文件重复窗口把它们抓了出来。
//! 语义只有一份：**actor 只来自平台 sessions 表，不信请求体自报**；
//! 错误只回稳定分类字符串，不回显内部细节。
//!
//! ADR-28：`actor_of` 是同步函数——连接锁在里面拿、里面放，
//! async handler 调它时不持锁跨 await。

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use wxlike_platform::{Authed, PlatformState};

/// 从请求头解析可信 actor（平台 sessions 表）。失败 → `None` → 调用方回 401。
pub(crate) fn actor_of(platform: &PlatformState, headers: &HeaderMap) -> Option<String> {
    let token = Authed::bearer_from(headers)?;
    platform.lookup_token(token).ok().flatten()
}

/// 稳定错误形状：`{"error": "<分类>"}`。只回分类，不回显内部细节。
pub(crate) fn err_response(status: StatusCode, label: &str) -> Response {
    (status, Json(serde_json::json!({"error": label}))).into_response()
}

/// 请求体 JSON 解析：空体 = `{}`（动作面允许无参）；坏 JSON = `None` → 400。
pub(crate) fn parse_json(body: &Bytes) -> Option<serde_json::Value> {
    if body.is_empty() {
        return Some(serde_json::json!({}));
    }
    serde_json::from_slice(body).ok()
}

/// 会话 + JSON 的公共前置：一次拿齐 actor 与 payload，失败即回响应。
/// 三个挂载的 handler 开头不再各抄八行。
pub(crate) fn authed_json(
    platform: &PlatformState,
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<(String, serde_json::Value), Response> {
    let Some(actor) = actor_of(platform, headers) else {
        return Err(err_response(StatusCode::UNAUTHORIZED, "UNAUTHORIZED"));
    };
    let Some(payload) = parse_json(body) else {
        return Err(err_response(StatusCode::BAD_REQUEST, "BAD_INPUT"));
    };
    Ok((actor, payload))
}
