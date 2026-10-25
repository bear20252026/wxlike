use serde::{Deserialize, Serialize};

use crate::error::{DiscoveryError, Result};

pub const MAX_PAYLOAD_BYTES: usize = 2048;
pub const MAX_ID_LEN: usize = 128;
pub const MAX_PATH_LEN: usize = 256;
pub const MAX_TEXT_BYTES: usize = 4096;
pub const MAX_AMOUNT_MINOR: i64 = 100_000_000_00;

/// 业务二维码分型。好友/群/支付/小程序码必须分型解析，不能共用一个
/// "打开任意链接"入口。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QrTarget {
    Friend {
        user_id: String,
    },
    Group {
        group_id: String,
    },
    Payment {
        intent_id: String,
        amount_minor: i64,
        currency: String,
        payee_id: String,
    },
    MiniApp {
        app_id: String,
        path: String,
    },
}

impl QrTarget {
    pub fn kind(&self) -> QrKind {
        match self {
            Self::Friend { .. } => QrKind::Friend,
            Self::Group { .. } => QrKind::Group,
            Self::Payment { .. } => QrKind::Payment,
            Self::MiniApp { .. } => QrKind::MiniApp,
        }
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Friend { user_id } => validate_id(user_id),
            Self::Group { group_id } => validate_id(group_id),
            Self::Payment {
                intent_id,
                amount_minor,
                currency,
                payee_id,
            } => validate_payment(intent_id, *amount_minor, currency, payee_id),
            Self::MiniApp { app_id, path } => {
                validate_id(app_id)?;
                validate_miniapp_path(path)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QrKind {
    Friend,
    Group,
    Payment,
    MiniApp,
}

impl QrKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Friend => "friend",
            Self::Group => "group",
            Self::Payment => "payment",
            Self::MiniApp => "miniapp",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "friend" => Some(Self::Friend),
            "group" => Some(Self::Group),
            "payment" => Some(Self::Payment),
            "miniapp" => Some(Self::MiniApp),
            _ => None,
        }
    }
}

pub fn validate_id(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_ID_LEN {
        return Err(DiscoveryError::InvalidArgument("id length"));
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':' | b'@'))
    {
        return Err(DiscoveryError::InvalidArgument("id charset"));
    }
    Ok(())
}

pub fn validate_miniapp_path(path: &str) -> Result<()> {
    if !path.starts_with('/') || path.len() > MAX_PATH_LEN {
        return Err(DiscoveryError::InvalidArgument("miniapp path"));
    }
    if path.contains("..") || path.bytes().any(|b| b.is_ascii_control()) {
        return Err(DiscoveryError::InvalidArgument("miniapp path traversal"));
    }
    Ok(())
}

fn validate_payment(
    intent_id: &str,
    amount_minor: i64,
    currency: &str,
    payee_id: &str,
) -> Result<()> {
    validate_id(intent_id)?;
    validate_id(payee_id)?;
    if amount_minor <= 0 || amount_minor > MAX_AMOUNT_MINOR {
        return Err(DiscoveryError::InvalidArgument("payment amount"));
    }
    if currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(DiscoveryError::InvalidArgument("payment currency"));
    }
    Ok(())
}

/// 域名白名单按后缀匹配：`wxlike.com` 覆盖 `a.wxlike.com`，不覆盖
/// `evilwxlike.com`；条目本身不是 URL，拒绝带 scheme/路径的写法。
pub fn validate_allowed_host(host: &str) -> Result<()> {
    if host.is_empty()
        || host.len() > MAX_ID_LEN
        || host.contains(['/', '@', ':'])
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
    {
        return Err(DiscoveryError::InvalidArgument("allowed host"));
    }
    Ok(())
}
