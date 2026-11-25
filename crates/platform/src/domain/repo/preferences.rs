//! **仓储层**（`preferences`）：`identity_pref` 的服务端偏好读写。
//!
//! identity crate 的同名函数仍以 `&Connection` 为入口；本文件把它们搬到
//! `StorageConnection` 端口，让 SQLite 与 PG 共用一份 SQL。校验规则必须与
//! `wxlike_identity::preferences` 完全一致（键白名单、值长度/控制字符）。

use std::collections::BTreeMap;

use crate::error::{PlatformError, Result};
use crate::storage::{params, StorageConnection, StorageRow};
use wxlike_identity::preferences::{PrefRow, ServerPrefs, MAX_VALUE_CHARS, SERVER_KEYS};

pub(in crate::domain) fn get(conn: &StorageConnection<'_>, user_id: &str) -> Result<ServerPrefs> {
    let mut values = BTreeMap::new();
    let stmt = conn.prepare("SELECT key, value FROM identity_pref WHERE user_id = ?1")?;
    let rows = stmt.query_map(params![user_id], |r: &StorageRow<'_>| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (k, v) = row?;
        values.insert(k, v);
    }
    Ok(ServerPrefs { values })
}

pub(in crate::domain) fn head_seq(conn: &StorageConnection<'_>, user_id: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM identity_pref WHERE user_id = ?1",
        params![user_id],
        |r: &StorageRow<'_>| r.get(0),
    )
}

pub(in crate::domain) fn patch(
    conn: &StorageConnection<'_>,
    user_id: &str,
    patch: &BTreeMap<String, String>,
    device_id: &str,
    now: i64,
) -> Result<ServerPrefs> {
    for (key, value) in patch {
        if !SERVER_KEYS.contains(&key.as_str()) {
            return Err(PlatformError::BadInput("pref key"));
        }
        if value.chars().count() > MAX_VALUE_CHARS || value.chars().any(|c| c.is_control()) {
            return Err(PlatformError::BadInput("pref value"));
        }
        conn.execute(
            "INSERT INTO identity_pref(user_id, key, value, updated_at, device_id) \
             VALUES(?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(user_id, key) DO UPDATE SET \
                 value = ?3, updated_at = ?4, device_id = ?5",
            params![user_id, key.as_str(), value.as_str(), now, device_id],
        )?;
    }
    get(conn, user_id)
}

pub(in crate::domain) fn since(
    conn: &StorageConnection<'_>,
    user_id: &str,
    since_seq: i64,
    limit: i64,
) -> Result<Vec<PrefRow>> {
    let stmt = conn.prepare(
        "SELECT seq, key, value, updated_at, device_id FROM identity_pref \
         WHERE user_id = ?1 AND seq > ?2 ORDER BY seq ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![user_id, since_seq, limit], |r: &StorageRow<'_>| {
        let seq: i64 = r.get(0)?;
        let key: String = r.get(1)?;
        let value: String = r.get(2)?;
        let updated_at: i64 = r.get(3)?;
        let device_id: String = r.get(4)?;
        Ok(PrefRow {
            seq,
            key,
            value,
            updated_at,
            device_id,
        })
    })?;
    rows.collect::<Result<Vec<_>>>()
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
