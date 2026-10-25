//! G02-01 账号会话：签发、鉴权、登出。
//!
//! 与平台既有 `sessions` 表同形（`token` 存摘要、`sid` 供设备页寻址），本模块
//! 补的是**规则**：设备必须活跃、受众必须匹配（管理面与用户面令牌不通用）、
//! 登出要连带作废刷新链并给出缓存清理事件。
//!
//! 令牌是 32 字节随机数，库里只有 `sha256` 摘要；`Display`/日志里永远不出现
//! 明文令牌。

use rusqlite::{params, Connection, OptionalExtension as _};

use crate::{digest, random_hex, Audience, IdError, Result, ACCESS_TTL_MS, TOKEN_BYTES};

/// `sid` 字节数（12B → 24 hex，够用且能进 URL）。
pub const SID_BYTES: usize = 12;
/// 单个会话的过期清扫批量上限，避免一次登出把整个表扫穿。
pub const PURGE_LIMIT: i64 = 500;

/// 一次登录的产物。**只在这里**出现明文令牌，调用方负责送达客户端。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionGrant {
    pub sid: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub audience: Audience,
}

impl SessionGrant {
    /// 日志/审计用的脱敏描述：不含任何凭据。
    pub fn audit_label(&self) -> String {
        format!("sid={} audience={}", self.sid, self.audience.as_str())
    }
}

/// 鉴权通过后的调用者上下文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthContext {
    pub sid: String,
    pub user_id: String,
    pub device_id: String,
    pub audience: Audience,
}

/// 设备管理页的一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    pub sid: String,
    pub device_id: String,
    pub device_name: String,
    pub ip: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub last_seen: i64,
    pub audience: Audience,
}

/// 登出后要清掉的缓存域（由调用方投递到事件总线，本 crate 不发 IO）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheEvent {
    pub scope: &'static str,
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogoutOutcome {
    pub sid: String,
    pub user_id: String,
    pub device_id: String,
    pub refresh_revoked: usize,
    pub cache_events: Vec<CacheEvent>,
}

fn row_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        sid: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
        device_id: r.get(1)?,
        device_name: r.get(2)?,
        ip: r.get(3)?,
        created_at: r.get(4)?,
        expires_at: r.get(5)?,
        last_seen: r.get(6)?,
        audience: Audience::parse(&r.get::<_, String>(7)?).unwrap_or(Audience::User),
    })
}

/// 签发会话：先确认设备活跃，再落会话与刷新链。
pub fn open(
    conn: &Connection,
    user_id: &str,
    device_id: &str,
    audience: Audience,
    ttl_ms: i64,
    ip: &str,
    now: i64,
) -> Result<SessionGrant> {
    require_active_device(conn, user_id, device_id)?;
    let sid = random_hex(SID_BYTES);
    let access = random_hex(TOKEN_BYTES);
    let expires_at = now.saturating_add(ttl_ms);
    conn.execute(
        "INSERT INTO sessions(token, user_id, created_at, expires_at, sid, device_id, \
         device_name, ip, last_seen, audience) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, '', ?7, ?3, ?8)",
        params![
            digest(&access),
            user_id,
            now,
            expires_at,
            sid,
            device_id,
            ip,
            audience.as_str()
        ],
    )?;
    let refresh = crate::refresh::issue_default(conn, user_id, &sid, device_id, audience, now)?;
    Ok(SessionGrant {
        sid,
        access_token: access,
        refresh_token: refresh,
        expires_at,
        audience,
    })
}

/// 默认 2 小时有效期的便捷入口。
pub fn open_default(
    conn: &Connection,
    user_id: &str,
    device_id: &str,
    audience: Audience,
    ip: &str,
    now: i64,
) -> Result<SessionGrant> {
    open(conn, user_id, device_id, audience, ACCESS_TTL_MS, ip, now)
}

fn require_active_device(conn: &Connection, user_id: &str, device_id: &str) -> Result<()> {
    match crate::device::get(conn, device_id)? {
        Some(d) if d.user_id == user_id && d.is_active() => Ok(()),
        Some(_) => Err(IdError::Forbidden("device revoked")),
        None => Err(IdError::NotFound("device")),
    }
}

