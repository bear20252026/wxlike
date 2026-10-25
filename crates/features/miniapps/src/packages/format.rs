//! Package envelope: build, sign and verify a `.dpage` blob that carries a
//! signed `mini.json` plus its complete resource set.

use std::collections::BTreeMap;

use super::error::{MiniError, Result};
use super::manifest::{digest_hex, seal_dir, Manifest, MANIFEST_FILE};
use super::trust::{sign_manifest, PackageSignature, TrustStore};

pub const MAX_MANIFEST_BYTES: usize = 512 * 1024;

#[derive(Clone, Debug)]
pub struct SignedPackage {
    pub blob: Vec<u8>,
    pub manifest: Manifest,
    pub manifest_bytes: Vec<u8>,
    pub signature: PackageSignature,
}

#[derive(Clone, Debug)]
pub struct VerifiedPackage {
    pub manifest: Manifest,
    pub manifest_bytes: Vec<u8>,
    pub files: BTreeMap<String, Vec<u8>>,
    /// SHA-256 of `mini.json`; identifies the package content independently
    /// of its version string.
    pub digest: String,
}

/// A packed but unsigned package. `mini pack` stops here; the signature is
/// a detached sidecar produced by `mini sign`, so packing and signing can
/// happen in different processes (CI builder vs. offline publisher key).
#[derive(Clone, Debug)]
pub struct PackedPackage {
    pub blob: Vec<u8>,
    pub manifest: Manifest,
    pub manifest_bytes: Vec<u8>,
}

/// Pack a source directory, compute resource digests and embed `mini.json`.
pub fn pack_unsigned(
    dir: &std::path::Path,
    template: &Manifest,
    host_version: &str,
) -> Result<PackedPackage> {
    let (manifest, mut files) = seal_dir(dir, template, host_version)?;
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    files.0.push(wxlike_dpage::FileEntry {
        path: MANIFEST_FILE.into(),
        data: manifest_bytes.clone(),
    });
    let blob = wxlike_dpage::pack_files(files)?;
    Ok(PackedPackage {
        blob,
        manifest,
        manifest_bytes,
    })
}

/// Pack a source directory and sign the exact manifest bytes.
pub fn build_signed(
    dir: &std::path::Path,
    template: &Manifest,
    host_version: &str,
    key: &ed25519_dalek::SigningKey,
    key_id: &str,
) -> Result<SignedPackage> {
    let packed = pack_unsigned(dir, template, host_version)?;
    let signature = sign_manifest(&packed.manifest_bytes, key, key_id);
    Ok(SignedPackage {
        blob: packed.blob,
        manifest: packed.manifest,
        manifest_bytes: packed.manifest_bytes,
        signature,
    })
}

/// Sign an already packed blob (CLI `mini sign`). Refuses to sign a blob
/// whose manifest does not match its resources.
pub fn sign_blob(
    blob: &[u8],
    host_version: &str,
    key: &ed25519_dalek::SigningKey,
    key_id: &str,
) -> Result<SignedPackage> {
    let parts = read_parts(blob)?;
    parts.manifest.validate(host_version)?;
    verify_resources(&parts.manifest, &parts.files)?;
    let signature = sign_manifest(&parts.manifest_bytes, key, key_id);
    Ok(SignedPackage {
        blob: blob.to_vec(),
        manifest: parts.manifest,
        manifest_bytes: parts.manifest_bytes,
        signature,
    })
}

/// Verify signature, certificate, host compatibility and every resource
/// digest. Any mismatch rejects the whole package.
pub fn verify(
    blob: &[u8],
    sig: &PackageSignature,
    trust: &TrustStore,
    now: u64,
    host_version: &str,
) -> Result<VerifiedPackage> {
    let parts = read_parts(blob)?;
    parts.manifest.validate(host_version)?;
    trust.verify_manifest(&parts.manifest, &parts.manifest_bytes, sig, now)?;
    verify_resources(&parts.manifest, &parts.files)?;
    Ok(VerifiedPackage {
        digest: digest_hex(&parts.manifest_bytes),
        manifest: parts.manifest,
        manifest_bytes: parts.manifest_bytes,
        files: parts.files,
    })
}

/// Parse a blob without any signature trust decision: shape, host
/// compatibility and every resource digest. `mini inspect` uses this to show
/// what a package claims before checking who signed it.
pub fn open(blob: &[u8], host_version: &str) -> Result<VerifiedPackage> {
    let parts = read_parts(blob)?;
    parts.manifest.validate(host_version)?;
    verify_resources(&parts.manifest, &parts.files)?;
    Ok(VerifiedPackage {
        digest: digest_hex(&parts.manifest_bytes),
        manifest: parts.manifest,
        manifest_bytes: parts.manifest_bytes,
        files: parts.files,
    })
}

struct Parts {
    manifest: Manifest,
    manifest_bytes: Vec<u8>,
    files: BTreeMap<String, Vec<u8>>,
}

fn read_parts(blob: &[u8]) -> Result<Parts> {
    let raw = wxlike_dpage::unpack_raw(blob)?;
    let manifest_bytes = raw
        .files
        .get(MANIFEST_FILE)
        .ok_or_else(|| MiniError::BadPackage("mini.json missing".into()))?
        .clone();
    if manifest_bytes.len() > MAX_MANIFEST_BYTES {
        return Err(MiniError::BadPackage("mini.json too large".into()));
    }
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    Ok(Parts {
        manifest,
        manifest_bytes,
        files: raw.files,
    })
}

fn verify_resources(manifest: &Manifest, files: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    for (path, expected) in &manifest.resources {
        let data = files
            .get(path)
            .ok_or_else(|| MiniError::MissingResource(path.clone()))?;
        if digest_hex(data) != *expected {
            return Err(MiniError::DigestMismatch(path.clone()));
        }
    }
    for path in files.keys() {
        if path != MANIFEST_FILE && !manifest.resources.contains_key(path) {
            return Err(MiniError::ExtraResource(path.clone()));
        }
    }
    Ok(())
}
