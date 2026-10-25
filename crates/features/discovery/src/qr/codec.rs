use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use super::wire::{QrKind, QrTarget, MAX_PAYLOAD_BYTES};
use crate::crypto::Signer;
use crate::error::{DiscoveryError, Result};

pub const WIRE_VERSION: &str = "v1";
pub const SCHEME_PREFIX: &str = "wxlike://";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedQr {
    pub target: QrTarget,
    pub expires_at_ms: Option<i64>,
    pub nonce: String,
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    target: QrTarget,
    exp: Option<i64>,
    nonce: String,
}

pub struct QrCodec {
    signer: Signer,
}

impl QrCodec {
    pub fn new(signing_key: impl AsRef<[u8]>) -> Self {
        Self {
            signer: Signer::new(signing_key),
        }
    }

    pub fn encode(
        &self,
        target: &QrTarget,
        nonce: &str,
        expires_at_ms: Option<i64>,
    ) -> Result<String> {
        target.validate()?;
        if nonce.is_empty() || nonce.len() > super::wire::MAX_ID_LEN {
            return Err(DiscoveryError::InvalidArgument("nonce"));
        }
        let envelope = Envelope {
            target: target.clone(),
            exp: expires_at_ms,
            nonce: nonce.to_owned(),
        };
        let json = serde_json::to_vec(&envelope)
            .map_err(|_| DiscoveryError::InvalidArgument("envelope encode"))?;
        let payload = URL_SAFE_NO_PAD.encode(json);
        let canonical = canonical(target.kind(), &payload);
        let signature = self.signer.sign(&canonical);
        let wire = format!(
            "{SCHEME_PREFIX}{WIRE_VERSION}/{}?p={payload}&sig={signature}",
            target.kind().as_str()
        );
        if wire.len() > MAX_PAYLOAD_BYTES {
            return Err(DiscoveryError::InvalidArgument("payload too large"));
        }
        Ok(wire)
    }

    pub fn decode(&self, raw: &str, now_ms: i64) -> Result<DecodedQr> {
        if raw.len() > MAX_PAYLOAD_BYTES {
            return Err(DiscoveryError::InvalidArgument("payload too large"));
        }
        let rest = raw
            .strip_prefix(SCHEME_PREFIX)
            .ok_or(DiscoveryError::InvalidArgument("scheme"))?;
        let (path, query) = rest
            .split_once('?')
            .ok_or(DiscoveryError::InvalidArgument("query"))?;
        let path = path
            .strip_prefix(&format!("{WIRE_VERSION}/"))
            .ok_or(DiscoveryError::InvalidArgument("wire version"))?;
        let kind = QrKind::parse(path).ok_or(DiscoveryError::InvalidArgument("qr kind"))?;
        let (payload, signature) = parse_query(query)?;
        let canonical = canonical(kind, payload);
        if !self.signer.verify(&canonical, signature) {
            return Err(DiscoveryError::InvalidArgument("signature"));
        }
        let json = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| DiscoveryError::InvalidArgument("payload encoding"))?;
        let envelope: Envelope = serde_json::from_slice(&json)
            .map_err(|_| DiscoveryError::InvalidArgument("payload schema"))?;
        if envelope.target.kind() != kind {
            return Err(DiscoveryError::InvalidArgument("qr kind mismatch"));
        }
        envelope.target.validate()?;
        if let Some(expires_at_ms) = envelope.exp {
            if expires_at_ms <= now_ms {
                return Err(DiscoveryError::Expired);
            }
        }
        Ok(DecodedQr {
            target: envelope.target,
            expires_at_ms: envelope.exp,
            nonce: envelope.nonce,
        })
    }
}

fn canonical(kind: QrKind, payload: &str) -> String {
    format!("wxlike:{WIRE_VERSION}:{}:{payload}", kind.as_str())
}

fn parse_query(query: &str) -> Result<(&str, &str)> {
    let mut payload = None;
    let mut signature = None;
    for pair in query.split('&') {
        let (key, value) = pair
            .split_once('=')
            .ok_or(DiscoveryError::InvalidArgument("query pair"))?;
        let slot = match key {
            "p" => &mut payload,
            "sig" => &mut signature,
            _ => return Err(DiscoveryError::InvalidArgument("unknown query key")),
        };
        if slot.replace(value).is_some() {
            return Err(DiscoveryError::InvalidArgument("duplicate query key"));
        }
    }
    match (payload, signature) {
        (Some(p), Some(s)) if !p.is_empty() && !s.is_empty() => Ok((p, s)),
        _ => Err(DiscoveryError::InvalidArgument("missing signature")),
    }
}