/// 用访问令牌鉴权。**受众必须显式给出**：管理面拿用户令牌进不来，反之亦然。
pub fn authenticate(
    conn: &Connection,
    access_token: &str,
    audience: Audience,
    now: i64,
) -> Result<AuthContext> {
    let found: Option<(String, Option<String>, String, i64, String)> = conn
        .query_row(
            "SELECT user_id, sid, device_id, expires_at, audience FROM sessions WHERE token = ?1",
            params![digest(access_token)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((user_id, sid, device_id, expires_at, stored_audience)) = found else {
        return Err(IdError::Unauthorized("invalid or expired token"));
    };
    if expires_at <= now {
        conn.execute(
            "DELETE FROM sessions WHERE token = ?1",
            params![digest(access_token)],
        )?;
        return Err(IdError::Unauthorized("invalid or expired token"));
    }
    let stored = Audience::parse(&stored_audience).ok_or(IdError::Unauthorized("bad audience"))?;
    if stored != audience {
        return Err(IdError::Unauthorized("wrong audience"));
    }
    if !device_id.is_empty() {
        require_active_device(conn, &user_id, &device_id)?;
    }
    Ok(AuthContext {
        sid: sid.unwrap_or_default(),
        user_id,
        device_id,
        audience,
    })
}

/// 心跳：更新最近活跃时间（失败不阻断请求，鉴权已经过了）。
pub fn touch(conn: &Connection, sid: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET last_seen = ?2 WHERE sid = ?1",
        params![sid, now],
    )?;
    Ok(())
}

/// 账号的有效会话，最近活跃在前；顺手清掉过期行。
pub fn list(conn: &Connection, user_id: &str, now: i64) -> Result<Vec<SessionRow>> {
    conn.execute(
        "DELETE FROM sessions WHERE user_id = ?1 AND expires_at <= ?2",
        params![user_id, now],
    )?;
    let mut stmt = conn.prepare(
        "SELECT sid, device_id, device_name, ip, created_at, expires_at, last_seen, audience \
         FROM sessions WHERE user_id = ?1 ORDER BY last_seen DESC, created_at DESC",
    )?;
    let rows = stmt.query_map(params![user_id], row_of)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// 登出指定会话：删会话 + 作废整条刷新链 + 给出缓存清理事件。
pub fn logout(conn: &Connection, user_id: &str, sid: &str, now: i64) -> Result<LogoutOutcome> {
    let tx = conn.unchecked_transaction()?;
    let device_id: Option<String> = tx
        .query_row(
            "SELECT device_id FROM sessions WHERE user_id = ?1 AND sid = ?2",
            params![user_id, sid],
            |r| r.get(0),
        )
        .optional()?;
    let Some(device_id) = device_id else {
        return Err(IdError::NotFound("session"));
    };
    tx.execute(
        "DELETE FROM sessions WHERE user_id = ?1 AND sid = ?2",
        params![user_id, sid],
    )?;
    let refresh_revoked = crate::refresh::revoke_sid(&tx, sid, now)?;
    tx.execute(
        "DELETE FROM identity_step_up WHERE user_id = ?1 AND sid = ?2",
        params![user_id, sid],
    )?;
    tx.commit()?;
    Ok(LogoutOutcome {
        sid: sid.to_string(),
        user_id: user_id.to_string(),
        device_id,
        refresh_revoked,
        cache_events: vec![
            CacheEvent {
                scope: "session",
                key: sid.to_string(),
            },
            CacheEvent {
                scope: "user",
                key: user_id.to_string(),
            },
        ],
    })
}

/// 登出全部会话（改密 / 注销 / "退出所有设备"）。返回被撤销的会话数。
pub fn logout_all(conn: &Connection, user_id: &str, now: i64) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let revoked = tx.execute("DELETE FROM sessions WHERE user_id = ?1", params![user_id])?;
    crate::refresh::revoke_user(&tx, user_id, now)?;
    tx.execute(
        "DELETE FROM identity_step_up WHERE user_id = ?1",
        params![user_id],
    )?;
    tx.commit()?;
    Ok(revoked)
}

/// 清扫过期会话（定时任务；返回删除行数）。
pub fn purge_expired(conn: &Connection, now: i64) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM sessions WHERE rowid IN (
             SELECT rowid FROM sessions WHERE expires_at <= ?1 LIMIT ?2)",
        params![now, PURGE_LIMIT],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::db;

    fn device(conn: &Connection, user: &str, id: &str, now: i64) {
        let reg = crate::device::Registration {
            platform: "android".into(),
            ..Default::default()
        };
        crate::device::register(conn, user, id, &reg, now).unwrap();
    }

    #[test]
    fn open_authenticate_logout_roundtrip() {
        let conn = db();
        device(&conn, "u1", "dev-aaaa1111", 1_000);
        let grant = open_default(
            &conn,
            "u1",
            "dev-aaaa1111",
            Audience::User,
            "1.2.3.4",
            1_000,
        )
        .expect("grant");
        assert_eq!(grant.audience, Audience::User);
        assert!(!grant.audit_label().contains(&grant.access_token));

        let ctx = authenticate(&conn, &grant.access_token, Audience::User, 1_100).unwrap();
        assert_eq!(ctx.user_id, "u1");
        assert_eq!(ctx.sid, grant.sid);
        assert_eq!(ctx.device_id, "dev-aaaa1111");

        let out = logout(&conn, "u1", &grant.sid, 1_200).unwrap();
        assert_eq!(out.refresh_revoked, 1);
        assert!(out.cache_events.iter().any(|e| e.scope == "session"));
        assert!(matches!(
            authenticate(&conn, &grant.access_token, Audience::User, 1_300),
            Err(IdError::Unauthorized(_))
        ));
    }

    #[test]
    fn audience_is_not_interchangeable() {
        let conn = db();
        device(&conn, "u1", "dev-aaaa1111", 1_000);
        let user_grant =
            open_default(&conn, "u1", "dev-aaaa1111", Audience::User, "", 1_000).unwrap();
        assert!(matches!(
            authenticate(&conn, &user_grant.access_token, Audience::Admin, 1_100),
            Err(IdError::Unauthorized("wrong audience"))
        ));
        let admin_grant =
            open_default(&conn, "u1", "dev-aaaa1111", Audience::Admin, "", 1_000).unwrap();
        assert!(matches!(
            authenticate(&conn, &admin_grant.access_token, Audience::User, 1_100),
            Err(IdError::Unauthorized("wrong audience"))
        ));
    }

    #[test]
    fn expired_token_is_rejected_and_purged() {
        let conn = db();
        device(&conn, "u1", "dev-aaaa1111", 1_000);
        let grant = open(&conn, "u1", "dev-aaaa1111", Audience::User, 10, "", 1_000).unwrap();
        assert!(matches!(
            authenticate(&conn, &grant.access_token, Audience::User, 2_000),
            Err(IdError::Unauthorized(_))
        ));
        assert!(list(&conn, "u1", 2_000).unwrap().is_empty());
    }

    #[test]
    fn revoked_device_loses_api_access() {
        let conn = db();
        device(&conn, "u1", "dev-aaaa1111", 1_000);
        let grant = open_default(&conn, "u1", "dev-aaaa1111", Audience::User, "", 1_000).unwrap();
        let out = crate::device::revoke(&conn, "u1", "dev-aaaa1111", 1_100).unwrap();
        assert_eq!(out.sessions, 1);
        assert!(matches!(
            authenticate(&conn, &grant.access_token, Audience::User, 1_200),
            Err(IdError::Unauthorized(_))
        ));
    }

    #[test]
    fn unknown_device_cannot_open() {
        let conn = db();
        assert!(matches!(
            open_default(&conn, "u1", "dev-missing0", Audience::User, "", 1_000),
            Err(IdError::NotFound(_))
        ));
    }

    #[test]
    fn logout_all_revokes_every_session() {
        let conn = db();
        device(&conn, "u1", "dev-aaaa1111", 1_000);
        device(&conn, "u1", "dev-bbbb2222", 1_000);
        let a = open_default(&conn, "u1", "dev-aaaa1111", Audience::User, "", 1_000).unwrap();
        let b = open_default(&conn, "u1", "dev-bbbb2222", Audience::User, "", 1_000).unwrap();
        assert_eq!(logout_all(&conn, "u1", 1_100).unwrap(), 2);
        assert!(authenticate(&conn, &a.access_token, Audience::User, 1_200).is_err());
        assert!(authenticate(&conn, &b.access_token, Audience::User, 1_200).is_err());
    }
}
