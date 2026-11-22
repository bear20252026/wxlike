//! **仓储层**（`users`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。
//!
//! # 第一批 seam（G00-04 S2）
//!
//! 本文件定义 `UsersRepo` 单一端口与 `SqliteUsersRepo` 实现。**现有 51 个
//! repo 函数的签名一个都没改**；旧调用点仍走原来的 SQL，本端口先作为
//! 后续分批迁移的落点。PostgreSQL 实现随 sqlx 落地时只新增
//! `PostgresUsersRepo`，不改 trait。
//!
//! `#[allow(dead_code)]` 是刻意的：seam 先就位、调用点后迁，避免为了消警告
//! 把 56 处 `users` SQL 一次性全改掉。

#![allow(dead_code)]

use rusqlite::{params, Connection, OptionalExtension as _};
use sqlx::Row;
use wxlike_persistence::{NewUser, UserCredentials, UserProfileRow, UsersRepo};

use crate::error::{PlatformError, Result};
use crate::storage::PostgresPool;

/// `users` 表的跨 crate 端口定义在 `wxlike-persistence`，SQLite 实现留在
/// 本 crate。这样身份 feature 后续接 PostgreSQL 时复用同一份 trait，不需要
/// 在 platform 与 identity 之间复制端口。
///
/// SQLite 实现。SQL 与 `domain::users` / `domain::security` 里的既有语句
/// 逐字一致；迁移调用点时先换端口，再换 SQL。
pub(in crate::domain) struct SqliteUsersRepo<'a> {
    conn: &'a Connection,
}

impl<'a> SqliteUsersRepo<'a> {
    pub(in crate::domain) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

impl UsersRepo for SqliteUsersRepo<'_> {
    type Error = PlatformError;

    fn find_id_by_name(
        &self,
        name: &str,
        include_deleted: bool,
        exclude_id: Option<&str>,
    ) -> Result<Option<String>> {
        // 动态拼的是**静态片段**，用户输入全部走绑定。
        let mut sql = String::from("SELECT id FROM users WHERE name = ?1");
        if !include_deleted {
            sql.push_str(" AND deleted_at IS NULL");
        }
        if exclude_id.is_some() {
            sql.push_str(" AND id <> ?2");
        }
        let row = if let Some(exclude) = exclude_id {
            self.conn
                .query_row(&sql, params![name, exclude], |r| r.get::<_, String>(0))
                .optional()?
        } else {
            self.conn
                .query_row(&sql, params![name], |r| r.get::<_, String>(0))
                .optional()?
        };
        Ok(row)
    }

    fn load_credentials(&self, name: &str) -> Result<Option<UserCredentials>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, pw_salt, pw_hash, pw_rounds, pw_algo
                 FROM users WHERE name = ?1 AND deleted_at IS NULL",
                params![name],
                |r| {
                    Ok(UserCredentials {
                        user_id: r.get(0)?,
                        pw_salt: r.get(1)?,
                        pw_hash: r.get(2)?,
                        pw_rounds: r.get(3)?,
                        pw_algo: r.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    fn load_profile(&self, user_id: &str) -> Result<Option<UserProfileRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT name, avatar, signature FROM users WHERE id = ?1",
                params![user_id],
                |r| {
                    Ok(UserProfileRow {
                        name: r.get(0)?,
                        avatar: r.get(1)?,
                        signature: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    fn insert(&self, user: &NewUser<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO users(id, name, pw_salt, pw_hash, pw_rounds, pw_algo, avatar, signature, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                user.id,
                user.name,
                user.pw_salt,
                user.pw_hash,
                user.pw_rounds,
                user.pw_algo,
                user.avatar,
                user.signature,
                user.created_at
            ],
        )?;
        Ok(())
    }

    fn update_name(&self, user_id: &str, name: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE users SET name = ?1 WHERE id = ?2",
            params![name, user_id],
        )?)
    }

    fn update_avatar(&self, user_id: &str, avatar: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE users SET avatar = ?1 WHERE id = ?2",
            params![avatar, user_id],
        )?)
    }

    fn update_signature(&self, user_id: &str, signature: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE users SET signature = ?1 WHERE id = ?2",
            params![signature, user_id],
        )?)
    }

    fn mark_deleted(&self, user_id: &str, deleted_at: i64) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE users SET deleted_at = ?2 WHERE id = ?1 AND deleted_at IS NULL",
            params![user_id, deleted_at],
        )?)
    }

    fn update_password_hash(&self, user_id: &str, phc: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE users SET pw_algo = 'argon2id', pw_salt = '', pw_hash = ?2, pw_rounds = 0
             WHERE id = ?1",
            params![user_id, phc],
        )?)
    }

    fn set_totp_secret(&self, user_id: &str, secret: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE users SET totp_secret = ?2, totp_enabled = 0 WHERE id = ?1",
            params![user_id, secret],
        )?)
    }

    fn enable_totp(&self, user_id: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE users SET totp_enabled = 1 WHERE id = ?1",
            params![user_id],
        )?)
    }

    fn disable_totp(&self, user_id: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE users SET totp_enabled = 0, totp_secret = NULL WHERE id = ?1",
            params![user_id],
        )?)
    }
}

