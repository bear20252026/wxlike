//! G09-02 通话记录与房间审计的真实落库实现。
//!
//! 状态机本身不碰数据库；`CallServer` 只依赖 [`CallRecordSink`]，房间只依赖
//! [`RoomAuditSink`]。两张表各给两个实现：内存版（单测/并行开发）与 SQLite 版
//! （真实服务、测试里也用真 SQLite，不是 mock）。通话表见 `schema/calls.sql`；
//! 房间审计表 DDL 内联在 [`ROOM_AUDIT_SCHEMA`]（ADR-25 把"房间审计留在
//! Rust"这条责任落在这里），正式迁移编号由 G00 分配后并入平台库——见
//! `integration-requests/G09/153-livekit-signaling-audit-schema.yaml`。
//!
//! 两张表都遵守个人数据最小化：只有 id、时间、结论码与水印，不含 SDP、IP、
//! 设备指纹、密钥或任何媒体内容。

use anyhow::Context as _;
use rusqlite::{params, OptionalExtension as _, Row};

use crate::call_session::{CallRecord, EndReason};
use crate::room::{RoomAuditAction, RoomAuditEntry};

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

// ---------------------------------------------------------------------------
// 房间审计（ADR-25：房间审计留在 Rust）
// ---------------------------------------------------------------------------

/// 房间审计 DDL。主键 `(room, seq)`：同一房间同一序号重复写入被忽略
/// （客户端重试、进程重启后重放同一批），这样 [`RoomAuditSink::append`]
/// 才是幂等的。
///
/// 回滚：只新增表与索引，不动既有表；旧进程不读它，所以
/// `DROP INDEX idx_room_audit_conv_time, idx_room_audit_actor;
///  DROP TABLE call_room_audit;` 即可安全回退。正式迁移编号由 G00 分配。
pub const ROOM_AUDIT_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS call_room_audit (
    room         TEXT    NOT NULL,
    seq          INTEGER NOT NULL,
    sidecar_room TEXT    NOT NULL,
    conv         TEXT    NOT NULL,
    at_ms        INTEGER NOT NULL,
    actor        TEXT    NOT NULL,
    action       TEXT    NOT NULL,
    target       TEXT,
    device       TEXT,
    revision     INTEGER NOT NULL,
    generation   INTEGER NOT NULL,
    code         TEXT,
    detail       TEXT,
    engine       TEXT    NOT NULL,
    PRIMARY KEY (room, seq)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_room_audit_room
    ON call_room_audit(room, seq DESC);
CREATE INDEX IF NOT EXISTS idx_room_audit_conv_time
    ON call_room_audit(conv, at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_room_audit_actor
    ON call_room_audit(actor, at_ms DESC);
"#;

/// 房间审计写入口。实现方负责落库或集中式日志；本模块不写磁盘。
pub trait RoomAuditSink {
    /// 追加一批；返回**新写入**的条数（重复投递的不计）。
    fn append(&mut self, entries: &[RoomAuditEntry]) -> anyhow::Result<usize>;
    /// 某房间最近 `limit` 条，按 `seq` 升序（可回放的顺序）。
    fn for_room(&self, room: &str, limit: usize) -> anyhow::Result<Vec<RoomAuditEntry>>;
    /// 某人做过/试过什么（越权尝试的排查入口）。
    fn for_actor(&self, actor: &str, limit: usize) -> anyhow::Result<Vec<RoomAuditEntry>>;
    /// 被拒动作条数——运维判断"有人在刷房间"的唯一指标。
    fn denied(&self, room: &str) -> anyhow::Result<u64>;
}

#[derive(Default)]
pub struct MemoryRoomAudit {
    rows: Vec<RoomAuditEntry>,
}

impl MemoryRoomAudit {
    pub fn rows(&self) -> &[RoomAuditEntry] {
        &self.rows
    }
}

impl RoomAuditSink for MemoryRoomAudit {
    fn append(&mut self, entries: &[RoomAuditEntry]) -> anyhow::Result<usize> {
        let mut inserted = 0usize;
        for entry in entries {
            if self
                .rows
                .iter()
                .any(|r| r.room == entry.room && r.seq == entry.seq)
            {
                continue;
            }
            self.rows.push(entry.clone());
            inserted += 1;
        }
        Ok(inserted)
    }

    fn for_room(&self, room: &str, limit: usize) -> anyhow::Result<Vec<RoomAuditEntry>> {
        let mut rows: Vec<RoomAuditEntry> = self
            .rows
            .iter()
            .filter(|r| r.room == room)
            .cloned()
            .collect();
        rows.sort_by_key(|r| r.seq);
        let start = rows.len().saturating_sub(limit);
        Ok(rows[start..].to_vec())
    }

    fn for_actor(&self, actor: &str, limit: usize) -> anyhow::Result<Vec<RoomAuditEntry>> {
        let mut rows: Vec<RoomAuditEntry> = self
            .rows
            .iter()
            .filter(|r| r.actor == actor)
            .cloned()
            .collect();
        rows.sort_by_key(|r| r.seq);
        let start = rows.len().saturating_sub(limit);
        Ok(rows[start..].to_vec())
    }

    fn denied(&self, room: &str) -> anyhow::Result<u64> {
        Ok(self
            .rows
            .iter()
            .filter(|r| r.room == room && r.code.is_some())
            .count() as u64)
    }
}

pub struct SqliteRoomAudit {
    conn: rusqlite::Connection,
}

impl SqliteRoomAudit {
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let conn = rusqlite::Connection::open(path).context("open room audit db")?;
        Self::from_conn(conn)
    }

    pub fn in_memory() -> anyhow::Result<Self> {
        Self::from_conn(rusqlite::Connection::open_in_memory()?)
    }

    fn from_conn(conn: rusqlite::Connection) -> anyhow::Result<Self> {
        conn.execute_batch(ROOM_AUDIT_SCHEMA)
            .context("apply room audit schema")?;
        Ok(Self { conn })
    }
}

const AUDIT_COLUMNS: &str = "room, seq, sidecar_room, conv, at_ms, actor, action, target, device, \
     revision, generation, code, detail, engine";

fn read_audit_row(row: &Row<'_>) -> rusqlite::Result<RoomAuditEntry> {
    let raw_action: String = row.get("action")?;
    // 库里出现不认识的动作说明有外部写入：宁可不返回，也不折成"其它"。
    let action = RoomAuditAction::parse(&raw_action).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Text,
            Box::from(format!("unknown room audit action {raw_action}")),
        )
    })?;
    Ok(RoomAuditEntry {
        room: row.get("room")?,
        seq: row.get::<_, i64>("seq")? as u64,
        sidecar_room: row.get("sidecar_room")?,
        conv: row.get("conv")?,
        at_ms: row.get::<_, i64>("at_ms")? as u64,
        actor: row.get("actor")?,
        action,
        target: row.get("target")?,
        device: row.get("device")?,
        revision: row.get::<_, i64>("revision")? as u64,
        generation: row.get::<_, i64>("generation")? as u64,
        code: row.get("code")?,
        detail: row.get("detail")?,
        engine: row.get("engine")?,
    })
}

