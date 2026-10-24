//! G02-01 登录/验证码限速：固定窗口 + 封禁。
//!
//! 口径刻意简单（一行一桶），因为它的目的是**挡住暴力枚举**，不是精确计费：
//! 窗口内超过阈值就整桶封禁一段时间，成功一次即清零。桶键由调用方给（IP、
//! 账号、手机号分别一桶），本模块不猜业务。

use rusqlite::{params, Connection, OptionalExtension as _};

use crate::Result;

/// 限速参数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limit {
    pub max_attempts: i64,
    pub window_ms: i64,
    pub block_ms: i64,
}

/// 5 分钟窗口（登录/轮询共用）。
const WINDOW_SHORT_MS: i64 = 5 * 60 * 1000;
/// 1 小时窗口（短信下发共用）。
const WINDOW_HOUR_MS: i64 = 60 * 60 * 1000;
/// 登录失败封禁时长。
const LOGIN_BLOCK_MS: i64 = 15 * 60 * 1000;
/// 扫码轮询封禁时长。
const POLL_BLOCK_MS: i64 = 60 * 1000;
/// 同 IP 每小时的短信上限。
const SMS_IP_MAX: i64 = 20;
/// 同 challenge 每 5 分钟的轮询上限（约 2 次/秒）。
const QR_POLL_MAX: i64 = 600;

/// 密码登录：同 IP 5 次 / 5 分钟，封 15 分钟。
pub const LOGIN_IP: Limit = Limit {
    max_attempts: 5,
    window_ms: WINDOW_SHORT_MS,
    block_ms: LOGIN_BLOCK_MS,
};
/// 密码登录：同账号 10 次 / 5 分钟，封 15 分钟（比 IP 宽，防误伤共用出口）。
pub const LOGIN_USER: Limit = Limit {
    max_attempts: 10,
    window_ms: WINDOW_SHORT_MS,
    block_ms: LOGIN_BLOCK_MS,
};
/// 短信下发：同号 5 次 / 小时。
pub const SMS_PHONE: Limit = Limit {
    max_attempts: 5,
    window_ms: WINDOW_HOUR_MS,
    block_ms: WINDOW_HOUR_MS,
};
/// 短信下发：同 IP 每小时上限。
pub const SMS_IP: Limit = Limit {
    max_attempts: SMS_IP_MAX,
    window_ms: WINDOW_HOUR_MS,
    block_ms: WINDOW_HOUR_MS,
};
/// 扫码轮询：同 challenge 每 5 分钟上限，防刷。
pub const QR_POLL: Limit = Limit {
    max_attempts: QR_POLL_MAX,
    window_ms: WINDOW_SHORT_MS,
    block_ms: POLL_BLOCK_MS,
};

/// 桶键构造：前缀区分维度，避免不同维度的值互相命中。
pub fn key_ip(ip: &str) -> String {
    format!("ip:{ip}")
}

pub fn key_user(user_id: &str) -> String {
    format!("user:{user_id}")
}

pub fn key_phone(e164: &str) -> String {
    format!("phone:{e164}")
}

