//! **仓储层**（`relations`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::domain::relations::StrangerRequest;
use crate::error::Result;
use rusqlite::{params, Connection, OptionalExtension as _};

pub(in crate::domain) fn row_request(r: &rusqlite::Row<'_>) -> rusqlite::Result<StrangerRequest> {
    Ok(StrangerRequest {
        id: r.get(0)?,
        from_uid: r.get(1)?,
        to_uid: r.get(2)?,
        text: r.get(3)?,
        status: r.get(4)?,
        created_at: r.get(5)?,
    })
}

pub(in crate::domain) fn user_exists(c: &Connection, user: &str) -> Result<bool> {
    let exists: Option<String> = c
        .query_row("SELECT id FROM users WHERE id = ?1", params![user], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(exists.is_some())
}

pub(in crate::domain) fn insert_block(
    c: &Connection,
    user: &str,
    target: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT OR IGNORE INTO blocks(user_id, blocked_id, created_at) VALUES(?1, ?2, ?3)",
        params![user, target, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn delete_block(
    c: &Connection,
    user: &str,
    target: &str,
) -> Result<usize> {
    Ok(c.execute(
        "DELETE FROM blocks WHERE user_id = ?1 AND blocked_id = ?2",
        params![user, target],
    )?)
}

pub(in crate::domain) fn blocked_ids(c: &Connection, user: &str) -> Result<Vec<String>> {
    let mut stmt =
        c.prepare("SELECT blocked_id FROM blocks WHERE user_id = ?1 ORDER BY created_at")?;
    let rows = stmt.query_map(params![user], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub(in crate::domain) fn blocked(c: &Connection, user: &str, by: &str) -> Result<bool> {
    Ok(c.query_row(
        "SELECT 1 FROM blocks WHERE user_id = ?1 AND blocked_id = ?2",
        params![by, user],
        |_| Ok(()),
    )
    .optional()?
    .is_some())
}

/// 同 from→to 已有 pending 则更新文本与时间；其余请求行不动。
pub(in crate::domain) fn upsert_request(
    c: &Connection,
    id: &str,
    from: &str,
    to: &str,
    text: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO message_requests(id, from_uid, to_uid, text, status, created_at)
         VALUES(?1, ?2, ?3, ?4, 'pending', ?5)
         ON CONFLICT(from_uid, to_uid, status) DO UPDATE SET
           text = excluded.text, created_at = excluded.created_at
         WHERE status = 'pending'",
        params![id, from, to, text, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn requests_in(c: &Connection, user: &str) -> Result<Vec<StrangerRequest>> {
    let mut stmt = c.prepare(
        "SELECT id, from_uid, to_uid, text, status, created_at FROM message_requests
         WHERE to_uid = ?1 AND status = 'pending' ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map(params![user], row_request)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub(in crate::domain) fn requests_out(c: &Connection, user: &str) -> Result<Vec<StrangerRequest>> {
    let mut stmt = c.prepare(
        "SELECT id, from_uid, to_uid, text, status, created_at FROM message_requests
         WHERE from_uid = ?1 ORDER BY created_at DESC LIMIT 50",
    )?;
    let rows = stmt.query_map(params![user], row_request)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub(in crate::domain) fn decide_request(
    c: &Connection,
    user: &str,
    id: &str,
    status: &str,
) -> Result<usize> {
    Ok(c.execute(
        "UPDATE message_requests SET status = ?1
         WHERE id = ?2 AND to_uid = ?3 AND status = 'pending'",
        params![status, id, user],
    )?)
}

pub(in crate::domain) fn accepted_request(c: &Connection, from: &str, to: &str) -> Result<bool> {
    Ok(c.query_row(
        "SELECT 1 FROM message_requests
         WHERE from_uid = ?1 AND to_uid = ?2 AND status = 'accepted'",
        params![from, to],
        |_| Ok(()),
    )
    .optional()?
    .is_some())
}

pub(in crate::domain) fn allow_strangers(c: &Connection, user: &str) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT allow_strangers FROM user_settings WHERE user_id = ?1",
        params![user],
        |r| r.get(0),
    )
    .optional()?)
}

pub(in crate::domain) fn set_allow_strangers(
    c: &Connection,
    user: &str,
    allow: bool,
) -> Result<()> {
    c.execute(
        "INSERT INTO user_settings(user_id, allow_strangers) VALUES(?1, ?2)
         ON CONFLICT(user_id) DO UPDATE SET allow_strangers = excluded.allow_strangers",
        params![user, allow as i64],
    )?;
    Ok(())
}

pub(in crate::domain) fn patch_friend_meta(
    c: &Connection,
    user: &str,
    friend: &str,
    starred: Option<i64>,
    tags_json: Option<String>,
    group: Option<String>,
) -> Result<usize> {
    Ok(c.execute(
        "UPDATE friends SET
           starred = COALESCE(?3, starred),
           tags = COALESCE(?4, tags),
           friend_group = COALESCE(?5, friend_group)
         WHERE user_id = ?1 AND friend_id = ?2",
        params![user, friend, starred, tags_json, group],
    )?)
}
