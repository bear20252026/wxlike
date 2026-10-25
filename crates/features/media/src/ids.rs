//! Validation for every identifier that can reach a SQL statement or a path.
//!
//! Object ids are content addresses but **never** capabilities: knowing the
//! SHA-256 does not authorize a read. The shape checks here exist so a forged
//! id becomes a clean 404 instead of a path escape or an unbounded lookup.

use rand::RngCore as _;
use sha2::{Digest as _, Sha256};

use crate::error::{MediaError, Result};

pub const SHA256_HEX_LEN: usize = wxlike_core::limits::SHA256_HEX_LEN;
const OPAQUE_BYTES: usize = 16;
const HEX_BYTES_PER_CHAR: usize = 2;
pub const MAX_NAME_CHARS: usize = 255;
pub const MAX_FOLDER_CHARS: usize = 512;
pub const MAX_MIME_CHARS: usize = 128;

pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == SHA256_HEX_LEN && value.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn random_bytes(bytes: usize) -> Vec<u8> {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    buf
}

pub fn random_hex(bytes: usize) -> String {
    hex::encode(random_bytes(bytes))
}

/// Opaque ids are prefixed so a share/session/job id cannot be mistaken for an
/// object id by a caller that logs or forwards it.
pub fn opaque_id(prefix: &str) -> String {
    format!("{prefix}_{}", random_hex(OPAQUE_BYTES))
}

pub fn validate_object_id(value: &str) -> Result<()> {
    if is_sha256_hex(value) {
        Ok(())
    } else {
        Err(MediaError::NotFound)
    }
}

pub fn validate_declared_digest(value: &str) -> Result<()> {
    if value.is_empty() || is_sha256_hex(value) {
        Ok(())
    } else {
        Err(MediaError::bad("sha256 must be 64 hex characters"))
    }
}

/// Display names are rendered, never joined onto a path. Refusing separators
/// and `..` outright means a future accidental `Path::join` cannot escape.
pub fn validate_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(MediaError::bad("name must not be empty"));
    }
    if trimmed.chars().count() > MAX_NAME_CHARS {
        return Err(MediaError::bad("name is too long"));
    }
    if trimmed.contains(['/', '\\']) || trimmed.contains("..") {
        return Err(MediaError::bad("name must not contain path separators"));
    }
    Ok(trimmed.to_string())
}

/// Folder is a display path inside the owner's file center; it is stored as
/// text and never used to build a filesystem path.
pub fn validate_folder(folder: &str) -> Result<String> {
    let trimmed = folder.trim().trim_matches('/').to_string();
    if trimmed.chars().count() > MAX_FOLDER_CHARS {
        return Err(MediaError::bad("folder is too long"));
    }
    for part in trimmed.split('/') {
        validate_name(part)?;
    }
    Ok(trimmed)
}

pub fn validate_mime(mime: &str) -> Result<String> {
    let trimmed = mime.trim().to_ascii_lowercase();
    if trimmed.is_empty() || trimmed.len() > MAX_MIME_CHARS {
        return Err(MediaError::bad("mime is empty or too long"));
    }
    let valid = trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'+' | b'.' | b'-'));
    if !valid || !trimmed.contains('/') {
        return Err(MediaError::bad("mime is malformed"));
    }
    Ok(trimmed)
}

pub fn hex_len(bytes: usize) -> usize {
    bytes * HEX_BYTES_PER_CHAR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_like_names_and_ids() {
        assert!(validate_name("../../etc/passwd").is_err());
        assert!(validate_name("a/b.txt").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_object_id("not-a-hash").is_err());
        assert!(validate_object_id(&"g".repeat(SHA256_HEX_LEN)).is_err());
    }

    #[test]
    fn accepts_plain_names_and_hashes() {
        assert_eq!(validate_name(" report.pdf ").unwrap(), "report.pdf");
        assert_eq!(validate_folder("/2026/十月/").unwrap(), "2026/十月");
        let hash = sha256_hex(b"hello");
        assert_eq!(hex_len(32), SHA256_HEX_LEN);
        assert!(validate_object_id(&hash).is_ok());
        assert!(validate_declared_digest("").is_ok());
    }
}