pub fn key_challenge(id: &str) -> String {
    format!("challenge:{id}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub attempts: i64,
    pub blocked_until: i64,
}

/// 当前是否被挡。被封时返回 `RateLimited`，`retry_after_ms` 可直接回给客户端。
pub fn check(conn: &Connection, bucket: &str, limit: Limit, now: i64) -> Result<()> {
    let row: Option<(i64, i64, i64)> = conn
        .query_row(
            "SELECT window_start, attempts, blocked_until FROM identity_throttle WHERE bucket = ?1",
            params![bucket],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((window_start, attempts, blocked_until)) = row else {
        return Ok(());
    };
    if blocked_until > now {
        return Err(crate::IdError::RateLimited {
            retry_after_ms: blocked_until - now,
        });
    }
    if now - window_start >= limit.window_ms {
        return Ok(());
    }
    if attempts >= limit.max_attempts {
        return Err(crate::IdError::RateLimited {
            retry_after_ms: limit.block_ms,
        });
    }
    Ok(())
}

/// 记一次失败：窗口过期就重开，达标即封。
pub fn record_failure(conn: &Connection, bucket: &str, limit: Limit, now: i64) -> Result<Status> {
    let tx = conn.unchecked_transaction()?;
    let row: Option<(i64, i64)> = tx
        .query_row(
            "SELECT window_start, attempts FROM identity_throttle WHERE bucket = ?1",
            params![bucket],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (start, attempts) = match row {
        Some((start, attempts)) if now - start < limit.window_ms => (start, attempts + 1),
        _ => (now, 1),
    };
    let blocked_until = if attempts >= limit.max_attempts {
        now.saturating_add(limit.block_ms)
    } else {
        0
    };
    tx.execute(
        "INSERT INTO identity_throttle(bucket, window_start, attempts, blocked_until) \
         VALUES(?1, ?2, ?3, ?4) \
         ON CONFLICT(bucket) DO UPDATE SET window_start = ?2, attempts = ?3, blocked_until = ?4",
        params![bucket, start, attempts, blocked_until],
    )?;
    tx.commit()?;
    Ok(Status {
        attempts,
        blocked_until,
    })
}

/// 成功一次即清零（不惩罚正常用户）。
pub fn reset(conn: &Connection, bucket: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM identity_throttle WHERE bucket = ?1",
        params![bucket],
    )?;
    Ok(())
}

/// 桶现状（运维视图 / 测试断言）。
pub fn status(conn: &Connection, bucket: &str) -> Result<Option<Status>> {
    Ok(conn
        .query_row(
            "SELECT attempts, blocked_until FROM identity_throttle WHERE bucket = ?1",
            params![bucket],
            |r| {
                Ok(Status {
                    attempts: r.get(0)?,
                    blocked_until: r.get(1)?,
                })
            },
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::db;

    #[test]
    fn blocks_after_threshold_and_recovers_after_window() {
        let conn = db();
        let bucket = key_ip("203.0.113.9");
        for i in 0..LOGIN_IP.max_attempts {
            check(&conn, &bucket, LOGIN_IP, 1_000 + i).unwrap();
            record_failure(&conn, &bucket, LOGIN_IP, 1_000 + i).unwrap();
        }
        let err = check(&conn, &bucket, LOGIN_IP, 2_000).unwrap_err();
        assert!(matches!(err, crate::IdError::RateLimited { .. }));
        // 封禁到期后放行（窗口也一起过去了）。
        let later = 1_000 + LOGIN_IP.block_ms + LOGIN_IP.window_ms;
        check(&conn, &bucket, LOGIN_IP, later).unwrap();
    }

    #[test]
    fn success_clears_the_bucket() {
        let conn = db();
        let bucket = key_user("u1");
        record_failure(&conn, &bucket, LOGIN_USER, 1_000).unwrap();
        assert_eq!(status(&conn, &bucket).unwrap().unwrap().attempts, 1);
        reset(&conn, &bucket).unwrap();
        assert!(status(&conn, &bucket).unwrap().is_none());
    }

    #[test]
    fn new_window_starts_over() {
        let conn = db();
        let bucket = key_phone("+8613800000000");
        for _ in 0..3 {
            record_failure(&conn, &bucket, SMS_PHONE, 1_000).unwrap();
        }
        let after = 1_000 + SMS_PHONE.window_ms + 1;
        let s = record_failure(&conn, &bucket, SMS_PHONE, after).unwrap();
        assert_eq!(s.attempts, 1, "窗口过期后计数必须重开，否则用户永远被挡");
    }

    #[test]
    fn empty_bucket_is_open() {
        let conn = db();
        check(&conn, "ip:none", LOGIN_IP, 1).unwrap();
        assert!(status(&conn, "ip:none").unwrap().is_none());
    }
}
