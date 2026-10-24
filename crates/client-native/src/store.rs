//! SQLite 实现：[`LocalStore`] 的原生侧唯一实现。
//!
//! 四条语义在这里落成具体的 SQL（对应端口文档里的四条不变量）：
//!
//! 1. **入队原子**：`INSERT … ON CONFLICT(key) DO UPDATE`，一条语句完成
//!    "新建或覆盖"，中途断电不会留下半条记录。
//! 2. **事件幂等**：`INSERT OR IGNORE` + `changes()` 判断是不是新的。
//! 3. **游标只前进**：写入前比对已存的 seq，回退直接报 `Corrupt`。
//! 4. **`synchronous = FULL`**：这是"杀进程不丢消息"的物理前提。
//!    WAL + `NORMAL` 更快，但掉电时会丢最后几个事务——那正好是用户刚点
//!    发送的那条消息。宁可慢一点。
//!
//! 为什么用 `spawn_blocking`：rusqlite 是同步的，直接在 async 上下文里调用
//! 会阻塞 executor 线程，表现是"发消息时整个界面卡一下"。所有数据库操作
//! 都搬到阻塞线程池，核心只等 future。

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension};
use wxlike_client_core::ports::{LocalStore, OutboxRecord, StoreError};
use wxlike_types::{ActionId, Cursor, EventEnvelope, IdempotencyKey};

/// 建表语句。**只增不改**：真要改结构，走 `user_version` 迁移。
const SCHEMA: &str = r"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = FULL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS outbox (
    key             TEXT PRIMARY KEY,
    action          TEXT NOT NULL,
    params          TEXT NOT NULL,
    attempts        INTEGER NOT NULL,
    next_attempt_at INTEGER NOT NULL,
    created_at      INTEGER NOT NULL,
    last_error      TEXT
);
CREATE INDEX IF NOT EXISTS outbox_due ON outbox(next_attempt_at, created_at);

CREATE TABLE IF NOT EXISTS events (
    event_id   TEXT PRIMARY KEY,
    seq        INTEGER NOT NULL,
    body       TEXT NOT NULL,
    emitted_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS events_seq ON events(seq);

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS cache (
    key       TEXT PRIMARY KEY,
    value     BLOB NOT NULL,
    sensitive INTEGER NOT NULL
);
";

/// SQLite 本地库。
pub struct SqliteStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteStore {
    /// 打开（或创建）一个库文件。
    ///
    /// # Errors
    /// 打不开、建表失败、目录不存在。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let conn = Connection::open(path).map_err(map_sqlite)?;
        Self::init(conn)
    }

    /// 内存库（测试用；进程退出即消失）。
    ///
    /// # Errors
    /// 建表失败。
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory().map_err(map_sqlite)?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        conn.execute_batch(SCHEMA).map_err(map_sqlite)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// 把一次数据库操作搬到阻塞线程池。
    async fn run<T, F>(&self, work: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StoreError> + Send + 'static,
    {
        let conn = self.conn.clone();
        let joined = tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            work(&mut guard)
        })
        .await;
        joined.map_err(|err| StoreError::Unavailable(format!("存储线程异常：{err}")))?
    }
}

/// SQLite 错误 → 端口错误。
fn map_sqlite(err: rusqlite::Error) -> StoreError {
    match err.sqlite_error_code() {
        Some(rusqlite::ErrorCode::DatabaseBusy) | Some(rusqlite::ErrorCode::DatabaseLocked) => {
            StoreError::Busy
        }
        Some(rusqlite::ErrorCode::DiskFull) => StoreError::QuotaExceeded,
        _ => StoreError::Unavailable(err.to_string()),
    }
}

fn parse_key(raw: &str) -> Result<IdempotencyKey, StoreError> {
    IdempotencyKey::parse(raw).map_err(|err| StoreError::Corrupt(err.to_string()))
}

