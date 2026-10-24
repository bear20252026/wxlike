//! 敏感缓存的 AEAD：**两端共用一份**。
//!
//! 原生与浏览器都需要"把缓存加密后再落盘"，差别只在**随机数从哪来**：
//! 原生用 `rand`，浏览器必须用 `crypto.getRandomValues`（不能用 `Math.random`）。
//! 除此之外的逻辑（nonce 布局、密钥长度、错误语义）一字不差，所以放在这里，
//! 由两个平台各传一个 nonce 进来。
//!
//! 密文布局：`nonce(24) || ciphertext`。nonce 跟着密文走，读取时不需要额外的
//! 元数据列——这也是"换机之后解不开"的原因：密钥没了，nonce 也就没意义了。
//!
//! 用 XChaCha20-Poly1305 而不是 AES-GCM：24 字节 nonce 可以直接随机生成而
//! 不必担心碰撞（AES-GCM 的 12 字节 nonce 在长期密钥下需要计数器管理）。

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

use crate::ports::{Secret, SecretError, Secrets};

/// 密钥长度（字节）。
pub const KEY_LEN: usize = 32;
/// nonce 长度（字节）。
pub const NONCE_LEN: usize = 24;
/// 缓存密钥在 [`Secrets`] 里的键名。
pub const CACHE_KEY_NAME: &str = "cache-key";

/// 加解密器。
pub struct CacheCipher {
    aead: XChaCha20Poly1305,
}

impl CacheCipher {
    /// 用 32 字节密钥构造。
    ///
    /// # Errors
    /// 密钥长度不是 [`KEY_LEN`]。
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

    /// 从 [`Secrets`] 取密钥；没有就调用 `generate` 生成并存回去。
    ///
    /// # Errors
    /// 密钥存储不可用。
    pub async fn load_or_create(
        secrets: &dyn Secrets,
        generate: impl FnOnce() -> Vec<u8>,
    ) -> Result<Self, SecretError> {
        if let Some(existing) = secrets.get(CACHE_KEY_NAME).await? {
            return Self::new(existing.expose());
        }
        let key = generate();
        secrets
            .put(CACHE_KEY_NAME, Secret::new(key.clone()))
            .await?;
        Self::new(&key)
    }

    /// 加密：`nonce || ciphertext`。
    ///
    /// # Errors
    /// 加密失败（长度越界等，实际不会发生）。
    pub fn seal(&self, plaintext: &[u8], nonce: &[u8; NONCE_LEN]) -> Result<Vec<u8>, SecretError> {
        let ciphertext = self
            .aead
            .encrypt(XNonce::from_slice(nonce), plaintext)
            .map_err(|err| SecretError::Unavailable(err.to_string()))?;
        let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        out.extend_from_slice(nonce);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// 解密。
    ///
    /// # Errors
    /// 密文截断、密钥不匹配（换机 / 重装 / 清站点数据）。
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

/// 十六进制编解码（密钥文件与 localStorage 都用它）。
pub mod hex {
    const TABLE: &[u8; 16] = b"0123456789abcdef";

    /// 编码为小写十六进制。
    #[must_use]
    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            out.push(char::from(TABLE[usize::from(byte >> 4)]));
            out.push(char::from(TABLE[usize::from(byte & 0x0f)]));
        }
        out
    }

    /// 解码；长度奇数或含非十六进制字符时返回 `None`。
    #[must_use]
    pub fn decode(text: &str) -> Option<Vec<u8>> {
        if !text.len().is_multiple_of(2) {
            return None;
        }
        let bytes = text.as_bytes();
        let mut out = Vec::with_capacity(bytes.len() / 2);
        for pair in bytes.chunks(2) {
            out.push((value(pair[0])? << 4) | value(pair[1])?);
        }
        Some(out)
    }

    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
}
