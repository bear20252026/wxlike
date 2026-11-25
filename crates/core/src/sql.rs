//! SQL 行映射与连接句柄的公共工具。
//!
//! `query_row` 的多列闭包是纯搬运：`Ok((r.get(0)?, r.get(1)?, …))`。
//! 这类闭包一旦超过七八列，复制粘贴时最容易漏一列，而漏列只会让某个
//! 字段悄悄变成下一列的值（类型相同就编译通过）。把列号写成一行，
//! 列数一眼可数，也让不同仓储的映射形状保持一致。

/// 把一行的若干列收成一个元组：`|r| row_tuple!(r; 0, 1, 2)`。
///
/// 返回值已经是 `Ok(...)`，可直接作为 `query_row` 的映射闭包。
#[macro_export]
macro_rules! row_tuple {
    ($row:expr; $($idx:expr),+ $(,)?) => {
        Ok(($( $row.get($idx)?, )+))
    };
}

/// rusqlite 把 UNIQUE 索引冲突折成 `SqliteFailure`，消息文本是唯一线索。
/// 这是并发兜底：能预检的地方仍应先做确定性检查。
pub fn is_unique_violation(err: &rusqlite::Error) -> bool {
    match err {
        rusqlite::Error::SqliteFailure(_, Some(msg)) => msg.contains("UNIQUE"),
        _ => false,
    }
}

/// 单连接 + 互斥锁：SQLite 的日常形态（outbox、移动端缓存都是它）。
///
/// 锁中毒时继续用内层连接：一次 panic 不该让队列永久不可用；数据一致性由
/// SQLite 事务保证，而不是靠把后续调用全部拒掉。
pub struct ConnectionHandle(std::sync::Mutex<rusqlite::Connection>);

impl ConnectionHandle {
    pub fn new(conn: rusqlite::Connection) -> Self {
        Self(std::sync::Mutex::new(conn))
    }

    pub fn lock(&self) -> std::sync::MutexGuard<'_, rusqlite::Connection> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
