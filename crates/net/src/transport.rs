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

/// 客户端的保活与空闲参数：**自适应**（见 [`crate::heartbeat`]）。
///
/// quinn 0.11 的默认值是 `max_idle_timeout = 30s`、`keep_alive_interval = None`——
/// 空闲 30 秒连接就死，且不发包维持 NAT 映射。移动端一进后台必断。
///
/// 现在由 `heartbeat::keepalive()` 给出：间隔 15–60 秒自适应（稳定网络更长、弱网更短），
/// 空闲上限取 3 倍间隔（夹在 45–180 秒）。
pub fn client_transport_config() -> quinn::TransportConfig {
    let (keep_alive, idle) = crate::heartbeat::keepalive();
    let mut cfg = quinn::TransportConfig::default();
    cfg.keep_alive_interval(Some(keep_alive));
    cfg.max_idle_timeout(Some(idle.try_into().expect("idle timeout fits in VarInt")));
    cfg
}

/// 服务端的保活与空闲参数：**固定**。
///
/// 服务端不主动探测（`keep_alive_interval = None`），只给一个空闲上限：
/// QUIC 的空闲超时按两端取小，所以这里的 90 秒同时是"客户端把心跳拉到 3 分钟"
/// 这类错误配置的兜底——服务端不会因为对端配置离谱就永远留着死连接。
pub fn server_transport_config() -> quinn::TransportConfig {
    let mut cfg = quinn::TransportConfig::default();
    cfg.max_idle_timeout(Some(
        std::time::Duration::from_secs(90)
            .try_into()
            .expect("90s fits in VarInt"),
    ));
    cfg
}

/// 与 [`make_server_endpoint`] 相同，但**证书持久化**在 `dir` 里（issue #12）。
///
/// 首次调用生成自签证书并写 `quic-cert.der` / `quic-key.der`，之后每次启动复用。
/// 这样客户端 pin 的叶证书指纹在重启后仍然匹配——否则"服务端正常重启"与
/// "有人换了证书"在客户端看起来完全一样，TOFU 就退化成了每次都要重新信任。
///
/// 目录不可写时**直接失败**，不悄悄退回"每次生成新证书"：那会把一个部署问题
/// 变成"所有人下次连不上"。
pub fn make_server_endpoint_persistent(
    addr: std::net::SocketAddr,
    dir: &Path,
) -> anyhow::Result<quinn::Endpoint> {
    std::fs::create_dir_all(dir).with_context(|| format!("create cert dir {}", dir.display()))?;
    let cert_path = dir.join("quic-cert.der");
    let key_path = dir.join("quic-key.der");

    let (cert_der, key_der) = match (cert_path.exists(), key_path.exists()) {
        (true, true) => {
            let cert = std::fs::read(&cert_path)
                .with_context(|| format!("read {}", cert_path.display()))?;
            let key = std::fs::read(&key_path)
                .with_context(|| format!("read {}", key_path.display()))?;
            (CertificateDer::from(cert), PrivatePkcs8KeyDer::from(key))
        }
        (false, false) => {
            let ck =
                rcgen::generate_simple_self_signed(vec!["localhost".into(), "wxlike.dev".into()])?;
            let cert = ck.cert.der().to_vec();
            let key = ck.key_pair.serialize_der();
            std::fs::write(&cert_path, &cert)
                .with_context(|| format!("write {}", cert_path.display()))?;
            std::fs::write(&key_path, &key)
                .with_context(|| format!("write {}", key_path.display()))?;
            tracing::info!(
                cert = %cert_path.display(),
                fingerprint = %cert_fingerprint(&CertificateDer::from(cert.clone())),
                "generated a new QUIC certificate (first start in this data dir)"
            );
            (CertificateDer::from(cert), PrivatePkcs8KeyDer::from(key))
        }
        // 一半有一半没有：说明有人删了其中一个。不能猜，否则会生成新证书
        // 让所有客户端被 pin 拒绝，而日志里看不出为什么。
        (c, k) => anyhow::bail!(
            "incomplete QUIC cert material in {}: cert={c} key={k}；             要么两个都保留，要么两个都删掉重新生成（后者会让客户端需要重新 pin）",
            dir.display()
        ),
    };

    endpoint_from_cert(addr, cert_der, key_der)
}

/// Bind a QUIC endpoint that accepts connections with a fresh self-signed cert.
///
/// **每次调用都换证书**，只适合测试：客户端按 TOFU 记住叶证书指纹，
/// 服务端一重启指纹就变，旧客户端会遭遇与中间人攻击一模一样的拒绝。
/// 生产用 [`make_server_endpoint_persistent`]。
pub fn make_server_endpoint(addr: std::net::SocketAddr) -> anyhow::Result<quinn::Endpoint> {
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".into(), "wxlike.dev".into()])?;
    endpoint_from_cert(
        addr,
        ck.cert.der().clone(),
        PrivatePkcs8KeyDer::from(ck.key_pair.serialize_der()),
    )
}

/// 用给定的证书/私钥 bind 端点。两条入口（每次新建 / 持久化）共用这段。
fn endpoint_from_cert(
    addr: std::net::SocketAddr,
    cert_der: CertificateDer<'static>,
    key_der: PrivatePkcs8KeyDer<'static>,
) -> anyhow::Result<quinn::Endpoint> {
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
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_server_crypto));
    server_config.transport_config(Arc::new(server_transport_config()));

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
    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_crypto));
    client_config.transport_config(Arc::new(client_transport_config()));
    endpoint.set_default_client_config(client_config);
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
    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_crypto));
    client_config.transport_config(Arc::new(client_transport_config()));
    endpoint.set_default_client_config(client_config);
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
