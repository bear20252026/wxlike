//! Credentials: PBKDF2-HMAC-SHA256 password hashing and Bearer auth.
//!
//! Passwords are never stored or compared in plaintext: per-user random salt,
//! a round count of at least 100_000 PBKDF2 rounds (persisted per user so it
//! can be raised later without breaking existing logins), and constant-time
//! digest comparison.

use axum::extract::FromRequestParts;
use axum::http::header;
use axum::http::request::Parts;
use axum::http::StatusCode;
use rand::RngCore as _;
use sha2::Sha256;

use crate::error::{ApiError, PlatformError, Result};
use crate::Platform;

pub const DEFAULT_ROUNDS: u32 = 100_000;
pub const TOKEN_BYTES: usize = 32;
const SALT_BYTES: usize = 16;
const DIGEST_BYTES: usize = 32;

/// URL-safe-ish random hex of `n` bytes of entropy.
pub fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

fn derive(password: &str, salt_hex: &str, rounds: u32) -> Result<[u8; DIGEST_BYTES]> {
    let salt =
        hex::decode(salt_hex).map_err(|_| PlatformError::BadInput("stored salt is malformed"))?;
    Ok(pbkdf2::pbkdf2_hmac_array::<Sha256, DIGEST_BYTES>(
        password.as_bytes(),
        &salt,
        rounds,
    ))
}

/// Returns `(salt_hex, hash_hex)` for a fresh password.
pub fn hash_password(password: &str, rounds: u32) -> Result<(String, String)> {
    let rounds = if rounds == 0 { DEFAULT_ROUNDS } else { rounds };
    let salt = random_hex(SALT_BYTES);
    let digest = derive(password, &salt, rounds)?;
    Ok((salt, hex::encode(digest)))
}

/// Constant-time verification: equal-length compare without early exit.
pub fn verify_password(
    password: &str,
    salt_hex: &str,
    expected_hex: &str,
    rounds: u32,
) -> Result<bool> {
    let expected = hex::decode(expected_hex).unwrap_or_default();
    if expected.len() != DIGEST_BYTES {
        return Ok(false);
    }
    let digest = derive(password, salt_hex, rounds)?;
    let diff = digest
        .iter()
        .zip(expected.iter())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b));
    Ok(diff == 0)
}

/// Authenticated caller, produced by the `Authorization: Bearer <token>` check.
/// Every write endpoint and `/api/me` extracts this, so an unauthenticated
/// request cannot reach business logic.
pub struct Authed {
    pub user_id: String,
    pub token: String,
}

impl Authed {
    pub fn bearer_from(headers: &axum::http::HeaderMap) -> Option<&str> {
        let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
        let (_, token) = raw.split_once(char::is_whitespace)?;
        let scheme = &raw[..raw.len() - token.len() - 1];
        if scheme.trim().eq_ignore_ascii_case("bearer") && !token.trim().is_empty() {
            Some(token.trim())
        } else {
            None
        }
    }
}

impl FromRequestParts<std::sync::Arc<Platform>> for Authed {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &std::sync::Arc<Platform>,
    ) -> std::result::Result<Self, Self::Rejection> {
        let Some(token) = Self::bearer_from(&parts.headers) else {
            return Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "missing bearer token",
            ));
        };
        let token = token.to_string();
        let found = state.lookup_token(&token).map_err(|_| {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "session lookup failed")
        })?;
        match found {
            Some(user_id) => Ok(Authed { user_id, token }),
            None => Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "invalid or expired token",
            )),
        }
    }
}
