//! 运行配置。
//!
//! 供应商凭据分两套流程：
//! * Sandbox 只允许沙箱密钥，禁止同时带真实 secret 引用；
//! * Live 必须提供 secret 引用，且必须使用真实资金账户。
//!   两者互为排他，避免"沙箱测试打真实通道"或"生产误用测试密钥"。

use std::fmt;

use wxlike_ledger::FundClass;

use crate::error::{CommerceError, Result};

/// 供应商模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderMode {
    /// 沙箱/演示。
    Sandbox,
    /// 真实通道（需要持牌通道与 secret 引用；本 crate 不内置网络实现）。
    Live,
}

/// 供应商凭据配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderCredentials {
    pub mode: ProviderMode,
    /// 生产 secret 的引用名（不是 secret 本身）。
    pub api_secret_ref: Option<String>,
    /// 沙箱密钥标识（同样不是真实凭据）。
    pub sandbox_key: Option<String>,
}

impl ProviderCredentials {
    /// 沙箱配置。
    #[must_use]
    pub fn sandbox() -> Self {
        Self {
            mode: ProviderMode::Sandbox,
            api_secret_ref: None,
            sandbox_key: Some("sandbox".to_string()),
        }
    }

    /// 真实通道配置；只保存引用名。
    #[must_use]
    pub fn live(secret_ref: impl Into<String>) -> Self {
        Self {
            mode: ProviderMode::Live,
            api_secret_ref: Some(secret_ref.into()),
            sandbox_key: None,
        }
    }
}

/// 内存中的密钥字节；Debug 永远不打印内容。
#[derive(Clone)]
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    /// 包装密钥。
    #[must_use]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.0
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretBytes(redacted)")
    }
}

/// commerce 配置。
#[derive(Clone, Debug)]
pub struct CommerceConfig {
    pub fund_class: FundClass,
    pub credentials: ProviderCredentials,
    pub webhook_secret: SecretBytes,
    pub webhook_secret_ref: Option<String>,
    pub qr_secret: SecretBytes,
    pub confirmation_secret: SecretBytes,
    pub max_payment_minor: i64,
    pub max_transfer_minor: i64,
    pub payment_ttl_ms: i64,
    pub transfer_ttl_ms: i64,
    pub redpacket_ttl_ms: i64,
    pub redemption_token_ttl_ms: i64,
    pub callback_skew_ms: i64,
}

impl CommerceConfig {
    /// 沙箱默认配置；密钥只用于本地验签/签名，生产由 G00 从 secret 引用解析。
    #[must_use]
    pub fn sandbox(webhook_secret: impl Into<Vec<u8>>) -> Self {
        Self {
            fund_class: FundClass::Virtual,
            credentials: ProviderCredentials::sandbox(),
            webhook_secret: SecretBytes::new(webhook_secret),
            webhook_secret_ref: None,
            qr_secret: SecretBytes::new(b"wxlike-sandbox-qr".to_vec()),
            confirmation_secret: SecretBytes::new(b"wxlike-sandbox-confirm".to_vec()),
            max_payment_minor: 1_000_000_000,
            max_transfer_minor: 10_000_000_000,
            payment_ttl_ms: 15 * 60 * 1000,
            transfer_ttl_ms: 24 * 60 * 60 * 1000,
            redpacket_ttl_ms: 24 * 60 * 60 * 1000,
            redemption_token_ttl_ms: 5 * 60 * 1000,
            callback_skew_ms: 5 * 60 * 1000,
        }
    }

    /// 真实通道配置。本 crate 不内置网络实现，构造后 vendor 端口由组合根注入。
    #[must_use]
    pub fn live(fund_class: FundClass, secret_ref: impl Into<String>) -> Self {
        let mut config = Self::sandbox(Vec::new());
        config.fund_class = fund_class;
        config.credentials = ProviderCredentials::live(secret_ref);
        config.webhook_secret_ref = config.credentials.api_secret_ref.clone();
        config
    }

    /// 校验凭据与资金性质没有混用。
    ///
    /// # Errors
    /// 模式与凭据/资金性质不匹配，或密钥为空。
    pub fn validate(&self) -> Result<()> {
        match self.credentials.mode {
            ProviderMode::Sandbox => self.validate_sandbox(),
            ProviderMode::Live => self.validate_live(),
        }
    }

    fn validate_sandbox(&self) -> Result<()> {
        if self.fund_class != FundClass::Virtual {
            return Err(CommerceError::BadInput(
                "sandbox provider requires virtual funds".into(),
            ));
        }
        if self.credentials.api_secret_ref.is_some() {
            return Err(CommerceError::BadInput(
                "sandbox provider must not carry a live secret reference".into(),
            ));
        }
        if self.credentials.sandbox_key.is_none() {
            return Err(CommerceError::BadInput("missing sandbox key".into()));
        }
        if self.webhook_secret.is_empty()
            || self.qr_secret.is_empty()
            || self.confirmation_secret.is_empty()
        {
            return Err(CommerceError::BadInput(
                "sandbox signing secrets must be non-empty".into(),
            ));
        }
        Ok(())
    }

    fn validate_live(&self) -> Result<()> {
        if self.fund_class != FundClass::Real {
            return Err(CommerceError::BadInput(
                "live provider requires real funds".into(),
            ));
        }
        if self.credentials.sandbox_key.is_some() {
            return Err(CommerceError::BadInput(
                "live provider must not carry a sandbox key".into(),
            ));
        }
        match self.credentials.api_secret_ref.as_deref() {
            Some(reference) if !reference.is_empty() => {}
            _ => {
                return Err(CommerceError::BadInput(
                    "live provider requires a secret reference".into(),
                ))
            }
        }
        if self.webhook_secret.is_empty() || self.webhook_secret_ref.is_none() {
            return Err(CommerceError::BadInput(
                "live webhook secret must be resolved from a secret reference".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_and_live_credentials_do_not_mix() {
        let mut sandbox = CommerceConfig::sandbox(b"webhook".to_vec());
        sandbox.credentials.api_secret_ref = Some("prod/pay".into());
        assert!(sandbox.validate().is_err());

        let mut live = CommerceConfig::live(FundClass::Real, "prod/pay");
        live.webhook_secret = SecretBytes::new(b"resolved".to_vec());
        assert!(live.validate().is_ok());
        live.credentials.sandbox_key = Some("sandbox".into());
        assert!(live.validate().is_err());
    }

    #[test]
    fn sandbox_must_not_use_real_funds() {
        let mut config = CommerceConfig::sandbox(b"webhook".to_vec());
        config.fund_class = FundClass::Real;
        assert!(config.validate().is_err());
    }

    #[test]
    fn secret_debug_is_redacted() {
        let secret = SecretBytes::new(b"super-secret-value".to_vec());
        assert!(!format!("{secret:?}").contains("super-secret"));
    }
}
