//! G02-01 刷新令牌链：轮换 + 重放检测。
//!
//! 每次刷新**作废旧令牌、签发新令牌**，形成一条 `generation` 递增的链。旧令牌
//! 被再次使用只有两种解释：客户端重试竞态，或令牌已泄露。两者都按重放处理——
//! 整条链立刻作废（含当前有效令牌），用户必须重新登录。宁可多一次登录，也不
//! 让攻击者与被盗设备共享一条长期有效的链。
//!
//! 库里只存 `sha256(token)`：备份/快照泄露不产生可用凭据。

use rusqlite::{params, Connection, OptionalExtension as _};

use crate::{digest, random_hex, Audience, IdError, Result, REFRESH_TTL_MS, TOKEN_BYTES};

/// 链上一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainRow {
    pub token_hash: String,
    pub sid: String,
    pub user_id: String,
    pub device_id: String,
    pub audience: Audience,
    pub generation: i64,
    pub created_at: i64,
    pub expires_at: i64,
    pub used_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

/// 轮换结果：新令牌 + 该链的归属信息（调用方据此重建访问令牌）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rotated {
    pub sid: String,
    pub user_id: String,
    pub device_id: String,
    pub audience: Audience,
    pub generation: i64,
    pub refresh_token: String,
    pub expires_at: i64,
}

fn row_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChainRow> {
    Ok(ChainRow {
        token_hash: r.get(0)?,
        sid: r.get(1)?,
        user_id: r.get(2)?,
        device_id: r.get(3)?,
        audience: Audience::parse(&r.get::<_, String>(4)?).unwrap_or(Audience::User),
        generation: r.get(5)?,
        created_at: r.get(6)?,
        expires_at: r.get(7)?,
        used_at: r.get(8)?,
        revoked_at: r.get(9)?,
    })
}

const SELECT_ROW: &str = "SELECT token_hash, sid, user_id, device_id, audience, generation, \
     created_at, expires_at, used_at, revoked_at FROM identity_refresh";

/// 为一次登录签发首枚刷新令牌（generation = 1）。
pub fn issue(
    conn: &Connection,
    user_id: &str,
    sid: &str,
    device_id: &str,
    audience: Audience,
    ttl_ms: i64,
    now: i64,
) -> Result<String> {
    let raw = random_hex(TOKEN_BYTES);
    conn.execute(
        "INSERT INTO identity_refresh(token_hash, sid, user_id, device_id, audience, \
         generation, created_at, expires_at, used_at, revoked_at) \
         VALUES(?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, NULL, NULL)",
        params![
            digest(&raw),
            sid,
            user_id,
            device_id,
            audience.as_str(),
            now,
            now.saturating_add(ttl_ms)
        ],
    )?;
    Ok(raw)
}

/// 默认有效期签发的便捷入口。
pub fn issue_default(
    conn: &Connection,
    user_id: &str,
    sid: &str,
    device_id: &str,
    audience: Audience,
    now: i64,
) -> Result<String> {
    issue(conn, user_id, sid, device_id, audience, REFRESH_TTL_MS, now)
}

/// 单行查询（含已用/已撤销）。
pub fn find(conn: &Connection, raw: &str) -> Result<Option<ChainRow>> {
    let sql = format!("{SELECT_ROW} WHERE token_hash = ?1");
    Ok(conn
        .query_row(&sql, params![digest(raw)], row_of)
        .optional()?)
}

/// 轮换。失败即封闭：任何异常状态都会顺手作废整条链。
pub fn rotate(conn: &Connection, raw: &str, now: i64) -> Result<Rotated> {
    let tx = conn.unchecked_transaction()?;
    let row = find(&tx, raw)?;
    let Some(row) = row else {
        return Err(IdError::Unauthorized("unknown refresh token"));
    };
    if row.revoked_at.is_some() {
        return Err(IdError::Unauthorized("refresh token revoked"));
    }
    if row.used_at.is_some() {
        revoke_sid(&tx, &row.sid, now)?;
        tx.commit()?;
        return Err(IdError::Unauthorized("refresh token replay detected"));
    }
    if row.expires_at <= now {
        revoke_sid(&tx, &row.sid, now)?;
        tx.commit()?;
        return Err(IdError::Expired);
    }
    ensure_access_alive(&tx, &row.sid, now)?;
    let next = mint_successor(&tx, &row, now)?;
    tx.commit()?;
    Ok(next)
}

/// 访问会话已被删除（设备被移除 / 已登出）时，刷新令牌一并失效。
fn ensure_access_alive(conn: &Connection, sid: &str, now: i64) -> Result<()> {
    let alive: Option<i64> = conn
        .query_row(
            "SELECT expires_at FROM sessions WHERE sid = ?1",
            params![sid],
            |r| r.get(0),
        )
        .optional()?;
    match alive {
        Some(expires_at) if expires_at > now => Ok(()),
        _ => {
            revoke_sid(conn, sid, now)?;
            Err(IdError::Unauthorized("session revoked"))
        }
    }
}

