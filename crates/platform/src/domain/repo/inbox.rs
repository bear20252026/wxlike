//! **仓储层**（`inbox`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::domain::inbox::Notification;
use crate::error::Result;
use crate::storage::StorageConnection;
use rusqlite::params;

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
    let conn = conn.sqlite()?;
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

/// 事务内调用：`with_tx` 闭包只给 `&Transaction`，这里转成端口值。
#[allow(clippy::too_many_arguments)]
pub(in crate::domain) fn notify_tx(
    tx: &rusqlite::Transaction<'_>,
    user_id: &str,
    kind: &str,
    actor: &str,
    target_kind: &str,
    target_id: &str,
    text: &str,
    dedupe: &str,
    now: i64,
) -> Result<()> {
    notify(
        &StorageConnection::sqlite_tx(tx),
        user_id,
        kind,
        actor,
        target_kind,
        target_id,
        text,
        dedupe,
        now,
    )
}

/// 旧签名 repo 内部的调用：把已借出的 `&Connection` 转成端口值。
#[allow(clippy::too_many_arguments)]
pub(in crate::domain) fn notify_conn(
    conn: &rusqlite::Connection,
    user_id: &str,
    kind: &str,
    actor: &str,
    target_kind: &str,
    target_id: &str,
    text: &str,
    dedupe: &str,
    now: i64,
) -> Result<()> {
    notify(
        &StorageConnection::sqlite_borrow(conn),
        user_id,
        kind,
        actor,
        target_kind,
        target_id,
        text,
        dedupe,
        now,
    )
}

pub(in crate::domain) fn page(
    conn: &StorageConnection<'_>,
    user: &str,
    before: i64,
    limit: i64,
) -> Result<Vec<Notification>> {
    let conn = conn.sqlite()?;
    let mut stmt = conn.prepare(
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
    Ok(it.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub(in crate::domain) fn unread(conn: &StorageConnection<'_>, user: &str) -> Result<i64> {
    let conn = conn.sqlite()?;
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM notifications WHERE user_id = ?1 AND read_at IS NULL",
        params![user],
        |r| r.get(0),
    )?)
}

pub(in crate::domain) fn mark_up_to(
    conn: &StorageConnection<'_>,
    user: &str,
    up_to: i64,
    now: i64,
) -> Result<usize> {
    let conn = conn.sqlite()?;
    Ok(conn.execute(
        "UPDATE notifications SET read_at = ?1
         WHERE user_id = ?2 AND seq <= ?3 AND read_at IS NULL",
        params![now, user, up_to],
    )?)
}

pub(in crate::domain) fn mark_id(
    conn: &StorageConnection<'_>,
    user: &str,
    id: &str,
    now: i64,
) -> Result<usize> {
    let conn = conn.sqlite()?;
    Ok(conn.execute(
        "UPDATE notifications SET read_at = ?1
         WHERE user_id = ?2 AND id = ?3 AND read_at IS NULL",
        params![now, user, id],
    )?)
}
