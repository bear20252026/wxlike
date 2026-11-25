//! **仓储层**（`communities`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。
//!
//! 社区/频道/话题的角色继承、发送闸门与成员上限留在
//! [`crate::domain::communities`]；本文件只负责语句与行映射。

use crate::domain::communities::{
    ChannelRow, CommunityRow, CommunityThreadRow, ThreadRow, ThreadSubscription,
};
use crate::error::Result;
use crate::storage::{params, OptionalExt as _, StorageConnection, StorageRow};

pub(in crate::domain) fn community_role(
    conn: &StorageConnection<'_>,
    community: &str,
    user: &str,
) -> Result<Option<i64>> {
    conn
        .query_row(
            "SELECT role FROM community_members WHERE community_id = ?1 AND user_id = ?2",
            params![community, user],
            |r| r.get::<_, i64>(0),
        )
        .optional()
}

pub(in crate::domain) fn channel_community(
    conn: &StorageConnection<'_>,
    channel: &str,
) -> Result<Option<String>> {
    conn
        .query_row(
            "SELECT community_id FROM channels WHERE id = ?1",
            params![channel],
            |r| r.get(0),
        )
        .optional()
}

pub(in crate::domain) fn channel_kind(
    conn: &StorageConnection<'_>,
    channel: &str,
) -> Result<Option<String>> {
    conn
        .query_row(
            "SELECT kind FROM channels WHERE id = ?1",
            params![channel],
            |r| r.get::<_, String>(0),
        )
        .optional()
}

pub(in crate::domain) fn channel_role_raw(
    conn: &StorageConnection<'_>,
    channel: &str,
    user: &str,
) -> Result<Option<String>> {
    conn
        .query_row(
            "SELECT role FROM channel_role WHERE channel_id = ?1 AND user_id = ?2",
            params![channel, user],
            |r| r.get::<_, String>(0),
        )
        .optional()
}

