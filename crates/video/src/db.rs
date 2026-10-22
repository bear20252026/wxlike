//! Zone E 的持久层：与对象目录同级的 `video.db`。
//!
//! 为什么用 SQLite：`wxlike-core` 已经带着 rusqlite（`outbox`/`Store` 都在用），
//! 加进本 crate 不会引入新的依赖版本；而播放统计要按人聚合、作业队列要按
//! "最早该重试"筛选，JSON 文件两者都做不好（前者要全量重读，后者要自己造索引）。
//!
//! 连接形状照搬 `crates/core/src/outbox.rs`：单连接 + `Mutex` +
//! `PoisonError::into_inner`（一次 panic 不该把存储永久锁死）、
//! WAL + `busy_timeout(5s)`。video-service 与平台库各开各的连接，
//! 同一进程内并发写靠这两条 pragma 兜住。

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use anyhow::Context as _;
use rusqlite::Connection;

/// 一个文件库里的两张表：播放事件（E1）与转码作业（E3）。
///
/// 直播（E2）不落库——播放窗口是文件系统的既有真相（`live/{key}/` 目录），
/// 再抄一份进数据库只会多一个会不同步的地方。
pub struct VideoDb {
    conn: Mutex<Connection>,
}

impl VideoDb {
    /// 打开（或创建）文件库并建表。
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn =
            Connection::open(path).with_context(|| format!("open video db {}", path.display()))?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> anyhow::Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .context("set busy_timeout")?;
        conn.execute_batch(SCHEMA)
            .context("video-service schema (jobs + watch_events)")?;
        // 老库迁移：`owner` 是 issue #7 加的列，已经在跑的部署里没有它。
        // 用 `pragma_table_info` 判断而不是 `ALTER ... IF NOT EXISTS`（SQLite
        // 没有后者），失败时宁可报错也不要静默跳过。
        ensure_column(&conn, "video_jobs", "owner", "TEXT NOT NULL DEFAULT ''")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub(crate) fn c(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// 给已有的表补一列（幂等）。老库没有这一列时补上，新库直接跳过。
fn ensure_column(conn: &Connection, table: &str, column: &str, decl: &str) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let existing: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<_, _>>()?;
    if existing.iter().any(|c| c == column) {
        return Ok(());
    }
    conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))
        .with_context(|| format!("add column {table}.{column}"))?;
    Ok(())
}

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;

-- E1 播放时长统计：append-only 事件，聚合靠重放（形状照搬 crates/ledger 的 entries）
CREATE TABLE IF NOT EXISTS watch_events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    video_id     TEXT NOT NULL,
    viewer       TEXT NOT NULL,
    session_id   TEXT NOT NULL,
    -- 本次上报的新增观看时长（心跳增量，不是累计值）
    watched_ms   INTEGER NOT NULL,
    -- 播放器当前进度，用于判断"看到了第几分钟"
    position_ms  INTEGER NOT NULL,
    -- 客户端自报看完（服务端不猜）
    completed    INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL
);
-- 聚合的热路径：按视频取全部事件
CREATE INDEX IF NOT EXISTS watch_events_video
    ON watch_events(video_id, created_at);

-- E3 转码作业 + 出站回调，状态机形状照搬 crates/core/src/outbox.rs
CREATE TABLE IF NOT EXISTS video_jobs (
    job_id                TEXT PRIMARY KEY,
    -- 提交者（issue #7）：查作业/重试只对自己的人开放
    owner                 TEXT NOT NULL DEFAULT '',
    -- pending: 待转码；running: 在飞；succeeded: 可播；failed: 不再自动重试
    state                 TEXT NOT NULL,
    attempts              INTEGER NOT NULL DEFAULT 0,
    next_retry_at         INTEGER NOT NULL,
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL,
    last_error            TEXT,
    source_size           INTEGER NOT NULL,
    -- 成功后的 Ingested 结果（客户端拉作业状态就能拿到播放地址）
    result                TEXT,
    -- 回调与作业分开：转码成功但回调失败时，作业不能退回 pending 重转一遍
    -- none: 未配置回调；pending: 待发；delivered: 已送达；failed: 不再重试
    callback_state        TEXT NOT NULL DEFAULT 'none',
    -- 本作业的回调地址（提交时给；没给则用服务端的 WXLIKE_VIDEO_CALLBACK_URL）
    callback_url          TEXT,
    callback_attempts     INTEGER NOT NULL DEFAULT 0,
    callback_next_retry_at INTEGER NOT NULL DEFAULT 0,
    callback_error        TEXT
);
CREATE INDEX IF NOT EXISTS video_jobs_ready ON video_jobs(state, next_retry_at);
CREATE INDEX IF NOT EXISTS video_jobs_cb_ready
    ON video_jobs(callback_state, callback_next_retry_at);
-- 配额查询的热路径：按人统计在飞/排队作业与字节数
CREATE INDEX IF NOT EXISTS video_jobs_owner ON video_jobs(owner, state);

-- 同步上传（`POST /api/video`）不建作业，但同样要记归属：
-- 否则"谁传的这条视频"只存在于调用方的记忆里。
CREATE TABLE IF NOT EXISTS video_owners (
    video_id   TEXT NOT NULL,
    owner      TEXT NOT NULL,
    -- 源字节大小：同步上传也吃磁盘，配额要按人算总占用
    size       INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    -- 同一份字节可以被多个人上传（内容寻址去重），所以归属是多对多
    PRIMARY KEY (video_id, owner)
);
CREATE INDEX IF NOT EXISTS video_owners_owner ON video_owners(owner);
"#;
