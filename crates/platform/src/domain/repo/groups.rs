//! **仓储层**（`groups`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。
//!
//! 业务规则（谁能建群、谁能移除谁、成员上限、解散语义）留在
//! [`crate::domain::groups`]；本文件只负责语句、事务边界外的读写与行映射。

use crate::domain::groups::{GroupSummary, MemberRow};
use crate::error::Result;
use crate::storage::StorageConnection;
use rusqlite::{params, OptionalExtension as _};

pub(in crate::domain) fn idempotent_response(
    conn: &StorageConnection<'_>,
    key: &str,
) -> Result<Option<String>> {
    let conn = conn.sqlite()?;
    Ok(conn
        .query_row(
            "SELECT response FROM idempotency WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()?)
}

pub(in crate::domain) fn remember_idempotent(
    conn: &StorageConnection<'_>,
    key: &str,
    response_json: &str,
    now: i64,
) -> Result<()> {
    let conn = conn.sqlite()?;
    conn.execute(
        "INSERT INTO idempotency(key, response, created_at) VALUES(?1, ?2, ?3)
         ON CONFLICT(key) DO NOTHING",
        params![key, response_json, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn member_ids(
    conn: &StorageConnection<'_>,
    group_id: &str,
) -> Result<Vec<String>> {
    let conn = conn.sqlite()?;
    let mut stmt =
        conn.prepare("SELECT user_id FROM group_members WHERE group_id = ?1 ORDER BY joined_at")?;
    let rows = stmt
        .query_map(params![group_id], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(in crate::domain) fn user_exists(conn: &StorageConnection<'_>, user_id: &str) -> Result<bool> {
    let conn = conn.sqlite()?;
    Ok(conn
        .query_row(
            "SELECT 1 FROM users WHERE id = ?1",
            params![user_id],
            |_| Ok(1i64),
        )
        .optional()?
        .is_some())
}

pub(in crate::domain) fn insert_group(
    conn: &StorageConnection<'_>,
    id: &str,
    name: &str,
    owner: &str,
    now: i64,
) -> Result<()> {
    let conn = conn.sqlite()?;
    conn.execute(
        "INSERT INTO groups(id, name, owner_uid, created_at) VALUES(?1, ?2, ?3, ?4)",
        params![id, name, owner, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn insert_member(
    conn: &StorageConnection<'_>,
    group_id: &str,
    user_id: &str,
    role: &str,
    now: i64,
) -> Result<usize> {
    let conn = conn.sqlite()?;
    Ok(conn.execute(
        "INSERT INTO group_members(group_id, user_id, role, my_remark, joined_at)
         VALUES(?1, ?2, ?3, '', ?4)
         ON CONFLICT(group_id, user_id) DO NOTHING",
        params![group_id, user_id, role, now],
    )?)
}

pub(in crate::domain) fn group_header(
    conn: &StorageConnection<'_>,
    group_id: &str,
) -> Result<Option<(String, String, String, i64)>> {
    let conn = conn.sqlite()?;
    Ok(conn
        .query_row(
            "SELECT id, name, owner_uid, created_at FROM groups WHERE id = ?1",
            params![group_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?)
}

pub(in crate::domain) fn group_exists(
    conn: &StorageConnection<'_>,
    group_id: &str,
) -> Result<bool> {
    let conn = conn.sqlite()?;
    Ok(conn
        .query_row(
            "SELECT 1 FROM groups WHERE id = ?1",
            params![group_id],
            |_| Ok(1i64),
        )
        .optional()?
        .is_some())
}

pub(in crate::domain) fn role_of(
    conn: &StorageConnection<'_>,
    group_id: &str,
    user_id: &str,
) -> Result<Option<String>> {
    let conn = conn.sqlite()?;
    Ok(conn
        .query_row(
            "SELECT role FROM group_members WHERE group_id = ?1 AND user_id = ?2",
            params![group_id, user_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
}

pub(in crate::domain) fn members(
    conn: &StorageConnection<'_>,
    group_id: &str,
) -> Result<Vec<MemberRow>> {
    let conn = conn.sqlite()?;
    let mut stmt = conn.prepare(
        "SELECT user_id, role, my_remark, joined_at FROM group_members
         WHERE group_id = ?1 ORDER BY joined_at, user_id",
    )?;
    let rows = stmt.query_map(params![group_id], map_member)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub(in crate::domain) fn members_page(
    conn: &StorageConnection<'_>,
    group_id: &str,
    after_user: Option<&str>,
    limit: i64,
) -> Result<Vec<MemberRow>> {
    let conn = conn.sqlite()?;
    let rows = match after_user.filter(|u| !u.is_empty()) {
        Some(after) => {
            let joined: Option<i64> = conn
                .query_row(
                    "SELECT joined_at FROM group_members
                     WHERE group_id = ?1 AND user_id = ?2",
                    params![group_id, after],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(joined) = joined else {
                return Ok(Vec::new());
            };
            let mut stmt = conn.prepare(
                "SELECT user_id, role, my_remark, joined_at FROM group_members
                 WHERE group_id = ?1
                   AND (joined_at > ?2 OR (joined_at = ?2 AND user_id > ?3))
                 ORDER BY joined_at, user_id LIMIT ?4",
            )?;
            let mapped = stmt.query_map(params![group_id, joined, after, limit], map_member)?;
            mapped.collect::<std::result::Result<Vec<_>, _>>()?
        }
        None => {
            let mut stmt = conn.prepare(
                "SELECT user_id, role, my_remark, joined_at FROM group_members
                 WHERE group_id = ?1 ORDER BY joined_at, user_id LIMIT ?2",
            )?;
            let mapped = stmt.query_map(params![group_id, limit], map_member)?;
            mapped.collect::<std::result::Result<Vec<_>, _>>()?
        }
    };
    Ok(rows)
}

fn map_member(r: &rusqlite::Row<'_>) -> rusqlite::Result<MemberRow> {
    Ok(MemberRow {
        user_id: r.get(0)?,
        role: r.get(1)?,
        my_remark: r.get(2)?,
        joined_at: r.get(3)?,
    })
}

pub(in crate::domain) fn summary(
    conn: &StorageConnection<'_>,
    me: &str,
) -> Result<Vec<GroupSummary>> {
    let conn = conn.sqlite()?;
    let mut stmt = conn.prepare(
        "SELECT g.id, g.name, g.owner_uid, m.role,
                (SELECT COUNT(*) FROM group_members gm WHERE gm.group_id = g.id)
         FROM groups g JOIN group_members m ON m.group_id = g.id
         WHERE m.user_id = ?1 ORDER BY g.created_at DESC",
    )?;
    let rows = stmt.query_map(params![me], |r| {
        Ok(GroupSummary {
            id: r.get(0)?,
            name: r.get(1)?,
            owner_uid: r.get(2)?,
            my_role: r.get(3)?,
            member_count: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub(in crate::domain) fn member_count(conn: &StorageConnection<'_>, group_id: &str) -> Result<i64> {
    let conn = conn.sqlite()?;
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM group_members WHERE group_id = ?1",
        params![group_id],
        |r| r.get(0),
    )?)
}

pub(in crate::domain) fn has_member(
    conn: &StorageConnection<'_>,
    group_id: &str,
    user_id: &str,
) -> Result<bool> {
    let conn = conn.sqlite()?;
    Ok(conn
        .query_row(
            "SELECT 1 FROM group_members WHERE group_id = ?1 AND user_id = ?2",
            params![group_id, user_id],
            |_| Ok(1i64),
        )
        .optional()?
        .is_some())
}

pub(in crate::domain) fn delete_member(
    conn: &StorageConnection<'_>,
    group_id: &str,
    user_id: &str,
) -> Result<usize> {
    let conn = conn.sqlite()?;
    Ok(conn.execute(
        "DELETE FROM group_members WHERE group_id = ?1 AND user_id = ?2",
        params![group_id, user_id],
    )?)
}

pub(in crate::domain) fn set_my_remark(
    conn: &StorageConnection<'_>,
    group_id: &str,
    user_id: &str,
    remark: &str,
) -> Result<usize> {
    let conn = conn.sqlite()?;
    Ok(conn.execute(
        "UPDATE group_members SET my_remark = ?1 WHERE group_id = ?2 AND user_id = ?3",
        params![remark, group_id, user_id],
    )?)
}

pub(in crate::domain) fn groups_of_user(
    conn: &StorageConnection<'_>,
    user_id: &str,
) -> Result<Vec<String>> {
    let conn = conn.sqlite()?;
    let mut stmt =
        conn.prepare("SELECT group_id FROM group_members WHERE user_id = ?1 ORDER BY group_id")?;
    let rows = stmt.query_map(params![user_id], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub(in crate::domain) fn owner_of(
    conn: &StorageConnection<'_>,
    group_id: &str,
) -> Result<Option<String>> {
    let conn = conn.sqlite()?;
    Ok(conn
        .query_row(
            "SELECT owner_uid FROM groups WHERE id = ?1",
            params![group_id],
            |r| r.get(0),
        )
        .optional()?)
}

pub(in crate::domain) fn set_visibility(
    conn: &StorageConnection<'_>,
    group_id: &str,
    visibility: &str,
) -> Result<()> {
    let conn = conn.sqlite()?;
    conn.execute(
        "INSERT INTO group_meta(group_id, visibility) VALUES(?1, ?2) \
         ON CONFLICT(group_id) DO UPDATE SET visibility = excluded.visibility",
        params![group_id, visibility],
    )?;
    Ok(())
}

pub(in crate::domain) fn mark_dissolved(
    conn: &StorageConnection<'_>,
    group_id: &str,
    now: i64,
    actor: &str,
) -> Result<()> {
    let conn = conn.sqlite()?;
    conn.execute(
        "INSERT INTO group_meta(group_id, visibility, dissolved_at, dissolved_by) \
         VALUES(?1, 'public', ?2, ?3) \
         ON CONFLICT(group_id) DO UPDATE SET dissolved_at = excluded.dissolved_at, \
           dissolved_by = excluded.dissolved_by",
        params![group_id, now, actor],
    )?;
    Ok(())
}

pub(in crate::domain) fn meta_of(
    conn: &StorageConnection<'_>,
    group_id: &str,
) -> Result<Option<(String, Option<i64>)>> {
    let conn = conn.sqlite()?;
    Ok(conn
        .query_row(
            "SELECT visibility, dissolved_at FROM group_meta WHERE group_id = ?1",
            params![group_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}
