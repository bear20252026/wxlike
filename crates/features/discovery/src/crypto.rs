use std::collections::HashMap;
use std::sync::Mutex;

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// HMAC-SHA256 签名器。密钥由服务端配置注入，不进入二维码 payload。
#[derive(Clone)]
pub struct Signer {
    key: Vec<u8>,
}

impl Signer {
    pub fn new(key: impl AsRef<[u8]>) -> Self {
        Self {
            key: key.as_ref().to_vec(),
        }
    }

    pub fn sign(&self, canonical: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("hmac accepts any key length");
        mac.update(canonical.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    pub fn verify(&self, canonical: &str, signature_hex: &str) -> bool {
        let Ok(sig) = hex::decode(signature_hex) else {
            return false;
        };
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("hmac accepts any key length");
        mac.update(canonical.as_bytes());
        mac.verify_slice(&sig).is_ok()
    }
}

/// 一次性 nonce 账本：签名只证明来源，重放由这里拦住。
pub trait NonceStore: Send + Sync {
    /// 首次出现返回 `true`；已消费、已过期或非法 nonce 返回 `false`。
    fn consume(&self, nonce: &str, expires_at_ms: i64, now_ms: i64) -> bool;
}

#[derive(Debug, Default)]
pub struct MemoryNonceStore {
    seen: Mutex<HashMap<String, i64>>,
}

const MAX_NONCE_LEN: usize = 64;

impl MemoryNonceStore {
    pub fn prune(&self, now_ms: i64) {
        if let Ok(mut seen) = self.seen.lock() {
            seen.retain(|_, expires_at| *expires_at > now_ms);
        }
    }
}

impl NonceStore for MemoryNonceStore {
    fn consume(&self, nonce: &str, expires_at_ms: i64, now_ms: i64) -> bool {
        if nonce.is_empty() || nonce.len() > MAX_NONCE_LEN || expires_at_ms <= now_ms {
            return false;
        }
        let Ok(mut seen) = self.seen.lock() else {
            return false;
        };
        if seen.contains_key(nonce) {
            return false;
        }
        seen.insert(nonce.to_owned(), expires_at_ms);
        true
    }
}
