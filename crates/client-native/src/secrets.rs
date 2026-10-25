//! 设备密钥存储 + 敏感缓存的静态加密（原生侧）。
//!
//! 两条边界：
//!
//! **密钥只进这里**。token、缓存加密密钥都走 [`Secrets`]，不写进 SQLite 的
//! 明文列（库文件在应用私有目录里，但备份、越狱设备、调试导出都能整份拿走）。
//!
//! **敏感缓存落盘前加密**。AEAD 与十六进制编解码在
//! [`wxlike_client_core::cache_cipher`] 里，与浏览器共用一份；这里只负责
//! "随机数从哪来"（`rand`）和"密钥存哪"（文件）。
//!
//! 文件形态刻意保持"人能看懂"（`key<TAB>hex`）：真出问题时能手工确认
//! "密钥在不在"，比一个不透明二进制块有用。权限在 Unix 下收紧到 0600，
//! Windows 上依赖用户目录的默认 ACL。

use std::path::{Path, PathBuf};

use rand::RngCore as _;
use wxlike_client_core::cache_cipher::hex;
use wxlike_client_core::cache_cipher::{
    CacheCipher as CoreCipher, CACHE_KEY_NAME, KEY_LEN, NONCE_LEN,
};
use wxlike_client_core::ports::{Secret, SecretError, Secrets};

/// 缓存密钥在 [`Secrets`] 里的键名（供宿主判断"密钥在不在"）。
pub const CACHE_KEY: &str = CACHE_KEY_NAME;

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
            text.push(FIELD_SEP);
            text.push_str(&hex::encode(value));
            text.push(LINE_END);
        }
        std::fs::write(&self.path, text)
            .map_err(|err| SecretError::Unavailable(err.to_string()))?;
        restrict_permissions(&self.path);
        Ok(())
    }
}

/// 字段分隔符（制表符）。用转义常量而不是字面量：编辑器和格式化工具会把
/// 源码里的真制表符改来改去，改了就不报错地换了分隔符。
const FIELD_SEP: char = '\u{9}';
/// 行结束符。
const LINE_END: char = '\u{a}';

fn read_entries(path: &Path) -> Result<Vec<(String, Vec<u8>)>, SecretError> {
    let text =
        std::fs::read_to_string(path).map_err(|err| SecretError::Unavailable(err.to_string()))?;
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(FIELD_SEP) else {
            continue;
        };
        let bytes = hex::decode(value.trim()).ok_or_else(|| {
            SecretError::Unavailable("密钥文件损坏：不是合法十六进制".to_string())
        })?;
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

/// 原生侧的缓存加解密器：nonce 由系统随机数生成，AEAD 与浏览器共用一份。
pub struct CacheCipher {
    inner: CoreCipher,
}

impl CacheCipher {
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
        let inner = CoreCipher::load_or_create(secrets, generate_key).await?;
        Ok(Self { inner })
    }

    /// 加密：`nonce || ciphertext`。
    ///
    /// # Errors
    /// 加密失败（实际不会发生）。
    pub fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, SecretError> {
        let mut nonce = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce);
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

fn generate_key() -> Vec<u8> {
    let mut key = vec![0u8; KEY_LEN];
    rand::thread_rng().fill_bytes(&mut key);
    key
}
