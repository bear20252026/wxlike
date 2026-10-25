//! Error surface for package, trust and install operations.
//!
//! Variants are deliberately granular: the UI/admin needs to distinguish
//! "tampered" from "outdated" from "not allowed in this channel", and a
//! single opaque string would push that decision into string matching.

#[derive(Debug, thiserror::Error)]
pub enum MiniError {
    #[error("manifest invalid: {0}")]
    BadManifest(&'static str),
    #[error("manifest JSON invalid: {0}")]
    BadJson(String),
    #[error("package signature is malformed")]
    BadSignature,
    #[error("package signature does not verify")]
    BadSigning,
    #[error("publisher certificate is not trusted")]
    UntrustedPublisher,
    #[error("publisher certificate is outside its validity window")]
    CertExpired,
    #[error("publisher key or publisher is revoked")]
    Revoked,
    #[error("manifest publisher does not match the signing certificate")]
    PublisherMismatch,
    #[error("package is not valid: {0}")]
    BadPackage(String),
    #[error("resource {0:?} is declared but missing")]
    MissingResource(String),
    #[error("resource {0:?} is present but not declared")]
    ExtraResource(String),
    #[error("resource {0:?} digest mismatch")]
    DigestMismatch(String),
    #[error("package {0:?} version {1} is below the installed watermark {2}")]
    Downgrade(String, String, String),
    #[error("package {0:?} version {1} conflicts with the installed digest")]
    VersionConflict(String, String),
    #[error("package channel {0} cannot be installed into a {1} root")]
    WrongChannel(String, String),
    #[error("package needs host >= {0}, running {1}")]
    HostTooOld(String, String),
    #[error("app {0:?} is not installed")]
    NotInstalled(String),
    #[error("injected fault at {0:?}")]
    InjectedFault(&'static str),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("package codec error: {0}")]
    Codec(String),
}

pub type Result<T> = std::result::Result<T, MiniError>;

impl From<anyhow::Error> for MiniError {
    fn from(err: anyhow::Error) -> Self {
        MiniError::BadPackage(format!("{err:#}"))
    }
}

impl From<serde_json::Error> for MiniError {
    fn from(err: serde_json::Error) -> Self {
        MiniError::BadJson(err.to_string())
    }
}
