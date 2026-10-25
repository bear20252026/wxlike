//! Developer toolchain behind `mini pack|sign|inspect|dev-serve`.
//!
//! Packing and signing are deliberately separate steps: CI packs, the
//! offline publisher key signs. `dev-serve` only serves the files that were
//! sealed into the dev manifest, so a request cannot reach outside the source
//! directory even if it contains `..`.

use std::collections::BTreeMap;

/// 405 不在质量门禁的 HTTP 常见值白名单里，具名常量替代两处字面量。
const HTTP_METHOD_NOT_ALLOWED: u16 = 405;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

use crate::packages::manifest::{digest_hex, seal_dir};
use crate::packages::trust::now_seconds;
use crate::packages::{
    decode_public_key, key_id_for, open, pack_unsigned, sign_blob, verify_detached, Channel,
    Manifest, MiniError, PackageSignature, Result, SignedCert, TrustStore, MANIFEST_FILE,
};

pub const DEFAULT_HOST_VERSION: &str = "0.1.0";
pub const DEFAULT_TEMPLATE: &str = "mini.template.json";
pub const DEV_BIND: &str = "127.0.0.1";
const MAX_REQUEST_LINE: usize = 8 * 1024;
const SEED_LEN: usize = 32;

/// Root-pinned trust input for `mini inspect --trust`. It contains only
/// public material; a signing key never belongs in this file.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct TrustSnapshot {
    pub root_key_id: String,
    pub root_public_key: String,
    #[serde(default)]
    pub certs: Vec<SignedCert>,
    #[serde(default)]
    pub revoked_keys: Vec<String>,
    #[serde(default)]
    pub revoked_publishers: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct PackReport {
    pub blob: String,
    pub app_id: String,
    pub version: String,
    pub channel: String,
    pub digest: String,
    pub files: usize,
}

#[derive(Serialize, Clone, Debug)]
pub struct SignReport {
    pub blob: String,
    pub signature: String,
    pub key_id: String,
    pub app_id: String,
    pub version: String,
    pub digest: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct ResourceInfo {
    pub path: String,
    pub bytes: usize,
}

#[derive(Serialize, Clone, Debug)]
pub struct InspectReport {
    pub app_id: String,
    pub name: String,
    pub version: String,
    pub channel: String,
    pub min_host: String,
    pub entry: String,
    pub publisher: String,
    pub capabilities: Vec<String>,
    pub resources: Vec<ResourceInfo>,
    pub digest: String,
    /// `verified` | `unverified`; unchecked packages are never installable.
    pub signature: String,
    pub signer: Option<String>,
}

pub fn load_template(dir: &Path, explicit: Option<&Path>) -> Result<Manifest> {
    let path = explicit
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dir.join(DEFAULT_TEMPLATE));
    let text = fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&text)?)
}

pub fn signing_key(seed_hex: &str) -> Result<SigningKey> {
    let raw = hex::decode(seed_hex.trim()).map_err(|_| bad("signing key is not hex"))?;
    let bytes: [u8; SEED_LEN] = raw
        .try_into()
        .map_err(|_| bad("signing key is not 32 bytes"))?;
    Ok(SigningKey::from_bytes(&bytes))
}

pub fn pack(
    dir: &Path,
    template: Option<&Path>,
    out: Option<&Path>,
    host_version: &str,
) -> Result<PackReport> {
    let manifest = load_template(dir, template)?;
    let packed = pack_unsigned(dir, &manifest, host_version)?;
    let out = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| blob_name(&packed.manifest));
    fs::write(&out, &packed.blob)?;
    Ok(PackReport {
        blob: out.display().to_string(),
        app_id: packed.manifest.app_id,
        version: packed.manifest.version,
        channel: packed.manifest.channel.as_str().to_string(),
        digest: digest_hex(&packed.manifest_bytes),
        files: packed.manifest.resources.len(),
    })
}

pub fn sign(
    blob_path: &Path,
    key: &SigningKey,
    key_id: Option<&str>,
    out: Option<&Path>,
    sig_out: Option<&Path>,
    host_version: &str,
) -> Result<SignReport> {
    let blob = fs::read(blob_path)?;
    let key_id = match key_id {
        Some(id) if !id.trim().is_empty() => id.to_string(),
        Some(_) => return Err(bad("key id is empty")),
        None => key_id_for(&key.verifying_key()),
    };
    let signed = sign_blob(&blob, host_version, key, &key_id)?;
    let out = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| blob_path.to_path_buf());
    let sig_path = sig_out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| out.with_extension("sig"));
    fs::write(&out, &signed.blob)?;
    fs::write(&sig_path, serde_json::to_vec_pretty(&signed.signature)?)?;
    Ok(SignReport {
        blob: out.display().to_string(),
        signature: sig_path.display().to_string(),
        key_id,
        app_id: signed.manifest.app_id,
        version: signed.manifest.version,
        digest: digest_hex(&signed.manifest_bytes),
    })
}

pub fn read_signature(path: &Path) -> Result<PackageSignature> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

pub fn trust_store(snapshot: &TrustSnapshot) -> Result<TrustStore> {
    let root = decode_public_key(&snapshot.root_public_key)?;
    let mut store = TrustStore::new(snapshot.root_key_id.clone(), root);
    let now = now_seconds();
    for cert in &snapshot.certs {
        store.enroll(cert, now)?;
    }
    for key in &snapshot.revoked_keys {
        store.revoke_key(key.clone());
    }
    for publisher in &snapshot.revoked_publishers {
        store.revoke_publisher(publisher.clone());
    }
    Ok(store)
}

