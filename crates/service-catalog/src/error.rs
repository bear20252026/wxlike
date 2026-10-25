use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceErrorCode {
    InvalidArgument,
    Unauthorized,
    NotFound,
    Conflict,
    Expired,
    RateLimited,
    Cancelled,
    Timeout,
    DependencyUnavailable,
    BlockedExternal,
    PaymentDeclined,
    Internal,
}

impl ServiceErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument => "INVALID_ARGUMENT",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::NotFound => "NOT_FOUND",
            Self::Conflict => "CONFLICT",
            Self::Expired => "EXPIRED",
            Self::RateLimited => "RATE_LIMITED",
            Self::Cancelled => "CANCELLED",
            Self::Timeout => "TIMEOUT",
            Self::DependencyUnavailable => "DEPENDENCY_UNAVAILABLE",
            Self::BlockedExternal => "BLOCKED_EXTERNAL",
            Self::PaymentDeclined => "PAYMENT_DECLINED",
            Self::Internal => "INTERNAL",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceError {
    code: ServiceErrorCode,
    detail: String,
}

impl ServiceError {
    pub fn new(code: ServiceErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }

    pub fn code(&self) -> ServiceErrorCode {
        self.code
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.detail)
    }
}

impl std::error::Error for ServiceError {}

pub type Result<T> = std::result::Result<T, ServiceError>;
