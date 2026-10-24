//! 设备密钥存储 + 敏感缓存的静态加密。
//!
//! 两条边界：
//!
//! * **密钥只进这里**。token、缓存加密密钥都走 [`Secrets`]，不写进 SQLite
//!    的明文列（SQLite 文件在应用私有目录里，但备份、越狱设备、调试导出
//!    都能把它整份拿走）。
//! * **敏感缓存落盘前加密**。[`CacheCipher`] 用设备密钥做 XChaCha20-Poly1305，
//!    密文里带 nonce；密钥换了（重装/换机）就解不开——那时应该清缓存重拉，
//!    而不是把密文当明文渲染。
//!
//! 文件形态刻意保持"人能看懂"（`key<TAB>hex`）：真出问题时能手工确认
//! "密钥在不在"，比一个不透明二进制块有用。权限在 `open` 时尽量收紧
//! （Unix 下 0600），Windows 上依赖用户目录的默认 ACL。

use std::path::{Path, PathBuf};

use rand::RngCore as _;

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use wxlike_client_core::ports::{Secret, SecretError, Secrets};

/// 缓存加密密钥在 [`Secrets`] 里的键名。
pub const CACHE_KEY_NAME: &str = "cache-key";

/// 基于文件的密钥存储。
pub struct FileSecrets {
    path: PathBuf,
    entries: std::sync::Mutex<Vec<(String, Vec<u8>)>>,
}

impl FileSecrets {
    /// 打开（不存在则创建空文件）。
    ///
    /// # Errors
    /// 目录/文件不可读写。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SecretError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| SecretError::Unavailable(err.to_string()))?;
        }
        let entries = if path.exists() {
            read_entries(&path)?
        } else {
            Vec::new()
        };
        Ok(Self {
            path,
            entries: std::sync::Mutex::new(entries),
        })
    }

    fn flush(&self, entries: &[(String, Vec<u8>)]) -> Result<(), SecretError> {
        let mut text = String::new();
        for (key, value) in entries {
            text.push_str(key);
            text.push('\t');
            text.push_str(&hex::encode(value));
            text.push('\n');
        }
        std::fs::write(&self.path, text)
            .map_err(|err| SecretError::Unavailable(err.to_string()))?;
        restrict_permissions(&self.path);
        Ok(())
    }
}

fn read_entries(path: &Path) -> Result<Vec<(String, Vec<u8>)>, SecretError> {
    let text =
        std::fs::read_to_string(path).map_err(|err| SecretError::Unavailable(err.to_string()))?;
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('\t') else {
            continue;
        };
        let bytes = hex::decode(value.trim())
            .map_err(|err| SecretError::Unavailable(format!("密钥文件损坏：{err}")))?;
        out.push((key.to_string(), bytes));
    }
    Ok(out)
}

/// Unix 下把权限收紧到 0600；其它平台尽力而为。
fn restrict_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Secrets for FileSecrets {
    async fn get(&self, key: &str) -> Result<Option<Secret>, SecretError> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        Ok(entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| Secret::new(value.clone())))
    }

    async fn put(&self, key: &str, value: Secret) -> Result<(), SecretError> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let bytes = value.expose().to_vec();
        match entries.iter_mut().find(|(name, _)| name == key) {
            Some(slot) => slot.1 = bytes,
            None => entries.push((key.to_string(), bytes)),
        }
        self.flush(&entries)
    }

    async fn delete(&self, key: &str) -> Result<(), SecretError> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|(name, _)| name != key);
        self.flush(&entries)
    }
}

/// 敏感缓存的加解密器。
pub struct CacheCipher {
    aead: XChaCha20Poly1305,
}

impl CacheCipher {
    /// 用 32 字节密钥构造。
    ///
    /// # Errors
    /// 密钥长度不是 32。
    pub fn new(key: &[u8]) -> Result<Self, SecretError> {
        if key.len() != KEY_LEN {
            return Err(SecretError::Unavailable(format!(
                "缓存密钥长度应为 {KEY_LEN} 字节，实际 {}",
                key.len()
            )));
        }
        let cipher = XChaCha20Poly1305::new_from_slice(key)
            .map_err(|err| SecretError::Unavailable(err.to_string()))?;
        Ok(Self { aead: cipher })
    }

    /// 从 [`Secrets`] 取密钥；没有就生成一个并存回去。
    ///
    /// # Errors
    /// 密钥存储不可用。
    pub async fn load_or_create(secrets: &dyn Secrets) -> Result<Self, SecretError> {
        if let Some(existing) = secrets.get(CACHE_KEY_NAME).await? {
            return Self::new(existing.expose());
        }
        let key = generate_key();
        secrets
            .put(CACHE_KEY_NAME, Secret::new(key.clone()))
            .await?;
        Self::new(&key)
    }

    /// 加密：`nonce || ciphertext`。
    ///
    /// # Errors
    /// 加密失败（长度越界等，实际不会发生）。
    pub fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, SecretError> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = XNonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .aead
            .encrypt(nonce, plaintext)
            .map_err(|err| SecretError::Unavailable(err.to_string()))?;
        let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        out.extend_from_slice(nonce.as_slice());
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// 解密。
    ///
    /// # Errors
    /// 密文被截断、密钥不对（换机/重装）时返回 `Unavailable`。
    pub fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, SecretError> {
        if sealed.len() <= NONCE_LEN {
            return Err(SecretError::Unavailable("缓存密文过短".to_string()));
        }
        let (nonce, body) = sealed.split_at(NONCE_LEN);
        let nonce = XNonce::from_slice(nonce);
        self.aead
            .decrypt(nonce, body)
            .map_err(|_| SecretError::Unavailable("缓存解密失败（密钥已更换？）".to_string()))
    }
}

/// XChaCha20-Poly1305 的 nonce 长度。
pub const NONCE_LEN: usize = 24;
/// 密钥长度。
pub const KEY_LEN: usize = 32;

fn generate_key() -> Vec<u8> {
    let mut key = vec![0u8; KEY_LEN];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

/// 十六进制编解码（与 `crates/core` 同口径）。
mod hex {
    const TABLE: &[u8; 16] = b"0123456789abcdef";

    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            out.push(char::from(TABLE[usize::from(byte >> 4)]));
            out.push(char::from(TABLE[usize::from(byte & 0x0f)]));
        }
        out
    }

    pub fn decode(text: &str) -> Result<Vec<u8>, String> {
        if text.len() % 2 != 0 {
            return Err("十六进制长度为奇数".to_string());
        }
        let mut out = Vec::with_capacity(text.len() / 2);
        let bytes = text.as_bytes();
        for pair in bytes.chunks(2) {
            let high = value(pair[0])?;
            let low = value(pair[1])?;
            out.push((high << 4) | low);
        }
        Ok(out)
    }

    fn value(byte: u8) -> Result<u8, String> {
        match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            b'A'..=b'F' => Ok(byte - b'A' + 10),
            other => Err(format!("非法十六进制字符 {}", other as char)),
        }
    }
}
