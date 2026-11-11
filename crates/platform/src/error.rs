//! 平台错误：库层的 `PlatformError` + 它的领域映射。
//!
//! **HTTP 错误契约本身不在这里**——`ApiError` / `ApiJson` 已提到
//! `wxlike-api`，因为 `wxlike-video` 也要用同一份形状。这个模块保留
//! 三件平台专属的事：
//!
//! 1. `PlatformError`：库层错误枚举（带 SQLite / IO 来源，供 `?` 传播）；
//! 2. `From<PlatformError> for DomainError`：**语义映射**——服务端说的
//!    话与客户端听到的话必须一致，这是唯一保证；
//! 3. `From<PlatformError> for ApiError`：把领域错误渲染成 HTTP 形状，
//!    **带 `code`**，客户端按 code 精确分流。
//!
//! # 为什么需要 `From<PlatformError> for DomainError`
//!
//! 以前服务端→客户端是两段手工映射：`PlatformError` → `ApiError`（状态码
//! + 消息）→ 客户端 `from_http_status`（按状态码猜）。两段都会丢语义：
//!
//! * `PayloadTooLarge`(413) 与 `CaptchaRequired`(428) 在 `from_http_status`
//!   里落到 `_ => Unavailable`，于是客户端 `is_retryable()` 为真，把"文件
//!   太大"和"需要人机验证"重试 5 次；
//! * `Blocked("wallet not empty")` 与 `Conflict("already exists")` 都是
//!   409，客户端分不出"撞了唯一键"与"当前状态不允许"。
//!
//! 现在服务端直接把 `PlatformError` 映射成 `DomainError`，把 `code` 放进
//! HTTP body，客户端不再猜。`from_http_status` 只作老客户端的兜底。

use axum::http::StatusCode;