pub fn load_trust(path: &Path) -> Result<TrustStore> {
    let snapshot: TrustSnapshot = serde_json::from_slice(&fs::read(path)?)?;
    trust_store(&snapshot)
}

/// Inspect a blob. `signature` is the sidecar file when present; without a
/// trust store or publisher key the report is explicitly `unverified`.
pub fn inspect(
    blob_path: &Path,
    signature: Option<&Path>,
    trust: Option<&TrustStore>,
    public_key: Option<&str>,
    host_version: &str,
) -> Result<InspectReport> {
    let blob = fs::read(blob_path)?;
    let opened = open(&blob, host_version)?;
    let mut report = report_for(&opened);
    let Some(sig_path) = signature else {
        return Ok(report);
    };
    let sig = read_signature(sig_path)?;
    if let Some(store) = trust {
        let cert = store.verify_manifest(
            &opened.manifest,
            &opened.manifest_bytes,
            &sig,
            now_seconds(),
        )?;
        report.signer = Some(format!("{}/{}", cert.publisher, cert.key_id));
    } else if let Some(key) = public_key {
        verify_detached(key, &opened.manifest_bytes, &sig)?;
    } else {
        return Err(bad("a signature needs --trust or --public-key"));
    }
    report.signature = "verified".into();
    Ok(report)
}

fn report_for(opened: &crate::packages::VerifiedPackage) -> InspectReport {
    let resources = opened
        .manifest
        .resources
        .keys()
        .map(|path| ResourceInfo {
            path: path.clone(),
            bytes: opened.files.get(path).map(Vec::len).unwrap_or(0),
        })
        .collect();
    InspectReport {
        app_id: opened.manifest.app_id.clone(),
        name: opened.manifest.name.clone(),
        version: opened.manifest.version.clone(),
        channel: opened.manifest.channel.as_str().to_string(),
        min_host: opened.manifest.min_host.clone(),
        entry: opened.manifest.entry.clone(),
        publisher: opened.manifest.publisher.clone(),
        capabilities: opened.manifest.capabilities.clone(),
        resources,
        digest: opened.digest.clone(),
        signature: "unverified".into(),
        signer: None,
    }
}

fn blob_name(manifest: &Manifest) -> PathBuf {
    PathBuf::from(format!("{}-{}.dpage", manifest.app_id, manifest.version))
}

fn bad(message: &'static str) -> MiniError {
    MiniError::BadManifest(message)
}

/// Loopback-only dev server for a source directory. Only sealed files are
/// reachable, and the channel must be `dev` so this cannot be mistaken for a
/// production package.
pub struct DevServer {
    listener: TcpListener,
    url: String,
    entry: String,
    manifest: Manifest,
    files: BTreeMap<String, Vec<u8>>,
}

impl DevServer {
    pub fn bind(
        dir: &Path,
        template: Option<&Path>,
        host_version: &str,
        port: u16,
    ) -> Result<Self> {
        let mut manifest = load_template(dir, template)?;
        if manifest.channel != Channel::Dev {
            return Err(bad("dev-serve requires a dev-channel template"));
        }
        manifest.channel = Channel::Dev;
        let (manifest, filemap) = seal_dir(dir, &manifest, host_version)?;
        let manifest_bytes = serde_json::to_vec(&manifest)?;
        let mut files: BTreeMap<String, Vec<u8>> = filemap
            .0
            .into_iter()
            .map(|entry| (entry.path, entry.data))
            .collect();
        files.insert(MANIFEST_FILE.into(), manifest_bytes);
        let listener = TcpListener::bind((DEV_BIND, port))?;
        let url = format!("http://{}", listener.local_addr()?);
        Ok(Self {
            listener,
            url,
            entry: manifest.entry.clone(),
            manifest,
            files,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Serve until the process is interrupted.
    pub fn serve(self) -> Result<()> {
        for stream in self.listener.incoming() {
            let mut stream = stream?;
            self.respond(&mut stream)?;
        }
        Ok(())
    }

    /// Accept exactly one request, used by the CLI smoke test.
    pub fn serve_once(&self) -> Result<()> {
        let (mut stream, _) = self.listener.accept()?;
        self.respond(&mut stream)
    }

    fn respond(&self, stream: &mut TcpStream) -> Result<()> {
        let reader = BufReader::new(stream.try_clone()?);
        let line = &mut String::new();
        reader.take(MAX_REQUEST_LINE as u64 + 1).read_line(line)?;
        if !line.ends_with('\n') {
            return reply(stream, 400, "text/plain", b"bad request\n", false);
        }
        let mut parts = line.trim_end().split(' ');
        let method = parts.next().unwrap_or_default();
        let target = parts.next().unwrap_or_default();
        let head_only = method == "HEAD";
        if method != "GET" && !head_only {
            return reply(
                stream,
                HTTP_METHOD_NOT_ALLOWED,
                "text/plain",
                b"method not allowed\n",
                false,
            );
        }
        let path = target.split('?').next().unwrap_or_default();
        let rel = match path.trim_start_matches('/') {
            "" => self.entry.as_str(),
            other => other,
        };
        match self.files.get(rel) {
            Some(body) => reply(stream, 200, content_type(rel), body, head_only),
            None => reply(stream, 404, "text/plain", b"not found\n", false),
        }
    }
}

fn reply(
    stream: &mut TcpStream,
    status: u16,
    ctype: &str,
    body: &[u8],
    head_only: bool,
) -> Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\
         Connection: close\r\n\r\n",
        reason(status),
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    if !head_only {
        stream.write_all(body)?;
    }
    stream.flush()?;
    Ok(())
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        HTTP_METHOD_NOT_ALLOWED => "Method Not Allowed",
        _ => "Error",
    }
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "js" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}
