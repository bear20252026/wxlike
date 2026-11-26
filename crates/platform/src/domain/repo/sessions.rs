//! **仓储层**（`sessions`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。
//!
//! # 第一批 seam（G00-04 S2）
//!
//! `SessionsRepo` 是访问会话的单一端口；`SqliteSessionsRepo` 复用现有 SQL，
//! PG 实现随 sqlx 落地时只新增 `PostgresSessionsRepo`。现有调用点与签名
//! **一个都没改**。

#![allow(dead_code)]

use rusqlite::{params, Connection, OptionalExtension as _};
use sqlx::Row;
use wxlike_persistence::{NewSession, SessionRecord, SessionsRepo};

use crate::error::{PlatformError, Result};
use crate::storage::sql_port::StorageSqlExec;
use crate::storage::{OptionalExt as _, PostgresPool, StorageConnection, StorageDialect};
use wxlike_identity::refresh;
use wxlike_identity::sqlport::{ColKind, SqlExec as _, SqlParam, SqlValue};

/// 端口定义在 `wxlike-persistence`；SQLite 实现在本 crate。
pub(in crate::domain) struct SqliteSessionsRepo<'a> {
    conn: &'a Connection,
}

impl<'a> SqliteSessionsRepo<'a> {
    pub(in crate::domain) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

impl SessionsRepo for SqliteSessionsRepo<'_> {
    type Error = PlatformError;

    fn insert(&self, s: &NewSession<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sessions(token, user_id, created_at, expires_at, sid, device_id, device_name, ip, last_seen, audience)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                s.token,
                s.user_id,
                s.created_at,
                s.expires_at,
                s.sid,
                s.device_id,
                s.device_name,
                s.ip,
                s.last_seen,
                s.audience
            ],
        )?;
        Ok(())
    }

    fn find_by_token(&self, token: &str) -> Result<Option<SessionRecord>> {
        Ok(self
            .conn
            .query_row(
                "SELECT token, user_id, sid, device_id, device_name, ip, created_at, expires_at, last_seen, audience
                 FROM sessions WHERE token = ?1",
                params![token],
                row_record,
            )
            .optional()?)
    }

    fn sid_by_token(&self, token: &str) -> Result<Option<String>> {
        let sid: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT sid FROM sessions WHERE token = ?1",
                params![token],
                |r| r.get(0),
            )
            .optional()?;
        Ok(sid.flatten())
    }

    fn device_id_by_token(&self, token: &str) -> Result<Option<Option<String>>> {
        let device: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT device_id FROM sessions WHERE token = ?1",
                params![token],
                |r| r.get(0),
            )
            .optional()?;
        Ok(device)
    }

    fn count_for_user(&self, user_id: &str) -> Result<i64> {
        let n = self.conn.query_row(
            "SELECT COUNT(*) FROM sessions WHERE user_id = ?1",
            params![user_id],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    fn device_name_by_sid(&self, sid: &str) -> Result<Option<String>> {
        let name: Option<String> = self
            .conn
            .query_row(
                "SELECT device_name FROM sessions WHERE sid = ?1",
                params![sid],
                |r| r.get(0),
            )
            .optional()?;
        Ok(name.filter(|v| !v.is_empty()))
    }

    fn list_for_user(&self, user_id: &str, now: i64) -> Result<Vec<SessionRecord>> {
        self.conn.execute(
            "DELETE FROM sessions WHERE user_id = ?1 AND expires_at <= ?2",
            params![user_id, now],
        )?;
        let mut stmt = self.conn.prepare(
            "SELECT token, user_id, sid, device_id, device_name, ip, created_at, expires_at, last_seen, audience
             FROM sessions WHERE user_id = ?1 ORDER BY last_seen DESC, created_at DESC",
        )?;
        let rows = stmt
            .query_map(params![user_id], row_record)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn delete_by_token(&self, token: &str) -> Result<usize> {
        Ok(self
            .conn
            .execute("DELETE FROM sessions WHERE token = ?1", params![token])?)
    }

    fn delete_by_user(&self, user_id: &str) -> Result<usize> {
        Ok(self
            .conn
            .execute("DELETE FROM sessions WHERE user_id = ?1", params![user_id])?)
    }

    fn delete_expired_batch(&self, now: i64, limit: i64) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM sessions WHERE rowid IN (
                 SELECT rowid FROM sessions WHERE expires_at <= ?1 LIMIT ?2)",
            params![now, limit],
        )?)
    }

    fn revoke_by_sid(&self, user_id: &str, sid: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM sessions WHERE user_id = ?1 AND sid = ?2",
            params![user_id, sid],
        )?)
    }

    fn revoke_others(&self, user_id: &str, keep_token: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM sessions WHERE user_id = ?1 AND token <> ?2",
            params![user_id, keep_token],
        )?)
    }

    fn update_sid(&self, token: &str, sid: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE sessions SET sid = ?2 WHERE token = ?1",
            params![token, sid],
        )?)
    }

    fn delete_by_sid_except_token(&self, sid: &str, token: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM sessions WHERE sid = ?1 AND token <> ?2",
            params![sid, token],
        )?)
    }

    fn delete_by_device(&self, user_id: &str, device_id: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM sessions WHERE user_id = ?1 AND device_id = ?2",
            params![user_id, device_id],
        )?)
    }

    fn touch(&self, token: &str, now: i64) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE sessions SET last_seen = ?2 WHERE token = ?1",
            params![token, now],
        )?)
    }
}

