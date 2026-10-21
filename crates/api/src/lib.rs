//! 全仓库统一的 HTTP 错误契约：**一律 `{"error": "..."}`**。
//!
//! 为什么值得单独一个 crate：这份契约原来只存在于 `wxlike-platform`，
//! 于是 `wxlike-video` 自己用 `(StatusCode, String)` 返回**纯文本**——
//! 同一个服务端进程，两套错误形状。客户端按 `body["error"]` 取原因，
//! 遇到视频接口的错误只能拿到 `null`，把"这段视频还没转完"显示成"未知错误"。
//!
//! 契约本身只有两条，但两条都必须由**一份代码**保证：
//!
//! 1. 任何非 2xx 响应体都是 `{"error": "<一句给人看的原因>"}`；
//! 2. 请求体解析失败也是这个形状（[`ApiJson`]），并且要说出**哪个字段**错了。
//!
//! 刻意不放进来的东西：业务错误枚举（`PlatformError` / `JobFailure` 之类）。
//! 那些属于各自领域，这个 crate 只管"怎么把错误讲给 HTTP 客户端听"。
//!
//! # 关于 `message`
//!
//! [`ApiError::message`] 会**原样回显**给客户端。所以：
//!
//! - 不要塞 SQL、文件路径、密钥、内部类型名；
//! - 需要保留内部细节时，先 `tracing::error!` 记全量，再给客户端一句概括。
//!
//! 目前 `wxlike-video` 的 500 分支仍是 `format!("{e:#}")`（anyhow 链，
//! 可能含路径）。这是**已知的待收紧项**，记在 `docs/api.md` 的"已知偏差"里；
//! 收紧它要逐个判断哪些消息对调用方有意义，不适合夹在契约统一里做。

use axum::extract::{FromRequest, Request};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;

/// HTTP 可见错误：状态码 + 一句给客户端看的原因。
///
/// 与 axum 的 `(StatusCode, String)` 的区别只有一个，但很关键：
/// 后者渲染成**纯文本**，前者渲染成 `{"error": ...}`。
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

/// 处理函数的常见返回类型。名字短，是因为它出现在几乎每个 handler 的签名里。
pub type ApiResult<T> = std::result::Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    /// 400：请求读不出来、参数缺失这类"还没走到业务"的问题。
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    /// 422：语法对、内容不合法（与 `PlatformError::BadInput` 同口径）。
    pub fn unprocessable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, message)
    }

    pub fn payload_too_large(message: impl Into<String>) -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, message)
    }

    /// 503：依赖暂时不可用，稍后重试可能成功。
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.status.as_u16(), self.message)
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            axum::Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

/// 与 [`axum::Json`] 同形，但**反序列化失败也返回 `{"error": ...}`**。
///
/// 为什么不用 axum 自带的 `Json<T>`：它在请求体不合法时返回的是**纯文本**：
///
/// ```text
/// 422 Unprocessable Entity
/// Failed to deserialize the JSON body into the target type: ...
/// ```
///
/// 客户端按 `body["error"]` 取原因，遇到这类错误只能拿到 `null`，
/// 于是把"你少传了 duration_ms"显示成"未知错误"。错误信息再准，
/// 形状不对就等于没给。
///
/// 这个提取器只做三件事：
///
/// 1. 读原始字节（读不出来 → 400）；
/// 2. 空 body → 400，并说清"需要一个 JSON 对象"（而不是 serde 的 `EOF while parsing`）；
/// 3. 解析失败 → **422**，`{"error": "请求体不合法（字段 x）：<具体原因>"}`。
///
/// 两点刻意的取舍：
///
/// - **不看 `Content-Type`**：axum 的 `Json` 在缺 `Content-Type: application/json`
///   时直接 415，而我们的客户端总会带；少一次 415 就少一类"明明能解析却被拒"的
///   误伤。真正的坏数据会在解析时得到 422，原因更具体。
/// - **422 而不是 400**：语法对、内容不合法，与 `BadInput` 的口径一致
///   （400 留给"请求体读不出来"这种传输层问题）。
///
/// 用法与 `Json` 一样，只是把参数类型换掉：
///
/// ```ignore
/// async fn create(ApiJson(body): ApiJson<CreateBody>) -> ApiResult<Json<Value>> { .. }
/// ```
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
        // 调用方才知道该改哪儿。
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 契约的**唯一硬断言**：错误体必须是 `{"error": "..."}`，且只有这一个键。
    ///
    /// 用 `into_response()` 直接验，不经过 HTTP：这条测试要挡的是
    /// "有人换回 `(StatusCode, String)`"，那种改动会渲染成纯文本。
    #[tokio::test]
    async fn every_constructor_renders_the_same_json_shape() {
        let cases = [
            ApiError::bad_request("缺参数"),
            ApiError::unauthorized("令牌无效"),
            ApiError::forbidden("不允许"),
            ApiError::not_found("没有这条"),
            ApiError::conflict("已经在跑"),
            ApiError::unprocessable("字段不合法"),
            ApiError::payload_too_large("太大"),
            ApiError::unavailable("稍后再试"),
            ApiError::internal("炸了"),
        ];
        for err in cases {
            let expected_status = err.status;
            let expected_message = err.message.clone();
            let resp = err.into_response();
            assert_eq!(resp.status(), expected_status);

            let bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes)
                .expect("错误体必须是 JSON —— 纯文本就是这次要消灭的东西");
            assert_eq!(
                body,
                serde_json::json!({ "error": expected_message }),
                "只许有一个 error 键"
            );
        }
    }

    /// 状态码不能靠"猜"：构造器名与码必须一一对应。
    #[test]
    fn constructors_map_to_the_expected_status_codes() {
        assert_eq!(ApiError::bad_request("").status, StatusCode::BAD_REQUEST);
        assert_eq!(
            ApiError::unprocessable("").status,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(ApiError::not_found("").status, StatusCode::NOT_FOUND);
        assert_eq!(ApiError::conflict("").status, StatusCode::CONFLICT);
        assert_eq!(
            ApiError::payload_too_large("").status,
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(
            ApiError::unavailable("").status,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