/// 标记旧令牌已用 + 落新令牌。
fn mint_successor(conn: &Connection, row: &ChainRow, now: i64) -> Result<Rotated> {
    let raw = random_hex(TOKEN_BYTES);
    let n = conn.execute(
        "UPDATE identity_refresh SET used_at = ?2 WHERE token_hash = ?1 AND used_at IS NULL",
        params![row.token_hash, now],
    )?;
    if n != 1 {
        // 并发轮换：另一个请求抢先用了这枚令牌，本次按重放处理。
        revoke_sid(conn, &row.sid, now)?;
        return Err(IdError::Unauthorized("refresh token replay detected"));
    }
    conn.execute(
        "INSERT INTO identity_refresh(token_hash, sid, user_id, device_id, audience, \
         generation, created_at, expires_at, used_at, revoked_at) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL)",
        params![
            digest(&raw),
            row.sid,
            row.user_id,
            row.device_id,
            row.audience.as_str(),
            row.generation + 1,
            now,
            row.expires_at
        ],
    )?;
    Ok(Rotated {
        sid: row.sid.clone(),
        user_id: row.user_id.clone(),
        device_id: row.device_id.clone(),
        audience: row.audience,
        generation: row.generation + 1,
        refresh_token: raw,
        expires_at: row.expires_at,
    })
}

/// 作废一个会话下的整条链。
pub fn revoke_sid(conn: &Connection, sid: &str, now: i64) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE identity_refresh SET revoked_at = ?2 WHERE sid = ?1 AND revoked_at IS NULL",
        params![sid, now],
    )?)
}

/// 作废一台设备的全部链（设备移除 / 远程下线）。
pub fn revoke_device(conn: &Connection, user_id: &str, device_id: &str, now: i64) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE identity_refresh SET revoked_at = ?3 \
         WHERE user_id = ?1 AND device_id = ?2 AND revoked_at IS NULL",
        params![user_id, device_id, now],
    )?)
}

/// 作废一个账号的全部链（注销 / 改密后强制全端重登）。
pub fn revoke_user(conn: &Connection, user_id: &str, now: i64) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE identity_refresh SET revoked_at = ?2 WHERE user_id = ?1 AND revoked_at IS NULL",
        params![user_id, now],
    )?)
}

/// 一个会话的完整链（运维视图 / 测试断言用）。
pub fn chain_of(conn: &Connection, sid: &str) -> Result<Vec<ChainRow>> {
    let sql = format!("{SELECT_ROW} WHERE sid = ?1 ORDER BY generation ASC");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![sid], row_of)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::db;

    fn session_row(conn: &Connection, sid: &str, now: i64) {
        conn.execute(
            "INSERT INTO sessions(token, user_id, created_at, expires_at, sid, device_id, \
             device_name, ip, last_seen, audience) \
             VALUES(?1, 'u1', ?2, ?3, ?4, 'dev-aaaa1111', 'Pixel', '127.0.0.1', ?2, 'user')",
            params![format!("tok-{sid}"), now, now + 1_000, sid],
        )
        .unwrap();
    }

    #[test]
    fn rotation_advances_generation() {
        let conn = db();
        session_row(&conn, "s1", 1_000);
        let first =
            issue_default(&conn, "u1", "s1", "dev-aaaa1111", Audience::User, 1_000).unwrap();
        let second = rotate(&conn, &first, 1_100).unwrap();
        assert_eq!(second.generation, 2);
        assert_ne!(second.refresh_token, first);
        let third = rotate(&conn, &second.refresh_token, 1_200).unwrap();
        assert_eq!(third.generation, 3);
        assert_eq!(chain_of(&conn, "s1").unwrap().len(), 3);
    }

    #[test]
    fn replay_kills_the_whole_chain() {
        let conn = db();
        session_row(&conn, "s1", 1_000);
        let first =
            issue_default(&conn, "u1", "s1", "dev-aaaa1111", Audience::User, 1_000).unwrap();
        let second = rotate(&conn, &first, 1_100).unwrap();
        let err = rotate(&conn, &first, 1_200).unwrap_err();
        assert!(matches!(err, IdError::Unauthorized(m) if m.contains("replay")));
        // 攻击者重放之后，合法客户端手里的新令牌也必须失效。
        assert!(matches!(
            rotate(&conn, &second.refresh_token, 1_300),
            Err(IdError::Unauthorized(_))
        ));
    }

    #[test]
    fn expired_and_unknown_are_rejected() {
        let conn = db();
        session_row(&conn, "s1", 1_000);
        let raw = issue(&conn, "u1", "s1", "dev-aaaa1111", Audience::User, 10, 1_000).unwrap();
        assert!(matches!(rotate(&conn, &raw, 5_000), Err(IdError::Expired)));
        assert!(matches!(
            rotate(&conn, "not-a-token", 1_000),
            Err(IdError::Unauthorized(_))
        ));
    }

    #[test]
    fn device_revocation_kills_refresh() {
        let conn = db();
        session_row(&conn, "s1", 1_000);
        let raw = issue_default(&conn, "u1", "s1", "dev-aaaa1111", Audience::User, 1_000).unwrap();
        assert_eq!(
            revoke_device(&conn, "u1", "dev-aaaa1111", 1_100).unwrap(),
            1
        );
        assert!(matches!(
            rotate(&conn, &raw, 1_200),
            Err(IdError::Unauthorized(_))
        ));
    }

    #[test]
    fn deleting_the_session_kills_refresh() {
        let conn = db();
        session_row(&conn, "s1", 1_000);
        let raw = issue_default(&conn, "u1", "s1", "dev-aaaa1111", Audience::User, 1_000).unwrap();
        conn.execute("DELETE FROM sessions WHERE sid = 's1'", [])
            .unwrap();
        assert!(matches!(
            rotate(&conn, &raw, 1_200),
            Err(IdError::Unauthorized("session revoked"))
        ));
    }
}
