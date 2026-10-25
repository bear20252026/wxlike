//! G14 运营域的存储层：**一张库、一套迁移、一个写锁**。
//!
//! 为什么不是每个子域各开一条连接：SQLite 在同一文件上开多条连接没问题，
//! 但"谁在什么时候写了什么"会散成六份 WAL 状态，回滚和备份都要拼。
//! G14 的六个子域（auth/audit/identity/publishing/commerce/jobs）共用
//! [`Store`]，迁移只有一份（`migrations/0001_g14_ops.sql`），回滚也只有一份。
//!
//! 这个 store 与 [`crate::accounts::AccountStore`] 指向**同一个文件**
//! （`WXLIKE_ADMIN_DB`），但各自持有连接、各管自己的表：账号/会话/留痕是 D3 的
//! 既有契约，G14 只在旁边加 `g14_*` 表，不做任何 `ALTER`。
//!
//! 一条硬约束写在 SQL 里而不是只写在文档里：`g14_audit` 与 `g14_journal`
//! 由触发器拒绝 UPDATE/DELETE。追责链和补账分录都不允许"事后编辑"。

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::Connection;

use crate::error::Result;

/// 迁移正本。内嵌进二进制：部署时不需要额外分发 .sql 文件。
pub const UP_SQL: &str = include_str!("../migrations/0001_g14_ops.sql");

/// 回滚脚本。同样内嵌，供 `Store::rollback_all`（运维/测试）使用。
pub const DOWN_SQL: &str = include_str!("../migrations/0001_g14_ops.down.sql");

/// 本进程认识的 schema 版本。`PRAGMA user_version` 比它大 = 有新版本进程写过
/// 这份库，旧进程不该继续猜结构。
pub const SCHEMA_VERSION: i64 = 1;

const CONNECTION_PRAGMAS: &str = concat!(
    "PRAGMA journal_mode = WAL;\n",
    "PRAGMA foreign_keys = ON;\n",
    "PRAGMA busy_timeout = 5000;",
);

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    /// 打开（必要时创建）adminx 库并跑迁移。迁移是幂等的：`CREATE ... IF NOT EXISTS`。
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let conn = Connection::open(path)?;
        Self::prepare(conn)
    }

    /// 内存库：只给测试用。生产配置里 `WXLIKE_ADMIN_DB=:memory:` 是允许的，
    /// 因为 adminx 自己就是这份库的唯一进程；平台库才禁止内存（见 config.rs）。
    pub fn open_memory() -> Result<Self> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(conn: Connection) -> Result<Self> {
        conn.execute_batch(CONNECTION_PRAGMAS)?;
        // 先读**既有**版本再决定是否迁移：反过来的话，迁移脚本里的
        // `PRAGMA user_version = 1` 会把"这是个更新的库"这个事实直接抹掉。
        let found: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if found > SCHEMA_VERSION {
            return Err(crate::error::AdminError::bad_input(format!(
                "adminx 库的 schema 版本是 {found}，本进程只认识到 {SCHEMA_VERSION}：\
                 拒绝在更新的库上按旧结构写数据（换新版本二进制再起）"
            )));
        }
        if found < SCHEMA_VERSION {
            conn.execute_batch(UP_SQL)?;
        }
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// 锁中毒只可能发生在一次 panic 中途，连接本身还活着：恢复守卫比整进程崩掉好。
    pub(crate) fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn user_version(&self) -> Result<i64> {
        Ok(self
            .lock()
            .query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    /// 执行回滚脚本。**会丢数据**，只给测试与人工回滚用（生产先快照）。
    pub fn rollback_all(&self) -> Result<()> {
        self.lock().execute_batch(DOWN_SQL)?;
        Ok(())
    }

    /// 库里的表名快照。给 `/health/summary` 与迁移用例核对"该建的都建了"。
    pub fn tables(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'g14_%' ORDER BY name",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_is_idempotent_and_brings_every_table() {
        let store = Store::open_memory().unwrap();
        let before = store.tables().unwrap();
        assert_eq!(store.user_version().unwrap(), SCHEMA_VERSION);
        for want in [
            "g14_audit",
            "g14_stepup",
            "g14_case",
            "g14_review_item",
            "g14_service_config",
            "g14_mini_release",
            "g14_merchant",
            "g14_card",
            "g14_card_redeem",
            "g14_order",
            "g14_refund",
            "g14_journal",
            "g14_recon_diff",
            "g14_job",
            "g14_privacy_request",
        ] {
            assert!(before.iter().any(|t| t == want), "缺表 {want}");
        }
        // 再跑一遍迁移：不该报错，也不该多出表
        store.lock().execute_batch(UP_SQL).unwrap();
        assert_eq!(store.tables().unwrap(), before);
    }

    #[test]
    fn rollback_removes_only_its_own_tables() {
        let store = Store::open_memory().unwrap();
        // 模拟 D3 既有表存在
        store
            .lock()
            .execute_batch("CREATE TABLE admin_oplog (id INTEGER PRIMARY KEY);")
            .unwrap();
        store.rollback_all().unwrap();
        assert!(store.tables().unwrap().is_empty());
        assert_eq!(store.user_version().unwrap(), 0);
        // 既有表必须还在：回滚不碰别人的对象
        let keep: i64 = store
            .lock()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='admin_oplog'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(keep, 1);
        // 回滚后还能重新迁移（降级-升级往返）
        store.lock().execute_batch(UP_SQL).unwrap();
        assert_eq!(store.user_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn audit_and_journal_are_append_only_at_the_database_level() {
        let store = Store::open_memory().unwrap();
        let conn = store.lock();
        conn.execute(
            "INSERT INTO g14_audit (at_ms, actor, role, action, object_kind, object_id, reason, \
             trace, outcome, prev_hash, hash) VALUES (1,'ops','viewer','x','y','z','','t','ok','p','h1')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO g14_journal (id, entry_no, line_no, account, direction, amount_cents, memo, \
             source, created_by, created_at) VALUES ('j1','E1',1,'acct','debit',100,'m','manual','ops',1)",
            [],
        )
        .unwrap();
        for sql in [
            "UPDATE g14_audit SET actor='evil'",
            "DELETE FROM g14_audit",
            "UPDATE g14_journal SET amount_cents=999",
        ] {
            let err = conn.execute(sql, []).unwrap_err();
            assert!(
                err.to_string().contains("append-only"),
                "{sql} 必须被数据库自己拦下，实际 {err}"
            );
        }
    }

    #[test]
    fn newer_schema_is_refused_instead_of_guessed() {
        let path = std::env::temp_dir().join(format!(
            "wxlike-admin-g14-{}.db",
            wxlike_platform::random_hex(8)
        ));
        {
            let store = Store::open(&path).unwrap();
            store
                .lock()
                .execute_batch("PRAGMA user_version = 9;")
                .unwrap();
        }
        let err = match Store::open(&path) {
            Ok(_) => panic!("更新的 schema 必须被拒"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("schema 版本"), "{err}");
        let _ = std::fs::remove_file(&path);
    }
}
