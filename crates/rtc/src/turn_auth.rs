//! G09-03 coturn 短期凭据与配额。
//!
//! 生产环境用 coturn 的 REST 短凭据（`use-auth-secret`）：
//!
//! ```text
//! username = "<expiry-unix>:<user>:<device>"
//! password = base64(HMAC-SHA1(shared_secret, username))
//! ```
//!
//! 这样 TURN 服务器不查我们的库，Kubernetes/coturn 侧只需共享一个 secret；
//! 过期时间写进用户名，无法被客户端延长。每用户并发分配数与流量配额在
//! 签发侧限制，避免一个客户端把中继带宽吃光。
//!
//! 注意：媒体令牌（[`crate::token`]）与 TURN 凭据是两套东西——前者约束
//! 房间内的发布/订阅，后者只约束中继网络资源。

use std::collections::HashMap;

use hmac::{Hmac, Mac};
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

/// coturn 共享 secret 的最短长度。
pub const MIN_TURN_SECRET_LEN: usize = 16;
/// 默认凭据寿命：1 小时。
pub const DEFAULT_TTL_SECS: u64 = 3_600;
/// 上限：防止签发一张"永久有效"的凭据。
pub const MAX_TTL_SECS: u64 = 6 * 3_600;

#[derive(Clone, PartialEq, Eq, serde::Serialize)]
pub struct TurnCredentials {
    pub username: String,
    pub password: String,
    pub ttl_secs: u64,
    pub expires_at_unix: u64,
}

impl std::fmt::Debug for TurnCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnCredentials")
            .field("username", &self.username)
            .field("password", &"[redacted]")
            .field("ttl_secs", &self.ttl_secs)
            .field("expires_at_unix", &self.expires_at_unix)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnGrant {
    pub user: String,
    pub device: String,
    pub expires_at_unix: u64,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TurnAuthError {
    #[error("turn secret is too short")]
    WeakSecret,
    #[error("ttl must be within 1..={MAX_TTL_SECS} seconds")]
    InvalidTtl,
    #[error("malformed turn username")]
    Malformed,
    #[error("turn credential expired")]
    Expired,
    #[error("bad turn credential signature")]
    BadSignature,
}

/// SHA-1 摘要宽度（coturn 协议固定）。
const SHA1_LEN: usize = 20;
/// base64 把 24 bit 分成 3 段 8 bit：第一段移 18，第二段移 12。
const B64_SHIFT_FIRST: u32 = 18;
const B64_SHIFT_SECOND: u32 = 12;

fn hmac_sha1(secret: &[u8], data: &[u8]) -> [u8; SHA1_LEN] {
    let mut mac = HmacSha1::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// 标准 base64（带 padding）。只用于 coturn 协议，不引入额外依赖。
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> B64_SHIFT_FIRST) as usize & 0x3F] as char);
        out.push(ALPHABET[(n >> B64_SHIFT_SECOND) as usize & 0x3F] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 0x3F] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 0x3F] as char
        } else {
            '='
        });
    }
    out
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 签发短期凭据。`now_ms` 由调用方传入，便于测试。
pub fn issue(
    secret: &[u8],
    user: &str,
    device: &str,
    now_ms: u64,
    ttl_secs: u64,
) -> Result<TurnCredentials, TurnAuthError> {
    if secret.len() < MIN_TURN_SECRET_LEN {
        return Err(TurnAuthError::WeakSecret);
    }
    if ttl_secs == 0 || ttl_secs > MAX_TTL_SECS {
        return Err(TurnAuthError::InvalidTtl);
    }
    if user.is_empty() || device.is_empty() || user.contains(':') || device.contains(':') {
        return Err(TurnAuthError::Malformed);
    }
    let expires_at_unix = now_ms / 1_000 + ttl_secs;
    let username = format!("{expires_at_unix}:{user}:{device}");
    let password = base64_encode(&hmac_sha1(secret, username.as_bytes()));
    Ok(TurnCredentials {
        username,
        password,
        ttl_secs,
        expires_at_unix,
    })
}

