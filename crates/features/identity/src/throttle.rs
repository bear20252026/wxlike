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

// ───────────────────────────────────────────────────────────────────────────
// 重试预算（借 Tinode `server/auth/code/auth_code.go` 与
// `server/validate/email/validate.go` 的计数语义）
//
// 上面那套固定窗口桶管的是「**多久能再发一次**」。这里管的是另一件事：
// 「**这一枚凭据一共能被试错几次**」。两者的生命周期不同，混用必出洞：
//
// - 窗口桶：键是 IP / 账号 / 手机号；窗口滑走或验证成功即重置；到上限封一段时间。
// - 预算桶：键是**凭据本身**（`method:value`）；只有重新下发凭据或验证成功才归零；
//   到上限就随凭据一起死，没有时间窗滑走后自动放行这回事。
//
// 上游 `auth_code.Authenticate` 读的就是 `"code:count:uid"` 这一个值：计数与码
// 同生共死、先比上限再比码、成功即 `Delete`。本平台把它单独做成一张表，是因为
// 我们的码存在 `identity_challenge` 里，而计数若也只存在那一行，**重发**（resend
// 换一行新 id）就会把计数一起洗掉——那正是上游用「key = 凭据」避开的问题。
// ───────────────────────────────────────────────────────────────────────────

/// 上限的两种口径。**上游两处本来就不一样，不许合并成一种**：
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cap {
    /// `auth_code`：`count >= maxRetries` 即拒——到上限就封，最后一次都不给试。
    AtCap,
    /// `validate/email`：`cred.Retries > MaxRetries` 才拒——多给一次机会。
    AfterCap,
}

/// 一枚凭据的重试预算。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    pub max_retries: i64,
    pub cap: Cap,
    /// 预算的存活时长：与凭据本身的 TTL 同量级，过期即随行（可重新下发）。
    pub ttl_ms: i64,
}

/// 默认预算：同一凭据上累计 15 次试错（`AtCap`，按 `auth_code` 的口径）。
///
/// 单枚挑战自己还有 `identity_challenge.max_attempts`（默认 5）这一层，先它触发；
/// 这一层拦的是**跨重发累计**：换 3 枚码各试错 5 次，在上游是一枚码 5 次就到顶，
/// 在我们的 `resend` 语义下如果不加这层，就变成「无限次重发即无限次试」。
pub const CODE_BUDGET: Budget = Budget {
    max_retries: 15,
    cap: Cap::AtCap,
    ttl_ms: 60 * 60 * 1000,
};

/// 预算清扫的批量上限（调用方与测试共用，避免各处写死魔法数）。
pub const SWEEP_LIMIT: i64 = 1_000;

/// 预算桶键：按**凭据**（方法 + 值）而不是按单次挑战。
pub fn key_credential(method: &str, value: &str) -> String {
    format!("budget:{method}:{value}")
}