/// PostgreSQL 实现。物理表是 `identity_user`，不是 `users` 视图；写路径
/// 不依赖视图触发器，读路径也不依赖视图的列别名。
pub(in crate::domain) struct PostgresUsersRepo<'a> {
    pg: &'a PostgresPool,
}

impl<'a> PostgresUsersRepo<'a> {
    pub(in crate::domain) fn new(pg: &'a PostgresPool) -> Self {
        Self { pg }
    }
}

fn pg_error(e: sqlx::Error) -> PlatformError {
    PlatformError::Storage(format!("postgres users: {e}"))
}

impl UsersRepo for PostgresUsersRepo<'_> {
    type Error = PlatformError;

    fn find_id_by_name(
        &self,
        name: &str,
        include_deleted: bool,
        exclude_id: Option<&str>,
    ) -> Result<Option<String>> {
        let mut sql = String::from("SELECT user_id FROM identity_user WHERE name = $1");
        if !include_deleted {
            sql.push_str(" AND deleted_at IS NULL");
        }
        if exclude_id.is_some() {
            sql.push_str(" AND user_id <> $2");
        }
        let mut query = sqlx::query_scalar::<_, String>(&sql).bind(name);
        if let Some(exclude) = exclude_id {
            query = query.bind(exclude);
        }
        self.pg
            .block_on(query.fetch_optional(self.pg.pool()))
            .map_err(pg_error)
    }

    fn load_credentials(&self, name: &str) -> Result<Option<UserCredentials>> {
        let row = self
            .pg
            .block_on(
                sqlx::query(
                    "SELECT user_id, pw_salt, pw_hash, pw_rounds, pw_algo
                     FROM identity_user WHERE name = $1 AND deleted_at IS NULL",
                )
                .bind(name)
                .fetch_optional(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(row.map(|r| UserCredentials {
            user_id: r.get("user_id"),
            pw_salt: r.get("pw_salt"),
            pw_hash: r.get("pw_hash"),
            // PG 的 INTEGER 是 i32，端口 DTO 沿用 SQLite 的 i64；显式转换，
            // 不让 sqlx 的 INT8/INT4 类型检查在运行时才炸。
            pw_rounds: i64::from(r.get::<i32, _>("pw_rounds")),
            pw_algo: r.get("pw_algo"),
        }))
    }

    fn load_profile(&self, user_id: &str) -> Result<Option<UserProfileRow>> {
        let row = self
            .pg
            .block_on(
                sqlx::query("SELECT name, avatar, signature FROM identity_user WHERE user_id = $1")
                    .bind(user_id)
                    .fetch_optional(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(row.map(|r| UserProfileRow {
            name: r.get("name"),
            avatar: r.get("avatar"),
            signature: r.get("signature"),
        }))
    }

    fn insert(&self, user: &NewUser<'_>) -> Result<()> {
        self.pg
            .block_on(
                sqlx::query(
                    "INSERT INTO identity_user(
                         user_id, name, pw_salt, pw_hash, pw_rounds, pw_algo,
                         avatar, signature, created_at
                     ) VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                )
                .bind(user.id)
                .bind(user.name)
                .bind(user.pw_salt)
                .bind(user.pw_hash)
                .bind(user.pw_rounds)
                .bind(user.pw_algo)
                .bind(user.avatar)
                .bind(user.signature)
                .bind(user.created_at)
                .execute(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(())
    }

    fn update_name(&self, user_id: &str, name: &str) -> Result<usize> {
        let result = self
            .pg
            .block_on(
                sqlx::query("UPDATE identity_user SET name = $1 WHERE user_id = $2")
                    .bind(name)
                    .bind(user_id)
                    .execute(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(result.rows_affected() as usize)
    }

    fn update_avatar(&self, user_id: &str, avatar: &str) -> Result<usize> {
        let result = self
            .pg
            .block_on(
                sqlx::query("UPDATE identity_user SET avatar = $1 WHERE user_id = $2")
                    .bind(avatar)
                    .bind(user_id)
                    .execute(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(result.rows_affected() as usize)
    }

    fn update_signature(&self, user_id: &str, signature: &str) -> Result<usize> {
        let result = self
            .pg
            .block_on(
                sqlx::query("UPDATE identity_user SET signature = $1 WHERE user_id = $2")
                    .bind(signature)
                    .bind(user_id)
                    .execute(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(result.rows_affected() as usize)
    }

    fn mark_deleted(&self, user_id: &str, deleted_at: i64) -> Result<usize> {
        let result = self
            .pg
            .block_on(
                sqlx::query(
                    "UPDATE identity_user SET deleted_at = $2
                     WHERE user_id = $1 AND deleted_at IS NULL",
                )
                .bind(user_id)
                .bind(deleted_at)
                .execute(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(result.rows_affected() as usize)
    }

    fn update_password_hash(&self, user_id: &str, phc: &str) -> Result<usize> {
        let result = self
            .pg
            .block_on(
                sqlx::query(
                    "UPDATE identity_user
                     SET pw_algo = 'argon2id', pw_salt = '', pw_hash = $2, pw_rounds = 0
                     WHERE user_id = $1",
                )
                .bind(user_id)
                .bind(phc)
                .execute(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(result.rows_affected() as usize)
    }

    fn set_totp_secret(&self, user_id: &str, secret: &str) -> Result<usize> {
        let result = self
            .pg
            .block_on(
                sqlx::query(
                    "UPDATE identity_user SET totp_secret = $2, totp_enabled = 0
                     WHERE user_id = $1",
                )
                .bind(user_id)
                .bind(secret)
                .execute(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(result.rows_affected() as usize)
    }

    fn enable_totp(&self, user_id: &str) -> Result<usize> {
        let result = self
            .pg
            .block_on(
                sqlx::query("UPDATE identity_user SET totp_enabled = 1 WHERE user_id = $1")
                    .bind(user_id)
                    .execute(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(result.rows_affected() as usize)
    }

    fn disable_totp(&self, user_id: &str) -> Result<usize> {
        let result = self
            .pg
            .block_on(
                sqlx::query(
                    "UPDATE identity_user SET totp_enabled = 0, totp_secret = NULL
                     WHERE user_id = $1",
                )
                .bind(user_id)
                .execute(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(result.rows_affected() as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::{NewUser, PostgresUsersRepo, SqliteUsersRepo, UsersRepo};
    use crate::{storage::PostgresPool, Platform};

    #[test]
    fn sqlite_repo_covers_register_profile_and_delete() {
        let platform = Platform::in_memory().expect("in-memory platform");
        let db = platform.db().lock();
        let repo = SqliteUsersRepo::new(&db);

        repo.insert(&NewUser {
            id: "u1",
            name: "alice",
            pw_salt: "",
            pw_hash: "phc",
            pw_rounds: 0,
            pw_algo: "argon2id",
            avatar: "",
            signature: "",
            created_at: 1,
        })
        .expect("insert");

        assert_eq!(
            repo.find_id_by_name("alice", true, None)
                .unwrap()
                .as_deref(),
            Some("u1")
        );
        assert_eq!(
            repo.load_credentials("alice").unwrap().unwrap().user_id,
            "u1"
        );
        assert_eq!(repo.load_profile("u1").unwrap().unwrap().name, "alice");

        assert_eq!(repo.update_name("u1", "alice2").unwrap(), 1);
        assert_eq!(repo.update_avatar("u1", "a.png").unwrap(), 1);
        assert_eq!(repo.update_signature("u1", "sig").unwrap(), 1);
        assert_eq!(repo.load_profile("u1").unwrap().unwrap().name, "alice2");

        assert_eq!(repo.mark_deleted("u1", 99).unwrap(), 1);
        assert!(repo.load_credentials("alice2").unwrap().is_none());
        assert_eq!(
            repo.find_id_by_name("alice2", false, None).unwrap(),
            None,
            "已注销行不参与活跃查询"
        );
    }

    #[test]
    #[ignore = "requires WXLIKE_TEST_PG_URL pointing at a DB with M011 applied"]
    fn postgres_repo_covers_register_profile_and_delete() {
        let url = std::env::var("WXLIKE_TEST_PG_URL")
            .expect("set WXLIKE_TEST_PG_URL to run the PostgreSQL repo test");
        let pg = PostgresPool::open(&url).expect("postgres pool");
        let cleanup = || {
            pg.block_on(
                sqlx::query("DELETE FROM identity_user WHERE user_id = 'pgtest-u1'")
                    .execute(pg.pool()),
            )
            .expect("cleanup");
        };
        cleanup();

        let repo = PostgresUsersRepo::new(&pg);
        repo.insert(&NewUser {
            id: "pgtest-u1",
            name: "pgtest-alice",
            pw_salt: "",
            pw_hash: "phc",
            pw_rounds: 0,
            pw_algo: "argon2id",
            avatar: "",
            signature: "",
            created_at: 1,
        })
        .expect("insert");

        assert_eq!(
            repo.find_id_by_name("pgtest-alice", true, None)
                .unwrap()
                .as_deref(),
            Some("pgtest-u1")
        );
        assert_eq!(
            repo.load_credentials("pgtest-alice")
                .unwrap()
                .unwrap()
                .user_id,
            "pgtest-u1"
        );
        assert_eq!(
            repo.load_profile("pgtest-u1").unwrap().unwrap().name,
            "pgtest-alice"
        );
        assert_eq!(repo.update_name("pgtest-u1", "pgtest-alice2").unwrap(), 1);
        assert_eq!(repo.update_avatar("pgtest-u1", "a.png").unwrap(), 1);
        assert_eq!(repo.update_signature("pgtest-u1", "sig").unwrap(), 1);
        assert_eq!(
            repo.load_profile("pgtest-u1").unwrap().unwrap().name,
            "pgtest-alice2"
        );
        assert_eq!(repo.mark_deleted("pgtest-u1", 99).unwrap(), 1);
        assert!(repo.load_credentials("pgtest-alice2").unwrap().is_none());
        assert_eq!(
            repo.find_id_by_name("pgtest-alice2", false, None).unwrap(),
            None
        );

        cleanup();
    }
}
