//! **仓储层**（`preferences`）：`identity_pref` 的服务端偏好读写。
//!
//! SQL 与校验只有一份：本层只把 `StorageConnection` 适配成 `SqlExec`，调用
//! identity 的 `preferences::*_with`。SQLite 与 PG 共用同一套键白名单/值校验，
//! 平台不再手抄第二份 SQL。

use std::collections::BTreeMap;

use crate::error::Result;
use crate::storage::sql_port::StorageSqlExec;
use crate::storage::StorageConnection;
use wxlike_identity::preferences::{self, PrefRow, ServerPrefs};

pub(in crate::domain) fn get(conn: &StorageConnection<'_>, user_id: &str) -> Result<ServerPrefs> {
    Ok(preferences::get_with(&StorageSqlExec(conn), user_id)?)
}

pub(in crate::domain) fn head_seq(conn: &StorageConnection<'_>, user_id: &str) -> Result<i64> {
    Ok(preferences::head_seq_with(&StorageSqlExec(conn), user_id)?)
}

pub(in crate::domain) fn patch(
    conn: &StorageConnection<'_>,
    user_id: &str,
    patch: &BTreeMap<String, String>,
    device_id: &str,
    now: i64,
) -> Result<ServerPrefs> {
    Ok(preferences::patch_with(
        &StorageSqlExec(conn),
        user_id,
        patch,
        device_id,
        now,
    )?)
}

pub(in crate::domain) fn since(
    conn: &StorageConnection<'_>,
    user_id: &str,
    since_seq: i64,
    limit: i64,
) -> Result<Vec<PrefRow>> {
    Ok(preferences::since_with(
        &StorageSqlExec(conn),
        user_id,
        since_seq,
        limit,
    )?)
}

#[cfg(test)]
mod tests {
    use super::{get, head_seq, patch, since};
    use crate::storage::params;
    use crate::Db;

    #[test]
    fn sqlite_preferences_roundtrip_and_increment() {
        let db = Db::open(None).expect("db");
        let conn = db.acquire();
        conn.execute(
            "DELETE FROM identity_pref WHERE user_id = ?1",
            params!["pref-user"],
        )
        .expect("clean");
        let patch_map = [("notify.dm".to_string(), "on".to_string())]
            .into_iter()
            .collect();
        patch(&conn, "pref-user", &patch_map, "dev-1", 10).expect("patch");
        let read = get(&conn, "pref-user").expect("get");
        assert_eq!(read.get("notify.dm"), Some("on"));
        let head = head_seq(&conn, "pref-user").expect("head");
        let rows = since(&conn, "pref-user", 0, 50).expect("since");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seq, head);
        assert_eq!(rows[0].device_id, "dev-1");
    }
}