pub(in crate::domain) fn create_community(
    conn: &StorageConnection<'_>,
    id: &str,
    name: &str,
    owner: &str,
    member_limit: i64,
    now: i64,
) -> Result<()> {
    // 调用方必须在 `Db::with_tx` 里调用：两行必须一起成功。
    conn.execute(
        "INSERT INTO communities(id, name, owner_uid, member_limit, tenant_id, created_at)
         VALUES(?1, ?2, ?3, ?4, 'default', ?5)",
        params![id, name, owner, member_limit, now],
    )?;
    conn.execute(
        "INSERT INTO community_members(community_id, user_id, role, joined_at)
         VALUES(?1, ?2, ?3, ?4)",
        params![id, owner, crate::domain::communities::ROLE_OWNER, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn join_limits(
    conn: &StorageConnection<'_>,
    community: &str,
) -> Result<Option<(i64, i64)>> {
    conn
        .query_row(
            "SELECT member_limit,
                    (SELECT COUNT(*) FROM community_members WHERE community_id = ?1)
             FROM communities WHERE id = ?1",
            params![community],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
}

pub(in crate::domain) fn insert_community_member(
    conn: &StorageConnection<'_>,
    community: &str,
    user: &str,
    role: i64,
    now: i64,
) -> Result<usize> {
    conn.execute(
        "INSERT INTO community_members(community_id, user_id, role, joined_at)
         VALUES(?1, ?2, ?3, ?4)",
        params![community, user, role, now],
    )
}

pub(in crate::domain) fn my_communities(
    conn: &StorageConnection<'_>,
    user: &str,
) -> Result<Vec<(CommunityRow, i64)>> {
    let stmt = conn.prepare(
        "SELECT c.id, c.name, c.owner_uid, c.member_limit, c.announcement, c.tenant_id,
                c.created_at, m.role
         FROM communities c JOIN community_members m
           ON m.community_id = c.id AND m.user_id = ?1
         ORDER BY c.created_at",
    )?;
    let rows = stmt.query_map(params![user], |r| {
        Ok((
            CommunityRow {
                id: r.get(0)?,
                name: r.get(1)?,
                owner_uid: r.get(2)?,
                member_limit: r.get(3)?,
                announcement: r.get(4)?,
                tenant_id: r.get(5)?,
                created_at: r.get(6)?,
            },
            r.get(7)?,
        ))
    })?;
    rows.collect::<std::result::Result<_, _>>()
}

pub(in crate::domain) fn community_members(
    conn: &StorageConnection<'_>,
    community: &str,
) -> Result<Vec<(String, i64)>> {
    let stmt = conn.prepare(
        "SELECT user_id, role FROM community_members WHERE community_id = ?1
         ORDER BY role DESC, joined_at",
    )?;
    let rows = stmt.query_map(params![community], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    rows.collect::<std::result::Result<_, _>>()
}

pub(in crate::domain) fn insert_channel(
    conn: &StorageConnection<'_>,
    id: &str,
    community: &str,
    name: &str,
    kind: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO channels(id, community_id, name, kind, tenant_id, created_at)
         VALUES(?1, ?2, ?3, ?4, 'default', ?5)",
        params![id, community, name, kind, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn list_channels(
    conn: &StorageConnection<'_>,
    community: &str,
) -> Result<Vec<ChannelRow>> {
    let stmt = conn.prepare(
        "SELECT id, community_id, name, kind, tenant_id, created_at
         FROM channels WHERE community_id = ?1 ORDER BY created_at",
    )?;
    let rows = stmt
        .query_map(params![community], row_channel)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(in crate::domain) fn row_channel(r: &StorageRow<'_>) -> Result<ChannelRow> {
    Ok(ChannelRow {
        id: r.get(0)?,
        community_id: r.get(1)?,
        name: r.get(2)?,
        kind: r.get(3)?,
        tenant_id: r.get(4)?,
        created_at: r.get(5)?,
    })
}

/// 频道角色名单的原始列：`(user_id, 社区角色, 显式频道角色, granted_by)`。
/// 角色继承是业务规则，留给 domain 决定，仓储层不在这里造 `ChannelRole`。
pub(in crate::domain) type ChannelRoleRaw = (String, i64, Option<String>, Option<String>);

pub(in crate::domain) fn channel_roles_raw(
    conn: &StorageConnection<'_>,
    channel: &str,
    community: &str,
) -> Result<Vec<ChannelRoleRaw>> {
    let stmt = conn.prepare(
        "SELECT m.user_id, m.role, r.role, r.granted_by
         FROM community_members m
         LEFT JOIN channel_role r ON r.channel_id = ?1 AND r.user_id = m.user_id
         WHERE m.community_id = ?2
         ORDER BY m.joined_at",
    )?;
    let rows = stmt.query_map(params![channel, community], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })?;
    rows.collect::<std::result::Result<_, _>>()
}

pub(in crate::domain) fn upsert_channel_role(
    conn: &StorageConnection<'_>,
    channel: &str,
    user: &str,
    role: &str,
    actor: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO channel_role(channel_id, user_id, role, granted_by, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(channel_id, user_id) DO UPDATE
           SET role = excluded.role, granted_by = excluded.granted_by",
        params![channel, user, role, actor, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn delete_channel_role(
    conn: &StorageConnection<'_>,
    channel: &str,
    user: &str,
) -> Result<usize> {
    conn.execute(
        "DELETE FROM channel_role WHERE channel_id = ?1 AND user_id = ?2",
        params![channel, user],
    )
}

pub(in crate::domain) fn insert_thread(
    conn: &StorageConnection<'_>,
    id: &str,
    channel: &str,
    root_seq: i64,
    title: &str,
    actor: &str,
    now: i64,
) -> Result<()> {
    // 调用方必须在 `Db::with_tx` 里调用：帖子与订阅行必须一起成功。
    conn.execute(
        "INSERT INTO threads(id, channel_id, root_seq, title, created_by, tenant_id, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, 'default', ?6)",
        params![id, channel, root_seq, title, actor, now],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO thread_subs(thread_id, user_id) VALUES(?1, ?2)",
        params![id, actor],
    )?;
    Ok(())
}

pub(in crate::domain) fn list_threads(
    conn: &StorageConnection<'_>,
    channel: &str,
) -> Result<Vec<ThreadRow>> {
    let stmt = conn.prepare(
        "SELECT id, channel_id, root_seq, title, created_by, created_at
         FROM threads WHERE channel_id = ?1 ORDER BY created_at, id",
    )?;
    let rows = stmt.query_map(params![channel], |r| {
        Ok(ThreadRow {
            id: r.get(0)?,
            channel_id: r.get(1)?,
            root_seq: r.get(2)?,
            title: r.get(3)?,
            created_by: r.get(4)?,
            created_at: r.get(5)?,
        })
    })?;
    rows.collect::<std::result::Result<_, _>>()
}

pub(in crate::domain) fn community_threads(
    conn: &StorageConnection<'_>,
    community: &str,
    user: &str,
    limit: i64,
) -> Result<Vec<CommunityThreadRow>> {
    let stmt = conn.prepare(
        "SELECT t.id, t.channel_id, ch.name, t.root_seq, t.title, t.created_by, t.created_at,
                (SELECT COUNT(*) FROM thread_messages m WHERE m.thread_id = t.id),
                EXISTS(SELECT 1 FROM thread_subs s WHERE s.thread_id = t.id AND s.user_id = ?2)
         FROM threads t
         JOIN channels ch ON ch.id = t.channel_id
         WHERE ch.community_id = ?1
         ORDER BY t.created_at DESC, t.id DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![community, user, limit], |r| {
        Ok(CommunityThreadRow {
            thread_id: r.get(0)?,
            channel_id: r.get(1)?,
            channel_name: r.get(2)?,
            root_seq: r.get(3)?,
            title: r.get(4)?,
            created_by: r.get(5)?,
            created_at: r.get(6)?,
            replies: r.get(7)?,
            subscribed: r.get::<_, i64>(8)? != 0,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
}

pub(in crate::domain) fn thread_community(
    conn: &StorageConnection<'_>,
    thread: &str,
) -> Result<Option<String>> {
    conn
        .query_row(
            "SELECT c.community_id FROM channels c JOIN threads t ON t.channel_id = c.id
             WHERE t.id = ?1",
            params![thread],
            |r| r.get(0),
        )
        .optional()
}

pub(in crate::domain) fn thread_channel(
    conn: &StorageConnection<'_>,
    thread: &str,
) -> Result<Option<String>> {
    conn
        .query_row(
            "SELECT channel_id FROM threads WHERE id = ?1",
            params![thread],
            |r| r.get::<_, String>(0),
        )
        .optional()
}

pub(in crate::domain) fn insert_thread_message(
    conn: &StorageConnection<'_>,
    thread: &str,
    actor: &str,
    text: &str,
    now: i64,
) -> Result<i64> {
    conn.query_row(
        "INSERT INTO thread_messages(thread_id, sender, text, created_at)
         VALUES(?1, ?2, ?3, ?4) RETURNING seq",
        params![thread, actor, text, now],
        |r| r.get(0),
    )
}

pub(in crate::domain) fn thread_fanout(
    conn: &StorageConnection<'_>,
    thread: &str,
    community: &str,
) -> Result<Vec<String>> {
    let stmt = conn.prepare(
        "SELECT s.user_id FROM thread_subs s
         JOIN community_members m ON m.community_id = ?2 AND m.user_id = s.user_id
         WHERE s.thread_id = ?1 AND s.muted = 0
         ORDER BY s.user_id",
    )?;
    let rows = stmt.query_map(params![thread, community], |r| r.get::<_, String>(0))?;
    rows.collect::<std::result::Result<_, _>>()
}

pub(in crate::domain) fn fetch_thread_messages(
    conn: &StorageConnection<'_>,
    thread: &str,
    after_seq: i64,
    limit: i64,
) -> Result<Vec<(i64, String, String, i64)>> {
    let stmt = conn.prepare(
        "SELECT seq, sender, text, created_at FROM thread_messages
         WHERE thread_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
    )?;
    let rows = stmt
        .query_map(params![thread, after_seq, limit], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?
        .collect::<std::result::Result<_, _>>()?;
    Ok(rows)
}

pub(in crate::domain) fn upsert_thread_sub(
    conn: &StorageConnection<'_>,
    thread: &str,
    user: &str,
    muted: bool,
) -> Result<()> {
    conn.execute(
        "INSERT INTO thread_subs(thread_id, user_id, muted) VALUES(?1, ?2, ?3)
         ON CONFLICT(thread_id, user_id) DO UPDATE SET muted = excluded.muted",
        params![thread, user, muted as i64],
    )?;
    Ok(())
}

pub(in crate::domain) fn delete_thread_sub(
    conn: &StorageConnection<'_>,
    thread: &str,
    user: &str,
) -> Result<usize> {
    conn.execute(
        "DELETE FROM thread_subs WHERE thread_id = ?1 AND user_id = ?2",
        params![thread, user],
    )
}

pub(in crate::domain) fn thread_subscribers(
    conn: &StorageConnection<'_>,
    thread: &str,
) -> Result<Vec<ThreadSubscription>> {
    let stmt = conn
        .prepare("SELECT user_id, muted FROM thread_subs WHERE thread_id = ?1 ORDER BY user_id")?;
    let rows = stmt.query_map(params![thread], |r| {
        Ok(ThreadSubscription {
            user_id: r.get(0)?,
            muted: r.get::<_, i64>(1)? != 0,
        })
    })?;
    rows.collect::<std::result::Result<_, _>>()
}

pub(in crate::domain) fn channel_ids(
    conn: &StorageConnection<'_>,
    community: &str,
) -> Result<Vec<String>> {
    let stmt = conn.prepare("SELECT id FROM channels WHERE community_id = ?1")?;
    let rows = stmt.query_map(params![community], |r| r.get::<_, String>(0))?;
    rows.collect::<std::result::Result<_, _>>()
}

pub(in crate::domain) fn community_member_ids(
    conn: &StorageConnection<'_>,
    community: &str,
) -> Result<Vec<String>> {
    let stmt = conn.prepare("SELECT user_id FROM community_members WHERE community_id = ?1")?;
    let rows = stmt.query_map(params![community], |r| r.get::<_, String>(0))?;
    rows.collect::<std::result::Result<_, _>>()
}

#[cfg(test)]
mod pg_tests {
    use super::{create_community, insert_thread, thread_subscribers};
    use crate::storage::{params, StorageProfile};

    #[test]
    #[ignore = "requires WXLIKE_TEST_PG_URL pointing at a DB with M014 applied"]
    fn postgres_community_and_thread_transactions_commit() {
        let url = std::env::var("WXLIKE_TEST_PG_URL")
            .expect("set WXLIKE_TEST_PG_URL to run the PostgreSQL communities test");
        let db = crate::Db::open_profile(StorageProfile::Postgres { url }, None)
            .expect("postgres db");
        let cleanup = || {
            db.with_tx(|conn| {
                for sql in [
                    "DELETE FROM thread_subs WHERE thread_id = 'pgbe-th1'",
                    "DELETE FROM threads WHERE id = 'pgbe-th1'",
                    "DELETE FROM community_members WHERE community_id = 'pgbe-c1'",
                    "DELETE FROM communities WHERE id = 'pgbe-c1'",
                ] {
                    conn.execute(sql, params![])?;
                }
                Ok(())
            })
            .expect("cleanup");
        };
        cleanup();

        db.with_tx(|conn| {
            create_community(conn, "pgbe-c1", "PG community", "pgbe-u1", 5, 1)?;
            insert_thread(conn, "pgbe-th1", "pgbe-ch1", 1, "hello", "pgbe-u1", 2)
        })
        .expect("create community and thread");
        let conn = db.acquire();
        let subs = thread_subscribers(&conn, "pgbe-th1").expect("thread subs");
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].user_id, "pgbe-u1");
        drop(conn);

        cleanup();
    }
}
