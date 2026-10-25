//! commerce 错误。面向客户端只给稳定分类，不回显供应商原始报文、SQL、
//! 账号密码或完整卡号。

use thiserror::Error;

/// commerce 错误。
#[derive(Debug, Error)]
pub enum CommerceError {
    #[error("invalid request: {0}")]
    BadInput(String),
    #[error("account/order does not belong to the caller")]
    Forbidden,
    #[error("authentication required")]
    Unauthorized,
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    #[error("expired")]
    Expired,
    #[error("idempotency key reused with different parameters")]
    IdempotencyConflict,
    #[error("webhook signature verification failed")]
    InvalidSignature,
    #[error("duplicate provider event")]
    Replay,
    #[error("illegal state transition: {0}")]
    IllegalTransition(String),
    #[error("payment provider is temporarily unavailable")]
    ProviderUnavailable,
    #[error("payment provider timed out; state is unknown")]
    ProviderTimeout,
    #[error("payment provider rejected the request")]
    ProviderRejected,
    #[error("payment provider is not configured for this deployment")]
    ProviderNotConfigured,
    #[error("group membership check is unavailable")]
    MembershipUnavailable,
    #[error("insufficient funds")]
    InsufficientFunds,
    #[error("amount exceeds the allowed maximum")]
    AmountTooLarge,
    #[error("accounting error")]
    Ledger(#[from] wxlike_ledger::LedgerError),
    #[error("storage error")]
    Storage(#[from] rusqlite::Error),
}

/// commerce 结果。
pub type Result<T> = std::result::Result<T, CommerceError>;
