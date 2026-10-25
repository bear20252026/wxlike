//! Publisher certificates, revocation and signature verification.
//!
//! A package signature is only as trustworthy as the key behind it, so the
//! signing key is wrapped in a certificate issued by an offline root key.
//! The trust store holds root-pinned certificates and two revocation sets
//! (keys and whole publishers). Revocation is checked on **every** verify,
//! not only at enrollment, so removing a key takes effect for packages that
//! were already downloaded.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::error::{MiniError, Result};
use super::manifest::{digest_hex, Manifest};

pub const PUBLIC_KEY_HEX_LEN: usize = 64;
/// Ed25519 种子与发布者 ID 的字节宽度。
pub const ED25519_KEY_LEN: usize = 32;
/// Ed25519 签名的字节宽度。
pub const ED25519_SIGNATURE_LEN: usize = 64;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PublisherCert {
    pub publisher: String,
    pub key_id: String,
    /// hex-encoded 32-byte ed25519 public key.
    pub public_key: String,
    pub not_before: u64,
    pub not_after: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignedCert {
    pub root_key_id: String,
    pub cert: PublisherCert,
    pub sig: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackageSignature {
    pub key_id: String,
    pub sig: String,
}

#[derive(Clone)]
pub struct TrustStore {
    root_key_id: String,
    root: VerifyingKey,
    certs: BTreeMap<String, PublisherCert>,
    revoked_keys: BTreeSet<String>,
    revoked_publishers: BTreeSet<String>,
}

impl TrustStore {
    pub fn new(root_key_id: impl Into<String>, root: VerifyingKey) -> Self {
        Self {
            root_key_id: root_key_id.into(),
            root,
            certs: BTreeMap::new(),
            revoked_keys: BTreeSet::new(),
            revoked_publishers: BTreeSet::new(),
        }
    }

    /// Verify a root-signed certificate and add it to the trust store.
    pub fn enroll(&mut self, signed: &SignedCert, now: u64) -> Result<()> {
        if signed.root_key_id != self.root_key_id {
            return Err(MiniError::UntrustedPublisher);
        }
        let bytes = canonical_cert(&signed.cert)?;
        verify_ed25519(&self.root, &bytes, &signed.sig)?;
        // Reject certificates that advertise a malformed key up front.
        let _key = decode_key(&signed.cert.public_key)?;
        check_window(&signed.cert, now)?;
        self.check_not_revoked(&signed.cert)?;
        self.certs
            .insert(signed.cert.key_id.clone(), signed.cert.clone());
        Ok(())
    }

    pub fn revoke_key(&mut self, key_id: impl Into<String>) {
        self.revoked_keys.insert(key_id.into());
    }

    pub fn revoke_publisher(&mut self, publisher: impl Into<String>) {
        self.revoked_publishers.insert(publisher.into());
    }

    pub fn is_revoked(&self, publisher: &str, key_id: &str) -> bool {
        self.revoked_keys.contains(key_id) || self.revoked_publishers.contains(publisher)
    }

    /// Verify the signature over the exact manifest bytes and return the
    /// certificate that signed it.
    pub fn verify_manifest(
        &self,
        manifest: &Manifest,
        manifest_bytes: &[u8],
        sig: &PackageSignature,
        now: u64,
    ) -> Result<PublisherCert> {
        let cert = self
            .certs
            .get(&sig.key_id)
            .ok_or(MiniError::UntrustedPublisher)?;
        if cert.publisher != manifest.publisher {
            return Err(MiniError::PublisherMismatch);
        }
        check_window(cert, now)?;
        self.check_not_revoked(cert)?;
        let key = decode_key(&cert.public_key)?;
        verify_ed25519(&key, manifest_bytes, &sig.sig)?;
        Ok(cert.clone())
    }

    fn check_not_revoked(&self, cert: &PublisherCert) -> Result<()> {
        if self.is_revoked(&cert.publisher, &cert.key_id) {
            Err(MiniError::Revoked)
        } else {
            Ok(())
        }
    }
}

/// Issue a publisher certificate with an offline root key (CLI/CI use).
pub fn issue_cert(root_key_id: &str, root: &SigningKey, cert: PublisherCert) -> Result<SignedCert> {
    let bytes = canonical_cert(&cert)?;
    Ok(SignedCert {
        root_key_id: root_key_id.to_string(),
        cert,
        sig: hex::encode(root.sign(&bytes).to_bytes()),
    })
}

pub fn sign_manifest(manifest_bytes: &[u8], key: &SigningKey, key_id: &str) -> PackageSignature {
    PackageSignature {
        key_id: key_id.to_string(),
        sig: hex::encode(key.sign(manifest_bytes).to_bytes()),
    }
}

pub fn generate_key() -> SigningKey {
    use rand::RngCore as _;
    let mut seed = [0u8; ED25519_KEY_LEN];
    rand::thread_rng().fill_bytes(&mut seed);
    SigningKey::from_bytes(&seed)
}

pub fn key_id_for(key: &VerifyingKey) -> String {
    // Human-readable fingerprint of the public key (not a secret).
    let digest = Sha256::digest(key.as_bytes());
    hex::encode(&digest[..8])
}

pub fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn canonical_cert(cert: &PublisherCert) -> Result<Vec<u8>> {
    serde_json::to_vec(cert).map_err(Into::into)
}

fn check_window(cert: &PublisherCert, now: u64) -> Result<()> {
    if now < cert.not_before || now > cert.not_after {
        Err(MiniError::CertExpired)
    } else {
        Ok(())
    }
}

fn decode_key(hex_key: &str) -> Result<VerifyingKey> {
    decode_public_key(hex_key)
}

/// Decode a hex-encoded ed25519 public key (CLI/trust-snapshot input).
pub fn decode_public_key(hex_key: &str) -> Result<VerifyingKey> {
    if hex_key.len() != PUBLIC_KEY_HEX_LEN {
        return Err(MiniError::UntrustedPublisher);
    }
    let raw = hex::decode(hex_key).map_err(|_| MiniError::UntrustedPublisher)?;
    let bytes: [u8; ED25519_KEY_LEN] = raw.try_into().map_err(|_| MiniError::UntrustedPublisher)?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| MiniError::UntrustedPublisher)
}

/// Verify a detached signature with a bare publisher key. Used by `mini
/// inspect` when the caller only has the publisher's public key, not a full
/// trust snapshot; installation always goes through [`TrustStore`].
pub fn verify_detached(public_key_hex: &str, message: &[u8], sig: &PackageSignature) -> Result<()> {
    let key = decode_public_key(public_key_hex)?;
    verify_ed25519(&key, message, &sig.sig)
}

fn verify_ed25519(key: &VerifyingKey, message: &[u8], sig_hex: &str) -> Result<()> {
    let raw = hex::decode(sig_hex).map_err(|_| MiniError::BadSignature)?;
    let bytes: [u8; ED25519_SIGNATURE_LEN] = raw.try_into().map_err(|_| MiniError::BadSignature)?;
    let signature = Signature::from_bytes(&bytes);
    key.verify_strict(message, &signature)
        .map_err(|_| MiniError::BadSigning)
}

/// Deterministic keypair for fixtures (never use in production).
pub fn fixture_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// Fingerprint used by tests and the CLI to display a key.
pub fn fingerprint(key: &SigningKey) -> String {
    digest_hex(key.verifying_key().as_bytes())[..16].to_string()
}
