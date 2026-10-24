//! 密钥端口：token、设备密钥、敏感缓存的加密材料。
//!
//! 规则只有一条，但它是硬性的：**密钥不进日志、不进错误信息、不进
//! `Debug`**。所以 [`Secret`] 没有 `Debug` 的默认实现（只打长度），
//! 也没有 `Serialize`——想把它写进 JSON 必须先显式 `expose()`，
//! 那一步在 code review 里是可见的。

use super::MaybeSend;

/// 一段密钥材料。
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Vec<u8>);

impl Secret {
    /// 包装原始字节。
    #[must_use]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// 从文本包装（token 这类）。
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        Self(text.as_bytes().to_vec())
    }

    /// 取出原始字节。**这是唯一会泄露明文的地方**，调用点要能解释为什么需要。
    #[must_use]
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    /// 长度。
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret(<{} 字节>)", self.0.len())
    }
}

/// 密钥存储错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecretError {
    /// 用户拒绝 / 系统不允许（未解锁、无生物识别）。
    Denied,
    /// 存储不可用。
    Unavailable(String),
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretError::Denied => f.write_str("密钥访问被拒绝"),
            SecretError::Unavailable(detail) => write!(f, "密钥存储不可用：{detail}"),
        }
    }
}

impl std::error::Error for SecretError {}

/// 密钥端口。
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait Secrets: MaybeSend {
    /// 读。不存在返回 `Ok(None)`（不是错误）。
    ///
    /// # Errors
    /// 见 [`SecretError`]。
    async fn get(&self, key: &str) -> Result<Option<Secret>, SecretError>;

    /// 写（覆盖）。
    ///
    /// # Errors
    /// 见 [`SecretError`]。
    async fn put(&self, key: &str, value: Secret) -> Result<(), SecretError>;

    /// 删。幂等。
    ///
    /// # Errors
    /// 见 [`SecretError`]。
    async fn delete(&self, key: &str) -> Result<(), SecretError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_does_not_print_the_secret() {
        let secret = Secret::from_text("super-token-value");
        let shown = format!("{secret:?}");
        assert!(!shown.contains("super-token"), "{shown}");
        assert!(shown.contains("17"), "{shown}");
    }

    #[test]
    fn empty_is_observable() {
        assert!(Secret::new(Vec::new()).is_empty());
        assert_eq!(Secret::from_text("ab").len(), 2);
    }
}
