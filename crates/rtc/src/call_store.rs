//! G09-02 通话记录的真实落库实现。
//!
//! 状态机本身不碰数据库；`CallServer` 只依赖 [`CallRecordSink`]。这里给出
//! 两个实现：内存版（单测/并行开发）与 SQLite 版（真实服务、测试里也用
//! 真 SQLite，不是 mock）。迁移草稿见 `schema/proposals/G09/0001_calls.sql`，
//! 由 G00 分配正式编号后并入平台库。

use anyhow::Context as _;
use rusqlite::{params, OptionalExtension as _, Row};

use crate::call_session::{CallRecord, EndReason};

/// 通话记录写入口。所有方法都必须是幂等的：重复挂断只会触发一次保存。
pub trait CallRecordSink {
    fn save(&mut self, record: &CallRecord) -> anyhow::Result<()>;
    fn get(&self, call_id: &str) -> anyhow::Result<Option<CallRecord>>;
    fn recent(&self, conv_id: &str, limit: usize) -> anyhow::Result<Vec<CallRecord>>;
}

#[derive(Default)]
pub struct MemoryCallRecords {
    rows: Vec<CallRecord>,
}

impl MemoryCallRecords {
    pub fn rows(&self) -> &[CallRecord] {
        &self.rows
    }
}

impl CallRecordSink for MemoryCallRecords {
    fn save(&mut self, record: &CallRecord) -> anyhow::Result<()> {
        if let Some(existing) = self.rows.iter_mut().find(|r| r.call_id == record.call_id) {
            *existing = record.clone();
        } else {
            self.rows.push(record.clone());
        }
        Ok(())
    }

    fn get(&self, call_id: &str) -> anyhow::Result<Option<CallRecord>> {
        Ok(self.rows.iter().find(|r| r.call_id == call_id).cloned())
    }

    fn recent(&self, conv_id: &str, limit: usize) -> anyhow::Result<Vec<CallRecord>> {
        let mut rows: Vec<CallRecord> = self
            .rows
            .iter()
            .filter(|r| r.conv_id == conv_id)
            .cloned()
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.ended_at_ms));
        rows.truncate(limit);
        Ok(rows)
    }
}

/// G00 分配迁移编号时直接复用这段 DDL。
pub const CALLS_SCHEMA: &str = include_str!("../schema/calls.sql");

pub struct SqliteCallRecords {
    conn: rusqlite::Connection,
}

impl SqliteCallRecords {
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let conn = rusqlite::Connection::open(path).context("open call records db")?;
        Self::from_conn(conn)
    }

    pub fn in_memory() -> anyhow::Result<Self> {
        Self::from_conn(rusqlite::Connection::open_in_memory()?)
    }

    fn from_conn(conn: rusqlite::Connection) -> anyhow::Result<Self> {
        conn.execute_batch(CALLS_SCHEMA)
            .context("apply calls schema")?;
        Ok(Self { conn })
    }
}

fn read_row(row: &Row<'_>) -> rusqlite::Result<CallRecord> {
    let reason: String = row.get("reason")?;
    let ended_at_ms: i64 = row.get("ended_at_ms")?;
    let started_at_ms: i64 = row.get("started_at_ms")?;
    Ok(CallRecord {
        call_id: row.get("call_id")?,
        conv_id: row.get("conv_id")?,
        caller: row.get("caller")?,
        callee: row.get("callee")?,
        caller_device: row.get("caller_device")?,
        answer_device: row.get("answer_device")?,
        video: row.get::<_, i64>("video")? != 0,
        generation: row.get::<_, i64>("generation")? as u64,
        started_at_ms: started_at_ms as u64,
        answered_at_ms: row
            .get::<_, Option<i64>>("answered_at_ms")?
            .map(|v| v as u64),
        ended_at_ms: ended_at_ms as u64,
        reason: reason_from_str(&reason),
    })
}

const SELECT_COLUMNS: &str = "call_id, conv_id, caller, callee, caller_device, answer_device, \
     video, generation, started_at_ms, answered_at_ms, ended_at_ms, reason";

impl CallRecordSink for SqliteCallRecords {
    fn save(&mut self, record: &CallRecord) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO calls VALUES (?,?,?,?,?,?,?,?,?,?,?,?)
             ON CONFLICT(call_id) DO UPDATE SET
               answer_device = excluded.answer_device,
               answered_at_ms = excluded.answered_at_ms,
               ended_at_ms = excluded.ended_at_ms,
               reason = excluded.reason",
            params![
                record.call_id,
                record.conv_id,
                record.caller,
                record.callee,
                record.caller_device,
                record.answer_device,
                i64::from(record.video),
                record.generation as i64,
                record.started_at_ms as i64,
                record.answered_at_ms.map(|v| v as i64),
                record.ended_at_ms as i64,
                reason_str(record.reason),
            ],
        )?;
        Ok(())
    }

    fn get(&self, call_id: &str) -> anyhow::Result<Option<CallRecord>> {
        let sql = format!("SELECT {SELECT_COLUMNS} FROM calls WHERE call_id = ?1");
        Ok(self
            .conn
            .query_row(&sql, params![call_id], read_row)
            .optional()?)
    }

    fn recent(&self, conv_id: &str, limit: usize) -> anyhow::Result<Vec<CallRecord>> {
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM calls WHERE conv_id = ?1
             ORDER BY ended_at_ms DESC LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![conv_id, limit as i64], read_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

fn reason_str(reason: EndReason) -> &'static str {
    match reason {
        EndReason::Rejected => "rejected",
        EndReason::Cancelled => "cancelled",
        EndReason::HungUp => "hung_up",
        EndReason::Busy => "busy",
        EndReason::Timeout => "timeout",
        EndReason::Failed => "failed",
        EndReason::AuthorizationRevoked => "authorization_revoked",
    }
}

fn reason_from_str(raw: &str) -> EndReason {
    match raw {
        "rejected" => EndReason::Rejected,
        "cancelled" => EndReason::Cancelled,
        "hung_up" => EndReason::HungUp,
        "busy" => EndReason::Busy,
        "timeout" => EndReason::Timeout,
        "authorization_revoked" => EndReason::AuthorizationRevoked,
        _ => EndReason::Failed,
    }
}
