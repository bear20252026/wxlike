//! **仓储层**（`report`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::domain::report::ReportRow;
use crate::error::Result;
use crate::storage::{params, OptionalExt as _, StorageConnection};
use wxlike_report::{Ticket, TicketRecord};

pub(in crate::domain) fn load(conn: &StorageConnection<'_>, id: &str) -> Result<Option<ReportRow>> {
    conn
        .query_row(
            "SELECT id, reporter, target_kind, target_id, reason, detail, state, action_json,
                    handler, created_at, updated_at, history_json
             FROM reports WHERE id = ?1",
            params![id],
            |r| {
                wxlike_core::row_tuple!(r; 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, wxlike_report::LAST_TICKET_COLUMN)
                    .map(TicketRecord::from)
            },
        )
        .optional()
}

pub(in crate::domain) fn ids_by_reporter(
    conn: &StorageConnection<'_>,
    reporter: &str,
    limit: i64,
) -> Result<Vec<String>> {
    let stmt = conn
        .prepare("SELECT id FROM reports WHERE reporter = ?1 ORDER BY created_at DESC LIMIT ?2")?;
    let rows = stmt.query_map(params![reporter, limit], |r| r.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
}

pub(in crate::domain) fn ids_by_state(
    conn: &StorageConnection<'_>,
    state: &str,
    limit: i64,
) -> Result<Vec<String>> {
    let stmt =
        conn.prepare("SELECT id FROM reports WHERE state = ?1 ORDER BY created_at ASC LIMIT ?2")?;
    let rows = stmt.query_map(params![state, limit], |r| r.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
}

/// 未终结工单：open / triaged / actioned 都还在处置队列里。
pub(in crate::domain) fn open_ids(conn: &StorageConnection<'_>, limit: i64) -> Result<Vec<String>> {
    let stmt = conn.prepare(
        "SELECT id FROM reports WHERE state IN ('open','triaged','actioned')
         ORDER BY created_at ASC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], |r| r.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
}

pub(in crate::domain) fn save(
    conn: &StorageConnection<'_>,
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

#[cfg(test)]
mod pg_tests {
    use super::{ids_by_reporter, ids_by_state, load, open_ids, save};
    use crate::storage::{PostgresPool, StorageConnection};
    use wxlike_report::{Reason, State, Ticket};

    /// `reports` 在 PG 上走同一份 SQL：占位符转换 + 行适配。
    /// 依赖 M014 已执行；只有 `WXLIKE_TEST_PG_URL` 存在时跑。
    #[test]
    #[ignore = "requires WXLIKE_TEST_PG_URL with M014 applied"]
    fn postgres_report_roundtrip() {
        let url = std::env::var("WXLIKE_TEST_PG_URL").expect("set WXLIKE_TEST_PG_URL");
        let pg = PostgresPool::open(&url).expect("postgres pool");
        let cleanup = || {
            pg.block_on(
                sqlx::query("DELETE FROM reports WHERE id LIKE 'pg14-report-%'")
                    .execute(pg.pool()),
            )
            .expect("cleanup");
        };
        cleanup();

        let id = "pg14-report-1";
        let ticket = Ticket {
            id: id.into(),
            reporter: "pg14-user".into(),
            target_kind: "user".into(),
            target_id: "pg14-target".into(),
            reason: Reason::Spam,
            detail: "pg 批次 14".into(),
            state: State::Open,
            created_at: 11,
            updated_at: 12,
            action: None,
            handler: None,
            history: Vec::new(),
        };
        let port = StorageConnection::Postgres(&pg);
        save(&port, &ticket, None, "[]".into()).expect("pg save");

        let loaded = load(&port, id).expect("pg load").expect("row");
        assert_eq!(loaded.reporter, "pg14-user");
        assert_eq!(loaded.state, "open");
        assert_eq!(loaded.created_at, 11);
        assert_eq!(ids_by_reporter(&port, "pg14-user", 10).unwrap(), vec![id]);
        let open = ids_by_state(&port, "open", 10).unwrap();
        assert!(open.contains(&id.to_string()), "open ids: {open:?}");
        assert!(open_ids(&port, 10).unwrap().contains(&id.to_string()));

        cleanup();
    }
}
