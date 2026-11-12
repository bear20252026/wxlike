//! 事务包装：把"开事务 → 跑 → 成功才提交"收敛到一处。
//!
//! ## 为什么收
//!
//! 手写事务是四段：`c.transaction()?` → body → `tx.commit()?` → 返回。这个形状本身
//! 没有正确性问题——`Transaction` 在 drop 时回滚，漏写 `commit` 的结果是"什么都没
//! 发生"，不是"写了一半"。但它把**同一条不变量**（body 里任何 `Err` 都不该留下半条
//! 写入）抄在了 19 个文件、57 个地方，每处都要靠人重新论证一遍。收成一个函数后，
//! 这条不变量只论证一次，就在下面。
//!
//! ## 不收什么（以及为什么）
//!
//! 1. **`transaction_with_behavior(Immediate)`**：它先拿 `BEGIN IMMEDIATE` 写锁，与
//!    `transaction()` 的 Deferred 语义不同——并发写场景下两者不可互换。用 Deferred
//!    的包装去套 Immediate 站点，等于悄悄丢掉"先拿锁"，所以那些站点保持显式。
//! 2. **`unchecked_transaction()`**：它给的是 `&Connection` 而不是 `&mut`，用于已经
//!    持有读句柄时复用连接；包成 `&mut self` 会迫使调用方改锁的拿法。
//! 3. **错误类型不认识 `rusqlite::Error` 的存储**：`E` 必须是
//!    `From<rusqlite::Error>`——开事务与提交失败都是 `rusqlite::Error`，得有个统一
//!    落点。`wxlike-client-core` 的 `StoreError` 是平台中立的端口类型，刻意不认识
//!    具体驱动，所以 `client-native` 那一处不套用这个包装（理由写在那里）。

use rusqlite::{Connection, Transaction};

/// 在 [`Connection`] 上开事务跑一段写入。
///
/// 两个方法都**只在成功时提交**：`Err` 与"主动放弃"都走 drop 回滚，调用方不需要、
/// 也不应该自己调 `commit` / `rollback`。
pub trait ConnectionTx {
    /// 跑 `body`：`Ok(v)` → 提交并交出 `v`；`Err(e)` → 回滚并交出 `e`。
    fn with_tx<T, E>(
        &mut self,
        body: impl FnOnce(&Transaction<'_>) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<rusqlite::Error>;

    /// 同 [`ConnectionTx::with_tx`]，但 `body` 可以**主动放弃**：`Ok(None)` → 回滚，
    /// `Ok(Some(v))` → 提交。
    ///
    /// 给"这件事做了、但结论是不做"的场合用：比如重复投递——序号已经烧掉了，必须
    /// 回滚把序号还回去，而返回值是"没有新消息"，不是错误。
    fn with_tx_opt<T, E>(
        &mut self,
        body: impl FnOnce(&Transaction<'_>) -> Result<Option<T>, E>,
    ) -> Result<Option<T>, E>
    where
        E: From<rusqlite::Error>;
}

impl ConnectionTx for Connection {
    fn with_tx<T, E>(&mut self, body: impl FnOnce(&Transaction<'_>) -> Result<T, E>) -> Result<T, E>
    where
        E: From<rusqlite::Error>,
    {
        let tx = self.transaction()?;
        let out = body(&tx)?;
        // 只有走到这一行才提交：上面任何 `?` 都会让 `tx` 离开作用域并回滚。
        tx.commit()?;
        Ok(out)
    }

    fn with_tx_opt<T, E>(
        &mut self,
        body: impl FnOnce(&Transaction<'_>) -> Result<Option<T>, E>,
    ) -> Result<Option<T>, E>
    where
        E: From<rusqlite::Error>,
    {
        let tx = self.transaction()?;
        match body(&tx)? {
            Some(out) => {
                tx.commit()?;
                Ok(Some(out))
            }
            // 主动放弃：不调 `commit`，`tx` 离开作用域时回滚。
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT NOT NULL);")
            .unwrap();
        conn
    }

    fn count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn commits_when_body_is_ok() {
        let mut conn = conn();
        conn.with_tx(|tx| -> rusqlite::Result<()> {
            tx.execute("INSERT INTO t(v) VALUES('a')", [])?;
            Ok(())
        })
        .unwrap();
        assert_eq!(count(&conn), 1);
    }

    #[test]
    fn rolls_back_when_body_errors() {
        let mut conn = conn();
        let err = conn
            .with_tx(|tx| -> rusqlite::Result<()> {
                tx.execute("INSERT INTO t(v) VALUES('a')", [])?;
                // 表不存在：制造一个真实的 SQLite 错误
                tx.execute("INSERT INTO no_such_table VALUES('x')", [])?;
                Ok(())
            })
            .unwrap_err();
        assert!(err.to_string().contains("no_such_table"), "{err}");
        assert_eq!(count(&conn), 0, "出错后写入必须回滚");
    }

    #[test]
    fn opt_commits_on_some_and_rolls_back_on_none() {
        let mut committed_conn = conn();
        let committed = committed_conn
            .with_tx_opt(|tx| -> rusqlite::Result<Option<i64>> {
                tx.execute("INSERT INTO t(v) VALUES('a')", [])?;
                Ok(Some(7))
            })
            .unwrap();
        assert_eq!(committed, Some(7));
        assert_eq!(count(&committed_conn), 1);

        let mut discarded_conn = conn();
        let discarded = discarded_conn
            .with_tx_opt(|tx| -> rusqlite::Result<Option<i64>> {
                tx.execute("INSERT INTO t(v) VALUES('a')", [])?;
                Ok(None)
            })
            .unwrap();
        assert_eq!(discarded, None);
        assert_eq!(count(&discarded_conn), 0, "放弃后写入必须回滚");
    }

    /// `E` 不必是 `rusqlite::Error`：只要它有 `From<rusqlite::Error>`，开事务与
    /// 提交的错误也能落进来。
    ///
    /// 注意闭包上要写 `-> Result<(), Boom>`：`with_tx` 的 `E` 由闭包返回类型决定，
    /// 光一个 `Ok(())` 定不下来（外层 `?` 只给 `Boom: From<E>`，给不出 `E` 是谁）。
    #[test]
    fn error_type_is_generic() {
        #[derive(Debug, thiserror::Error)]
        enum Boom {
            #[error("sqlite: {0}")]
            Sqlite(#[from] rusqlite::Error),
        }

        fn run(conn: &mut Connection) -> Result<(), Boom> {
            conn.with_tx(|tx| -> Result<(), Boom> {
                tx.execute("INSERT INTO t(v) VALUES('a')", [])?;
                Ok(())
            })?;
            Ok(())
        }

        let mut conn = conn();
        run(&mut conn).unwrap();
    }
}