use wxlike_types::DomainError;

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
    /// 依赖暂时不可用（存储/下游挂了）→ 503。
    ///
    /// 与 [`PlatformError::Internal`] 的区别：`Internal` 是"我们自己的代码
    /// 出了问题"，`Unavailable` 是"外部依赖暂时连不上"。后者**可重试**，
    /// 客户端的重试按钮要亮；前者不重试，交给上层熔断。
    #[error("dependency unavailable: {0}")]
    Unavailable(&'static str),
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

impl PlatformError {
    /// HTTP 状态码。
    ///
    /// 与语义映射（`From<PlatformError> for DomainError`）是**两张表**，刻意
    /// 分开：状态码是 HTTP 关注点（403 与 409 谁是谁、422 还是 400），
    /// 语义分类是三端契约。两张表都对 `PlatformError` 做**全匹配**——
    /// 加一个变体，编译器会在这两处一起报 non-exhaustive，
    /// 这正是"语义不断层"能长期成立的机制。
    #[must_use]
    pub fn status(&self) -> StatusCode {
        match self {
            // 存储/编码/邮箱/自身代码：都是 5xx。客户端不重试（见
            // `DomainError::is_retryable`），交给上层熔断。
            PlatformError::Sqlite(_)
            | PlatformError::Io(_)
            | PlatformError::Codec(_)
            | PlatformError::Mail(_)
            | PlatformError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            PlatformError::BadInput(_) => StatusCode::UNPROCESSABLE_ENTITY,
            PlatformError::NotFound => StatusCode::NOT_FOUND,
            // 409 有两种语义（撞唯一键 / 当前状态不允许），状态码分不开，
            // 靠 `code` 分。
            PlatformError::Conflict(_) | PlatformError::Blocked(_) => StatusCode::CONFLICT,
            PlatformError::Unauthorized => StatusCode::UNAUTHORIZED,
            // 403 也有两种：登录了但没权限 / 管理面未启用。
            PlatformError::Forbidden | PlatformError::AdminRequired => StatusCode::FORBIDDEN,
            PlatformError::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            PlatformError::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            PlatformError::CaptchaRequired => StatusCode::PRECONDITION_REQUIRED,
            PlatformError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

/// 语义映射：**服务端说的话与客户端听到的话必须一致**。
///
/// 这张表只做两件事：分类（`DomainError` 的哪个变体）与脱敏（底层 SQL / IO
/// 文本在这里换成固定措辞，只进服务端日志）。文案与 [`ApiError`] 改造前
/// 逐字一致——这是一次纯管道改造，不额外改对外行为。
///
/// 与 [`PlatformError::status`] 的分工见那边的文档。
impl From<PlatformError> for DomainError {
    fn from(e: PlatformError) -> Self {
        match e {
            PlatformError::Sqlite(_) => DomainError::internal("database failure"),
            PlatformError::Io(_) => DomainError::internal("storage failure"),
            PlatformError::Codec(_) => DomainError::internal("encoding failure"),
            PlatformError::BadInput(m) => DomainError::invalid(m),
            PlatformError::NotFound => DomainError::not_found("not found"),
            PlatformError::Mail(msg) => DomainError::internal(format!("mailbox: {msg}")),
            PlatformError::Conflict(reason) => {
                DomainError::conflict(format!("already exists: {reason}"))
            }
            PlatformError::Unauthorized => DomainError::unauthorized("unauthorized"),
            PlatformError::Forbidden => DomainError::forbidden("not permitted"),
            // 413 / 428 必须落到**不可重试**的变体（`is_retryable()` 为假）：
            // 同样的请求体再发一次还是太大，人机验证也不会因为重发而消失。
            PlatformError::PayloadTooLarge => DomainError::payload_too_large("media too large"),
            PlatformError::RateLimited { retry_after_ms } => DomainError::RateLimited {
                message: format!("rate limited, retry after {retry_after_ms} ms"),
                retry_after_ms,
            },
            PlatformError::CaptchaRequired => DomainError::Precondition {
                message: "human verification required".to_string(),
            },
            PlatformError::AdminRequired => DomainError::forbidden("admin endpoint unavailable"),
            PlatformError::Internal(msg) => DomainError::internal(msg),
            // 503：依赖暂时连不上。可重试——与 `Internal` 的分工见变体文档。
            PlatformError::Unavailable(what) => DomainError::unavailable(what),
            // 409 的状态机拒绝：文案原样透出（它来自封闭字符串常量）。
            PlatformError::Blocked(what) => DomainError::blocked(what),
        }
    }
}

impl From<PlatformError> for ApiError {
    fn from(e: PlatformError) -> Self {
        // 先取状态码（它要按变体分辨 403 / 409 与 422），再取语义。
        let status = e.status();
        let domain = DomainError::from(e);
        let code = domain.code();
        Self::with_code(status, domain.message().to_owned(), code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxlike_types::error::CODES;

    /// 每个变体**手写**的预期领域码。这是回归网（不是从实现里抄的）：
    /// 语义一旦漂移，这里就红。少一个变体不会编译过——上面两个全匹配会提醒。
    fn expected_codes() -> Vec<(PlatformError, &'static str)> {
        vec![
            (
                PlatformError::Sqlite(rusqlite::Error::QueryReturnedNoRows),
                "internal",
            ),
            (PlatformError::Io(std::io::Error::other("disk")), "internal"),
            (
                PlatformError::Codec(serde_json::from_str::<serde_json::Value>("{").unwrap_err()),
                "internal",
            ),
            (PlatformError::BadInput("bad"), "invalid_argument"),
            (PlatformError::NotFound, "not_found"),
            (PlatformError::Mail("down".into()), "internal"),
            (PlatformError::Conflict("dup".into()), "conflict"),
            (PlatformError::Unauthorized, "unauthorized"),
            (PlatformError::Forbidden, "forbidden"),
            (PlatformError::PayloadTooLarge, "payload_too_large"),
            (
                PlatformError::RateLimited { retry_after_ms: 5 },
                "rate_limited",
            ),
            (PlatformError::CaptchaRequired, "precondition"),
            (PlatformError::AdminRequired, "forbidden"),
            (PlatformError::Internal("boom".into()), "internal"),
            (PlatformError::Unavailable("storage"), "unavailable"),
            (PlatformError::Blocked("wallet not empty"), "blocked"),
        ]
    }

    #[test]
    fn every_variant_maps_to_its_expected_code() {
        for (e, expected) in expected_codes() {
            let domain = DomainError::from(e);
            assert_eq!(domain.code(), expected, "语义映射漂移");
            assert!(CODES.contains(&domain.code()), "{expected} 不在 CODES 里");
        }
    }

    /// HTTP body 的 `code` 与领域映射必须同源——两处漂移等于没打通。
    #[test]
    fn api_errors_carry_the_same_code_as_the_domain_mapping() {
        for (e, expected) in expected_codes() {
            assert_eq!(
                ApiError::from(e).code,
                Some(expected),
                "HTTP code 与领域码漂移"
            );
        }
    }

    /// 409 的两种语义必须能被客户端分开：状态码做不到，`code` 做得到。
    #[test]
    fn conflict_and_blocked_are_distinguishable_by_code() {
        let conflict = ApiError::from(PlatformError::Conflict("dup".into()));
        let blocked = ApiError::from(PlatformError::Blocked("wallet not empty"));
        assert_eq!(conflict.status, StatusCode::CONFLICT);
        assert_eq!(blocked.status, StatusCode::CONFLICT);
        assert_eq!(conflict.code, Some("conflict"));
        assert_eq!(blocked.code, Some("blocked"));
    }

    /// 413 / 428 / 503 的精确码与可重试性——修掉"文件太大重试 5 次"那个 bug。
    #[test]
    fn size_and_captcha_are_precise_and_not_retried() {
        let too_large = ApiError::from(PlatformError::PayloadTooLarge);
        assert_eq!(too_large.status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(too_large.code, Some("payload_too_large"));
        assert!(!DomainError::payload_too_large("x").is_retryable());

        let captcha = ApiError::from(PlatformError::CaptchaRequired);
        assert_eq!(captcha.status, StatusCode::PRECONDITION_REQUIRED);
        assert_eq!(captcha.code, Some("precondition"));
        assert!(!DomainError::Precondition {
            message: "x".into()
        }
        .is_retryable());

        let down = ApiError::from(PlatformError::Unavailable("storage"));
        assert_eq!(down.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(down.code, Some("unavailable"));
        assert!(DomainError::unavailable("x").is_retryable());
    }

    /// 503 与 500 的分工：依赖挂了可重试，自身代码出错不可重试。
    #[test]
    fn unavailable_is_503_while_internal_stays_500() {
        assert_eq!(
            ApiError::from(PlatformError::Unavailable("storage")).status,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            ApiError::from(PlatformError::Internal("boom".into())).status,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert!(!DomainError::internal("x").is_retryable());
    }

    /// 脱敏：底层 SQL / IO 文本不进 HTTP body，只进服务端日志。
    #[test]
    fn storage_and_io_details_stay_server_side() {
        let sqlite = ApiError::from(PlatformError::Sqlite(rusqlite::Error::QueryReturnedNoRows));
        assert_eq!(sqlite.message, "database failure");
        let io = ApiError::from(PlatformError::Io(std::io::Error::other("/etc/passwd")));
        assert_eq!(io.message, "storage failure");
        let codec = ApiError::from(PlatformError::Codec(
            serde_json::from_str::<serde_json::Value>("{").unwrap_err(),
        ));
        assert_eq!(codec.message, "encoding failure");
        assert_eq!(
            ApiError::from(PlatformError::Mail("smtp down".into())).message,
            "mailbox: smtp down"
        );
    }
}
