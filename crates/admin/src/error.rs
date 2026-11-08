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
    /// 准入这一步跑不下去了（blocking 池里的任务崩掉或被取消）。
    ///
    /// **fail closed**：认证结论不可信时一律拒绝，绝不"当没这回事继续往下走"。
    /// 细节只进日志——"后台的准入逻辑坏了"本身就是不该对外披露的事实。
    #[error("admin admission failure")]
    Admission,
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
            AdminError::Admission => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "admin admission failure".into(),
            ),
            AdminError::Upstream { status, detail } => {
                // 平台的原文可能带表名、列名甚至栈信息。只有一类原文是运维真要看的
                // 结论（404 没有这个对象 / 409 非法流转 / 422 参数错），那三种转述；
                // 其余（平台的 401/403、5xx、3xx）只转述**状态码**，原文进日志。
                // 文件头那条规矩在这里是真的被执行，不是只写着好看。
                let (label, echo) = match status {
                    404 => ("upstream: not found", true),
                    409 => ("upstream rejected: illegal state transition", true),
                    422 => ("upstream rejected: invalid input", true),
                    401 | 403 => (
                        "upstream refused the admin token (check WXLIKE_ADMIN_PLATFORM_TOKEN)",
                        false,
                    ),
                    _ => ("upstream failure", false),
                };
                if echo {
                    (
                        StatusCode::BAD_GATEWAY,
                        format!("{label} (platform {status}: {detail})"),
                    )
                } else {
                    tracing::warn!(
                        platform_status = status,
                        %detail,
                        "平台管理端返回了原文，已转述为状态码（不透给面板）"
                    );
                    (
                        StatusCode::BAD_GATEWAY,
                        format!("{label} (platform {status})"),
                    )
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 把错误渲染成**实际发出去的字节**。这里用 `expect` 是测试自己的前提条件，
    /// 不是生产路径：要钉住的是"对方收到了什么"。
    async fn render(err: AdminError) -> (StatusCode, String) {
        let resp = err.into_response();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .expect("响应体必须收得完");
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// [`AdminError::Admission`] 说的是"后台自己的准入逻辑坏了"。
    /// 它对外必须是一句没有信息量的 500：哪个线程池、什么 panic、哪张表，
    /// 都只进日志——内部故障的细节对外就是探针素材。
    #[tokio::test]
    async fn admission_failure_is_a_bare_500() {
        let (status, body) = render(AdminError::Admission).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body, r#"{"error":"admin admission failure"}"#);
        for leak in [
            "panic",
            "JoinError",
            "spawn_blocking",
            "SELECT",
            "adminx.db",
            "token",
        ] {
            assert!(
                !body.contains(leak),
                "{leak:?} 不该出现在 500 响应里：{body}"
            );
        }
    }

    #[tokio::test]
    async fn missing_credentials_and_wrong_audience_are_different_codes() {
        // 401 = 没带或认不出；403 = 带对了但不是这扇门的人
        assert_eq!(
            render(AdminError::Unauthorized).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            render(AdminError::SessionExpired).await.0,
            StatusCode::UNAUTHORIZED
        );
        let (status, body) = render(AdminError::Forbidden("走错门".into())).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.contains("走错门"), "{body}");
        let (status, body) = render(AdminError::MfaRequired("需要动态码".into())).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.starts_with(r#"{"error":"mfa required: "#), "{body}");
    }

    #[tokio::test]
    async fn internal_failures_never_quote_the_underlying_text() {
        let db = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(1),
            Some(r#"near "UPDATE": syntax error at adminx.db"#.into()),
        );
        let (status, body) = render(AdminError::Sqlite(db)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body, r#"{"error":"database failure"}"#);
        assert!(!body.contains("UPDATE"), "{body}");

        // 上游的 409/422 就是运维要看的结论，必须转述；
        // 其余（含平台的 401/403）只说"上游拒了"，不把平台的内部话透出去。
        let (status, body) = render(AdminError::upstream(409, "工单状态非法")).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(body.contains("illegal state transition"), "{body}");
        let (status, body) = render(AdminError::upstream(401, "admin_accounts 表没有该行")).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(body.contains("refused the admin token"), "{body}");
        assert!(
            !body.contains("admin_accounts"),
            "平台内部事实不许转出：{body}"
        );
    }
}