impl RoomAuditSink for SqliteRoomAudit {
    fn append(&mut self, entries: &[RoomAuditEntry]) -> anyhow::Result<usize> {
        let mut inserted = 0usize;
        for entry in entries {
            let changed = self.conn.execute(
                "INSERT OR IGNORE INTO call_room_audit
                 (room, seq, sidecar_room, conv, at_ms, actor, action, target, device,
                  revision, generation, code, detail, engine)
                 VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                rusqlite::params![
                    entry.room,
                    entry.seq as i64,
                    entry.sidecar_room,
                    entry.conv,
                    entry.at_ms as i64,
                    entry.actor,
                    entry.action.as_str(),
                    entry.target,
                    entry.device,
                    entry.revision as i64,
                    entry.generation as i64,
                    entry.code,
                    entry.detail,
                    entry.engine,
                ],
            )?;
            inserted += changed;
        }
        Ok(inserted)
    }

    fn for_room(&self, room: &str, limit: usize) -> anyhow::Result<Vec<RoomAuditEntry>> {
        let sql = format!(
            "SELECT {AUDIT_COLUMNS} FROM call_room_audit WHERE room = ?1
             ORDER BY seq DESC LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt
            .query_map(rusqlite::params![room, limit as i64], read_audit_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.reverse();
        Ok(rows)
    }

    fn for_actor(&self, actor: &str, limit: usize) -> anyhow::Result<Vec<RoomAuditEntry>> {
        let sql = format!(
            "SELECT {AUDIT_COLUMNS} FROM call_room_audit WHERE actor = ?1
             ORDER BY seq DESC LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt
            .query_map(rusqlite::params![actor, limit as i64], read_audit_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.reverse();
        Ok(rows)
    }

    fn denied(&self, room: &str) -> anyhow::Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM call_room_audit WHERE room = ?1 AND code IS NOT NULL",
            rusqlite::params![room],
            |row| row.get(0),
        )?;
        Ok(n as u64)
    }
}
