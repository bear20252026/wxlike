//! QUIC transport setup: self-signed server certs (lab) and a skip-verify client.
//!
//! Lab-only trust model. Production replaces this with real PKI / pinned keys.

use std::sync::Arc;

use anyhow::Context as _;
use quinn::Connection;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

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
        .with_single_cert(vec![cert_der], rustls::pki_types::PrivateKeyDer::Pkcs8(key_der))
        .context("server cert")?;
    server_crypto.alpn_protocols = vec![b"wxlike/1".to_vec()];
    let quic_server_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
        .context("quic server crypto from rustls config")?;
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_server_crypto));

    let endpoint = quinn::Endpoint::server(server_config, addr).context("bind server endpoint")?;
    Ok(endpoint)
}

/// Connect a client that skips server-cert verification (lab only).
pub async fn connect_insecure(
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
