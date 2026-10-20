//! QUIC transport setup: self-signed server certs and client trust.
//!
//! Two trust models:
//!
//! * [`connect_tofu`] — **trust on first use** (WP-12). The first connection
//!   to a server name pins the SHA-256 fingerprint of its leaf certificate in
//!   a local file; every later connection must present the same certificate.
//!   A man-in-the-middle that appears *before* the first connection is not
//!   detected (that is what TOFU means), but one that appears *after* is.
//! * `connect_insecure` — skip-verify, **crate-private and test-only**. It is
//!   not exported: a public skip-verify entry point is a foot-gun that every
//!   caller eventually reaches for. Production code goes through
//!   [`connect_tofu`] (or `Client::connect_verified`), which pins the leaf
//!   certificate.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use quinn::Connection;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use sha2::Digest as _;

/// Bind a QUIC endpoint that accepts connections with a fresh self-signed cert.
pub fn make_server_endpoint(addr: std::net::SocketAddr) -> anyhow::Result<quinn::Endpoint> {
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".into(), "wxlike.dev".into()])?;
    let cert_der = ck.cert.der().clone();
    let key_der = PrivatePkcs8KeyDer::from(ck.key_pair.serialize_der());

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut server_crypto = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("protocol versions")?
        .with_no_client_auth()
        .with_single_cert(
            vec![cert_der],
            rustls::pki_types::PrivateKeyDer::Pkcs8(key_der),
        )
        .context("server cert")?;
    server_crypto.alpn_protocols = vec![b"wxlike/1".to_vec()];
    let quic_server_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
        .context("quic server crypto from rustls config")?;
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_server_crypto));

    let endpoint = quinn::Endpoint::server(server_config, addr).context("bind server endpoint")?;
    Ok(endpoint)
}

/// Connect a client that skips server-cert verification (lab only).
///
/// Production code must call [`connect_tofu`] instead; keep this behind an
/// explicit `--insecure-lab` switch wherever it is used.
pub(crate) async fn connect_insecure(
    server_addr: std::net::SocketAddr,
    server_name: &str,
) -> anyhow::Result<Connection> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut crypto = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("protocol versions")?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification::new(provider)))
        .with_no_client_auth();
    crypto.alpn_protocols = vec![b"wxlike/1".to_vec()];

    let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?).context("client endpoint")?;
    let quic_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
        .context("quic crypto from rustls config")?;
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(quic_crypto)));
    let conn = endpoint
        .connect(server_addr, server_name)
        .context("connect")?
        .await
        .context("handshake")?;
    Ok(conn)
}

// ---------------------------------------------------------------------------
// TOFU (WP-12)
// ---------------------------------------------------------------------------

/// Local pin store for trust-on-first-use: `server name -> fingerprint`.
///
/// The map lives in a small JSON file owned by the client process (for the
/// desktop app, an app-local directory). A missing file is an empty store:
/// the first connection to any name pins what it sees.
#[derive(Debug)]
pub struct TofuPins {
    path: PathBuf,
    pins: HashMap<String, String>,
}

fn poison<T>(lock: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl TofuPins {
    pub fn load(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        let pins = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { path, pins }
    }

    /// `Ok(true)` = freshly pinned, `Ok(false)` = pin already matched,
    /// `Err` = pinned fingerprint differs (possible MITM or cert rotation).
    pub fn check_or_pin(&mut self, server_name: &str, fingerprint: &str) -> Result<bool, String> {
        match self.pins.get(server_name) {
            Some(pinned) if pinned == fingerprint => Ok(false),
            Some(pinned) => Err(format!(
                "certificate fingerprint changed for '{server_name}': pinned {pinned}, saw {fingerprint}"
            )),
            None => {
                self.pins
                    .insert(server_name.to_string(), fingerprint.to_string());
                self.save().map_err(|e| e.to_string())?;
                Ok(true)
            }
        }
    }

    fn save(&self) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        std::fs::write(&self.path, serde_json::to_string_pretty(&self.pins)?)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.pins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pins.is_empty()
    }
}

/// SHA-256 of the DER encoding, hex-encoded: the fingerprint we pin.
pub fn cert_fingerprint(cert: &CertificateDer<'_>) -> String {
    let digest = sha2::Sha256::digest(cert.as_ref());
    hex::encode(digest)
}

/// Connect with trust-on-first-use against `pins_path` (WP-12).
///
/// First connection to `server_name` records the server certificate's
/// fingerprint; later connections refuse any different certificate.
pub async fn connect_tofu(
    server_addr: std::net::SocketAddr,
    server_name: &str,
    pins_path: impl AsRef<Path>,
) -> anyhow::Result<Connection> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = TofuVerifier {
        provider: provider.clone(),
        pins: Mutex::new(TofuPins::load(pins_path)),
    };
    let mut crypto = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("protocol versions")?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    crypto.alpn_protocols = vec![b"wxlike/1".to_vec()];

    let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?).context("client endpoint")?;
    let quic_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
        .context("quic crypto from rustls config")?;
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(quic_crypto)));
    let conn = endpoint
        .connect(server_addr, server_name)
        .context("connect")?
        .await
        .map_err(|e| anyhow::anyhow!("handshake rejected: {e}"))?;
    Ok(conn)
}

#[derive(Debug)]
struct TofuVerifier {
    provider: Arc<rustls::crypto::CryptoProvider>,
    pins: Mutex<TofuPins>,
}

fn server_name_key(name: &ServerName<'_>) -> String {
    match name {
        ServerName::DnsName(d) => d.as_ref().to_string(),
        ServerName::IpAddress(ip) => format!("{ip:?}"),
        _ => String::new(),
    }
}

impl ServerCertVerifier for TofuVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fingerprint = cert_fingerprint(end_entity);
        let mut pins = poison(&self.pins);
        pins.check_or_pin(&server_name_key(server_name), &fingerprint)
            .map(|_| ServerCertVerified::assertion())
            .map_err(rustls::Error::General)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // Unlike the skip-verify verifier, TOFU is a real trust decision, so
        // the handshake signature is actually checked.
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[derive(Debug)]
struct SkipServerVerification {
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl SkipServerVerification {
    fn new(provider: Arc<rustls::crypto::CryptoProvider>) -> Self {
        Self { provider }
    }
}

impl ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
