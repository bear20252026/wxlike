//! Domain error with an explicit HTTP projection.
//!
//! Internal details (SQLite, filesystem paths) are logged at the edge and are
//! never copied into the client-facing `ApiError` message.

use axum::http::StatusCode;

pub type Result<T> = std::result::Result<T, MediaError>;

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("database failure")]
    Sqlite(#[from] rusqlite::Error),
    #[error("filesystem failure")]
    Io(#[from] std::io::Error),
    #[error("codec failure")]
    Codec(#[from] serde_json::Error),
    #[error("invalid input: {0}")]
    BadInput(String),
    #[error("not found")]
    NotFound,
    #[error("not permitted")]
    Forbidden,
    #[error("unauthenticated")]
    Unauthorized,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("gone: {0}")]
    Gone(String),
    #[error("payload too large")]
    PayloadTooLarge,
    #[error("quota exceeded")]
    QuotaExceeded,
    #[error("unsupported media: {0}")]
    Unsupported(String),
    #[error("budget exceeded: {0}")]
    BudgetExceeded(String),
    #[error("canceled")]
    Canceled,
    #[error("integrity check failed: {0}")]
    Tampered(String),
    #[error("rate limited")]
    RateLimited { retry_after_ms: u64 },
    #[error("internal failure: {0}")]
    Internal(String),
}

impl MediaError {
    pub fn bad(message: impl Into<String>) -> Self {
        Self::BadInput(message.into())
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict(message.into())
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::BadInput(_) | Self::Unsupported(_) | Self::BudgetExceeded(_) => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Conflict(_) | Self::Canceled => StatusCode::CONFLICT,
            Self::Gone(_) => StatusCode::GONE,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::QuotaExceeded | Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::Tampered(_) => StatusCode::BAD_REQUEST,
            Self::Sqlite(_) | Self::Io(_) | Self::Codec(_) | Self::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }

    /// A client-safe sentence. SQL/IO text stays in the server log.
    pub fn public_message(&self) -> String {
        match self {
            Self::Sqlite(_) => "database failure".into(),
            Self::Io(_) => "storage failure".into(),
            Self::Codec(_) => "encoding failure".into(),
            Self::Internal(_) => "internal media failure".into(),
            other => other.to_string(),
        }
    }

    pub fn into_api(self) -> wxlike_api::ApiError {
        wxlike_api::ApiError::new(self.status(), self.public_message())
    }
}

impl From<MediaError> for wxlike_api::ApiError {
    fn from(err: MediaError) -> Self {
        err.into_api()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_errors_do_not_leak_paths() {
        let err = MediaError::Internal("C:\\secret\\media.db is locked".into());
        assert_eq!(err.public_message(), "internal media failure");
        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn user_errors_keep_their_status() {
        assert_eq!(MediaError::NotFound.status(), StatusCode::NOT_FOUND);
        assert_eq!(MediaError::Forbidden.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            MediaError::QuotaExceeded.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }
}