/// 校验凭据（coturn 侧等价逻辑）：解析过期时间、重算 HMAC、常数时间比较。
pub fn verify(
    secret: &[u8],
    username: &str,
    password: &str,
    now_ms: u64,
) -> Result<TurnGrant, TurnAuthError> {
    if secret.len() < MIN_TURN_SECRET_LEN {
        return Err(TurnAuthError::WeakSecret);
    }
    let mut parts = username.splitn(3, ':');
    let (Some(exp), Some(user), Some(device), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(TurnAuthError::Malformed);
    };
    if user.is_empty() || device.is_empty() {
        return Err(TurnAuthError::Malformed);
    }
    let expires_at_unix: u64 = exp.parse().map_err(|_| TurnAuthError::Malformed)?;
    if now_ms / 1_000 >= expires_at_unix {
        return Err(TurnAuthError::Expired);
    }
    let expected = base64_encode(&hmac_sha1(secret, username.as_bytes()));
    if !constant_time_eq(expected.as_bytes(), password.as_bytes()) {
        return Err(TurnAuthError::BadSignature);
    }
    Ok(TurnGrant {
        user: user.to_string(),
        device: device.to_string(),
        expires_at_unix,
    })
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum QuotaError {
    #[error("too many concurrent TURN allocations")]
    TooManyAllocations,
    #[error("TURN byte budget exceeded for the window")]
    ByteBudgetExceeded,
}

#[derive(Default)]
struct Usage {
    allocations: usize,
    window_start_ms: u64,
    bytes: u64,
}

/// 每用户中继配额。窗口按小时滚动；同一进程内即可裁决。
pub struct TurnQuota {
    pub max_allocations: usize,
    pub max_bytes_per_window: u64,
    pub window_ms: u64,
    users: HashMap<String, Usage>,
}

impl TurnQuota {
    pub fn new(max_allocations: usize, max_bytes_per_window: u64, window_ms: u64) -> Self {
        Self {
            max_allocations,
            max_bytes_per_window,
            window_ms: window_ms.max(1),
            users: HashMap::new(),
        }
    }

    /// 生产默认：每用户 2 路中继、每小时 2 GiB。
    pub fn production() -> Self {
        Self::new(2, 2 * 1024 * 1024 * 1024, 3_600_000)
    }

    pub fn reserve(&mut self, user: &str, now_ms: u64) -> Result<(), QuotaError> {
        let max = self.max_allocations;
        let u = self.entry(user, now_ms);
        if u.allocations >= max {
            return Err(QuotaError::TooManyAllocations);
        }
        u.allocations += 1;
        Ok(())
    }

    pub fn release(&mut self, user: &str) {
        if let Some(u) = self.users.get_mut(user) {
            u.allocations = u.allocations.saturating_sub(1);
        }
    }

    pub fn account(&mut self, user: &str, bytes: u64, now_ms: u64) -> Result<(), QuotaError> {
        let limit = self.max_bytes_per_window;
        let u = self.entry(user, now_ms);
        if u.bytes.saturating_add(bytes) > limit {
            return Err(QuotaError::ByteBudgetExceeded);
        }
        u.bytes += bytes;
        Ok(())
    }

    pub fn usage(&self, user: &str) -> (usize, u64) {
        self.users
            .get(user)
            .map(|u| (u.allocations, u.bytes))
            .unwrap_or((0, 0))
    }

    fn entry(&mut self, user: &str, now_ms: u64) -> &mut Usage {
        let window = self.window_ms;
        let u = self.users.entry(user.to_string()).or_default();
        if now_ms.saturating_sub(u.window_start_ms) >= window {
            u.window_start_ms = now_ms;
            u.bytes = 0;
        }
        u
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

    #[test]
    fn credentials_round_trip_and_bind_identity() {
        let c = issue(SECRET, "alice", "phone", 1_000_000, 600).unwrap();
        assert_eq!(c.expires_at_unix, 1_000 + 600);
        assert!(c.username.starts_with("1600:alice:phone"));
        let grant = verify(SECRET, &c.username, &c.password, 1_500_000).unwrap();
        assert_eq!(grant.user, "alice");
        assert_eq!(grant.device, "phone");
    }

    #[test]
    fn debug_never_prints_the_turn_password() {
        let c = issue(SECRET, "alice", "phone", 1_000_000, 600).unwrap();
        let rendered = format!("{c:?}");
        assert!(rendered.contains("[redacted]"));
        assert!(!rendered.contains(&c.password));
    }

    #[test]
    fn expiry_and_tampering_fail_closed() {
        let c = issue(SECRET, "alice", "phone", 1_000_000, 60).unwrap();
        assert_eq!(
            verify(SECRET, &c.username, &c.password, 1_061_000),
            Err(TurnAuthError::Expired)
        );
        assert_eq!(
            verify(SECRET, "9999999999:mallory:phone", &c.password, 1_000_000),
            Err(TurnAuthError::BadSignature)
        );
        assert_eq!(
            verify(b"another-secret-16", &c.username, &c.password, 1_000_000),
            Err(TurnAuthError::BadSignature)
        );
        assert_eq!(
            verify(SECRET, "no-colons", &c.password, 1_000_000),
            Err(TurnAuthError::Malformed)
        );
    }

    #[test]
    fn weak_secret_and_bad_ttl_are_refused() {
        assert_eq!(
            issue(b"short", "alice", "phone", 0, 60),
            Err(TurnAuthError::WeakSecret)
        );
        assert_eq!(
            issue(SECRET, "alice", "phone", 0, 0),
            Err(TurnAuthError::InvalidTtl)
        );
        assert_eq!(
            issue(SECRET, "alice", "phone", 0, MAX_TTL_SECS + 1),
            Err(TurnAuthError::InvalidTtl)
        );
        assert_eq!(
            issue(SECRET, "a:b", "phone", 0, 60),
            Err(TurnAuthError::Malformed)
        );
    }

    #[test]
    fn quota_caps_allocations_and_bytes_with_a_rolling_window() {
        let mut q = TurnQuota::new(2, 1_000, 60_000);
        q.reserve("alice", 0).unwrap();
        q.reserve("alice", 0).unwrap();
        assert_eq!(q.reserve("alice", 0), Err(QuotaError::TooManyAllocations));
        q.release("alice");
        q.reserve("alice", 0).unwrap();

        q.account("alice", 600, 0).unwrap();
        assert_eq!(
            q.account("alice", 500, 1_000),
            Err(QuotaError::ByteBudgetExceeded)
        );
        // 窗口滚动后额度恢复（分配数不清零，仍在配额内）
        assert_eq!(q.account("alice", 900, 60_000), Ok(()));
        assert_eq!(q.usage("alice").1, 900);
    }

    #[test]
    fn quota_is_per_user() {
        let mut q = TurnQuota::new(1, 100, 60_000);
        q.reserve("alice", 0).unwrap();
        assert!(q.reserve("bob", 0).is_ok());
        assert_eq!(q.usage("bob"), (1, 0));
    }
}
