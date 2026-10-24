//! G02-01 找回流程：**不泄漏账号是否存在**。
//!
//! 这个模块的形状本身就是契约：对外只有一个固定响应。调用方必须先调
//! [`should_issue`] 判断是否要真的发信/发短信，然后**无论如何**返回同一个
//! [`response`]。账号不存在、被限速、刚发过——用户看到的完全一样。
//!
//! 唯一的差别是后台有没有真的发一封邮件，这个差别不能出现在响应体、状态码
//! 或响应时间上（发送走异步队列，不在请求路径里等）。

use rusqlite::{params, Connection};

use crate::{digest, random_hex, Result, TOKEN_BYTES};

/// 找回链接有效期：30 分钟。
pub const RESET_TTL_MS: i64 = 30 * 60 * 1000;
/// 重发间隔：60 秒。
pub const RESET_RESEND_MS: i64 = 60 * 1000;

/// 对外唯一响应。**字段不随账号是否存在变化。**
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResetResponse {
    pub accepted: bool,
    pub retry_after_ms: i64,
}

/// 固定响应：永远"已受理"。
pub fn response() -> ResetResponse {
    ResetResponse {
        accepted: true,
        retry_after_ms: RESET_RESEND_MS,
    }
}

/// 是否允许真的下发一次找回凭据（限速桶由调用方给：按提交的标识符建桶）。
///
/// 返回 `false` 时调用方仍然返回 [`response`]，只是不排队发送。
pub fn should_issue(
    conn: &Connection,
    bucket: &str,
    limit: crate::throttle::Limit,
    now: i64,
) -> Result<bool> {
    match crate::throttle::check(conn, bucket, limit, now) {
        Ok(()) => {
            crate::throttle::record_failure(conn, bucket, limit, now)?;
            Ok(true)
        }
        Err(crate::IdError::RateLimited { .. }) => Ok(false),
        Err(other) => Err(other),
    }
}

/// 给已确认存在的账号签发找回令牌（库里只存摘要）。重复调用各自独立，互不覆盖。
pub fn issue_token(conn: &Connection, user_id: &str, now: i64) -> Result<String> {
    let raw = random_hex(TOKEN_BYTES);
    conn.execute(
        "INSERT INTO identity_reset_token(token_hash, user_id, created_at, expires_at, used_at) \
         VALUES(?1, ?2, ?3, ?4, NULL)",
        params![digest(&raw), user_id, now, now.saturating_add(RESET_TTL_MS)],
    )?;
    Ok(raw)
}

/// 消费找回令牌：单次、限时。成功后同一账号其余令牌一并作废。
pub fn consume_token(conn: &Connection, raw: &str, now: i64) -> Result<String> {
    let tx = conn.unchecked_transaction()?;
    let n = tx.execute(
        "UPDATE identity_reset_token SET used_at = ?2 \
         WHERE token_hash = ?1 AND used_at IS NULL AND expires_at > ?2",
        params![digest(raw), now],
    )?;
    if n != 1 {
        return Err(crate::IdError::Unauthorized(
            "invalid or expired reset token",
        ));
    }
    let user_id: String = tx.query_row(
        "SELECT user_id FROM identity_reset_token WHERE token_hash = ?1",
        params![digest(raw)],
        |r| r.get(0),
    )?;
    tx.execute(
        "UPDATE identity_reset_token SET used_at = ?2 \
         WHERE user_id = ?1 AND used_at IS NULL",
        params![user_id, now],
    )?;
    tx.commit()?;
    Ok(user_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::db;

    #[test]
    fn response_is_identical_for_unknown_accounts() {
        let conn = db();
        let known = should_issue(&conn, "id:alice", crate::throttle::SMS_PHONE, 1_000).unwrap();
        let unknown = should_issue(&conn, "id:nobody", crate::throttle::SMS_PHONE, 1_000).unwrap();
        assert!(known && unknown);
        assert_eq!(response(), response());
        assert!(response().accepted);
    }

    #[test]
    fn throttled_still_looks_accepted() {
        let conn = db();
        let limit = crate::throttle::Limit {
            max_attempts: 1,
            window_ms: 60_000,
            block_ms: 60_000,
        };
        assert!(should_issue(&conn, "id:alice", limit, 1_000).unwrap());
        assert!(!should_issue(&conn, "id:alice", limit, 2_000).unwrap());
        // 对外仍是"已受理"。
        assert!(response().accepted);
    }

    #[test]
    fn token_is_single_use_and_short_lived() {
        let conn = db();
        let raw = issue_token(&conn, "u1", 1_000).unwrap();
        assert_eq!(consume_token(&conn, &raw, 1_100).unwrap(), "u1");
        assert!(consume_token(&conn, &raw, 1_200).is_err());

        let stale = issue_token(&conn, "u1", 2_000).unwrap();
        assert!(consume_token(&conn, &stale, 2_000 + RESET_TTL_MS + 1).is_err());
        assert!(consume_token(&conn, "forged", 2_100).is_err());
    }

    #[test]
    fn consuming_one_token_kills_the_others() {
        let conn = db();
        let a = issue_token(&conn, "u1", 1_000).unwrap();
        let b = issue_token(&conn, "u1", 1_000).unwrap();
        consume_token(&conn, &a, 1_100).unwrap();
        assert!(consume_token(&conn, &b, 1_200).is_err());
    }
}