/// 预算是否已经见底。**必须在比对凭据之前调用**（上游就是这个顺序：
/// `count >= maxRetries → ErrFailed`，压根不去比码）。反过来做的话，
/// 「已经锁住」与「码不对」的耗时会不一样，等于给攻击者一根探针。
pub fn check_budget(conn: &Connection, bucket: &str, budget: Budget, now: i64) -> Result<()> {
    let row: Option<(i64, i64)> = conn
        .query_row(
            "SELECT attempts, expires_at FROM identity_retry WHERE bucket = ?1",
            params![bucket],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((attempts, expires_at)) = row else {
        return Ok(());
    };
    if expires_at <= now {
        // 预算随行（凭据本来也已经作废）：清掉，让下一次下发从 0 开始。
        conn.execute(
            "DELETE FROM identity_retry WHERE bucket = ?1",
            params![bucket],
        )?;
        return Ok(());
    }
    let exhausted = match budget.cap {
        Cap::AtCap => attempts >= budget.max_retries,
        Cap::AfterCap => attempts > budget.max_retries,
    };
    if exhausted {
        return Err(crate::IdError::RateLimited {
            retry_after_ms: expires_at.saturating_sub(now).max(0),
        });
    }
    Ok(())
}

/// 记一次**失败**的比对（上游只在 `parts[0] != code` 时自增，比对成功不计数）。
pub fn spend_failure(conn: &Connection, bucket: &str, budget: Budget, now: i64) -> Result<i64> {
    conn.execute(
        "INSERT INTO identity_retry(bucket, attempts, created_at, updated_at, expires_at) VALUES(?1, 1, ?2, ?2, ?3) ON CONFLICT(bucket) DO UPDATE SET attempts = attempts + 1, updated_at = ?2",
        params![bucket, now, now.saturating_add(budget.ttl_ms)],
    )?;
    Ok(conn.query_row(
        "SELECT attempts FROM identity_retry WHERE bucket = ?1",
        params![bucket],
        |r| r.get(0),
    )?)
}

/// 当前计数（审计与测试断言用）。没有行 = 0。
pub fn budget_attempts(conn: &Connection, bucket: &str) -> Result<i64> {
    Ok(conn
        .query_row(
            "SELECT attempts FROM identity_retry WHERE bucket = ?1",
            params![bucket],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

/// 验证成功即删（上游 `store.PCache.Delete(key)`：留着没有意义——
/// 一枚用过的凭据不该继续占着一个计数，也不该让「重试预算」变成可查询的账号状态）。
pub fn clear_budget(conn: &Connection, bucket: &str) -> Result<()> {
    conn.execute("DELETE FROM identity_retry WHERE bucket = ?1", params![bucket])?;
    Ok(())
}

/// 下发新凭据时**上弦**：没有行就建一条从 0 开始，已有行只续期、**不清零**。
///
/// 这里是与上游的一处**刻意分歧**，写在 `UP-TINODE-identity-retry-budget.md` 里：
/// 上游 `GenSecret` 每次都 `Upsert(code + ":0:" + uid)`，即重新下发就把重试计数归零，
/// 因为它那一侧的 key 就是这枚码本身，重发在协议上是"另一件事"。
/// 本平台的 `resend()` 由客户端在同一个挑战上反复触发，如果照抄归零，
/// "换一枚码又能试 5 次"就成了无限次尝试的通道——所以计数留在原处，只把寿命往前推。
pub fn arm_budget(conn: &Connection, bucket: &str, budget: Budget, now: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO identity_retry(bucket, attempts, created_at, updated_at, expires_at) VALUES(?1, 0, ?2, ?2, ?3) ON CONFLICT(bucket) DO UPDATE SET updated_at = ?2, expires_at = ?3",
        params![bucket, now, now.saturating_add(budget.ttl_ms)],
    )?;
    Ok(())
}

/// 垃圾回收：删掉 `now` 之前就该随凭据一起消失的预算行。
///
/// 上游在 `GenSecret` 里显式跑一次 `PCache.Expire(realName+"_", now-lifetime)`
/// ——**在下发新码的时候顺手扫**，而不是另起一个定时任务。本平台照抄这个位置
/// （见 `login_challenge::issue`）：这些行没人扫就会一直躺在库里，
/// 既占空间又会被误读成「这个手机号还在被锁」。
pub fn sweep(conn: &Connection, now: i64, limit: i64) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM identity_retry WHERE rowid IN (SELECT rowid FROM identity_retry WHERE expires_at <= ?1 LIMIT ?2)",
        params![now, limit],
    )?)
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
    fn budget_locks_at_cap_and_does_not_roll_back_like_a_window() {
        let conn = db();
        let bucket = key_credential("email", "a@b.c");
        for i in 0..CODE_BUDGET.max_retries {
            check_budget(&conn, &bucket, CODE_BUDGET, 1_000 + i).unwrap();
            let now = 1_000 + i;
            let n = spend_failure(&conn, &bucket, CODE_BUDGET, now).unwrap();
            assert_eq!(n, i + 1);
        }
        // 与窗口桶的关键差别：窗口滑走也不会放行，预算只随凭据寿命走。
        assert!(check_budget(&conn, &bucket, CODE_BUDGET, 1_000).is_err());
        assert!(check_budget(&conn, &bucket, CODE_BUDGET, 1_000 + 60_000).is_err());
        // TTL（= 凭据寿命）过去后才清行，之后允许从 0 重新计。
        check_budget(&conn, &bucket, CODE_BUDGET, 1_000 + CODE_BUDGET.ttl_ms).unwrap();
        assert_eq!(budget_attempts(&conn, &bucket).unwrap(), 0);
    }

    #[test]
    fn after_cap_gives_one_more_try_than_at_cap() {
        let conn = db();
        let at = Budget {
            max_retries: 2,
            cap: Cap::AtCap,
            ttl_ms: 10_000,
        };
        let after = Budget {
            max_retries: 2,
            cap: Cap::AfterCap,
            ttl_ms: 10_000,
        };
        for (method, budget, still_open) in [("m1", at, false), ("m2", after, true)] {
            let bucket = key_credential(method, "same");
            spend_failure(&conn, &bucket, budget, 1_000).unwrap();
            spend_failure(&conn, &bucket, budget, 1_000).unwrap();
            assert_eq!(
                check_budget(&conn, &bucket, budget, 1_000).is_ok(),
                still_open,
                "{budget:?}"
            );
        }
    }

    #[test]
    fn arm_clear_and_sweep() {
        let conn = db();
        let bucket = key_credential("email", "x@y.z");
        spend_failure(&conn, &bucket, CODE_BUDGET, 1_000).unwrap();
        // 上弦：已有行只续期，计数不清零（与上游的分歧点，见 arm_budget 文档）。
        arm_budget(&conn, &bucket, CODE_BUDGET, 2_000).unwrap();
        assert_eq!(budget_attempts(&conn, &bucket).unwrap(), 1);
        let fresh = key_credential("email", "brand-new@y.z");
        arm_budget(&conn, &fresh, CODE_BUDGET, 2_000).unwrap();
        assert_eq!(budget_attempts(&conn, &fresh).unwrap(), 0, "没有行才从 0 开始");
        spend_failure(&conn, &bucket, CODE_BUDGET, 3_000).unwrap();
        assert_eq!(budget_attempts(&conn, &bucket).unwrap(), 2);
        clear_budget(&conn, &bucket).unwrap();
        assert_eq!(budget_attempts(&conn, &bucket).unwrap(), 0);
        // sweep 只删过期的，活着的行不动。
        let short = Budget {
            max_retries: 5,
            cap: Cap::AtCap,
            ttl_ms: 100,
        };
        spend_failure(&conn, &bucket, short, 4_000).unwrap();
        let long_bucket = key_credential("email", "keep@y.z");
        spend_failure(&conn, &long_bucket, CODE_BUDGET, 4_000).unwrap();
        assert_eq!(sweep(&conn, 5_000, SWEEP_LIMIT).unwrap(), 1);
        assert_eq!(budget_attempts(&conn, &long_bucket).unwrap(), 1);
    }

    #[test]
    fn empty_bucket_is_open() {
        let conn = db();
        check(&conn, "ip:none", LOGIN_IP, 1).unwrap();
        assert!(status(&conn, "ip:none").unwrap().is_none());
    }
}
