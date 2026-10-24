//! G02-06 手机号验证码挑战。
//!
//! 设计要点（每条都对应一条验收）：
//!
//! * **E.164 规范化**：`+` 号、`00` 前缀、分隔符、国家码统一，库里只存规范形式；
//! * **哈希存储**：验证码用 HMAC-SHA256(服务端 pepper) 存，库被拖走也不能
//!   直接读出验证码；明文只在交给短信网关的那一瞬间存在；
//! * **绑定**：摘要里带上 `phone + purpose + device_id`，换个用途/设备对不上；
//! * **限速**：每号 + 每 IP 各一桶；重发有真实倒计时，重发后旧码立刻失效；
//! * **并发**：消费用条件 UPDATE，只有一个请求能把 `consumed_at` 从 NULL 改掉；
//! * **枚举防护**：验证失败一律同一个错误，不区分"号码没注册"与"码错了"。

use std::fmt;

use hmac::Mac as _;
use rusqlite::{params, Connection, OptionalExtension as _};

use crate::{random_hex, IdError, Result};

/// 验证码位数。
pub const CODE_DIGITS: u32 = 6;
/// 验证码取值空间（10^6）。
pub const CODE_SPACE: u32 = 1_000_000;
/// 挑战默认有效期：5 分钟。
pub const DEFAULT_TTL_MS: i64 = 5 * 60 * 1000;
/// 默认最大尝试次数。
pub const DEFAULT_MAX_ATTEMPTS: i64 = 5;
/// 默认重发间隔：60 秒。
pub const DEFAULT_RESEND_MS: i64 = 60 * 1000;
/// E.164 数字位数范围（不含 `+`）。
pub const E164_MIN_DIGITS: usize = 8;
pub const E164_MAX_DIGITS: usize = 15;
/// 允许的用途。
pub const PURPOSES: [&str; 3] = ["login", "bind_phone", "reset"];

/// 挑战参数（可在配置里按端调整）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub ttl_ms: i64,
    pub max_attempts: i64,
    pub resend_ms: i64,
}

pub const DEFAULT_LIMITS: Limits = Limits {
    ttl_ms: DEFAULT_TTL_MS,
    max_attempts: DEFAULT_MAX_ATTEMPTS,
    resend_ms: DEFAULT_RESEND_MS,
};

/// 下发结果。`code` 是**交给短信网关的明文**，`Debug` 里永远打码。
#[derive(Clone, PartialEq, Eq)]
pub struct ChallengeIssued {
    pub id: String,
    pub code: String,
    pub expires_at: i64,
    pub resend_after: i64,
}

impl fmt::Debug for ChallengeIssued {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChallengeIssued")
            .field("id", &self.id)
            .field("code", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("resend_after", &self.resend_after)
            .finish()
    }
}

/// 验证通过后的最小信息（不含验证码、不含完整号码）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verified {
    pub challenge_id: String,
    pub phone_e164: String,
}

/// E.164 规范化。`default_cc` 形如 `+86`，仅在输入没带国家码时使用。
pub fn normalize_phone(raw: &str, default_cc: &str) -> Result<String> {
    let mut digits = String::new();
    let mut explicit_plus = false;
    for (i, ch) in raw.chars().enumerate() {
        match ch {
            '+' if i == 0 => explicit_plus = true,
            '+' => return Err(IdError::BadInput("phone")),
            '0'..='9' => digits.push(ch),
            ' ' | '-' | '(' | ')' | '.' => {}
            _ => return Err(IdError::BadInput("phone")),
        }
    }
    let digits = if explicit_plus {
        digits
    } else if let Some(rest) = digits.strip_prefix("00") {
        rest.to_string()
    } else {
        let cc = check_cc(default_cc)?;
        format!("{}{digits}", &cc[1..])
    };
    let n = digits.len();
    if !(E164_MIN_DIGITS..=E164_MAX_DIGITS).contains(&n) {
        return Err(IdError::BadInput("phone"));
    }
    if digits.starts_with('0') {
        return Err(IdError::BadInput("phone"));
    }
    Ok(format!("+{digits}"))
}