/// 治理闸门的双后端版本：PG 直接读 `identity_user_governance`，SQLite 仍走
/// identity 的既有实现。无行 = active；`disabled` 或未到期的 `suspended` 拒绝。
pub(in crate::domain) fn ensure_active_port(
    c: &StorageConnection<'_>,
    user_id: &str,
    now: i64,
) -> Result<()> {
    wxlike_identity::governance::ensure_user_active_with(&StorageSqlExec(c), user_id, now)
        .map_err(|err| match err {
            wxlike_identity::IdError::Forbidden(_) => PlatformError::Forbidden,
            other => other.into(),
        })
}

/// 认证入口专用查询：会话有效且账号未注销，才返回 `user_id`。
///
/// 与 `find_by_token` 分开：后者是设备管理面的纯会话读取，不关心账号状态；
/// 认证咽喉必须把 `users.deleted_at` 和过期判定放进**同一条 SQL**，避免
/// "先查会话、再查账号"之间被注销事务插队。过期行顺手删除，和旧实现一致。
pub(in crate::domain) fn lookup_live(
    c: &crate::storage::StorageConnection<'_>,
    token: &str,
    now: i64,
) -> Result<Option<String>> {
    let row: Option<(String, i64)> = c
        .query_row(
            "SELECT s.user_id, s.expires_at FROM sessions s
             JOIN users u ON u.id = s.user_id
             WHERE s.token = ?1 AND u.deleted_at IS NULL",
            crate::storage::params![token],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?;
    match row {
        Some((user_id, expires)) if expires > now => Ok(Some(user_id)),
        Some(_) => {
            c.execute("DELETE FROM sessions WHERE token = ?1", crate::storage::params![token])?;
            Ok(None)
        }
        None => Ok(None),
    }
}

fn row_record(r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRecord> {
    Ok(SessionRecord {
        token: r.get(0)?,
        user_id: r.get(1)?,
        sid: r.get(2)?,
        device_id: r.get(3)?,
        device_name: r.get(4)?,
        ip: r.get(5)?,
        created_at: r.get(6)?,
        expires_at: r.get(7)?,
        last_seen: r.get(8)?,
        audience: r.get(9)?,
    })
}

/// PostgreSQL 实现。物理表是 `identity_session_token`，不是 `sessions` 视图。
/// `delete_expired_batch` 不再依赖 SQLite 的 `rowid`：直接用 token 子查询。
pub(in crate::domain) struct PostgresSessionsRepo<'a> {
    pg: &'a PostgresPool,
}

impl<'a> PostgresSessionsRepo<'a> {
    pub(in crate::domain) fn new(pg: &'a PostgresPool) -> Self {
        Self { pg }
    }
}

fn pg_error(e: sqlx::Error) -> PlatformError {
    PlatformError::Storage(format!("postgres sessions: {e}"))
}

fn pg_record(row: &sqlx::postgres::PgRow) -> Result<SessionRecord> {
    Ok(SessionRecord {
        token: row.try_get("token").map_err(pg_error)?,
        user_id: row.try_get("user_id").map_err(pg_error)?,
        sid: row.try_get("sid").map_err(pg_error)?,
        device_id: row.try_get("device_id").map_err(pg_error)?,
        device_name: row.try_get("device_name").map_err(pg_error)?,
        ip: row.try_get("ip").map_err(pg_error)?,
        created_at: row.try_get("created_at").map_err(pg_error)?,
        expires_at: row.try_get("expires_at").map_err(pg_error)?,
        last_seen: row.try_get("last_seen").map_err(pg_error)?,
        audience: row.try_get("audience").map_err(pg_error)?,
    })
}

impl SessionsRepo for PostgresSessionsRepo<'_> {
    type Error = PlatformError;

    fn insert(&self, s: &NewSession<'_>) -> Result<()> {
        self.pg
            .block_on(
                sqlx::query(
                    "INSERT INTO identity_session_token(
                         token, user_id, created_at, expires_at, sid,
                         device_id, device_name, ip, last_seen, audience
                     ) VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                )
                .bind(s.token)
                .bind(s.user_id)
                .bind(s.created_at)
                .bind(s.expires_at)
                .bind(s.sid)
                .bind(s.device_id)
                .bind(s.device_name)
                .bind(s.ip)
                .bind(s.last_seen)
                .bind(s.audience)
                .execute(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(())
    }

    fn find_by_token(&self, token: &str) -> Result<Option<SessionRecord>> {
        let row = self
            .pg
            .block_on(
                sqlx::query(
                    "SELECT token, user_id, sid, device_id, device_name, ip,
                            created_at, expires_at, last_seen, audience
                     FROM identity_session_token WHERE token = $1",
                )
                .bind(token)
                .fetch_optional(self.pg.pool()),
            )
            .map_err(pg_error)?;
        row.as_ref().map(pg_record).transpose()
    }

    fn sid_by_token(&self, token: &str) -> Result<Option<String>> {
        let sid: Option<Option<String>> = self
            .pg
            .block_on(
                sqlx::query_scalar::<_, Option<String>>(
                    "SELECT sid FROM identity_session_token WHERE token = $1",
                )
                .bind(token)
                .fetch_optional(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(sid.flatten())
    }

    fn device_id_by_token(&self, token: &str) -> Result<Option<Option<String>>> {
        self.pg
            .block_on(
                sqlx::query_scalar::<_, Option<String>>(
                    "SELECT device_id FROM identity_session_token WHERE token = $1",
                )
                .bind(token)
                .fetch_optional(self.pg.pool()),
            )
            .map_err(pg_error)
    }

    fn count_for_user(&self, user_id: &str) -> Result<i64> {
        self.pg
            .block_on(
                sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM identity_session_token WHERE user_id = $1",
                )
                .bind(user_id)
                .fetch_one(self.pg.pool()),
            )
            .map_err(pg_error)
    }

    fn device_name_by_sid(&self, sid: &str) -> Result<Option<String>> {
        let name: Option<String> = self
            .pg
            .block_on(
                sqlx::query_scalar::<_, String>(
                    "SELECT device_name FROM identity_session_token WHERE sid = $1",
                )
                .bind(sid)
                .fetch_optional(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(name.filter(|v| !v.is_empty()))
    }

    fn list_for_user(&self, user_id: &str, now: i64) -> Result<Vec<SessionRecord>> {
        self.pg
            .block_on(
                sqlx::query(
                    "DELETE FROM identity_session_token
                     WHERE user_id = $1 AND expires_at <= $2",
                )
                .bind(user_id)
                .bind(now)
                .execute(self.pg.pool()),
            )
            .map_err(pg_error)?;
        let rows = self
            .pg
            .block_on(
                sqlx::query(
                    "SELECT token, user_id, sid, device_id, device_name, ip,
                            created_at, expires_at, last_seen, audience
                     FROM identity_session_token
                     WHERE user_id = $1
                     ORDER BY last_seen DESC, created_at DESC",
                )
                .bind(user_id)
                .fetch_all(self.pg.pool()),
            )
            .map_err(pg_error)?;
        rows.iter().map(pg_record).collect()
    }

    fn delete_by_token(&self, token: &str) -> Result<usize> {
        let result = self
            .pg
            .block_on(
                sqlx::query("DELETE FROM identity_session_token WHERE token = $1")
                    .bind(token)
                    .execute(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(result.rows_affected() as usize)
    }

    fn delete_by_user(&self, user_id: &str) -> Result<usize> {
        let result = self
            .pg
            .block_on(
                sqlx::query("DELETE FROM identity_session_token WHERE user_id = $1")
                    .bind(user_id)
                    .execute(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(result.rows_affected() as usize)
    }

    fn delete_expired_batch(&self, now: i64, limit: i64) -> Result<usize> {
        let result = self
            .pg
            .block_on(
                sqlx::query(
                    "DELETE FROM identity_session_token
                     WHERE token IN (
                         SELECT token FROM identity_session_token
                         WHERE expires_at <= $1
                         ORDER BY expires_at, token
                         LIMIT $2
                     )",
                )
                .bind(now)
                .bind(limit)
                .execute(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(result.rows_affected() as usize)
    }

    fn revoke_by_sid(&self, user_id: &str, sid: &str) -> Result<usize> {
        let result = self
            .pg
            .block_on(
                sqlx::query("DELETE FROM identity_session_token WHERE user_id = $1 AND sid = $2")
                    .bind(user_id)
                    .bind(sid)
                    .execute(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(result.rows_affected() as usize)
    }

    fn revoke_others(&self, user_id: &str, keep_token: &str) -> Result<usize> {
        let result = self
            .pg
            .block_on(
                sqlx::query(
                    "DELETE FROM identity_session_token
                     WHERE user_id = $1 AND token <> $2",
                )
                .bind(user_id)
                .bind(keep_token)
                .execute(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(result.rows_affected() as usize)
    }

    fn update_sid(&self, token: &str, sid: &str) -> Result<usize> {
        let result = self
            .pg
            .block_on(
                sqlx::query("UPDATE identity_session_token SET sid = $2 WHERE token = $1")
                    .bind(token)
                    .bind(sid)
                    .execute(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(result.rows_affected() as usize)
    }

    fn delete_by_sid_except_token(&self, sid: &str, token: &str) -> Result<usize> {
        let result = self
            .pg
            .block_on(
                sqlx::query("DELETE FROM identity_session_token WHERE sid = $1 AND token <> $2")
                    .bind(sid)
                    .bind(token)
                    .execute(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(result.rows_affected() as usize)
    }

    fn delete_by_device(&self, user_id: &str, device_id: &str) -> Result<usize> {
        let result = self
            .pg
            .block_on(
                sqlx::query(
                    "DELETE FROM identity_session_token
                     WHERE user_id = $1 AND device_id = $2",
                )
                .bind(user_id)
                .bind(device_id)
                .execute(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(result.rows_affected() as usize)
    }

    fn touch(&self, token: &str, now: i64) -> Result<usize> {
        let result = self
            .pg
            .block_on(
                sqlx::query("UPDATE identity_session_token SET last_seen = $2 WHERE token = $1")
                    .bind(token)
                    .bind(now)
                    .execute(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(result.rows_affected() as usize)
    }
}

/// 按 `Db` 后端选择 `sessions` 实现；domain 不再直接构造 SQLite repo。
pub(in crate::domain) enum BackendSessionsRepo<'a> {
    Sqlite(SqliteSessionsRepo<'a>),
    Postgres(PostgresSessionsRepo<'a>),
}

impl<'a> BackendSessionsRepo<'a> {
    pub(in crate::domain) fn new(conn: &'a StorageConnection<'_>) -> Result<Self> {
        Ok(match crate::storage::backend_ref(conn)? {
            crate::storage::BackendRef::Sqlite(c) => Self::Sqlite(SqliteSessionsRepo::new(c)),
            crate::storage::BackendRef::Postgres(p) => Self::Postgres(PostgresSessionsRepo::new(p)),
        })
    }
}

impl SessionsRepo for BackendSessionsRepo<'_> {
    type Error = PlatformError;

    crate::domain::repo::forward_backend! {
        fn insert(session: &NewSession<'_>) -> ();
        fn find_by_token(token: &str) -> Option<SessionRecord>;
        fn sid_by_token(token: &str) -> Option<String>;
        fn device_id_by_token(token: &str) -> Option<Option<String>>;
        fn count_for_user(user_id: &str) -> i64;
        fn device_name_by_sid(sid: &str) -> Option<String>;
        fn list_for_user(user_id: &str, now: i64) -> Vec<SessionRecord>;
        fn delete_by_token(token: &str) -> usize;
        fn delete_by_user(user_id: &str) -> usize;
        fn delete_expired_batch(now: i64, limit: i64) -> usize;
        fn revoke_by_sid(user_id: &str, sid: &str) -> usize;
        fn revoke_others(user_id: &str, keep_token: &str) -> usize;
        fn update_sid(token: &str, sid: &str) -> usize;
        fn delete_by_sid_except_token(sid: &str, token: &str) -> usize;
        fn delete_by_device(user_id: &str, device_id: &str) -> usize;
        fn touch(token: &str, now: i64) -> usize;
    }
}

/// 首枚刷新令牌：SQL 在 identity `refresh::issue_with`，这里只做端口适配。
pub(in crate::domain) fn issue_refresh(
    conn: &StorageConnection<'_>,
    user_id: &str,
    sid: &str,
    device_id: &str,
    now: i64,
) -> Result<String> {
    Ok(refresh::issue_default_with(
        &StorageSqlExec(conn),
        user_id,
        sid,
        device_id,
        wxlike_identity::Audience::User,
        now,
    )?)
}

/// 作废一台设备的刷新链。
pub(in crate::domain) fn revoke_device_refresh(
    conn: &StorageConnection<'_>,
    user_id: &str,
    device_id: &str,
    now: i64,
) -> Result<usize> {
    Ok(refresh::revoke_device_with(
        &StorageSqlExec(conn),
        user_id,
        device_id,
        now,
    )?)
}

/// 作废一个 sid 的刷新链。
pub(in crate::domain) fn revoke_sid_refresh(
    conn: &StorageConnection<'_>,
    sid: &str,
    now: i64,
) -> Result<usize> {
    Ok(refresh::revoke_sid_with(&StorageSqlExec(conn), sid, now)?)
}

/// 授予一次 step-up（SQL 在 identity `step_up::grant_with`）。
pub(in crate::domain) fn grant_step_up(
    conn: &StorageConnection<'_>,
    user_id: &str,
    sid: &str,
    purpose: &str,
    now: i64,
) -> Result<i64> {
    Ok(wxlike_identity::step_up::grant_default_with(
        &StorageSqlExec(conn),
        user_id,
        sid,
        purpose,
        now,
    )?)
}

/// 消费一次 step-up。
pub(in crate::domain) fn consume_step_up(
    conn: &StorageConnection<'_>,
    user_id: &str,
    sid: &str,
    purpose: &str,
    now: i64,
) -> Result<()> {
    Ok(wxlike_identity::step_up::consume_with(
        &StorageSqlExec(conn),
        user_id,
        sid,
        purpose,
        now,
    )?)
}

/// 清空一个账号的全部 step-up。
pub(in crate::domain) fn revoke_all_step_up(
    conn: &StorageConnection<'_>,
    user_id: &str,
) -> Result<usize> {
    Ok(wxlike_identity::step_up::revoke_all_with(
        &StorageSqlExec(conn),
        user_id,
    )?)
}

/// 会话的设备名：SQL 与行宽度在本函数；只做存储端口翻译。
pub(in crate::domain) fn device_name_by_sid(
    conn: &StorageConnection<'_>,
    sid: &str,
) -> Result<Option<String>> {
    let sql = format!("SELECT device_name FROM {} WHERE sid = ?1", session_table(conn));
    let row = StorageSqlExec(conn).query(
        &sql,
        &[SqlParam::Text(sid.to_string())],
        &[ColKind::OptText],
    )?;
    let Some(values) = row else {
        return Ok(None);
    };
    let name = values
        .into_iter()
        .next()
        .ok_or(PlatformError::Storage("device_name row width".into()))?
        .into_text()?;
    Ok(Some(name).filter(|v| !v.is_empty()))
}

/// 会话表的物理名：SQLite 用平台 `sessions`，PG 用 M014 的 `identity_session_token`。
fn session_table(conn: &StorageConnection<'_>) -> &'static str {
    match conn.dialect() {
        StorageDialect::Sqlite => "sessions",
        StorageDialect::Postgres => "identity_session_token",
    }
}

/// 会话行里的设备 id；`None` = 行不存在，`Some(None)` = 行在但字段为空。
pub(in crate::domain) fn device_id_by_token(
    conn: &StorageConnection<'_>,
    token: &str,
) -> Result<Option<Option<String>>> {
    let sql = format!("SELECT device_id FROM {} WHERE token = ?1", session_table(conn));
    let row = StorageSqlExec(conn).query(
        &sql,
        &[SqlParam::Text(token.to_string())],
        &[ColKind::OptText],
    )?;
    let Some(values) = row else {
        return Ok(None);
    };
    let value = values
        .into_iter()
        .next()
        .ok_or(PlatformError::Storage("device_id row width".into()))?;
    Ok(Some(match value {
        SqlValue::Null => None,
        other => Some(other.into_text()?),
    }))
}

/// 新访问会话沿用链上的 sid。
pub(in crate::domain) fn update_sid(
    conn: &StorageConnection<'_>,
    token: &str,
    sid: &str,
) -> Result<usize> {
    let sql = format!("UPDATE {} SET sid = ?2 WHERE token = ?1", session_table(conn));
    Ok(StorageSqlExec(conn).exec(
        &sql,
        &[
            SqlParam::Text(token.to_string()),
            SqlParam::Text(sid.to_string()),
        ],
    )?)
}

/// 同一 sid 只保留当前 token，其余会话行清掉。
pub(in crate::domain) fn delete_by_sid_except_token(
    conn: &StorageConnection<'_>,
    sid: &str,
    token: &str,
) -> Result<usize> {
    let sql = format!(
        "DELETE FROM {} WHERE sid = ?1 AND token <> ?2",
        session_table(conn)
    );
    Ok(StorageSqlExec(conn).exec(
        &sql,
        &[
            SqlParam::Text(sid.to_string()),
            SqlParam::Text(token.to_string()),
        ],
    )?)
}

#[cfg(test)]
mod tests {
    use super::{lookup_live, NewSession, PostgresSessionsRepo, SessionsRepo, SqliteSessionsRepo};
    use crate::{storage::PostgresPool, Platform};

    #[test]
    fn sqlite_repo_covers_insert_list_touch_and_revoke() {
        let platform = Platform::in_memory().expect("in-memory platform");
        let user_id = platform.register("alice", "pw123456").expect("register");
        let db = platform.db().lock().expect("db lock");
        let repo = SqliteSessionsRepo::new(&db);

        repo.insert(&NewSession {
            token: "t1",
            user_id: &user_id,
            created_at: 1,
            expires_at: 100,
            sid: Some("s1"),
            device_id: "d1",
            device_name: "phone",
            ip: "127.0.0.1",
            last_seen: 1,
            audience: "user",
        })
        .expect("insert");

        assert_eq!(
            repo.find_by_token("t1").unwrap().unwrap().sid.as_deref(),
            Some("s1")
        );
        assert_eq!(repo.sid_by_token("t1").unwrap().as_deref(), Some("s1"));
        assert_eq!(
            repo.device_name_by_sid("s1").unwrap().as_deref(),
            Some("phone")
        );
        assert_eq!(
            repo.device_id_by_token("t1").unwrap(),
            Some(Some("d1".to_string()))
        );
        assert_eq!(repo.count_for_user(&user_id).unwrap(), 1);
        assert_eq!(repo.list_for_user(&user_id, 0).unwrap().len(), 1);
        db.execute(
            "INSERT INTO identity_session_meta(
                sid, user_id, auth_level, features, serial, issued_at, expires_at
             ) VALUES('s1', ?1, 'auth', '', 0, 1, 100)",
            rusqlite::params![user_id],
        )
        .expect("insert meta");
        assert_eq!(repo.touch("t1", 50).unwrap(), 1);
        assert_eq!(repo.find_by_token("t1").unwrap().unwrap().last_seen, 50);
        assert_eq!(repo.revoke_by_sid(&user_id, "s1").unwrap(), 1);
        assert!(repo.find_by_token("t1").unwrap().is_none());
        let meta: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM identity_session_meta WHERE sid = 's1'",
                [],
                |r| r.get(0),
            )
            .expect("count meta");
        assert_eq!(meta, 0, "revoke must clean the sidecar");
        assert_eq!(repo.count_for_user(&user_id).unwrap(), 0);
    }

    #[test]
    #[ignore = "requires WXLIKE_TEST_PG_URL pointing at a DB with M011 applied"]
    fn postgres_repo_covers_insert_list_touch_and_revoke() {
        let url = std::env::var("WXLIKE_TEST_PG_URL")
            .expect("set WXLIKE_TEST_PG_URL to run the PostgreSQL repo test");
        let pg = PostgresPool::open(&url).expect("postgres pool");
        let cleanup = || {
            pg.block_on(
                sqlx::query("DELETE FROM identity_session_token WHERE user_id = 'pgtest-s1'")
                    .execute(pg.pool()),
            )
            .expect("cleanup sessions");
            pg.block_on(
                sqlx::query("DELETE FROM identity_user WHERE user_id = 'pgtest-s1'")
                    .execute(pg.pool()),
            )
            .expect("cleanup user");
        };
        cleanup();
        pg.block_on(
            sqlx::query(
                "INSERT INTO identity_user(user_id, name, pw_hash, created_at)
                 VALUES('pgtest-s1', 'pgtest-session', 'phc', 1)",
            )
            .execute(pg.pool()),
        )
        .expect("seed user");

        let repo = PostgresSessionsRepo::new(&pg);
        repo.insert(&NewSession {
            token: "pgtest-t1",
            user_id: "pgtest-s1",
            created_at: 1,
            expires_at: 100,
            sid: Some("pgtest-sid1"),
            device_id: "d1",
            device_name: "phone",
            ip: "127.0.0.1",
            last_seen: 1,
            audience: "user",
        })
        .expect("insert");
        assert_eq!(
            repo.find_by_token("pgtest-t1")
                .unwrap()
                .unwrap()
                .sid
                .as_deref(),
            Some("pgtest-sid1")
        );
        assert_eq!(
            repo.sid_by_token("pgtest-t1").unwrap().as_deref(),
            Some("pgtest-sid1")
        );
        assert_eq!(repo.list_for_user("pgtest-s1", 0).unwrap().len(), 1);
        assert_eq!(repo.touch("pgtest-t1", 50).unwrap(), 1);
        assert_eq!(
            repo.find_by_token("pgtest-t1").unwrap().unwrap().last_seen,
            50
        );
        pg.block_on(
            sqlx::query(
                "INSERT INTO identity_session_meta(
                     sid, user_id, auth_level, features, serial, issued_at, expires_at
                 ) VALUES('pgtest-sid1', 'pgtest-s1', 'auth', '', 0, 1, 100)
                 ON CONFLICT (sid) DO NOTHING",
            )
            .execute(pg.pool()),
        )
        .expect("seed meta");
        assert_eq!(repo.delete_expired_batch(200, 10).unwrap(), 1);
        assert!(repo.find_by_token("pgtest-t1").unwrap().is_none());
        let meta: i64 = pg
            .block_on(
                sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM identity_session_meta WHERE sid = 'pgtest-sid1'",
                )
                .fetch_one(pg.pool()),
            )
            .expect("count meta");
        assert_eq!(meta, 0, "expired delete must clean the sidecar");

        cleanup();
    }

    #[test]
    #[ignore = "requires WXLIKE_TEST_PG_URL pointing at a DB with M011 applied"]
    fn postgres_backend_sessions_repo_routes_to_postgres() {
        let url = std::env::var("WXLIKE_TEST_PG_URL")
            .expect("set WXLIKE_TEST_PG_URL to run the PostgreSQL backend test");
        let pg = PostgresPool::open(&url).expect("postgres pool");
        let cleanup = || {
            pg.block_on(
                sqlx::query("DELETE FROM identity_session_token WHERE user_id = 'pgbe-s1'")
                    .execute(pg.pool()),
            )
            .expect("cleanup sessions");
            pg.block_on(
                sqlx::query("DELETE FROM identity_user WHERE user_id = 'pgbe-s1'")
                    .execute(pg.pool()),
            )
            .expect("cleanup user");
        };
        cleanup();
        pg.block_on(
            sqlx::query(
                "INSERT INTO identity_user(user_id, name, pw_hash, created_at)
                 VALUES ('pgbe-s1', 'pgbe-bob', 'x', 1)",
            )
            .execute(pg.pool()),
        )
        .expect("seed user");

        let conn = crate::storage::StorageConnection::Postgres(&pg);
        let repo = super::BackendSessionsRepo::new(&conn).expect("backend sessions repo");
        repo.insert(&NewSession {
            token: "pgbe-t1",
            user_id: "pgbe-s1",
            created_at: 1,
            expires_at: 1000,
            sid: Some("pgbe-sid1"),
            device_id: "pgbe-d1",
            device_name: "phone",
            ip: "127.0.0.1",
            last_seen: 1,
            audience: "user",
        })
        .expect("insert through backend");
        assert_eq!(
            repo.sid_by_token("pgbe-t1").unwrap().as_deref(),
            Some("pgbe-sid1")
        );
        assert_eq!(
            lookup_live(&conn, "pgbe-t1", 10).unwrap().as_deref(),
            Some("pgbe-s1")
        );
        assert_eq!(repo.touch("pgbe-t1", 50).unwrap(), 1);
        assert_eq!(repo.delete_by_token("pgbe-t1").unwrap(), 1);

        cleanup();
    }
}

#[cfg(test)]
mod refresh_pg_tests {
    use super::{
        consume_step_up, grant_step_up, issue_refresh, revoke_device_refresh, revoke_sid_refresh,
    };
    use crate::storage::{params, StorageProfile};
    use crate::Db;

    /// PG 的 `identity_refresh` / `identity_session_token` 有 `user_id` 外键；
    /// 这些测试直接写刷新链与会话，必须先把账号行补上（SQLite 没有该约束）。
    fn seed_user(conn: &crate::storage::StorageConnection<'_>, user: &str) {
        conn.execute(
            "INSERT INTO identity_user(user_id, name, pw_hash, created_at) \
             VALUES(?1, ?1, 'phc', 1) ON CONFLICT DO NOTHING",
            params![user],
        )
        .expect("seed user");
    }

    #[test]
    #[ignore = "requires WXLIKE_TEST_PG_URL and M014 applied"]
    fn postgres_refresh_issue_and_revoke_share_the_port() {
        let Ok(url) = std::env::var("WXLIKE_TEST_PG_URL") else {
            return;
        };
        let db = Db::open_profile(StorageProfile::Postgres { url }, None).expect("pg db");
        let user = "pg-refresh-user";
        let conn = db.acquire();
        conn.execute(
            "DELETE FROM identity_refresh WHERE user_id = ?1",
            params![user],
        )
        .expect("clean refresh");
        seed_user(&conn, user);
        drop(conn);
        db.with_tx(|c| issue_refresh(c, user, "pg-refresh-sid", "pg-refresh-dev", 1_000))
            .expect("issue refresh");
        assert_eq!(
            db.with_tx(|c| revoke_device_refresh(c, user, "pg-refresh-dev", 1_100))
                .expect("revoke device"),
            1
        );
        assert_eq!(
            db.with_tx(|c| revoke_sid_refresh(c, "pg-refresh-sid", 1_200)).expect("revoke sid"),
            0
        );
        db.with_tx(|c| grant_step_up(c, user, "pg-refresh-sid", "account.export", 1_300))
            .expect("grant step-up");
        db.with_tx(|c| consume_step_up(c, user, "pg-refresh-sid", "account.export", 1_400))
            .expect("consume step-up");
        assert!(
            db.with_tx(|c| consume_step_up(c, user, "pg-refresh-sid", "account.export", 1_500))
                .is_err()
        );
    }

    #[test]
    #[ignore = "requires WXLIKE_TEST_PG_URL and M014 applied"]
    fn postgres_refresh_rotate_commits_replay_revocation() {
        let Ok(url) = std::env::var("WXLIKE_TEST_PG_URL") else {
            return;
        };
        let db = Db::open_profile(StorageProfile::Postgres { url }, None).expect("pg db");
        let user = "pg-rotate-user";
        let sid = "pg-rotate-sid";
        let conn = db.acquire();
        conn.execute(
            "DELETE FROM identity_refresh WHERE user_id = ?1",
            params![user],
        )
        .expect("clean refresh");
        conn.execute(
            "DELETE FROM identity_session_token WHERE user_id = ?1",
            params![user],
        )
        .expect("clean sessions");
        seed_user(&conn, user);
        let session = [
            ("token", "pg-rotate-token"),
            ("user_id", user),
            ("created_at", "1"),
            ("expires_at", "10000"),
            ("sid", sid),
            ("device_id", "pg-rotate-dev"),
            ("device_name", "PG rotate"),
            ("ip", "127.0.0.1"),
            ("last_seen", "1000"),
            ("audience", "user"),
        ];
        conn.execute(
            "INSERT INTO identity_session_token(token, user_id, created_at, expires_at, sid, \
             device_id, device_name, ip, last_seen, audience) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                session[0].1,
                session[1].1,
                session[2].1.parse::<i64>().unwrap(),
                session[3].1.parse::<i64>().unwrap(),
                session[4].1,
                session[5].1,
                session[6].1,
                session[7].1,
                session[8].1.parse::<i64>().unwrap(),
                session[9].1
            ],
        )
        .expect("insert session");
        drop(conn);

        let first = db
            .with_tx(|c| issue_refresh(c, user, sid, "pg-rotate-dev", 1_000))
            .expect("issue");
        let rotated = db
            .with_tx(|c| {
                super::refresh::rotate_outcome_with(&super::StorageSqlExec(c), &first, 2_000)
                    .map_err(Into::into)
            })
            .expect("rotate")
            .into_result()
            .expect("first rotate wins");
        let replay = db
            .with_tx(|c| {
                super::refresh::rotate_outcome_with(&super::StorageSqlExec(c), &first, 2_100)
                    .map_err(Into::into)
            })
            .expect("replay commits");
        match replay {
            super::refresh::RotateOutcome::Rejected(super::refresh::RotateReject::Replay { sid: s }) => {
                assert_eq!(s, sid);
            }
            other => panic!("expected replay rejection, got {other:?}"),
        }
        // 重放检测已撤销整条链：新令牌也不能再用。
        assert!(db
            .with_tx(|c| {
                super::refresh::rotate_outcome_with(&super::StorageSqlExec(c), &rotated.refresh_token, 2_200)
                    .map_err(Into::into)
            })
            .expect("second rotate commits")
            .into_result()
            .is_err());
    }
}
