//! G02-01 敏感操作二次确认（step-up）。
//!
//! 改密、改手机号、增删设备、导出、注销这些动作，光有访问令牌不够——令牌可能
//! 来自一台已经被借走的设备。这里签发**短时、单次、绑定会话与用途**的授权：
//!
//! * 短时：默认 5 分钟，过期即失效；
//! * 单次：`consume` 成功后立刻作废，重放无效；
//! * 绑定：`(user_id, sid, purpose)` 三元组，A 会话的授权不能给 B 会话用；
//! * 用途封闭：只接受白名单里的 purpose，写错的用途不会意外放行。

use rusqlite::{params, Connection};

use crate::{IdError, Result, STEP_UP_TTL_MS};

/// 允许 step-up 的动作。封闭集合：新增动作必须显式加进来。
pub const PURPOSES: [&str; 7] = [
    "password.change",
    "phone.change",
    "device.add",
    "device.revoke",
    "account.export",
    "account.delete",
    "recovery.reset",
];

fn check_purpose(purpose: &str) -> Result<()> {
    if PURPOSES.contains(&purpose) {
        Ok(())
    } else {
        Err(IdError::BadInput("step-up purpose"))
    }
}

/// 授予一次 step-up，返回到期时间。重复授予刷新有效期（旧授权不叠加）。
pub fn grant(
    conn: &Connection,
    user_id: &str,
    sid: &str,
    purpose: &str,
    ttl_ms: i64,
    now: i64,
) -> Result<i64> {
    check_purpose(purpose)?;
    let expires_at = now.saturating_add(ttl_ms);
    conn.execute(
        "INSERT INTO identity_step_up(user_id, sid, purpose, granted_at, expires_at, consumed_at) \
         VALUES(?1, ?2, ?3, ?4, ?5, NULL) \
         ON CONFLICT(user_id, sid, purpose) DO UPDATE SET \
             granted_at = ?4, expires_at = ?5, consumed_at = NULL",
        params![user_id, sid, purpose, now, expires_at],
    )?;
    Ok(expires_at)
}

/// 默认 5 分钟的授予。
pub fn grant_default(
    conn: &Connection,
    user_id: &str,
    sid: &str,
    purpose: &str,
    now: i64,
) -> Result<i64> {
    grant(conn, user_id, sid, purpose, STEP_UP_TTL_MS, now)
}

/// 消费授权：单次有效。失败一律同一个错误，不区分"没授权"和"已用过"。
pub fn consume(conn: &Connection, user_id: &str, sid: &str, purpose: &str, now: i64) -> Result<()> {
    check_purpose(purpose)?;
    let n = conn.execute(
        "UPDATE identity_step_up SET consumed_at = ?4 \
         WHERE user_id = ?1 AND sid = ?2 AND purpose = ?3 \
           AND consumed_at IS NULL AND expires_at > ?4",
        params![user_id, sid, purpose, now],
    )?;
    if n != 1 {
        return Err(IdError::Unauthorized("step-up required"));
    }
    Ok(())
}

/// 只检查不消费（页面渲染"是否还需要输密码"用）。
pub fn require(conn: &Connection, user_id: &str, sid: &str, purpose: &str, now: i64) -> Result<()> {
    check_purpose(purpose)?;
    let alive: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM identity_step_up \
             WHERE user_id = ?1 AND sid = ?2 AND purpose = ?3 \
               AND consumed_at IS NULL AND expires_at > ?4",
            params![user_id, sid, purpose, now],
            |r| r.get(0),
        )
        .ok();
    match alive {
        Some(_) => Ok(()),
        None => Err(IdError::Unauthorized("step-up required")),
    }
}

/// 改密 / 注销后清空该账号全部授权。
pub fn revoke_all(conn: &Connection, user_id: &str) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM identity_step_up WHERE user_id = ?1",
        params![user_id],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::db;

    #[test]
    fn grant_then_consume_once() {
        let conn = db();
        grant_default(&conn, "u1", "s1", "password.change", 1_000).unwrap();
        require(&conn, "u1", "s1", "password.change", 1_100).unwrap();
        consume(&conn, "u1", "s1", "password.change", 1_200).unwrap();
        assert!(matches!(
            consume(&conn, "u1", "s1", "password.change", 1_300),
            Err(IdError::Unauthorized(_))
        ));
    }

    #[test]
    fn expired_and_wrong_scope_are_rejected() {
        let conn = db();
        grant(&conn, "u1", "s1", "device.revoke", 10, 1_000).unwrap();
        assert!(matches!(
            consume(&conn, "u1", "s1", "device.revoke", 5_000),
            Err(IdError::Unauthorized(_))
        ));
        grant_default(&conn, "u1", "s1", "device.revoke", 1_000).unwrap();
        assert!(
            matches!(
                consume(&conn, "u1", "s2", "device.revoke", 1_100),
                Err(IdError::Unauthorized(_))
            ),
            "授权绑定会话，不能跨会话复用"
        );
        assert!(
            matches!(
                consume(&conn, "u1", "s1", "password.change", 1_100),
                Err(IdError::Unauthorized(_))
            ),
            "授权绑定用途"
        );
    }

    #[test]
    fn unknown_purpose_is_bad_input() {
        let conn = db();
        assert!(matches!(
            grant_default(&conn, "u1", "s1", "transfer.all.money", 1_000),
            Err(IdError::BadInput(_))
        ));
    }

    #[test]
    fn regrant_after_consume_works() {
        let conn = db();
        grant_default(&conn, "u1", "s1", "account.export", 1_000).unwrap();
        consume(&conn, "u1", "s1", "account.export", 1_100).unwrap();
        grant_default(&conn, "u1", "s1", "account.export", 1_200).unwrap();
        consume(&conn, "u1", "s1", "account.export", 1_300).unwrap();
        assert_eq!(revoke_all(&conn, "u1").unwrap(), 1);
    }
}
