use std::fmt;

/// 公共域错误。HTTP 层按 `code()` 映射，不把内部细节或供应商原文透给客户端。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiscoveryError {
    InvalidArgument(&'static str),
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict(&'static str),
    Expired,
    RateLimited,
    Cancelled,
    DependencyUnavailable,
    BlockedExternal(&'static str),
}

impl DiscoveryError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidArgument(_) => "INVALID_ARGUMENT",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::Forbidden => "FORBIDDEN",
            Self::NotFound => "NOT_FOUND",
            Self::Conflict(_) => "CONFLICT",
            Self::Expired => "EXPIRED",
            Self::RateLimited => "RATE_LIMITED",
            Self::Cancelled => "CANCELLED",
            Self::DependencyUnavailable => "DEPENDENCY_UNAVAILABLE",
            Self::BlockedExternal(_) => "BLOCKED_EXTERNAL",
        }
    }
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgument(what) => write!(f, "invalid argument: {what}"),
            Self::Unauthorized => f.write_str("unauthorized"),
            Self::Forbidden => f.write_str("forbidden"),
            Self::NotFound => f.write_str("not found"),
            Self::Conflict(what) => write!(f, "conflict: {what}"),
            Self::Expired => f.write_str("expired"),
            Self::RateLimited => f.write_str("rate limited"),
            Self::Cancelled => f.write_str("cancelled"),
            Self::DependencyUnavailable => f.write_str("dependency unavailable"),
            Self::BlockedExternal(what) => write!(f, "blocked external: {what}"),
        }
    }
}

impl std::error::Error for DiscoveryError {}

pub type Result<T> = std::result::Result<T, DiscoveryError>;
