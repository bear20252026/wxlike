use std::time::Instant;

use super::codec::QrCodec;
use super::render;
use super::wire::{QrTarget, MAX_TEXT_BYTES};
use crate::crypto::{NonceStore, Signer};
use crate::error::{DiscoveryError, Result};

const PAYMENT_CONFIRM_TTL_MS: i64 = 5 * 60 * 1000;
const DEFAULT_MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_PIXELS: u64 = 16 * 1024 * 1024;
const DEFAULT_BUDGET_MS: u64 = 5_000;

#[derive(Clone, Debug)]
pub struct ScanLimits {
    pub max_image_bytes: usize,
    pub max_dimension: u32,
    pub max_pixels: u64,
    pub budget_ms: u64,
}

impl Default for ScanLimits {
    fn default() -> Self {
        Self {
            max_image_bytes: DEFAULT_MAX_IMAGE_BYTES,
            max_dimension: render::DEFAULT_MAX_DIMENSION,
            max_pixels: DEFAULT_MAX_PIXELS,
            budget_ms: DEFAULT_BUDGET_MS,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DeeplinkPolicy {
    allowed_external_hosts: Vec<String>,
    allow_plain_text: bool,
}

impl DeeplinkPolicy {
    /// 默认最严：不自动打开任何外部域，也不接受纯文本。
    pub fn strict() -> Self {
        Self::default()
    }

    pub fn with_hosts(hosts: Vec<String>, allow_plain_text: bool) -> Result<Self> {
        for host in &hosts {
            super::wire::validate_allowed_host(host)?;
        }
        Ok(Self {
            allowed_external_hosts: hosts.into_iter().map(|h| h.to_ascii_lowercase()).collect(),
            allow_plain_text,
        })
    }

    pub fn allows_host(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        self.allowed_external_hosts
            .iter()
            .any(|allowed| host == *allowed || host.ends_with(&format!(".{allowed}")))
    }

    pub fn allows_plain_text(&self) -> bool {
        self.allow_plain_text
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanAction {
    /// 好友/群/小程序码原样携带签发目标；支付码要本地签发确认令牌，单列。
    Signed(QrTarget),
    Payment {
        intent_id: String,
        amount_minor: i64,
        currency: String,
        payee_id: String,
        confirm_token: String,
        confirm_expires_at_ms: i64,
    },
    External {
        url: String,
        host: String,
    },
    Text {
        text: String,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanStats {
    pub elapsed_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanResult {
    pub action: ScanAction,
    pub requires_confirmation: bool,
    pub stats: ScanStats,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaymentApproval {
    pub intent_id: String,
    pub amount_minor: i64,
    pub currency: String,
    pub payee_id: String,
    pub approved_at_ms: i64,
}

pub struct QrScanner<N: NonceStore> {
    codec: QrCodec,
    confirm_signer: Signer,
    policy: DeeplinkPolicy,
    nonces: N,
    limits: ScanLimits,
}

impl<N: NonceStore> QrScanner<N> {
    pub fn new(
        codec: QrCodec,
        signing_key: impl AsRef<[u8]>,
        policy: DeeplinkPolicy,
        nonces: N,
    ) -> Self {
        Self {
            codec,
            confirm_signer: Signer::new(signing_key),
            policy,
            nonces,
            limits: ScanLimits::default(),
        }
    }

    pub fn with_limits(mut self, limits: ScanLimits) -> Self {
        self.limits = limits;
        self
    }

    /// 相机帧/相册图片入口。任何情况下都不会自动执行链接；返回的动作
    /// 由 UI 展示后再调用对应确认接口。
    pub fn scan_image(&self, actor: &str, bytes: &[u8], now_ms: i64) -> Result<ScanResult> {
        if bytes.is_empty() || bytes.len() > self.limits.max_image_bytes {
            return Err(DiscoveryError::InvalidArgument("image size"));
        }
        let started = Instant::now();
        let texts = render::decode_strings_with_pixels(
            bytes,
            self.limits.max_dimension,
            self.limits.max_pixels,
        )?;
        let stats = ScanStats {
            elapsed_ms: started.elapsed().as_millis() as u64,
        };
        let Some(raw) = texts.first() else {
            return Err(DiscoveryError::NotFound);
        };
        let mut result = self.scan_text(actor, raw, now_ms)?;
        result.stats = stats;
        Ok(result)
    }

    pub fn scan_text(&self, actor: &str, raw: &str, now_ms: i64) -> Result<ScanResult> {
        let raw = raw.trim();
        if raw.is_empty() || raw.len() > MAX_TEXT_BYTES {
            return Err(DiscoveryError::InvalidArgument("payload size"));
        }
        if raw.starts_with(super::codec::SCHEME_PREFIX) {
            return self.scan_signed(actor, raw, now_ms);
        }
        self.scan_unsigned(raw)
    }

    fn scan_signed(&self, actor: &str, raw: &str, now_ms: i64) -> Result<ScanResult> {
        let decoded = self.codec.decode(raw, now_ms)?;
        match decoded.target {
            QrTarget::Payment {
                intent_id,
                amount_minor,
                currency,
                payee_id,
            } => self.payment_action(
                actor,
                intent_id,
                amount_minor,
                currency,
                payee_id,
                &decoded.nonce,
                decoded.expires_at_ms,
                now_ms,
            ),
            other => Ok(ScanResult {
                action: ScanAction::Signed(other),
                requires_confirmation: true,
                stats: ScanStats::default(),
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn payment_action(
        &self,
        actor: &str,
        intent_id: String,
        amount_minor: i64,
        currency: String,
        payee_id: String,
        nonce: &str,
        expires_at_ms: Option<i64>,
        now_ms: i64,
    ) -> Result<ScanResult> {
        let expires_at_ms = expires_at_ms.unwrap_or(now_ms + PAYMENT_CONFIRM_TTL_MS);
        if !self.nonces.consume(nonce, expires_at_ms, now_ms) {
            return Err(DiscoveryError::Conflict("payment code replayed"));
        }
        let confirm_expires_at_ms = now_ms + PAYMENT_CONFIRM_TTL_MS;
        let canonical = payment_canonical(
            actor,
            &intent_id,
            amount_minor,
            &currency,
            &payee_id,
            confirm_expires_at_ms,
        );
        let confirm_token = format!(
            "{confirm_expires_at_ms}.{}",
            self.confirm_signer.sign(&canonical)
        );
        Ok(ScanResult {
            action: ScanAction::Payment {
                intent_id,
                amount_minor,
                currency,
                payee_id,
                confirm_token,
                confirm_expires_at_ms,
            },
            requires_confirmation: true,
            stats: ScanStats::default(),
        })
    }

    fn scan_unsigned(&self, raw: &str) -> Result<ScanResult> {
        if let Some(rest) = raw.strip_prefix("https://") {
            let host = https_host(rest)?;
            return Ok(ScanResult {
                action: ScanAction::External {
                    url: raw.to_owned(),
                    host,
                },
                requires_confirmation: true,
                stats: ScanStats::default(),
            });
        }
        if raw.contains("://") {
            return Err(DiscoveryError::BlockedExternal("scheme not allowed"));
        }
        if has_uri_scheme(raw) {
            return Err(DiscoveryError::BlockedExternal("scheme not allowed"));
        }
        if !self.policy.allows_plain_text() {
            return Err(DiscoveryError::InvalidArgument("unsupported scheme"));
        }
        Ok(ScanResult {
            action: ScanAction::Text {
                text: raw.to_owned(),
            },
            requires_confirmation: false,
            stats: ScanStats::default(),
        })
    }

    /// 用户确认后调用。域名不在白名单则 BLOCKED_EXTERNAL，绝不自动跳转。
    pub fn execute_external(&self, url: &str) -> Result<String> {
        let rest = url
            .strip_prefix("https://")
            .ok_or(DiscoveryError::BlockedExternal("https required"))?;
        let host = https_host(rest)?;
        if !self.policy.allows_host(&host) {
            return Err(DiscoveryError::BlockedExternal("host not allowed"));
        }
        Ok(url.to_owned())
    }

    /// 校验确认令牌与金额/收款人绑定，返回 G06 可消费的批准对象。
    pub fn approve_payment(
        &self,
        actor: &str,
        intent_id: &str,
        amount_minor: i64,
        currency: &str,
        payee_id: &str,
        confirm_token: &str,
        now_ms: i64,
    ) -> Result<PaymentApproval> {
        let (exp_text, signature) = confirm_token
            .split_once('.')
            .ok_or(DiscoveryError::Unauthorized)?;
        let expires_at_ms: i64 = exp_text.parse().map_err(|_| DiscoveryError::Unauthorized)?;
        if expires_at_ms <= now_ms {
            return Err(DiscoveryError::Expired);
        }
        let canonical = payment_canonical(
            actor,
            intent_id,
            amount_minor,
            currency,
            payee_id,
            expires_at_ms,
        );
        if !self.confirm_signer.verify(&canonical, signature) {
            return Err(DiscoveryError::Unauthorized);
        }
        Ok(PaymentApproval {
            intent_id: intent_id.to_owned(),
            amount_minor,
            currency: currency.to_owned(),
            payee_id: payee_id.to_owned(),
            approved_at_ms: now_ms,
        })
    }
}

fn payment_canonical(
    actor: &str,
    intent_id: &str,
    amount_minor: i64,
    currency: &str,
    payee_id: &str,
    expires_at_ms: i64,
) -> String {
    format!("pay|{actor}|{intent_id}|{amount_minor}|{currency}|{payee_id}|{expires_at_ms}")
}

fn https_host(rest: &str) -> Result<String> {
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|a| !a.is_empty())
        .ok_or(DiscoveryError::InvalidArgument("url host"))?;
    if authority.contains('@') || authority.contains('[') {
        return Err(DiscoveryError::BlockedExternal("ambiguous authority"));
    }
    let host = authority
        .split(':')
        .next()
        .filter(|h| !h.is_empty())
        .ok_or(DiscoveryError::InvalidArgument("url host"))?;
    if !host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
    {
        return Err(DiscoveryError::BlockedExternal("host charset"));
    }
    Ok(host.to_ascii_lowercase())
}

/// `javascript:`/`file:`/`intent:` 这类没有 `//` 的 scheme 同样拒绝；
/// 普通文本若以 ASCII scheme 形状开头，按不安全链接处理而不是当文本。
fn has_uri_scheme(raw: &str) -> bool {
    let Some((scheme, _)) = raw.split_once(':') else {
        return false;
    };
    !scheme.is_empty()
        && scheme.len() <= super::wire::MAX_ID_LEN
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
}
