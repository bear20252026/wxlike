//! 错误类型与 HTTP 呈现。
//!
//! 与平台层同一条规矩：**给客户端的消息不含 SQL、路径、令牌片段**。
//! 内部细节留在日志里（tracing），面板看到的只有一句话 + 状态码。
//! 特别是 [`AdminError::Upstream`]：平台返回的原文可能带表名或栈信息，
//! 这里只把状态码转述给运维，正文进日志。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error("database failure")]
    Sqlite(#[from] rusqlite::Error),
    #[error("filesystem failure")]
    Io(#[from] std::io::Error),
    #[error("encoding failure")]
    Codec(#[from] serde_json::Error),
    /// 读面令牌缺失或不匹配。
    #[error("missing or invalid operator token")]
    Unauthorized,
    /// 会话令牌无效/过期。
    #[error("admin session expired")]
    SessionExpired,
    /// 权限不足。消息里必须带"该用哪个角色"（照 `crates/cmd::Deny` 的做法：
    /// 拒绝要可读，运维看完能自己纠正）。
    #[error("not permitted: {0}")]
    Forbidden(String),
    /// D3 的硬门：没有 MFA 就没有任何写操作。
    #[error("mfa required: {0}")]
    MfaRequired(String),
    #[error("not found")]
    NotFound,
    #[error("invalid input: {0}")]
    BadInput(String),
    #[error("account already exists")]
    AccountExists,
    /// 管理 API 限流（G14-05）。消息里带"多久后可重试"。
    #[error("rate limited: {0}")]
    RateLimited(String),
    /// 转发到平台失败（平台没起 / 令牌不对 / 工单状态非法）。
    /// 502：这条链路上 adminx 是客户端，平台才是上游。
    #[error("upstream platform failure")]
    Upstream { status: u16, detail: String },
}

impl AdminError {
    pub fn bad_input(msg: impl Into<String>) -> Self {
        Self::BadInput(msg.into())
    }

    pub fn upstream(status: u16, detail: impl Into<String>) -> Self {
        Self::Upstream {
            status,
            detail: detail.into(),
        }
    }

    fn parts(&self) -> (StatusCode, String) {
        match self {
            AdminError::Sqlite(_) => (StatusCode::INTERNAL_SERVER_ERROR, "database failure".into()),
            AdminError::Io(_) => (StatusCode::INTERNAL_SERVER_ERROR, "storage failure".into()),
            AdminError::Codec(_) => (StatusCode::INTERNAL_SERVER_ERROR, "encoding failure".into()),
            AdminError::Unauthorized => {
                (StatusCode::UNAUTHORIZED, "missing or invalid token".into())
            }
            AdminError::SessionExpired => (
                StatusCode::UNAUTHORIZED,
                "admin session expired, log in again".into(),
            ),
            AdminError::Forbidden(m) => (StatusCode::FORBIDDEN, format!("not permitted: {m}")),
            AdminError::MfaRequired(m) => (StatusCode::FORBIDDEN, format!("mfa required: {m}")),
            AdminError::NotFound => (StatusCode::NOT_FOUND, "not found".into()),
            AdminError::RateLimited(m) => (StatusCode::TOO_MANY_REQUESTS, m.clone()),
            AdminError::BadInput(m) => (StatusCode::UNPROCESSABLE_ENTITY, m.clone()),
            AdminError::AccountExists => (StatusCode::CONFLICT, "account already exists".into()),
            AdminError::Upstream { status, detail } => {
                // 409（非法流转）与 422（参数错）是运维真要看的结论，必须转述；
                // 其余（含平台的 401/403）只说明"上游拒绝了"，别把平台的内部消息透出去。
                let msg = match status {
                    404 => "upstream: not found",
                    409 => "upstream rejected: illegal state transition",
                    422 => "upstream rejected: invalid input",
                    401 | 403 => {
                        "upstream refused the admin token (check WXLIKE_ADMIN_PLATFORM_TOKEN)"
                    }
                    _ => "upstream failure",
                };
                (
                    StatusCode::BAD_GATEWAY,
                    format!("{msg} (platform {status}: {detail})"),
                )
            }
        }
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let (status, message) = self.parts();
        if status.is_server_error() {
            tracing::warn!(%status, %message, "admin request failed");
        }
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}

pub type Result<T> = std::result::Result<T, AdminError>;

/// 平台层错误的转述。
///
/// 只映射三类内部错误（SQL / IO / 编码），其余按"输入不对"转出去。
/// 这里**故意不用通配**把平台的新变体静默吞掉：平台加一个变体时这里编译失败，
/// 比"运维看到一个莫名其妙的 422"好修。
impl From<wxlike_platform::PlatformError> for AdminError {
    fn from(e: wxlike_platform::PlatformError) -> Self {
        use wxlike_platform::PlatformError as P;
        match e {
            P::Sqlite(r) => Self::Sqlite(r),
            P::Io(i) => Self::Io(i),
            P::Codec(c) => Self::Codec(c),
            other => Self::BadInput(other.to_string()),
        }
    }
}
