//! **仓储层**（`groups_admin`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use crate::storage::{params, OptionalExt as _, StorageConnection};


/// 群公告行；业务上限与管理员断言留在 domain。
pub(in crate::domain) struct AnnouncementRecord {
    pub text: String,
    pub media_id: String,
    pub pinned_by: String,
    pub updated_at: i64,
}

/// 邀请 token 的入群所需字段。
pub(in crate::domain) struct InviteRecord {
    pub group_id: String,
    pub expires_at: i64,
}

/// 待审入群请求。
pub(in crate::domain) struct JoinRequestRecord {
    pub user_id: String,
    pub requested_at: i64,
}

pub(in crate::domain) fn upsert_announcement(
    c: &StorageConnection<'_>,
    group: &str,
    text: &str,
    media_id: &str,
    pinned_by: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO group_announcement(group_id, text, media_id, pinned_by, updated_at)
         VALUES(?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(group_id) DO UPDATE SET
           text = excluded.text, media_id = excluded.media_id,
           pinned_by = excluded.pinned_by, updated_at = excluded.updated_at",
        params![group, text, media_id, pinned_by, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn announcement(
    c: &StorageConnection<'_>,
    group: &str,
) -> Result<Option<AnnouncementRecord>> {
    let row = c
        .query_row(
            "SELECT text, media_id, pinned_by, updated_at FROM group_announcement
             WHERE group_id = ?1",
            params![group],
            |r| {
                Ok(AnnouncementRecord {
                    text: r.get(0)?,
                    media_id: r.get(1)?,
                    pinned_by: r.get(2)?,
                    updated_at: r.get(3)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

pub(in crate::domain) fn upsert_member_mute(
    c: &StorageConnection<'_>,
    group: &str,
    target: &str,
    until_ms: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO group_mutes(group_id, user_id, until_ms) VALUES(?1, ?2, ?3)
         ON CONFLICT(group_id, user_id) DO UPDATE SET until_ms = excluded.until_ms",
        params![group, target, until_ms],
    )?;
    Ok(())
}

pub(in crate::domain) fn delete_member_mute(
    c: &StorageConnection<'_>,
    group: &str,
    target: &str,
) -> Result<bool> {
    let changed = c.execute(
        "DELETE FROM group_mutes WHERE group_id = ?1 AND user_id = ?2",
        params![group, target],
    )?;
    Ok(changed == 1)
}

pub(in crate::domain) fn set_all_muted(
    c: &StorageConnection<'_>,
    group: &str,
    on: bool,
) -> Result<()> {
    c.execute(
        "UPDATE groups SET all_muted = ?1 WHERE id = ?2",
        params![on as i64, group],
    )?;
    Ok(())
}

pub(in crate::domain) fn all_muted(c: &StorageConnection<'_>, group: &str) -> Result<bool> {
    let all: i64 = c.query_row(
        "SELECT all_muted FROM groups WHERE id = ?1",
        params![group],
        |r| r.get(0),
    )?;
    Ok(all != 0)
}

pub(in crate::domain) fn mute_until(
    c: &StorageConnection<'_>,
    group: &str,
    user: &str,
) -> Result<Option<i64>> {
    let until = c
        .query_row(
            "SELECT until_ms FROM group_mutes WHERE group_id = ?1 AND user_id = ?2",
            params![group, user],
            |r| r.get(0),
        )
        .optional()?;
    Ok(until)
}

pub(in crate::domain) fn member_count(c: &StorageConnection<'_>, group: &str) -> Result<i64> {
    let count = c.query_row(
        "SELECT COUNT(*) FROM group_members WHERE group_id = ?1",
        params![group],
        |r| r.get(0),
    )?;
    Ok(count)
}

pub(in crate::domain) use crate::domain::repo::users::user_exists;

pub(in crate::domain) fn insert_member_direct(
    c: &StorageConnection<'_>,
    group: &str,
    user: &str,
    role: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO group_members(group_id, user_id, role, my_remark, joined_at)
         VALUES(?1, ?2, ?3, '', ?4)
         ON CONFLICT(group_id, user_id) DO NOTHING",
        params![group, user, role, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn set_join_approval(
    c: &StorageConnection<'_>,
    group: &str,
    on: bool,
) -> Result<()> {
    c.execute(
        "UPDATE groups SET approval = ?1 WHERE id = ?2",
        params![on as i64, group],
    )?;
    Ok(())
}

pub(in crate::domain) fn insert_invite(
    c: &StorageConnection<'_>,
    token: &str,
    group: &str,
    created_by: &str,
    expires_at: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO group_invites(token, group_id, created_by, expires_at)
         VALUES(?1, ?2, ?3, ?4)",
        params![token, group, created_by, expires_at],
    )?;
    Ok(())
}

pub(in crate::domain) fn invite(
    c: &StorageConnection<'_>,
    token: &str,
) -> Result<Option<InviteRecord>> {
    let row = c
        .query_row(
            "SELECT group_id, expires_at FROM group_invites WHERE token = ?1",
            params![token],
            |r| {
                Ok(InviteRecord {
                    group_id: r.get(0)?,
                    expires_at: r.get(1)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

pub(in crate::domain) fn join_approval(c: &StorageConnection<'_>, group: &str) -> Result<i64> {
    let approval = c.query_row(
        "SELECT approval FROM groups WHERE id = ?1",
        params![group],
        |r| r.get(0),
    )?;
    Ok(approval)
}

pub(in crate::domain) fn upsert_join_request(
    c: &StorageConnection<'_>,
    group: &str,
    user: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO group_join_requests(group_id, user_id, status, requested_at)
         VALUES(?1, ?2, 'pending', ?3)
         ON CONFLICT(group_id, user_id) DO UPDATE SET status = 'pending',
           requested_at = excluded.requested_at",
        params![group, user, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn join_requests(
    c: &StorageConnection<'_>,
    group: &str,
) -> Result<Vec<JoinRequestRecord>> {
    let stmt = c.prepare(
        "SELECT user_id, requested_at FROM group_join_requests
         WHERE group_id = ?1 AND status = 'pending' ORDER BY requested_at",
    )?;
    let rows = stmt.query_map(params![group], |r| {
        Ok(JoinRequestRecord {
            user_id: r.get(0)?,
            requested_at: r.get(1)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
}

pub(in crate::domain) fn set_join_request_status(
    c: &StorageConnection<'_>,
    group: &str,
    user: &str,
    status: &str,
) -> Result<bool> {
    let changed = c.execute(
        "UPDATE group_join_requests SET status = ?1
         WHERE group_id = ?2 AND user_id = ?3 AND status = 'pending'",
        params![status, group, user],
    )?;
    Ok(changed == 1)
}
