//! **仓储层**（`report`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::domain::report::ReportRow;
use crate::error::Result;
use rusqlite::{params, Connection, OptionalExtension as _};
use wxlike_report::Ticket;

pub(in crate::domain) fn load(conn: &Connection, id: &str) -> Result<Option<ReportRow>> {
    Ok(conn
        .query_row(
            "SELECT reporter, target_kind, target_id, reason, detail, state, action_json,
                    handler, created_at, updated_at, history_json
             FROM reports WHERE id = ?1",
            params![id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                ))
            },
        )
        .optional()?)
}

pub(in crate::domain) fn ids_by_reporter(
    conn: &Connection,
    reporter: &str,
    limit: i64,
) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT id FROM reports WHERE reporter = ?1 ORDER BY created_at DESC LIMIT ?2")?;
    let rows = stmt.query_map(params![reporter, limit], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub(in crate::domain) fn ids_by_state(
    conn: &Connection,
    state: &str,
    limit: i64,
) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT id FROM reports WHERE state = ?1 ORDER BY created_at ASC LIMIT ?2")?;
    let rows = stmt.query_map(params![state, limit], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// 未终结工单：open / triaged / actioned 都还在处置队列里。
pub(in crate::domain) fn open_ids(conn: &Connection, limit: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM reports WHERE state IN ('open','triaged','actioned')
         ORDER BY created_at ASC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub(in crate::domain) fn save(
    conn: &Connection,
    t: &Ticket,
    action: Option<String>,
    history: String,
) -> Result<()> {
    conn.execute(
        "INSERT INTO reports(id, reporter, target_kind, target_id, reason, detail, state,
                             action_json, handler, created_at, updated_at, history_json)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(id) DO UPDATE SET
           state = excluded.state,
           action_json = excluded.action_json,
           handler = excluded.handler,
           updated_at = excluded.updated_at,
           history_json = excluded.history_json",
        params![
            t.id,
            t.reporter,
            t.target_kind,
            t.target_id,
            t.reason.as_str(),
            t.detail,
            t.state.as_str(),
            action,
            t.handler,
            // rusqlite 0.41 起 `u64` 不再实现 `ToSql`。SQLite 的 INTEGER 就是
            // 有符号 64 位，而工单时间戳是正数且远小于 `i64::MAX`，直接 `as i64`。
            t.created_at as i64,
            t.updated_at as i64,
            history
        ],
    )?;
    Ok(())
}
