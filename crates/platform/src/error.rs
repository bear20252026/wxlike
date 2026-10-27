//! 平台错误：库层的 `PlatformError` + 它的 HTTP 映射。
//!
//! **HTTP 错误契约本身不在这里**——`ApiError` / `ApiJson` 已提到
//! `wxlike-api`，因为 `wxlike-video` 也要用同一份形状。这个模块只保留
//! 两件平台专属的事：
//!
//! 1. `PlatformError`：库层错误枚举（带 SQLite / IO 来源，供 `?` 传播）；
//! 2. `From<PlatformError> for ApiError`：**脱敏映射**——SQL 与 IO 的细节
//!    留在服务端日志里，客户端只看到 "database failure" 这种概括。

use axum::http::StatusCode;

/// 错误契约的唯一实现，这里只做转发，保持 `wxlike_platform::error::ApiError`
/// 这个路径对既有调用点有效。
pub use wxlike_api::ApiError;

pub type Result<T> = std::result::Result<T, PlatformError>;

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("database failure")]
    Sqlite(#[from] rusqlite::Error),
    #[error("filesystem failure")]
    Io(#[from] std::io::Error),
    #[error("json codec failure")]
    Codec(#[from] serde_json::Error),
    #[error("invalid input: {0}")]
    BadInput(&'static str),
    #[error("not found")]
    NotFound,
    #[error("mailbox failure: {0}")]
    Mail(String),
    #[error("already exists: {0}")]
    Conflict(String),
    #[error("missing or invalid credentials")]
    Unauthorized,
    #[error("not permitted")]
    Forbidden,
    #[error("payload too large")]
    PayloadTooLarge,
    /// P4 风控限流/拒绝（429）。
    #[error("too many requests")]
    RateLimited { retry_after_ms: u64 },
    /// P4 需要人机校验（428）。
    #[error("human verification required")]
    CaptchaRequired,
    /// 管理面未启用或令牌错误（403）。
    #[error("admin endpoint unavailable")]
    AdminRequired,
    /// 内部依赖不可用（如风控引擎锁中毒）→ 500，不伪装成客户端错误。
    #[error("internal dependency unavailable: {0}")]
    Internal(String),
    /// 状态机拒绝：请求本身没毛病，但**当前状态下不允许**（409）。
    ///
    /// 与 `Conflict` 的区别：`Conflict` 的文案是"already exists: X"（创建撞了
    /// 唯一键），注销/清理这类"现在不行"要的是原样说清楚为什么（#114）。
    /// 例如 `Blocked("wallet not empty")` 会原样进作业失败原因，用户看到的是
    /// 真正拦下清理的那句话。
    #[error("{0}")]
    Blocked(&'static str),
}

impl From<wxlike_risk::RiskError> for PlatformError {
    fn from(e: wxlike_risk::RiskError) -> Self {
        Self::Internal(e.to_string())
    }
}

impl From<PlatformError> for ApiError {
    fn from(e: PlatformError) -> Self {
        // Client-safe messages only: SQL and IO details stay in the server log.
        match e {
            PlatformError::Sqlite(_) => {
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, "database failure")
            }
            PlatformError::Io(_) => Self::new(StatusCode::INTERNAL_SERVER_ERROR, "storage failure"),
            PlatformError::Codec(_) => {
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, "encoding failure")
            }
            PlatformError::BadInput(m) => Self::new(StatusCode::UNPROCESSABLE_ENTITY, m),
            PlatformError::NotFound => Self::new(StatusCode::NOT_FOUND, "not found"),
            PlatformError::Conflict(reason) => {
                Self::new(StatusCode::CONFLICT, format!("already exists: {reason}"))
            }
            PlatformError::Unauthorized => Self::new(StatusCode::UNAUTHORIZED, "unauthorized"),
            PlatformError::Forbidden => Self::new(StatusCode::FORBIDDEN, "not permitted"),
            PlatformError::Mail(msg) => {
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, format!("mailbox: {msg}"))
            }
            PlatformError::PayloadTooLarge => {
                Self::new(StatusCode::PAYLOAD_TOO_LARGE, "media too large")
            }
            PlatformError::RateLimited { retry_after_ms } => Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                format!("rate limited, retry after {retry_after_ms} ms"),
            ),
            PlatformError::CaptchaRequired => Self::new(
                StatusCode::PRECONDITION_REQUIRED,
                "human verification required",
            ),
            PlatformError::AdminRequired => {
                Self::new(StatusCode::FORBIDDEN, "admin endpoint unavailable")
            }
            PlatformError::Internal(msg) => Self::new(StatusCode::INTERNAL_SERVER_ERROR, msg),
            // 状态不允许：409，消息就是拦下这次操作的那句话（不加工、不脱敏，
            // 因为它来自上面的封闭字符串常量，不含用户数据）。
            PlatformError::Blocked(what) => Self::new(StatusCode::CONFLICT, what),
        }
    }
}
