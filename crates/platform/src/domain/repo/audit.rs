//! **仓储层**（`audit`）：审计日志的读写 SQL。
//!
//! 审计表只写面（不变式 7）；检索/留存策略归 P6 adminx。这里只把行写进去、
//! 按 actor 取最近 N 条，业务侧的时间戳、条数上限与失败留痕都在
//! `domain::audit`。

use rusqlite::{params, Connection};

use crate::error::Result;

/// 写一条审计。`at_ms` 由 domain 传进来，仓储层不读时钟。
pub(in crate::domain) fn write(
    conn: &Connection,
    at_ms: i64,
    actor: &str,
    action: &str,
    target: &str,
    detail: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO audit_log(at_ms, actor, action, target, detail) VALUES(?1, ?2, ?3, ?4, ?5)",
        params![at_ms, actor, action, target, detail],
    )?;
    Ok(())
}

/// 某个 actor 的最近 `limit` 条（`limit` 已由 domain 夹紧到合法区间）。
pub(in crate::domain) fn list_for_actor(
    conn: &Connection,
    actor: &str,
    limit: i64,
) -> Result<Vec<(i64, String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT at_ms, action, target, detail FROM audit_log
         WHERE actor = ?1 ORDER BY at_ms DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![actor, limit], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}
