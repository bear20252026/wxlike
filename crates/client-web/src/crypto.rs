//! 密钥与敏感缓存加密（浏览器侧）。
//!
//! **密钥放哪**：`localStorage`。它不是安全存储（同源的 JS 都读得到），真正的
//! 边界是"没有 XSS" + CSP；换成 IndexedDB 只是把攻击面从一个同步 API 换成一个
//! 异步 API，并不改变这件事。真要强隔离得用不可导出的 `CryptoKey` + IDB 结构化
//! 克隆，那是后续卡的事——这里把边界写在注释里，不假装它是 Keychain。
//!
//! **怎么加密**：XChaCha20-Poly1305（纯 Rust，wasm 上照样跑）。不用 WebCrypto 的
//! AES-GCM 是因为它要求 `importKey` 那套仪式，而加密算法不是这里的薄弱环节，
//! 密钥存储才是。随机数仍然取 WebCrypto 的 `getRandomValues`（不能用 `Math.random`）。
//!
//! 缓存密钥丢了（清站点数据、换设备）就解不开旧缓存：这时应当清缓存重拉，
//! 而不是把密文当明文渲染。`open` 因此返回明确错误。

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use wasm_bindgen::JsValue;
use wxlike_client_core::ports::{Secret, SecretError, Secrets};

/// 缓存密钥在 [`Secrets`] 里的键名。
pub const CACHE_KEY_NAME: &str = "cache-key";

/// nonce 长度。
pub const NONCE_LEN: usize = 24;
/// 密钥长度。
pub const KEY_LEN: usize = 32;

/// `localStorage` 前缀：与同源其它应用的键隔开。
const PREFIX: &str = "wxlike.";

/// 基于 `localStorage` 的密钥存储。
pub struct WebSecrets {
    storage: web_sys::Storage,
}

impl WebSecrets {
    /// 打开。私有模式下 `localStorage` 可能直接抛异常。
    ///
    /// # Errors
    /// 浏览器禁用存储（`SecurityError`）。
    pub fn new() -> Result<Self, SecretError> {
        let window =
            web_sys::window().ok_or_else(|| SecretError::Unavailable("没有 window".to_string()))?;
        let storage = window
            .local_storage()
            .map_err(|err| SecretError::Unavailable(format!("localStorage 不可用：{err:?}")))?
            .ok_or_else(|| {
                SecretError::Unavailable("localStorage 被禁用（私有模式？）".to_string())
            })?;
        Ok(Self { storage })
    }

    fn key_of(name: &str) -> String {
        format!("{PREFIX}{name}")
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Secrets for WebSecrets {
    async fn get(&self, key: &str) -> Result<Option<Secret>, SecretError> {
        let raw = self
            .storage
            .get_item(&Self::key_of(key))
            .map_err(|err| SecretError::Unavailable(format!("{err:?}")))?;
        match raw {
            None => Ok(None),
            Some(text) => {
                let bytes = decode_hex(&text).ok_or_else(|| {
                    SecretError::Unavailable("密钥内容损坏（不是合法十六进制）".to_string())
                })?;
                Ok(Some(Secret::new(bytes)))
            }
        }
    }

    async fn put(&self, key: &str, value: Secret) -> Result<(), SecretError> {
        self.storage
            .set_item(&Self::key_of(key), &encode_hex(value.expose()))
            .map_err(|err| {
                SecretError::Unavailable(format!("写入密钥失败（配额？私有模式？）：{err:?}"))
            })
    }

    async fn delete(&self, key: &str) -> Result<(), SecretError> {
        self.storage
            .remove_item(&Self::key_of(key))
            .map_err(|err| SecretError::Unavailable(format!("{err:?}")))
    }
}

/// 敏感缓存的加解密器。
pub struct WebCacheCipher {
    aead: XChaCha20Poly1305,
}

impl WebCacheCipher {
    /// 从 [`Secrets`] 取密钥；没有就生成并存回去。
    ///
    /// # Errors
    /// 密钥存储不可用。
    pub async fn load_or_create(secrets: &dyn Secrets) -> Result<Self, SecretError> {
        if let Some(existing) = secrets.get(CACHE_KEY_NAME).await? {
            return Self::new(existing.expose());
        }
        let key = random_bytes(KEY_LEN)?;
        secrets
            .put(CACHE_KEY_NAME, Secret::new(key.clone()))
            .await?;
        Self::new(&key)
    }

    /// 用 32 字节密钥构造。
    ///
    /// # Errors
    /// 长度不对。
    pub fn new(key: &[u8]) -> Result<Self, SecretError> {
        if key.len() != KEY_LEN {
            return Err(SecretError::Unavailable(format!(
                "缓存密钥长度应为 {KEY_LEN} 字节，实际 {}",
                key.len()
            )));
        }
        let aead = XChaCha20Poly1305::new_from_slice(key)
            .map_err(|err| SecretError::Unavailable(err.to_string()))?;
        Ok(Self { aead })
    }

    /// 加密：`nonce || ciphertext`。
    ///
    /// # Errors
    /// 加密失败（实际不会发生）。
    pub fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, SecretError> {
        let nonce_bytes = random_bytes(NONCE_LEN)?;
        let nonce = XNonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .aead
            .encrypt(nonce, plaintext)
            .map_err(|err| SecretError::Unavailable(err.to_string()))?;
        let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// 解密。
    ///
    /// # Errors
    /// 密文截断、密钥不匹配。
    pub fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, SecretError> {
        if sealed.len() <= NONCE_LEN {
            return Err(SecretError::Unavailable("缓存密文过短".to_string()));
        }
        let (nonce, body) = sealed.split_at(NONCE_LEN);
        self.aead
            .decrypt(XNonce::from_slice(nonce), body)
            .map_err(|_| SecretError::Unavailable("缓存解密失败（密钥已更换？）".to_string()))
    }
}

/// 取密码学安全随机数。**不要**用 `Math.random` 替代。
fn random_bytes(len: usize) -> Result<Vec<u8>, SecretError> {
    let window =
        web_sys::window().ok_or_else(|| SecretError::Unavailable("没有 window".to_string()))?;
    let mut buf = vec![0u8; len];
    window
        .crypto()
        .and_then(|crypto| crypto.get_random_values_with_u8_array(&mut buf))
        .map_err(|err: JsValue| {
            SecretError::Unavailable(format!("getRandomValues 失败：{err:?}"))
        })?;
    Ok(buf)
}

fn encode_hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(TABLE[usize::from(byte >> 4)]));
        out.push(char::from(TABLE[usize::from(byte & 0x0f)]));
    }
    out
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        out.push((high << 4) | low);
    }
    Some(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