fn parse_action(raw: &str) -> Result<ActionId, StoreError> {
    ActionId::parse(raw).map_err(|err| StoreError::Corrupt(err.to_string()))
}

type RawRecord = (String, String, String, i64, i64, i64, Option<String>);

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRecord> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
    ))
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl LocalStore for SqliteStore {
    async fn put_outbox(&self, record: &OutboxRecord) -> Result<(), StoreError> {
        let key = record.idempotency_key.as_str().to_string();
        let action = record.action_id.as_str().to_string();
        let params_text = record.params.to_string();
        let attempts = i64::from(record.attempts);
        let next_at = record.next_attempt_at_ms;
        let created = record.created_at_ms;
        let last_error = record.last_error.clone();
        self.run(move |conn| {
            conn.execute(
                "INSERT INTO outbox (key, action, params, attempts, next_attempt_at, created_at, last_error)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(key) DO UPDATE SET
                     action = excluded.action,
                     params = excluded.params,
                     attempts = excluded.attempts,
                     next_attempt_at = excluded.next_attempt_at,
                     last_error = excluded.last_error",
                params![key, action, params_text, attempts, next_at, created, last_error],
            )
            .map_err(map_sqlite)?;
            Ok(())
        })
        .await
    }

    async fn due_outbox(&self, now_ms: i64, limit: u32) -> Result<Vec<OutboxRecord>, StoreError> {
        let limit = i64::from(limit);
        self.run(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT key, action, params, attempts, next_attempt_at, created_at, last_error
                     FROM outbox WHERE next_attempt_at <= ?1
                     ORDER BY created_at ASC LIMIT ?2",
                )
                .map_err(map_sqlite)?;
            let rows = stmt
                .query_map(params![now_ms, limit], row_to_record)
                .map_err(map_sqlite)?;
            let mut out = Vec::new();
            for row in rows {
                let (key, action, body, attempts, next_at, created, last_error) =
                    row.map_err(map_sqlite)?;
                out.push(OutboxRecord {
                    idempotency_key: parse_key(&key)?,
                    action_id: parse_action(&action)?,
                    params: serde_json::from_str(&body)
                        .map_err(|err| StoreError::Corrupt(err.to_string()))?,
                    attempts: u32::try_from(attempts).unwrap_or(0),
                    next_attempt_at_ms: next_at,
                    created_at_ms: created,
                    last_error,
                });
            }
            Ok(out)
        })
        .await
    }

    async fn mark_outbox_done(&self, key: &IdempotencyKey) -> Result<(), StoreError> {
        let key = key.as_str().to_string();
        self.run(move |conn| {
            conn.execute("DELETE FROM outbox WHERE key = ?1", params![key])
                .map_err(map_sqlite)?;
            Ok(())
        })
        .await
    }

    async fn defer_outbox(
        &self,
        key: &IdempotencyKey,
        next_attempt_at_ms: i64,
        error: &str,
    ) -> Result<(), StoreError> {
        let key = key.as_str().to_string();
        let message = error.to_string();
        self.run(move |conn| {
            conn.execute(
                "UPDATE outbox SET attempts = attempts + 1, next_attempt_at = ?2, last_error = ?3
                 WHERE key = ?1",
                params![key, next_attempt_at_ms, message],
            )
            .map_err(map_sqlite)?;
            Ok(())
        })
        .await
    }

    async fn append_event(&self, event: &EventEnvelope) -> Result<bool, StoreError> {
        let id = event.event_id.as_str().to_string();
        let seq = i64::try_from(event.seq.get()).unwrap_or(i64::MAX);
        let body =
            serde_json::to_string(event).map_err(|err| StoreError::Corrupt(err.to_string()))?;
        let emitted = i64::try_from(event.emitted_at_ms).unwrap_or(i64::MAX);
        self.run(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO events (event_id, seq, body, emitted_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, seq, body, emitted],
            )
            .map_err(map_sqlite)?;
            Ok(conn.changes() > 0)
        })
        .await
    }

    async fn events_after(
        &self,
        cursor: Option<&Cursor>,
        limit: u32,
    ) -> Result<Vec<EventEnvelope>, StoreError> {
        let after = match cursor {
            Some(cursor) => cursor
                .decode()
                .map_err(|err| StoreError::Corrupt(err.to_string()))?
                .seq
                .get(),
            None => 0,
        };
        let after = i64::try_from(after).unwrap_or(i64::MAX);
        let limit = i64::from(limit);
        self.run(move |conn| {
            let mut stmt = conn
                .prepare("SELECT body FROM events WHERE seq > ?1 ORDER BY seq ASC LIMIT ?2")
                .map_err(map_sqlite)?;
            let rows = stmt
                .query_map(params![after, limit], |row| row.get::<_, String>(0))
                .map_err(map_sqlite)?;
            let mut out = Vec::new();
            for row in rows {
                let body = row.map_err(map_sqlite)?;
                let event: EventEnvelope = serde_json::from_str(&body)
                    .map_err(|err| StoreError::Corrupt(err.to_string()))?;
                out.push(event);
            }
            Ok(out)
        })
        .await
    }

    async fn put_cursor(&self, cursor: &Cursor) -> Result<(), StoreError> {
        let incoming = cursor
            .decode()
            .map_err(|err| StoreError::Corrupt(err.to_string()))?
            .seq
            .get();
        let text = cursor.as_str().to_string();
        self.run(move |conn| {
            let existing: Option<String> = conn
                .query_row("SELECT value FROM meta WHERE key = 'cursor'", [], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(map_sqlite)?;
            if let Some(raw) = existing {
                let previous =
                    Cursor::decode_seq(&raw).map_err(|err| StoreError::Corrupt(err.to_string()))?;
                if incoming < previous {
                    return Err(StoreError::Corrupt(format!(
                        "游标不许回退：{incoming} < {previous}"
                    )));
                }
            }
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('cursor', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![text],
            )
            .map_err(map_sqlite)?;
            Ok(())
        })
        .await
    }

    async fn cursor(&self) -> Result<Option<Cursor>, StoreError> {
        self.run(|conn| {
            let raw: Option<String> = conn
                .query_row("SELECT value FROM meta WHERE key = 'cursor'", [], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(map_sqlite)?;
            match raw {
                None => Ok(None),
                Some(text) => Cursor::parse_text(&text)
                    .map(Some)
                    .map_err(|err| StoreError::Corrupt(err.to_string())),
            }
        })
        .await
    }

    async fn put_cache(&self, key: &str, value: &[u8], sensitive: bool) -> Result<(), StoreError> {
        let key = key.to_string();
        let bytes = value.to_vec();
        self.run(move |conn| {
            conn.execute(
                "INSERT INTO cache (key, value, sensitive) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, sensitive = excluded.sensitive",
                params![key, bytes, i64::from(sensitive)],
            )
            .map_err(map_sqlite)?;
            Ok(())
        })
        .await
    }

    async fn get_cache(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let key = key.to_string();
        self.run(move |conn| {
            let row: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT value FROM cache WHERE key = ?1",
                    params![key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(map_sqlite)?;
            Ok(row)
        })
        .await
    }

    async fn purge(&self) -> Result<(), StoreError> {
        self.run(|conn| {
            let tx = conn.transaction().map_err(map_sqlite)?;
            tx.execute("DELETE FROM outbox", []).map_err(map_sqlite)?;
            tx.execute("DELETE FROM events", []).map_err(map_sqlite)?;
            tx.execute("DELETE FROM cache", []).map_err(map_sqlite)?;
            tx.execute("DELETE FROM meta", []).map_err(map_sqlite)?;
            tx.commit().map_err(map_sqlite)?;
            Ok(())
        })
        .await
    }
}
