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
use crate::storage::{OptionalExt as _, PostgresPool, StorageConnection};

/// `users` 表的跨 crate 端口定义在 `wxlike-persistence`，SQLite 实现留在
/// 本 crate。这样身份 feature 后续接 PostgreSQL 时复用同一份 trait，不需要
/// 在 platform 与 identity 之间复制端口。
///
/// SQLite 实现。SQL 与 `domain::users` / `domain::security` 里的既有语句
/// 逐字一致；迁移调用点时先换端口，再换 SQL。
/// 这个用户 id 在 `users` 表里存在吗（不区分注销状态，调用方各自再判）。
pub(in crate::domain) fn user_exists(c: &StorageConnection<'_>, user_id: &str) -> Result<bool> {
    Ok(c.query_row(
        "SELECT 1 FROM users WHERE id = ?1",
        crate::storage::params![user_id],
        |_| Ok(1i64),
    )
    .optional()?
    .is_some())
}

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

    fn active_exists(&self, user_id: &str) -> Result<bool> {
        let exists = self
            .conn
            .query_row(
                "SELECT 1 FROM users WHERE id = ?1 AND deleted_at IS NULL",
                params![user_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        Ok(exists)
    }

    fn update_profile(
        &self,
        user_id: &str,
        name: Option<&str>,
        avatar: Option<&str>,
        signature: Option<&str>,
    ) -> Result<usize> {
        // 三条 UPDATE 必须同事务：旧实现逐字段写，`signature` 校验失败会
        // 留下已经改掉的 `name`。这里把原子性收进仓储层，domain 只负责校验。
        let tx = self.conn.unchecked_transaction()?;
        let mut changed = 0;
        if let Some(name) = name {
            changed += tx.execute(
                "UPDATE users SET name = ?1 WHERE id = ?2",
                params![name, user_id],
            )?;
        }
        if let Some(avatar) = avatar {
            changed += tx.execute(
                "UPDATE users SET avatar = ?1 WHERE id = ?2",
                params![avatar, user_id],
            )?;
        }
        if let Some(signature) = signature {
            changed += tx.execute(
                "UPDATE users SET signature = ?1 WHERE id = ?2",
                params![signature, user_id],
            )?;
        }
        tx.commit()?;
        Ok(changed)
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

    fn totp_secret(&self, user_id: &str) -> Result<Option<String>> {
        let secret: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT totp_secret FROM users WHERE id = ?1",
                params![user_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(secret.flatten())
    }

    fn totp_enabled(&self, user_id: &str) -> Result<bool> {
        let enabled: Option<i64> = self
            .conn
            .query_row(
                "SELECT totp_enabled FROM users WHERE id = ?1",
                params![user_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(enabled.map(|v| v != 0).unwrap_or(false))
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
    if let Some(db) = e.as_database_error() {
        if db.code().as_deref() == Some("23505") {
            return PlatformError::Conflict("name".into());
        }
    }
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

    fn active_exists(&self, user_id: &str) -> Result<bool> {
        let row = self
            .pg
            .block_on(
                sqlx::query_scalar::<_, i32>(
                    "SELECT 1 FROM identity_user WHERE user_id = $1 AND deleted_at IS NULL",
                )
                .bind(user_id)
                .fetch_optional(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(row.is_some())
    }

    fn update_profile(
        &self,
        user_id: &str,
        name: Option<&str>,
        avatar: Option<&str>,
        signature: Option<&str>,
    ) -> Result<usize> {
        let changed = self
            .pg
            .block_on(async {
                let mut tx = self.pg.pool().begin().await?;
                let mut changed = 0u64;
                if let Some(name) = name {
                    changed += sqlx::query("UPDATE identity_user SET name = $1 WHERE user_id = $2")
                        .bind(name)
                        .bind(user_id)
                        .execute(&mut *tx)
                        .await?
                        .rows_affected();
                }
                if let Some(avatar) = avatar {
                    changed +=
                        sqlx::query("UPDATE identity_user SET avatar = $1 WHERE user_id = $2")
                            .bind(avatar)
                            .bind(user_id)
                            .execute(&mut *tx)
                            .await?
                            .rows_affected();
                }
                if let Some(signature) = signature {
                    changed +=
                        sqlx::query("UPDATE identity_user SET signature = $1 WHERE user_id = $2")
                            .bind(signature)
                            .bind(user_id)
                            .execute(&mut *tx)
                            .await?
                            .rows_affected();
                }
                tx.commit().await?;
                Ok::<u64, sqlx::Error>(changed)
            })
            .map_err(pg_error)?;
        Ok(changed as usize)
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

    fn totp_secret(&self, user_id: &str) -> Result<Option<String>> {
        let secret: Option<Option<String>> = self
            .pg
            .block_on(
                sqlx::query_scalar::<_, Option<String>>(
                    "SELECT totp_secret FROM identity_user WHERE user_id = $1",
                )
                .bind(user_id)
                .fetch_optional(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(secret.flatten())
    }

    fn totp_enabled(&self, user_id: &str) -> Result<bool> {
        let enabled: Option<i32> = self
            .pg
            .block_on(
                sqlx::query_scalar::<_, i32>(
                    "SELECT totp_enabled FROM identity_user WHERE user_id = $1",
                )
                .bind(user_id)
                .fetch_optional(self.pg.pool()),
            )
            .map_err(pg_error)?;
        Ok(enabled.map(|v| v != 0).unwrap_or(false))
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

/// 按 `Db` 后端选择 `users` 实现：SQLite 走旧守卫，PostgreSQL 走
/// `PostgresUsersRepo`。domain 只认这一层，不再直接构造 `SqliteUsersRepo`。
pub(in crate::domain) enum BackendUsersRepo<'a> {
    Sqlite(SqliteUsersRepo<'a>),
    Postgres(PostgresUsersRepo<'a>),
}

impl<'a> BackendUsersRepo<'a> {
    pub(in crate::domain) fn new(conn: &'a StorageConnection<'_>) -> Result<Self> {
        Ok(match crate::storage::backend_ref(conn)? {
            crate::storage::BackendRef::Sqlite(c) => Self::Sqlite(SqliteUsersRepo::new(c)),
            crate::storage::BackendRef::Postgres(p) => Self::Postgres(PostgresUsersRepo::new(p)),
        })
    }
}

impl UsersRepo for BackendUsersRepo<'_> {
    type Error = PlatformError;

    crate::domain::repo::forward_backend! {
        fn find_id_by_name(name: &str, include_deleted: bool, exclude_id: Option<&str>) -> Option<String>;
        fn load_credentials(name: &str) -> Option<UserCredentials>;
        fn load_profile(user_id: &str) -> Option<UserProfileRow>;
        fn active_exists(user_id: &str) -> bool;
        fn update_profile(user_id: &str, name: Option<&str>, avatar: Option<&str>, signature: Option<&str>) -> usize;
        fn insert(user: &NewUser<'_>) -> ();
        fn update_name(user_id: &str, name: &str) -> usize;
        fn update_avatar(user_id: &str, avatar: &str) -> usize;
        fn update_signature(user_id: &str, signature: &str) -> usize;
        fn mark_deleted(user_id: &str, deleted_at: i64) -> usize;
        fn update_password_hash(user_id: &str, phc: &str) -> usize;
        fn totp_secret(user_id: &str) -> Option<String>;
        fn totp_enabled(user_id: &str) -> bool;
        fn set_totp_secret(user_id: &str, secret: &str) -> usize;
        fn enable_totp(user_id: &str) -> usize;
        fn disable_totp(user_id: &str) -> usize;
    }
}

#[cfg(test)]
mod tests {
    use super::{NewUser, PostgresUsersRepo, SqliteUsersRepo, UsersRepo};
    use crate::{storage::PostgresPool, Platform};

    #[test]
    fn sqlite_repo_covers_register_profile_and_delete() {
        let platform = Platform::in_memory().expect("in-memory platform");
        let db = platform.db().lock().expect("db lock");
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
        assert!(repo.active_exists("u1").unwrap());

        assert_eq!(repo.update_name("u1", "alice2").unwrap(), 1);
        assert_eq!(repo.update_avatar("u1", "a.png").unwrap(), 1);
        assert_eq!(repo.update_signature("u1", "sig").unwrap(), 1);
        assert_eq!(
            repo.update_profile("u1", Some("alice3"), Some("a2.png"), Some("sig2"))
                .unwrap(),
            3
        );
        assert_eq!(repo.load_profile("u1").unwrap().unwrap().name, "alice3");

        assert_eq!(repo.set_totp_secret("u1", "sec").unwrap(), 1);
        assert_eq!(repo.totp_secret("u1").unwrap().as_deref(), Some("sec"));
        assert!(!repo.totp_enabled("u1").unwrap());
        assert_eq!(repo.enable_totp("u1").unwrap(), 1);
        assert!(repo.totp_enabled("u1").unwrap());
        assert_eq!(repo.disable_totp("u1").unwrap(), 1);
        assert!(repo.totp_secret("u1").unwrap().is_none());

        assert_eq!(repo.mark_deleted("u1", 99).unwrap(), 1);
        assert!(!repo.active_exists("u1").unwrap());
        assert!(repo.load_credentials("alice3").unwrap().is_none());
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

    #[test]
    #[ignore = "requires WXLIKE_TEST_PG_URL pointing at a DB with M011 applied"]
    fn postgres_backend_users_repo_routes_to_postgres() {
        let url = std::env::var("WXLIKE_TEST_PG_URL")
            .expect("set WXLIKE_TEST_PG_URL to run the PostgreSQL backend test");
        let pg = PostgresPool::open(&url).expect("postgres pool");
        let cleanup = || {
            pg.block_on(
                sqlx::query("DELETE FROM identity_user WHERE user_id = 'pgbe-u1'")
                    .execute(pg.pool()),
            )
            .expect("cleanup");
        };
        cleanup();

        let conn = crate::storage::StorageConnection::Postgres(&pg);
        let repo = super::BackendUsersRepo::new(&conn).expect("backend users repo");
        repo.insert(&NewUser {
            id: "pgbe-u1",
            name: "pgbe-alice",
            pw_salt: "",
            pw_hash: "phc",
            pw_rounds: 0,
            pw_algo: "argon2id",
            avatar: "",
            signature: "",
            created_at: 1,
        })
        .expect("insert through backend");
        assert_eq!(
            repo.find_id_by_name("pgbe-alice", true, None)
                .unwrap()
                .as_deref(),
            Some("pgbe-u1")
        );
        assert!(repo.active_exists("pgbe-u1").unwrap());
        assert_eq!(repo.update_name("pgbe-u1", "pgbe-alice2").unwrap(), 1);
        assert_eq!(repo.mark_deleted("pgbe-u1", 99).unwrap(), 1);
        assert!(!repo.active_exists("pgbe-u1").unwrap());

        cleanup();
    }
}
