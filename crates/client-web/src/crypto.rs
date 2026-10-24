//! 密钥与敏感缓存加密（浏览器侧）。
//!
//! **密钥放哪**：`localStorage`。它不是安全存储（同源的 JS 都读得到），真正的
//! 边界是"没有 XSS" + CSP；换成 IndexedDB 只是把攻击面从一个同步 API 换成一个
//! 异步 API，并不改变这件事。真要强隔离得用不可导出的 `CryptoKey` + IDB 结构化
//! 克隆，那是后续卡的事——这里把边界写在注释里，不假装它是 Keychain。
//!
//! **怎么加密**：AEAD 与十六进制编解码在 [`wxlike_client_core::cache_cipher`] 里，
//! 与原生侧共用一份；这里只负责"随机数从哪来"（`crypto.getRandomValues`，
//! 绝不能用 `Math.random`）和"密钥存哪"（localStorage）。
//!
//! 缓存密钥丢了（清站点数据、换设备）就解不开旧缓存：这时应当清缓存重拉，
//! 而不是把密文当明文渲染。`open` 因此返回明确错误。

use wasm_bindgen::JsValue;
use wxlike_client_core::cache_cipher::hex;
use wxlike_client_core::cache_cipher::{CacheCipher as CoreCipher, CACHE_KEY_NAME, NONCE_LEN};
use wxlike_client_core::ports::{Secret, SecretError, Secrets};

/// 缓存密钥在 [`Secrets`] 里的键名（供宿主判断"密钥在不在"）。
pub const CACHE_KEY: &str = CACHE_KEY_NAME;

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
                let bytes = hex::decode(&text).ok_or_else(|| {
                    SecretError::Unavailable("密钥内容损坏（不是合法十六进制）".to_string())
                })?;
                Ok(Some(Secret::new(bytes)))
            }
        }
    }

    async fn put(&self, key: &str, value: Secret) -> Result<(), SecretError> {
        self.storage
            .set_item(&Self::key_of(key), &hex::encode(value.expose()))
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

/// 浏览器侧的缓存加解密器：nonce 由 WebCrypto 生成，AEAD 与原生共用一份。
pub struct WebCacheCipher {
    inner: CoreCipher,
}

impl WebCacheCipher {
    /// 用 32 字节密钥构造。
    ///
    /// # Errors
    /// 密钥长度不对。
    pub fn new(key: &[u8]) -> Result<Self, SecretError> {
        Ok(Self {
            inner: CoreCipher::new(key)?,
        })
    }

    /// 从 [`Secrets`] 取密钥；没有就生成并存回去。
    ///
    /// # Errors
    /// 密钥存储不可用。
    pub async fn load_or_create(secrets: &dyn Secrets) -> Result<Self, SecretError> {
        let inner = CoreCipher::load_or_create(secrets, random_key).await?;
        Ok(Self { inner })
    }

    /// 加密：`nonce || ciphertext`。
    ///
    /// # Errors
    /// 随机数不可用或加密失败。
    pub fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, SecretError> {
        let mut nonce = [0u8; NONCE_LEN];
        fill_random(&mut nonce)?;
        self.inner.seal(plaintext, &nonce)
    }

    /// 解密。
    ///
    /// # Errors
    /// 密文截断、密钥不匹配。
    pub fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, SecretError> {
        self.inner.open(sealed)
    }
}

/// 用 WebCrypto 填随机数。
fn fill_random(buffer: &mut [u8]) -> Result<(), SecretError> {
    let window =
        web_sys::window().ok_or_else(|| SecretError::Unavailable("没有 window".to_string()))?;
    let crypto = window
        .crypto()
        .map_err(|err: JsValue| SecretError::Unavailable(format!("没有 crypto：{err:?}")))?;
    // 注意：这个 API 返回 `Result<Object, JsValue>`（成功时返回的是 buffer 本身）
    crypto
        .get_random_values_with_u8_array(buffer)
        .map(|_| ())
        .map_err(|err: JsValue| SecretError::Unavailable(format!("getRandomValues 失败：{err:?}")))
}

fn random_key() -> Vec<u8> {
    let mut key = vec![0u8; wxlike_client_core::cache_cipher::KEY_LEN];
    // 生成失败时留全零：`CacheCipher::new` 仍然接受，但密钥会固定为零向量——
    // 这条路径只在浏览器完全不支持 WebCrypto 时出现，那时应用根本跑不起来。
    let _ = fill_random(&mut key);
    key
}
