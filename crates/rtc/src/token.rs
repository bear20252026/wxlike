//! 短时媒体令牌（G09-02/G09-05）。
//!
//! 房间令牌只回答一个问题："这次连接是否属于这个 call、这个用户、这个设备、
//! 这个 scope，且签发代次仍然有效"。签名自包含，服务端不需要为每个 token
//! 建表；撤销靠 `generation` 自增（退群/封禁/停播即整代失效）。
//!
//! 格式：`v1.<hex(claims json)>.<hex(HMAC-SHA256)>`。claims 是公开数据，
//! 安全边界完全来自 HMAC；密钥只以 `Redacted` 形式出现在 Debug 输出里。

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// 令牌格式版本；不认识的版本直接拒绝，而不是半解析。
pub const TOKEN_VERSION: u8 = 1;
/// HMAC 密钥最短长度（256 bit）。短于这个长度拒绝构造签名器。
pub const MIN_SECRET_LEN: usize = 32;
/// 允许的客户端时钟偏移：只影响 `issued_at` 的未来值检查。
pub const CLOCK_SKEW_MS: u64 = 60_000;

/// 令牌用途。call 与 live 的权限面必须分开，不能用一个 scope 通吃。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaScope {
    /// 通话房间（上行+下行都受房间状态裁决）。
    Call,
    /// 直播推流（只有主播）。
    LivePublish,
    /// 直播观看（只下行，永不含上行麦克风）。
    LiveWatch,
    /// 直播连麦（允许上行，但需主播单独签发）。
    LiveCohost,
}

impl MediaScope {
    pub fn may_publish(self) -> bool {
        matches!(self, Self::Call | Self::LivePublish | Self::LiveCohost)
    }

    pub fn may_subscribe(self) -> bool {
        true
    }
}

/// 签进令牌的声明。`generation` 与 call/通道当前代次一致才有效。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaClaims {
    pub call: String,
    pub user: String,
    pub device: String,
    pub scope: MediaScope,
    pub generation: u64,
    pub issued_at: u64,
    pub expires_at: u64,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TokenError {
    #[error("token secret too short")]
    WeakSecret,
    #[error("malformed token")]
    Malformed,
    #[error("bad token signature")]
    BadSignature,
    #[error("token expired")]
    Expired,
    #[error("token lifetime invalid")]
    InvalidLifetime,
    #[error("token clock is ahead")]
    ClockAhead,
    #[error("token is for another call")]
    WrongCall,
    #[error("token is for another scope")]
    WrongScope,
    #[error("token generation is stale")]
    StaleGeneration,
}

/// HMAC-SHA256 签名器。克隆代价只是密钥字节；不要把它写进日志。
#[derive(Clone)]
pub struct TokenSigner {
    secret: Vec<u8>,
}

impl std::fmt::Debug for TokenSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSigner")
            .field("secret", &"[redacted]")
            .finish()
    }
}

impl TokenSigner {
    pub fn new(secret: impl AsRef<[u8]>) -> Result<Self, TokenError> {
        let secret = secret.as_ref();
        if secret.len() < MIN_SECRET_LEN {
            return Err(TokenError::WeakSecret);
        }
        Ok(Self {
            secret: secret.to_vec(),
        })
    }

    /// 从测试/部署密钥派生签名器，避免调用方手里再留一份原始字节。
    pub fn from_hex(secret_hex: &str) -> Result<Self, TokenError> {
        let raw = hex::decode(secret_hex).map_err(|_| TokenError::WeakSecret)?;
        Self::new(raw)
    }

    /// 签发一枚带明确寿命的令牌。
    pub fn issue(&self, claims: MediaClaims) -> Result<String, TokenError> {
        if claims.expires_at <= claims.issued_at {
            return Err(TokenError::InvalidLifetime);
        }
        let body = serde_json::to_vec(&claims).map_err(|_| TokenError::Malformed)?;
        let body_hex = hex::encode(body);
        let signing_input = format!("v{TOKEN_VERSION}.{body_hex}");
        let sig = self.sign(signing_input.as_bytes());
        Ok(format!("{signing_input}.{}", hex::encode(sig)))
    }

