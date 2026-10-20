//! Zone E 的持久层：与对象目录同级的 `video.db`。
//!
//! 为什么用 SQLite：`wxlike-core` 已经带着 rusqlite（`outbox`/`Store` 都在用），
//! 加进本 crate 不会引入新的依赖版本；而播放统计要按人聚合，JSON 文件得全量重读。
//!
//! 连接形状照搬 `crates/core/src/outbox.rs`：单连接 + `Mutex` +
//! `PoisonError::into_inner`（一次 panic 不该把存储永久锁死）、
//! WAL + `busy_timeout(5s)`。video-service 与平台库各开各的连接，
//! 同一进程内并发写靠这两条 pragma 兜住。

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use anyhow::Context as _;
use rusqlite::Connection;

/// 一个文件库：播放事件表（E1）。
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
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub(crate) fn c(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }
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
"#;
