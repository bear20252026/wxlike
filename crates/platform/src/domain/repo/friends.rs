//! **仓储层**（`friends`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use crate::storage::{params, OptionalExt as _, StorageConnection};

use crate::domain::friends::{FriendRecord, RequestRecord};

/// 增量通讯录变更行。
pub(in crate::domain) struct ChangeRecord {
    pub seq: i64,
    pub subject_id: String,
    pub kind: String,
    pub changed_at: i64,
}

fn request_record(row: &crate::storage::StorageRow<'_>) -> Result<RequestRecord> {
    Ok(RequestRecord {
        id: row.get(0)?,
        from_uid: row.get(1)?,
        to_uid: row.get(2)?,
        message: row.get(3)?,
        status: row.get(4)?,
        created_at: row.get(5)?,
    })
}

/// Appends one address-book change for `user` about `subject`.
pub(in crate::domain) fn log(
    c: &StorageConnection<'_>,
    user: &str,
    subject: &str,
    kind: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO contact_log(user_id, subject_id, kind, changed_at) VALUES(?1, ?2, ?3, ?4)",
        params![user, subject, kind, now],
    )?;
    Ok(())
}

pub(in crate::domain) use crate::domain::repo::users::user_exists;

/// 同一对 (from, to) 只保留一条申请；返回已有 id 与状态。
pub(in crate::domain) fn request_by_pair(
    c: &StorageConnection<'_>,
    from_uid: &str,
    to_uid: &str,
) -> Result<Option<(String, String)>> {
    let row = c
        .query_row(
            "SELECT id, status FROM friend_requests WHERE from_uid = ?1 AND to_uid = ?2",
            params![from_uid, to_uid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row)
}

pub(in crate::domain) fn insert_request(
    c: &StorageConnection<'_>,
    id: &str,
    from_uid: &str,
    to_uid: &str,
    message: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO friend_requests(id, from_uid, to_uid, message, status, created_at)
         VALUES(?1, ?2, ?3, ?4, 'pending', ?5)",
        params![id, from_uid, to_uid, message, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn reopen_request(
    c: &StorageConnection<'_>,
    id: &str,
    message: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "UPDATE friend_requests SET status = 'pending', message = ?2, created_at = ?3 WHERE id = ?1",
        params![id, message, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn request_by_id(
    c: &StorageConnection<'_>,
    req_id: &str,
) -> Result<Option<RequestRecord>> {
    let row = c
        .query_row(
            "SELECT id, from_uid, to_uid, message, status, created_at FROM friend_requests WHERE id = ?1",
            params![req_id],
            request_record,
        )
        .optional()?;
    Ok(row)
}

pub(in crate::domain) fn accept_request(c: &StorageConnection<'_>, req_id: &str) -> Result<bool> {
    let changed = c.execute(
        "UPDATE friend_requests SET status = 'accepted' WHERE id = ?1 AND status = 'pending'",
        params![req_id],
    )?;
    Ok(changed == 1)
}

pub(in crate::domain) fn decline_request(c: &StorageConnection<'_>, req_id: &str) -> Result<bool> {
    let changed = c.execute(
        "UPDATE friend_requests SET status = 'declined' WHERE id = ?1 AND status = 'pending'",
        params![req_id],
    )?;
    Ok(changed == 1)
}

pub(in crate::domain) fn ensure_friend(
    c: &StorageConnection<'_>,
    user_id: &str,
    friend_id: &str,
    source: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO friends(user_id, friend_id, remark, source, created_at)
         VALUES(?1, ?2, '', ?3, ?4)
         ON CONFLICT(user_id, friend_id) DO NOTHING",
        params![user_id, friend_id, source, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn incoming_requests(
    c: &StorageConnection<'_>,
    me: &str,
) -> Result<Vec<RequestRecord>> {
    let stmt = c.prepare(
        "SELECT id, from_uid, to_uid, message, status, created_at
         FROM friend_requests WHERE to_uid = ?1 AND status = 'pending'
         ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map(params![me], request_record)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
}

pub(in crate::domain) fn outgoing_requests(
    c: &StorageConnection<'_>,
    me: &str,
) -> Result<Vec<RequestRecord>> {
    let stmt = c.prepare(
        "SELECT id, from_uid, to_uid, message, status, created_at
         FROM friend_requests WHERE from_uid = ?1 ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map(params![me], request_record)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
}

pub(in crate::domain) fn friends(c: &StorageConnection<'_>, me: &str) -> Result<Vec<FriendRecord>> {
    let stmt = c.prepare(
        "SELECT friend_id, remark, source, created_at, starred, tags, friend_group
         FROM friends WHERE user_id = ?1 ORDER BY created_at",
    )?;
    let rows = stmt.query_map(params![me], |r| {
        Ok(FriendRecord {
            friend_id: r.get(0)?,
            remark: r.get(1)?,
            source: r.get(2)?,
            created_at: r.get(3)?,
            starred: r.get::<_, i64>(4)? != 0,
            tags: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or(serde_json::json!([])),
            friend_group: r.get(6)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
}

pub(in crate::domain) fn are_friends(c: &StorageConnection<'_>, a: &str, b: &str) -> Result<bool> {
    let hit = c
        .query_row(
            "SELECT 1 FROM friends WHERE user_id = ?1 AND friend_id = ?2",
            params![a, b],
            |r| r.get::<_, i64>(0),
        )
        .optional()?;
    Ok(hit.is_some())
}

pub(in crate::domain) fn set_remark(
    c: &StorageConnection<'_>,
    me: &str,
    friend: &str,
    remark: &str,
) -> Result<bool> {
    let changed = c.execute(
        "UPDATE friends SET remark = ?1 WHERE user_id = ?2 AND friend_id = ?3",
        params![remark, me, friend],
    )?;
    Ok(changed == 1)
}

pub(in crate::domain) fn delete_friend(
    c: &StorageConnection<'_>,
    user_id: &str,
    friend_id: &str,
) -> Result<()> {
    c.execute(
        "DELETE FROM friends WHERE user_id = ?1 AND friend_id = ?2",
        params![user_id, friend_id],
    )?;
    Ok(())
}

pub(in crate::domain) fn changes(
    c: &StorageConnection<'_>,
    me: &str,
    since: i64,
    limit: i64,
) -> Result<Vec<ChangeRecord>> {
    let stmt = c.prepare(
        "SELECT seq, subject_id, kind, changed_at FROM contact_log
         WHERE user_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![me, since, limit], |r| {
        Ok(ChangeRecord {
            seq: r.get(0)?,
            subject_id: r.get(1)?,
            kind: r.get(2)?,
            changed_at: r.get(3)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
}

pub(in crate::domain) fn changes_head(c: &StorageConnection<'_>, me: &str) -> Result<i64> {
    let seq = c.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM contact_log WHERE user_id = ?1",
        params![me],
        |r| r.get(0),
    )?;
    Ok(seq)
}
