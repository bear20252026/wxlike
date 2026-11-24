//! **仓储层**（`governance`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。
//!
//! 这里的函数沿用 `rusqlite::Result`，因为调用方（identity 端口实现）要按
//! `IdError::Db` 原样上报数据库错误；连接入口已统一成 `StorageConnection`，
//! 由 `sqlite_ref` 取旧句柄，PG 变体返回 `InvalidQuery`（治理镜像只有文件库
//! 的 sidecar 连接）。

use rusqlite::{params, Connection};

use crate::storage::StorageConnection;

fn sqlite<'a>(port: &'a StorageConnection<'_>) -> rusqlite::Result<&'a Connection> {
    port.sqlite_ref().ok_or(rusqlite::Error::InvalidQuery)
}

pub(in crate::domain) fn mirror_user_state(
    port: &StorageConnection<'_>,
    user_id: &str,
    suspended: bool,
    disabled: bool,
    until_ms: Option<i64>,
) -> rusqlite::Result<()> {
    sqlite(port)?.execute(
        "UPDATE users SET suspended = ?2, disabled = ?3, suspended_until_ms = ?4 \
         WHERE id = ?1",
        params![user_id, suspended as i64, disabled as i64, until_ms],
    )?;
    Ok(())
}

/// 参与的未解散群数（relations 域自有表，与平台同库）。
pub(in crate::domain) fn group_count(
    port: &StorageConnection<'_>,
    user_id: &str,
) -> rusqlite::Result<i64> {
    sqlite(port)?.query_row(
        "SELECT COUNT(DISTINCT m.group_id) FROM group_members m \
         LEFT JOIN group_meta g ON g.group_id = m.group_id \
         WHERE m.user_id = ?1 AND COALESCE(g.dissolved_at, 0) = 0",
        params![user_id],
        |r| r.get(0),
    )
}

/// 针对该用户的举报工单数（平台 `reports` 表，按 target_kind='user'）。
pub(in crate::domain) fn report_count(
    port: &StorageConnection<'_>,
    user_id: &str,
) -> rusqlite::Result<i64> {
    sqlite(port)?.query_row(
        "SELECT COUNT(*) FROM reports WHERE target_kind = 'user' AND target_id = ?1",
        params![user_id],
        |r| r.get(0),
    )
}
