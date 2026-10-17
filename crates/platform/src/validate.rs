//! Input validation shared by every platform endpoint.
//!
//! Two layers, deliberately asymmetric:
//! * every field gets shape limits (length, no control characters) and every
//!   statement is parameterized, so free text like `it's me` is stored inertly
//!   and cannot reach SQL;
//! * **names** additionally veto quoting metacharacters, because WP-01 A4 asks
//!   for injection-shaped input to be *rejected* rather than merely survived.
//!   That veto is not applied to notes/remarks/signatures, where an apostrophe
//!   is ordinary language.
//!
//! Generated identifiers (user_id, media_id, ...) are hex and go through
//! [`id`], never through the text checks.

use crate::error::{PlatformError, Result};

pub const MAX_NAME_CHARS: usize = 64;
pub const MAX_PASSWORD_CHARS: usize = 256;
pub const MAX_SIGNATURE_CHARS: usize = 200;
pub const MAX_AVATAR_CHARS: usize = 512;
pub const MAX_ID_CHARS: usize = 64;
pub const MAX_REMARK_CHARS: usize = 64;

const METACHARS: [char; 5] = ['\'', '"', ';', '\\', '%'];

fn check_shape(field: &'static str, s: &str, max: usize) -> Result<()> {
    if s.chars().count() > max {
        return Err(PlatformError::BadInput(match field {
            "name" => "name too long",
            "signature" => "signature too long",
            "avatar" => "avatar too long",
            "remark" => "remark too long",
            "note" => "note too long",
            _ => "text too long",
        }));
    }
    if s.chars().any(|c| c.is_control()) {
        return Err(PlatformError::BadInput(match field {
            "name" => "name contains control characters",
            "remark" => "remark contains control characters",
            "note" => "note contains control characters",
            _ => "text contains control characters",
        }));
    }
    Ok(())
}

/// Names only: refuse quoting metacharacters and SQL comment sequences.
fn check_label(s: &str) -> Result<()> {
    if s.chars().any(|c| METACHARS.contains(&c)) || s.contains("--") || s.contains("/*") {
        return Err(PlatformError::BadInput(
            "name contains forbidden characters",
        ));
    }
    Ok(())
}

pub fn name(s: &str) -> Result<&str> {
    if s.trim().is_empty() {
        return Err(PlatformError::BadInput("name must not be empty"));
    }
    check_shape("name", s, MAX_NAME_CHARS)?;
    check_label(s)?;
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

/// Free text: shape limits only, so `it's me` is accepted.
pub fn signature(s: &str) -> Result<&str> {
    check_shape("signature", s, MAX_SIGNATURE_CHARS)?;
    Ok(s)
}

pub fn avatar(s: &str) -> Result<&str> {
    check_shape("avatar", s, MAX_AVATAR_CHARS)?;
    Ok(s)
}

/// Friend remarks: short free text, empty allowed (clearing a remark is a
/// legitimate update).
pub fn remark(s: &str) -> Result<&str> {
    check_shape("remark", s, MAX_REMARK_CHARS)?;
    Ok(s.trim())
}

pub fn note(s: &str) -> Result<&str> {
    check_shape("note", s, MAX_SIGNATURE_CHARS)?;
    Ok(s)
}

/// Generated identifiers only: alphanumeric, bounded length. Guards lookups
/// and path parameters against traversal and oversized keys.
pub fn id(s: &str) -> Result<&str> {
    if s.is_empty() || s.len() > MAX_ID_CHARS || !s.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(PlatformError::BadInput("malformed identifier"));
    }
    Ok(s)
}

/// WP-06: lowercase platform tag, small alphabet so it can never be confused
/// with a path or SQL fragment downstream.
pub fn device_platform(s: &str) -> Result<&str> {
    let s = s.trim();
    if s.is_empty() || s.len() > 32 {
        return Err(PlatformError::BadInput("platform tag malformed"));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(PlatformError::BadInput("platform tag malformed"));
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apostrophes_are_allowed_in_free_text_but_not_in_names() {
        assert!(note("hi it's me").is_ok());
        assert!(remark("老同学").is_ok());
        assert!(signature("don't panic").is_ok());
        assert!(name("it's me").is_err(), "names keep the strict veto");
        assert!(name("alice").is_ok());
    }

    #[test]
    fn shape_limits() {
        assert!(name("").is_err());
        assert!(name("   ").is_err());
        assert!(name(&"a".repeat(65)).is_err());
        assert!(note(&"x".repeat(201)).is_err());
        assert!(password("").is_err());
        assert!(name("ali\nce").is_err());
        assert!(note("line\nbreak").is_err());
        assert!(id("deadbeef").is_ok());
        assert!(id("../../etc/passwd").is_err());
        assert!(id(&"a".repeat(65)).is_err());
        assert!(remark("").is_ok(), "clearing a remark is legal");
    }
}
