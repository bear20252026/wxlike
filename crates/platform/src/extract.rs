//! 请求体提取器：**反序列化失败也返回 `{"error": ...}`**。
//!
//! 为什么不用 axum 自带的 `Json<T>`：它在请求体不合法时返回的是**纯文本**：
//!
//! ```text
//! 422 Unprocessable Entity
//! Failed to deserialize the JSON body into the target type: ...
//! ```
//!
//! 而平台其它所有错误都是 `{"error": "..."}`。客户端按 `body["error"]` 取原因，
//! 遇到这类错误只能拿到 `null`，于是把"你少传了 duration_ms"显示成"未知错误"。
//! 错误信息再准，形状不对就等于没给。
//!
//! 这个提取器只做三件事：
//!
//! 1. 读原始字节（读不出来 → 400）；
//! 2. 空 body → 400，并说清"需要一个 JSON 对象"（而不是 serde 的 `EOF while parsing`）；
//! 3. `serde_json` 解析失败 → **422**，`{"error": "请求体不合法：<具体原因>"}`。
//!
//! 两点刻意的取舍：
//!
//! - **不看 `Content-Type`**。axum 的 `Json` 在缺 `Content-Type: application/json`
//!   时直接 415，而我们的客户端总会带；少一次 415 就少一类"明明能解析却被拒"的
//!   误伤。真正的坏数据会在解析时得到 422，原因更具体。
//! - **422 而不是 400**：语法对、内容不合法，与平台 `BadInput` 的口径一致
//!   （400 留给"请求体读不出来"这种传输层问题）。
//!
//! 用法与 `Json` 一样，只是把参数类型换掉：
//!
//! ```ignore
//! async fn create(ApiJson(body): ApiJson<CreateBody>) -> Result<Json<Value>, ApiError> { .. }
//! ```

use axum::extract::{FromRequest, Request};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;

use crate::error::ApiError;

/// 与 [`axum::Json`] 同形，但错误响应是 `{"error": ...}`。
#[derive(Debug, Clone, Copy)]
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    /// 直接返回 `Response`：拒绝的原因已经渲染成统一的 JSON 形状，
    /// 上层 handler 不需要再关心。
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = axum::body::Bytes::from_request(req, state)
            .await
            .map_err(|rejection| reject(StatusCode::BAD_REQUEST, rejection.to_string()))?;

        if bytes.is_empty() {
            return Err(reject(
                StatusCode::BAD_REQUEST,
                "请求体为空：这个接口需要一个 JSON 对象".to_string(),
            ));
        }

        // `serde_json` 自己的错误只说到"第几行第几列"，
        // `serde_path_to_error` 能补上**字段路径**（`items[0].id`），
        // 调用方才知道该改哪儿。它已经在依赖树里（axum 用它），不新增 crate。
        let mut deserializer = serde_json::Deserializer::from_slice(&bytes);
        let value = serde_path_to_error::deserialize::<_, T>(&mut deserializer).map_err(|err| {
            let path = err.path().to_string();
            let detail = err.into_inner();
            let at = match path.as_str() {
                "" | "." => String::new(),
                other => format!("（字段 {other}）"),
            };
            reject(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("请求体不合法{at}：{detail}"),
            )
        })?;
        Ok(ApiJson(value))
    }
}

fn reject(status: StatusCode, message: String) -> Response {
    ApiError::new(status, message).into_response()
}