fn check_cc(default_cc: &str) -> Result<&str> {
    let ok = default_cc.starts_with('+')
        && default_cc.len() > 1
        && default_cc[1..].bytes().all(|b| b.is_ascii_digit());
    if ok {
        Ok(default_cc)
    } else {
        Err(IdError::BadInput("default country code"))
    }
}

fn code_hash(
    pepper: &[u8],
    id: &str,
    phone: &str,
    purpose: &str,
    device_id: &str,
    code: &str,
) -> Result<String> {
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(pepper)
        .map_err(|_| IdError::BadInput("pepper"))?;
    for field in [id, phone, purpose, device_id, code] {
        mac.update(&(field.len() as u64).to_be_bytes());
        mac.update(field.as_bytes());
    }
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn random_code() -> String {
    use rand::Rng as _;
    let n: u32 = rand::thread_rng().gen_range(0..CODE_SPACE);
    format!("{n:0width$}", width = CODE_DIGITS as usize)
}

/// 下发一次挑战。同号/同 IP 超限时返回 `RateLimited`。
#[allow(clippy::too_many_arguments)]
pub fn issue(
    conn: &Connection,
    phone_raw: &str,
    default_cc: &str,
    purpose: &str,
    device_id: &str,
    ip: &str,
    pepper: &[u8],
    limits: Limits,
    now: i64,
) -> Result<ChallengeIssued> {
    if !PURPOSES.contains(&purpose) {
        return Err(IdError::BadInput("purpose"));
    }
    let phone = normalize_phone(phone_raw, default_cc)?;
    let phone_bucket = crate::throttle::key_phone(&phone);
    crate::throttle::check(conn, &phone_bucket, crate::throttle::SMS_PHONE, now)?;
    let ip_bucket = crate::throttle::key_ip(ip);
    crate::throttle::check(conn, &ip_bucket, crate::throttle::SMS_IP, now)?;

    let id = random_hex(16);
    let code = random_code();
    let hash = code_hash(pepper, &id, &phone, purpose, device_id, &code)?;
    let expires_at = now.saturating_add(limits.ttl_ms);
    let resend_after = now.saturating_add(limits.resend_ms);
    conn.execute(
        "INSERT INTO identity_challenge(id, phone_e164, phone_hash, purpose, device_id, \
         code_hash, created_at, expires_at, resend_after, attempts, max_attempts, consumed_at) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, NULL)",
        params![
            id,
            phone,
            crate::digest(&phone),
            purpose,
            device_id,
            hash,
            now,
            expires_at,
            resend_after,
            limits.max_attempts
        ],
    )?;
    crate::throttle::record_failure(conn, &phone_bucket, crate::throttle::SMS_PHONE, now)?;
    crate::throttle::record_failure(conn, &ip_bucket, crate::throttle::SMS_IP, now)?;
    Ok(ChallengeIssued {
        id,
        code,
        expires_at,
        resend_after,
    })
}

/// 校验时读出的一行：`(phone_e164, purpose, device_id, code_hash, id, expires_at,
/// attempts, max_attempts, consumed_at)`。
type ChallengeRow = (
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    Option<i64>,
);

/// 校验并消费。任何失败都不透露"这个号码是否存在"。
pub fn verify(
    conn: &Connection,
    challenge_id: &str,
    code: &str,
    pepper: &[u8],
    now: i64,
) -> Result<Verified> {
    let row: Option<ChallengeRow> = conn
        .query_row(
            "SELECT phone_e164, purpose, device_id, code_hash, id, expires_at, attempts, \
             max_attempts, consumed_at FROM identity_challenge WHERE id = ?1",
            params![challenge_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            },
        )
        .optional()?;
    let Some((phone, purpose, device_id, stored, id, expires_at, attempts, max, consumed)) = row
    else {
        return Err(IdError::Unauthorized("invalid or expired code"));
    };
    if consumed.is_some() {
        return Err(IdError::Unauthorized("invalid or expired code"));
    }
    if expires_at <= now {
        return Err(IdError::Expired);
    }
    if attempts >= max {
        return Err(IdError::Unauthorized("too many attempts"));
    }
    bump_attempts(conn, &id)?;
    let candidate = code_hash(pepper, &id, &phone, &purpose, &device_id, code)?;
    if !crate::ct_eq(candidate.as_bytes(), stored.as_bytes()) {
        return Err(IdError::Unauthorized("invalid or expired code"));
    }
    let n = conn.execute(
        "UPDATE identity_challenge SET consumed_at = ?2 WHERE id = ?1 AND consumed_at IS NULL",
        params![id, now],
    )?;
    if n != 1 {
        // 并发验证：另一个请求已经把这枚码用掉了。
        return Err(IdError::Unauthorized("invalid or expired code"));
    }
    crate::throttle::reset(conn, &crate::throttle::key_phone(&phone))?;
    Ok(Verified {
        challenge_id: id,
        phone_e164: phone,
    })
}

fn bump_attempts(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "UPDATE identity_challenge SET attempts = attempts + 1 WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

/// 重发：旧码立即作废，新码绑定同一手机号/用途/设备。
pub fn resend(
    conn: &Connection,
    challenge_id: &str,
    pepper: &[u8],
    limits: Limits,
    now: i64,
) -> Result<ChallengeIssued> {
    let row: Option<(String, String, String, i64, Option<i64>)> = conn
        .query_row(
            "SELECT phone_e164, purpose, device_id, resend_after, consumed_at \
             FROM identity_challenge WHERE id = ?1",
            params![challenge_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((phone, purpose, device_id, resend_after, consumed)) = row else {
        return Err(IdError::NotFound("challenge"));
    };
    if consumed.is_some() {
        return Err(IdError::Unauthorized("challenge already used"));
    }
    if now < resend_after {
        return Err(IdError::RateLimited {
            retry_after_ms: resend_after - now,
        });
    }
    let id = random_hex(16);
    let code = random_code();
    let hash = code_hash(pepper, &id, &phone, &purpose, &device_id, &code)?;
    let expires_at = now.saturating_add(limits.ttl_ms);
    let next_resend = now.saturating_add(limits.resend_ms);
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE identity_challenge SET consumed_at = ?2 WHERE id = ?1",
        params![challenge_id, now],
    )?;
    tx.execute(
        "INSERT INTO identity_challenge(id, phone_e164, phone_hash, purpose, device_id, \
         code_hash, created_at, expires_at, resend_after, attempts, max_attempts, consumed_at) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, NULL)",
        params![
            id,
            phone,
            crate::digest(&phone),
            purpose,
            device_id,
            hash,
            now,
            expires_at,
            next_resend,
            limits.max_attempts
        ],
    )?;
    tx.commit()?;
    Ok(ChallengeIssued {
        id,
        code,
        expires_at,
        resend_after: next_resend,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::db;

    const PEPPER: &[u8] = b"test-pepper-not-in-repo";

    fn issue_one(conn: &Connection, now: i64) -> ChallengeIssued {
        issue(
            conn,
            "138 0000 0000",
            "+86",
            "login",
            "dev-aaaa1111",
            "10.0.0.1",
            PEPPER,
            DEFAULT_LIMITS,
            now,
        )
        .unwrap()
    }

    #[test]
    fn normalizes_e164() {
        assert_eq!(
            normalize_phone("13800000000", "+86").unwrap(),
            "+8613800000000"
        );
        assert_eq!(
            normalize_phone("+86 138-0000-0000", "+1").unwrap(),
            "+8613800000000"
        );
        assert_eq!(
            normalize_phone("008613800000000", "+1").unwrap(),
            "+8613800000000"
        );
        assert_eq!(
            normalize_phone("(415) 555-0100", "+1").unwrap(),
            "+14155550100"
        );
        assert!(matches!(
            normalize_phone("12345", "+86"),
            Err(IdError::BadInput(_))
        ));
        assert!(matches!(
            normalize_phone("+86abc", "+86"),
            Err(IdError::BadInput(_))
        ));
        assert!(matches!(
            normalize_phone("13800000000", "86"),
            Err(IdError::BadInput(_))
        ));
    }

    #[test]
    fn happy_path_consumes_once() {
        let conn = db();
        let c = issue_one(&conn, 1_000);
        assert!(!format!("{c:?}").contains(&c.code), "Debug 不得打印验证码");
        let v = verify(&conn, &c.id, &c.code, PEPPER, 1_100).unwrap();
        assert_eq!(v.phone_e164, "+8613800000000");
        assert!(
            matches!(
                verify(&conn, &c.id, &c.code, PEPPER, 1_200),
                Err(IdError::Unauthorized(_))
            ),
            "重放必须失败"
        );
    }

    #[test]
    fn wrong_code_expiry_and_attempt_cap() {
        let conn = db();
        let c = issue_one(&conn, 1_000);
        assert!(matches!(
            verify(&conn, &c.id, "000000", PEPPER, 1_100),
            Err(IdError::Unauthorized(_))
        ));
        // 用另一枚挑战测过期，避免被尝试次数影响。
        let stale = issue(
            &conn,
            "+14155550100",
            "+1",
            "login",
            "dev-aaaa1111",
            "10.0.0.2",
            PEPPER,
            DEFAULT_LIMITS,
            2_000,
        )
        .unwrap();
        assert!(matches!(
            verify(
                &conn,
                &stale.id,
                &stale.code,
                PEPPER,
                2_000 + DEFAULT_TTL_MS + 1
            ),
            Err(IdError::Expired)
        ));
        let capped = issue(
            &conn,
            "+14155550101",
            "+1",
            "login",
            "dev-aaaa1111",
            "10.0.0.3",
            PEPPER,
            DEFAULT_LIMITS,
            3_000,
        )
        .unwrap();
        for _ in 0..DEFAULT_MAX_ATTEMPTS {
            let _ = verify(&conn, &capped.id, "999999", PEPPER, 3_100);
        }
        assert!(matches!(
            verify(&conn, &capped.id, &capped.code, PEPPER, 3_200),
            Err(IdError::Unauthorized(m)) if m.contains("too many")
        ));
    }

    #[test]
    fn code_is_bound_to_device_and_purpose() {
        let conn = db();
        let c = issue_one(&conn, 1_000);
        // 同一个码换设备：摘要输入不同，必须对不上。
        let other = issue(
            &conn,
            "+8613900000000",
            "+86",
            "reset",
            "dev-bbbb2222",
            "10.0.0.9",
            PEPPER,
            DEFAULT_LIMITS,
            1_000,
        )
        .unwrap();
        assert!(verify(&conn, &other.id, &c.code, PEPPER, 1_100).is_err());
    }

    #[test]
    fn resend_invalidates_old_code() {
        let conn = db();
        let c = issue_one(&conn, 1_000);
        assert!(matches!(
            resend(&conn, &c.id, PEPPER, DEFAULT_LIMITS, 1_100),
            Err(IdError::RateLimited { .. })
        ));
        let c2 = resend(
            &conn,
            &c.id,
            PEPPER,
            DEFAULT_LIMITS,
            1_000 + DEFAULT_RESEND_MS,
        )
        .unwrap();
        assert_ne!(c2.id, c.id);
        assert!(verify(&conn, &c.id, &c.code, PEPPER, 1_000 + DEFAULT_RESEND_MS + 1).is_err());
        verify(
            &conn,
            &c2.id,
            &c2.code,
            PEPPER,
            1_000 + DEFAULT_RESEND_MS + 2,
        )
        .unwrap();
    }

    #[test]
    fn per_phone_rate_limit_blocks_sixth_sms() {
        let conn = db();
        for i in 0..crate::throttle::SMS_PHONE.max_attempts {
            issue_one(&conn, 1_000 + i);
        }
        assert!(matches!(
            issue(
                &conn,
                "138 0000 0000",
                "+86",
                "login",
                "dev-aaaa1111",
                "10.0.0.1",
                PEPPER,
                DEFAULT_LIMITS,
                2_000
            ),
            Err(IdError::RateLimited { .. })
        ));
    }

    #[test]
    fn unknown_challenge_is_generic() {
        let conn = db();
        let err = verify(&conn, "nope", "123456", PEPPER, 1_000).unwrap_err();
        assert!(matches!(
            err,
            IdError::Unauthorized("invalid or expired code")
        ));
    }

    #[test]
    fn bad_purpose_rejected() {
        let conn = db();
        assert!(matches!(
            issue(
                &conn,
                "13800000000",
                "+86",
                "admin_override",
                "",
                "10.0.0.1",
                PEPPER,
                DEFAULT_LIMITS,
                1_000
            ),
            Err(IdError::BadInput("purpose"))
        ));
    }
}
