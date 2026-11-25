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
use crate::storage::{OptionalExt as _, PostgresPool, StorageConnection};

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
        assert_eq!(repo.touch("t1", 50).unwrap(), 1);
        assert_eq!(repo.find_by_token("t1").unwrap().unwrap().last_seen, 50);
        assert_eq!(repo.revoke_by_sid(&user_id, "s1").unwrap(), 1);
        assert!(repo.find_by_token("t1").unwrap().is_none());
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
        assert_eq!(repo.delete_expired_batch(200, 10).unwrap(), 1);
        assert!(repo.find_by_token("pgtest-t1").unwrap().is_none());

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
