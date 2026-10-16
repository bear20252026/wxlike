//! Input validation shared by every platform endpoint.
//!
//! Two layers: (1) shape/length limits that reject abusive payloads before
//! they reach SQL, and (2) parameterized statements everywhere. The task book
//! (WP-01 A4) asks for injection strings to be *rejected*, so display-ish text
//! fields also veto control characters and quoting metacharacters. Legitimate
//! ids (user_id, media_id, ...) are generated hex and never pass through here.

use crate::error::{PlatformError, Result};

pub const MAX_NAME_CHARS: usize = 64;
pub const MAX_PASSWORD_CHARS: usize = 256;
pub const MAX_SIGNATURE_CHARS: usize = 200;
pub const MAX_AVATAR_CHARS: usize = 512;
pub const MAX_ID_CHARS: usize = 64;

const METACHARS: [char; 5] = ['\'', '"', ';', '\\', '%'];

fn clean_text(field: &'static str, s: &str, max: usize) -> Result<()> {
    if s.chars().count() > max {
        return Err(PlatformError::BadInput(match field {
            "name" => "name too long",
            "signature" => "signature too long",
            "avatar" => "avatar too long",
            _ => "text too long",
        }));
    }
    if s.chars().any(|c| c.is_control()) {
        return Err(PlatformError::BadInput(match field {
            "name" => "name contains control characters",
            _ => "text contains control characters",
        }));
    }
    if s.chars().any(|c| METACHARS.contains(&c)) || s.contains("--") || s.contains("/*") {
        return Err(PlatformError::BadInput(match field {
            "name" => "name contains forbidden characters",
            "signature" => "signature contains forbidden characters",
            _ => "text contains forbidden characters",
        }));
    }
    Ok(())
}

pub fn name(s: &str) -> Result<&str> {
    if s.trim().is_empty() {
        return Err(PlatformError::BadInput("name must not be empty"));
    }
    clean_text("name", s, MAX_NAME_CHARS)?;
    Ok(s.trim())
}

pub fn password(s: &str) -> Result<&str> {
    if s.is_empty() {
        return Err(PlatformError::BadInput("password must not be empty"));
    }
    if s.chars().count() > MAX_PASSWORD_CHARS {
        return Err(PlatformError::BadInput("password too long"));
    }
    Ok(s)
}

pub fn signature(s: &str) -> Result<&str> {
    clean_text("signature", s, MAX_SIGNATURE_CHARS)?;
    Ok(s)
}

pub fn avatar(s: &str) -> Result<&str> {
    clean_text("avatar", s, MAX_AVATAR_CHARS)?;
    Ok(s)
}

/// Generated identifiers only: hex, bounded length. Guards lookups and path
/// parameters against traversal and oversized keys.
pub fn id(s: &str) -> Result<&str> {
    if s.is_empty() || s.len() > MAX_ID_CHARS || !s.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(PlatformError::BadInput("malformed identifier"));
    }
    Ok(s)
}