    /// 便捷签发：`ttl_ms` 由调用方给出，过期时间用饱和加法。
    #[allow(clippy::too_many_arguments)]
    pub fn issue_for(
        &self,
        call: &str,
        user: &str,
        device: &str,
        scope: MediaScope,
        generation: u64,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<String, TokenError> {
        self.issue(MediaClaims {
            call: call.to_string(),
            user: user.to_string(),
            device: device.to_string(),
            scope,
            generation,
            issued_at: now_ms,
            expires_at: now_ms.saturating_add(ttl_ms),
        })
    }

    /// 验签、验version、验时效。
    pub fn verify(&self, token: &str, now_ms: u64) -> Result<MediaClaims, TokenError> {
        let mut parts = token.split('.');
        let (Some(version), Some(body_hex), Some(sig_hex), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(TokenError::Malformed);
        };
        if version != format!("v{TOKEN_VERSION}") {
            return Err(TokenError::Malformed);
        }
        let sig = hex::decode(sig_hex).map_err(|_| TokenError::Malformed)?;
        let signing_input = format!("{version}.{body_hex}");
        self.verify_signature(signing_input.as_bytes(), &sig)?;
        let body = hex::decode(body_hex).map_err(|_| TokenError::Malformed)?;
        let claims: MediaClaims =
            serde_json::from_slice(&body).map_err(|_| TokenError::Malformed)?;
        if claims.expires_at <= now_ms {
            return Err(TokenError::Expired);
        }
        if claims.issued_at > now_ms.saturating_add(CLOCK_SKEW_MS) {
            return Err(TokenError::ClockAhead);
        }
        Ok(claims)
    }

    /// 验签之外再绑定 call/scope/代次。任何一项不符都拒绝。
    pub fn verify_for(
        &self,
        token: &str,
        now_ms: u64,
        call: &str,
        scope: MediaScope,
        generation: u64,
    ) -> Result<MediaClaims, TokenError> {
        let claims = self.verify(token, now_ms)?;
        if claims.call != call {
            return Err(TokenError::WrongCall);
        }
        if claims.scope != scope {
            return Err(TokenError::WrongScope);
        }
        if claims.generation != generation {
            return Err(TokenError::StaleGeneration);
        }
        Ok(claims)
    }

    fn sign(&self, data: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(&self.secret).expect("HMAC accepts any key");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }

    fn verify_signature(&self, data: &[u8], sig: &[u8]) -> Result<(), TokenError> {
        let mut mac = HmacSha256::new_from_slice(&self.secret).expect("HMAC accepts any key");
        mac.update(data);
        mac.verify_slice(sig).map_err(|_| TokenError::BadSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer() -> TokenSigner {
        TokenSigner::new(vec![7u8; MIN_SECRET_LEN]).unwrap()
    }

    fn issue(s: &TokenSigner) -> String {
        s.issue_for(
            "call-1",
            "alice",
            "phone",
            MediaScope::Call,
            3,
            1_000,
            60_000,
        )
        .unwrap()
    }

    #[test]
    fn round_trip_binds_every_field() {
        let s = signer();
        let token = issue(&s);
        let claims = s
            .verify_for(&token, 2_000, "call-1", MediaScope::Call, 3)
            .unwrap();
        assert_eq!(claims.user, "alice");
        assert_eq!(claims.device, "phone");
        assert_eq!(claims.expires_at, 61_000);
    }

    #[test]
    fn tampered_claims_are_rejected() {
        let s = signer();
        let token = issue(&s);
        let mut parts: Vec<&str> = token.split('.').collect();
        let mut body = hex::decode(parts[1]).unwrap();
        let mut claims: MediaClaims = serde_json::from_slice(&body).unwrap();
        claims.user = "mallory".into();
        body = serde_json::to_vec(&claims).unwrap();
        parts[1] = Box::leak(hex::encode(body).into_boxed_str());
        let forged = parts.join(".");
        assert_eq!(s.verify(&forged, 2_000), Err(TokenError::BadSignature));
    }

    #[test]
    fn expiry_is_enforced_at_the_boundary() {
        let s = signer();
        let token = issue(&s);
        assert!(s.verify(&token, 60_999).is_ok());
        assert_eq!(s.verify(&token, 61_000), Err(TokenError::Expired));
    }

    #[test]
    fn binding_mismatches_are_rejected() {
        let s = signer();
        let token = issue(&s);
        assert_eq!(
            s.verify_for(&token, 2_000, "call-2", MediaScope::Call, 3),
            Err(TokenError::WrongCall)
        );
        assert_eq!(
            s.verify_for(&token, 2_000, "call-1", MediaScope::LiveWatch, 3),
            Err(TokenError::WrongScope)
        );
        assert_eq!(
            s.verify_for(&token, 2_000, "call-1", MediaScope::Call, 4),
            Err(TokenError::StaleGeneration)
        );
    }

    #[test]
    fn malformed_and_weak_inputs_fail_closed() {
        assert_eq!(
            TokenSigner::new(vec![1u8; MIN_SECRET_LEN - 1]).unwrap_err(),
            TokenError::WeakSecret
        );
        let s = signer();
        assert_eq!(s.verify("v1.zz", 0), Err(TokenError::Malformed));
        assert_eq!(s.verify("v2.a.b", 0), Err(TokenError::Malformed));
        assert_eq!(s.verify("", 0), Err(TokenError::Malformed));
    }

    #[test]
    fn debug_never_prints_the_secret() {
        let s = TokenSigner::new([0xAB; 40]).unwrap();
        let rendered = format!("{s:?}");
        assert!(rendered.contains("redacted"));
        assert!(!rendered.contains("abab"));
    }
}
