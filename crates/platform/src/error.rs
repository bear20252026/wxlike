//! Error types for the platform service: a `thiserror` enum for library code
//! and an HTTP-facing wrapper that renders `{ "error": ... }` bodies.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

pub type Result<T> = std::result::Result<T, PlatformError>;

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("database failure")]
    Sqlite(#[from] rusqlite::Error),
    #[error("filesystem failure")]
    Io(#[from] std::io::Error),
    #[error("invalid input: {0}")]
    BadInput(&'static str),
    #[error("not found")]
    NotFound,
    #[error("already exists: {0}")]
    Conflict(String),
    #[error("missing or invalid credentials")]
    Unauthorized,
    #[error("not permitted")]
    Forbidden,
    #[error("payload too large")]
    PayloadTooLarge,
}

/// HTTP-visible error: status plus a client-safe message (no SQL text leaks).
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
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
            PlatformError::BadInput(m) => Self::new(StatusCode::UNPROCESSABLE_ENTITY, m),
            PlatformError::NotFound => Self::new(StatusCode::NOT_FOUND, "not found"),
            PlatformError::Conflict(reason) => {
                Self::new(StatusCode::CONFLICT, format!("already exists: {reason}"))
            }
            PlatformError::Unauthorized => Self::new(StatusCode::UNAUTHORIZED, "unauthorized"),
            PlatformError::Forbidden => Self::new(StatusCode::FORBIDDEN, "not permitted"),
            PlatformError::PayloadTooLarge => {
                Self::new(StatusCode::PAYLOAD_TOO_LARGE, "media too large")
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}
