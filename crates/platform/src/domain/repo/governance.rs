//! **仓储层**（`governance`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。
//!
//! 函数统一走 `StorageConnection` 端口：SQLite sidecar 与 PG profile 的池连接
//! 都能执行同一组 SQL，错误由调用方映射成 identity 的存储错误。

use crate::error::Result;
use crate::storage::{params, StorageConnection};

pub(in crate::domain) fn mirror_user_state(
    port: &StorageConnection<'_>,
    user_id: &str,
    suspended: bool,
    disabled: bool,
    until_ms: Option<i64>,
) -> Result<()> {
    port.execute(
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
) -> Result<i64> {
    port.query_row(
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
) -> Result<i64> {
    port.query_row(
        "SELECT COUNT(*) FROM reports WHERE target_kind = 'user' AND target_id = ?1",
        params![user_id],
        |r| r.get(0),
    )
}

#[cfg(test)]
mod pg_tests {
    use super::{group_count, report_count};
    use crate::storage::params;
    use crate::storage::StorageProfile;
    use crate::Db;

    #[test]
    #[ignore = "requires WXLIKE_TEST_PG_URL and M014 applied"]
    fn postgres_governance_counts_read_platform_tables() {
        let Ok(url) = std::env::var("WXLIKE_TEST_PG_URL") else {
            return;
        };
        let db = Db::open_profile(StorageProfile::Postgres { url }, None).expect("pg db");
        let conn = db.acquire();
        let user = "pg-gov-user";
        conn.execute("DELETE FROM reports WHERE target_id = ?1", params![user])
            .expect("clean reports");
        conn.execute(
            "INSERT INTO reports(id, reporter, target_kind, target_id, reason, detail, state, \
             created_at, updated_at, history_json) \
             VALUES(?1, 'r', 'user', ?2, 'reason', '', 'open', 1, 1, '[]')",
            params!["pg-report-1", user],
        )
        .expect("insert report");
        assert_eq!(report_count(&conn, user).expect("reports"), 1);
        let groups = group_count(&conn, user).expect("groups");
        assert!(groups >= 0);
    }
}
