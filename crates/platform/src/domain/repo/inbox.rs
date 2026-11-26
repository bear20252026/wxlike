//! **仓储层**（`inbox`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::domain::inbox::Notification;
use crate::error::Result;
use crate::storage::StorageConnection;
use crate::storage::params;

/// 事务内插入一条通知。空 `dedupe` = 不参与去重（如每条评论各一条）。
#[allow(clippy::too_many_arguments)]
pub(in crate::domain) fn notify(
    conn: &StorageConnection<'_>,
    user_id: &str,
    kind: &str,
    actor: &str,
    target_kind: &str,
    target_id: &str,
    text: &str,
    dedupe: &str,
    now: i64,
) -> Result<()> {
    if user_id == actor {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO notifications(id, user_id, kind, actor, target_kind, target_id, text,
                                   dedupe_key, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(user_id, dedupe_key) WHERE dedupe_key <> '' DO NOTHING",
        params![
            crate::auth::random_hex(8),
            user_id,
            kind,
            actor,
            target_kind,
            target_id,
            text,
            dedupe,
            now
        ],
    )?;
    Ok(())
}

/// 通知分页（按 `seq` 倒序）。
pub(in crate::domain) fn page(
    conn: &StorageConnection<'_>,
    user: &str,
    before: i64,
    limit: i64,
) -> Result<Vec<Notification>> {
    let stmt = conn.prepare(
        "SELECT seq, id, kind, actor, target_kind, target_id, text, created_at, read_at
         FROM notifications WHERE user_id = ?1 AND seq < ?2
         ORDER BY seq DESC LIMIT ?3",
    )?;
    let it = stmt.query_map(params![user, before, limit], |r| {
        Ok(Notification {
            seq: r.get(0)?,
            id: r.get(1)?,
            kind: r.get(2)?,
            actor: r.get(3)?,
            target_kind: r.get(4)?,
            target_id: r.get(5)?,
            text: r.get(6)?,
            created_at: r.get(7)?,
            read: r.get::<_, Option<i64>>(8)?.is_some(),
        })
    })?;
    it.collect::<std::result::Result<Vec<_>, _>>()
}

pub(in crate::domain) fn unread(conn: &StorageConnection<'_>, user: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM notifications WHERE user_id = ?1 AND read_at IS NULL",
        params![user],
        |r| r.get(0),
    )
}

pub(in crate::domain) fn mark_up_to(
    conn: &StorageConnection<'_>,
    user: &str,
    up_to: i64,
    now: i64,
) -> Result<usize> {
    conn.execute(
        "UPDATE notifications SET read_at = ?1
         WHERE user_id = ?2 AND seq <= ?3 AND read_at IS NULL",
        params![now, user, up_to],
    )
}

pub(in crate::domain) fn mark_id(
    conn: &StorageConnection<'_>,
    user: &str,
    id: &str,
    now: i64,
) -> Result<usize> {
    conn.execute(
        "UPDATE notifications SET read_at = ?1
         WHERE user_id = ?2 AND id = ?3 AND read_at IS NULL",
        params![now, user, id],
    )
}

#[cfg(test)]
mod pg_tests {
    use super::{mark_id, notify, page};
    use crate::storage::{PostgresPool, StorageConnection};

    #[test]
    #[ignore = "requires WXLIKE_TEST_PG_URL pointing at a DB with M014 applied"]
    fn postgres_notify_dedupes_and_marks_read() {
        let url = std::env::var("WXLIKE_TEST_PG_URL")
            .expect("set WXLIKE_TEST_PG_URL to run the PostgreSQL inbox test");
        let pg = PostgresPool::open(&url).expect("postgres pool");
        pg.block_on(
            sqlx::query("DELETE FROM notifications WHERE user_id = 'pgbe-inbox'")
                .execute(pg.pool()),
        )
        .expect("cleanup");

        let conn = StorageConnection::Postgres(&pg);
        notify(
            &conn,
            "pgbe-inbox",
            "mention",
            "pgbe-actor",
            "thread",
            "t1",
            "hello",
            "d1",
            10,
        )
        .expect("notify");
        notify(
            &conn,
            "pgbe-inbox",
            "mention",
            "pgbe-actor",
            "thread",
            "t1",
            "hello",
            "d1",
            11,
        )
        .expect("dedupe");
        let rows = page(&conn, "pgbe-inbox", i64::MAX, 10).expect("page");
        assert_eq!(rows.len(), 1, "dedupe key suppresses the second notify");
        assert_eq!(mark_id(&conn, "pgbe-inbox", &rows[0].id, 12).expect("mark"), 1);

        pg.block_on(
            sqlx::query("DELETE FROM notifications WHERE user_id = 'pgbe-inbox'")
                .execute(pg.pool()),
        )
        .expect("cleanup end");
    }
}
