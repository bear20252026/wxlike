//! SQLite-backed storage for conversations and messages.
//!
//! The connection sits behind a `Mutex` so one `Arc<Store>` can be shared by
//! every hub task (same shape as the platform's `Db`); no guard is ever held
//! across a call that would re-enter the lock.
//!
//! Responsibilities added by WP-03:
//! * **deduplication** — `messages.msg_id` carries a partial UNIQUE index, so a
//!   re-delivered envelope (at-least-once MQ, retrying client) is dropped
//!   silently instead of being stored twice;
//! * **recall** — the target row is flagged `withdrawn` and its body is
//!   overwritten with the placeholder, so the original text cannot be read back;
//! * **multi-device sync** — `device_cursor` remembers how far each device has
//!   seen each conversation, so `sync_since` returns only the delta.
//!
//! Legacy databases are upgraded in place by [`Store::open`]: missing columns
//! are added, nothing is rewritten.

use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::types::{Envelope, MsgBody, MsgId, NO_MSG_ID, PROTOCOL_VERSION};
use anyhow::Context as _;
use rusqlite::{params, Connection, OptionalExtension as _};

pub struct Store {
    conn: Mutex<Connection>,
    /// P3 search-index hook: fired after every successful (non-duplicate)
    /// append with `(conv_id, seq, envelope)`. The search service subscribes
    /// to keep its Tantivy index in step with the authoritative store; the
    /// store itself never reads it back (index is disposable).
    append_listener: Mutex<Option<std::sync::Arc<dyn AppendListener>>>,
}

/// Callback contract for index builders. Sync by design: the caller owns
/// any batching/commit cadence off-thread.
pub trait AppendListener: Send + Sync {
    fn on_append(&self, conv_id: &str, seq: i64, env: &Envelope);
}

#[derive(Clone, Debug)]
pub struct StoredMessage {
    pub seq: i64,
    pub envelope: Envelope,
    pub sealed: bool,
    /// True when the author recalled this message; `envelope.body` is then the
    /// placeholder and the original bytes are gone from the read path.
    pub withdrawn: bool,
}

#[derive(Clone, Debug)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub updated_at_ms: u64,
}

/// Columns read back into a [`StoredMessage`]; keep in sync with projections.
const MESSAGE_COLUMNS: &str = "seq, conv_id, sender, kind, body, sent_at_ms, sealed, \
     msg_id, quote_of, recipients, withdrawn";

impl Store {
    /// `None` opens an in-memory store (tests / demos).
    pub fn open(path: Option<&str>) -> anyhow::Result<Self> {
        let conn = match path {
            Some(p) => Connection::open(p).with_context(|| format!("open db {p}"))?,
            None => Connection::open_in_memory()?,
        };
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA busy_timeout = 5000;
            CREATE TABLE IF NOT EXISTS conversations (
                id           TEXT PRIMARY KEY,
                title        TEXT NOT NULL DEFAULT '',
                updated_at_ms INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS messages (
                seq        INTEGER PRIMARY KEY AUTOINCREMENT,
                conv_id    TEXT NOT NULL,
                sender     TEXT NOT NULL,
                kind       INTEGER NOT NULL,
                body       BLOB NOT NULL,
                sent_at_ms INTEGER NOT NULL,
                sealed     INTEGER NOT NULL DEFAULT 0,
                FOREIGN KEY(conv_id) REFERENCES conversations(id)
            );
            CREATE INDEX IF NOT EXISTS idx_messages_conv ON messages(conv_id, seq);
            CREATE TABLE IF NOT EXISTS device_cursor (
                device_id  TEXT NOT NULL,
                conv_id    TEXT NOT NULL,
                last_seq   INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (device_id, conv_id)
            );
            "#,
        )?;
        let store = Self {
            conn: Mutex::new(conn),
            append_listener: Mutex::new(None),
        };
        store.add_missing_columns()?;
        Ok(store)
    }

    /// A poisoned lock only follows a panic mid-statement; SQLite connections
    /// stay usable, so recovering beats crashing the hub.
    fn c(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Idempotent column upgrades for databases written by an earlier version.
    fn add_missing_columns(&self) -> anyhow::Result<()> {
        let mut have = Vec::new();
        {
            let c = self.c();
            let mut stmt = c.prepare("PRAGMA table_info(messages)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            for name in rows {
                have.push(name?);
            }
        }
        let c = self.c();
        for (col, ddl) in [
            ("msg_id", "BLOB"),
            ("quote_of", "BLOB"),
            ("recipients", "TEXT NOT NULL DEFAULT '[]'"),
            ("withdrawn", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            if !have.iter().any(|name| name == col) {
                c.execute_batch(&format!("ALTER TABLE messages ADD COLUMN {col} {ddl};"))
                    .with_context(|| format!("add column {col}"))?;
            }
        }
        // NULL msg_id = legacy row. The index is partial so old messages that
        // carry no dedup key can still coexist.
        c.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_msgid
             ON messages(msg_id) WHERE msg_id IS NOT NULL;",
        )?;
        Ok(())
    }

    pub fn upsert_conversation(&self, id: &str, title: &str) -> anyhow::Result<()> {
        self.c().execute(
            "INSERT INTO conversations(id, title, updated_at_ms) VALUES(?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET title=excluded.title",
            params![id, title, crate::types::now_ms() as i64],
        )?;
        Ok(())
    }

    /// Stores an envelope. `Ok(None)` means its `msg_id` was already present
    /// and it was dropped — a duplicate is not an error.
    pub fn append(&self, env: &Envelope, sealed: bool) -> anyhow::Result<Option<i64>> {
        let body = bincode::serialize(&env.body)?;
        let msg_id: Option<&[u8]> = if env.msg_id == NO_MSG_ID {
            None
        } else {
            Some(env.msg_id.as_slice())
        };
        let quote_of: Option<&[u8]> = env.quote_of.as_ref().map(|q| q.as_slice());
        let recipients = serde_json::to_string(&env.to)?;
        let c = self.c();
        // A message can arrive for a conversation that was never explicitly
        // created (first DM of a fresh pair), so provision it here: keeps the
        // foreign key honest wherever messages are written.
        c.execute(
            "INSERT INTO conversations(id, title, updated_at_ms) VALUES(?1, '', ?2)
             ON CONFLICT(id) DO UPDATE SET updated_at_ms = excluded.updated_at_ms",
            params![env.conv_id, env.sent_at_ms as i64],
        )?;
        match c.execute(
            "INSERT INTO messages(conv_id, sender, kind, body, sent_at_ms, sealed,
                                  msg_id, quote_of, recipients, withdrawn)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0)",
            params![
                env.conv_id,
                env.sender,
                kind_tag(env.kind),
                body,
                env.sent_at_ms as i64,
                sealed as i64,
                msg_id,
                quote_of,
                recipients
            ],
        ) {
            Ok(_) => {
                let seq = c.last_insert_rowid();
                drop(c);
                if let Some(listener) = self.listener() {
                    listener.on_append(&env.conv_id, seq, env);
                }
                Ok(Some(seq))
            }
            Err(e) if is_unique(&e) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Subscribes the search index (P3). One listener per store: the search
    /// service is the only consumer by design.
    pub fn set_append_listener(&self, listener: std::sync::Arc<dyn AppendListener>) {
        *self
            .append_listener
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(listener);
    }

    fn listener(&self) -> Option<std::sync::Arc<dyn AppendListener>> {
        self.append_listener
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// For callers that treat a duplicate as a mistake rather than noise.
    pub fn append_message(&self, env: &Envelope, sealed: bool) -> anyhow::Result<i64> {
        self.append(env, sealed)?
            .context("message already stored (duplicate msg_id)")
    }

    pub fn list_messages(&self, conv_id: &str) -> anyhow::Result<Vec<StoredMessage>> {
        let c = self.c();
        let mut stmt = c.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages WHERE conv_id = ?1 ORDER BY seq"
        ))?;
        let rows = stmt.query_map([conv_id], row_to_message)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Newest-first page of a conversation for UI history loading (P1).
    ///
    /// Returns up to `limit` messages with `seq < before_seq` (so the first
    /// page is `before_seq = 0` meaning "no bound"), in ascending order so the
    /// caller can prepend them directly. `next_before_seq` is what the caller
    /// passes next time — `None` when the conversation's start was reached.
    pub fn page_messages(
        &self,
        conv_id: &str,
        before_seq: i64,
        limit: usize,
    ) -> anyhow::Result<(Vec<StoredMessage>, Option<i64>)> {
        let limit = limit.clamp(1, 200);
        let c = self.c();
        let mut stmt = c.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages
             WHERE conv_id = ?1 AND (?2 = 0 OR seq < ?2)
             ORDER BY seq DESC LIMIT ?3"
        ))?;
        let mut rows = stmt
            .query_map(params![conv_id, before_seq, limit], row_to_message)?
            .collect::<Result<Vec<_>, _>>()?;
        if rows.is_empty() {
            return Ok((Vec::new(), None));
        }
        // rows arrive DESC; the page cursor is its OLDEST row so the next
        // page asks for strictly older messages
        let next_before = rows[rows.len() - 1].seq;
        let exhausted = (rows.len() as i64) < limit as i64;
        rows.reverse();
        Ok((rows, if exhausted { None } else { Some(next_before) }))
    }

    /// 按 msg_id 批量取归档行（信箱只存 id，正文还在归档里）。
    ///
    /// 信箱是**排序与未读**的真相，归档是**正文**的真相；history 先问信箱
    /// 要这一页有哪些 id、按什么顺序，再来这里取正文——这样翻页游标换成
    /// 信箱序列号，而正文仍然无损。
    pub fn by_msg_ids(
        &self,
        ids: &[Vec<u8>],
    ) -> anyhow::Result<std::collections::HashMap<Vec<u8>, StoredMessage>> {
        let mut out = std::collections::HashMap::with_capacity(ids.len());
        if ids.is_empty() {
            return Ok(out);
        }
        let c = self.c();
        let mut stmt = c.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages WHERE msg_id = ?1"
        ))?;
        for id in ids {
            if let Ok(row) = stmt.query_row(params![id], row_to_message) {
                out.insert(id.clone(), row);
            }
        }
        Ok(out)
    }

    /// Substring search over stored messages, restricted to the
    /// conversations in `conv_filter` (empty filter = no result: callers pass
    /// the conversations the requesting user is allowed to see).
    ///
    /// Matching happens in Rust on the decoded body, not in SQL: bodies are
    /// bincode blobs whose length prefix makes `CAST(body AS TEXT) LIKE …`
    /// unreliable (embedded NULs), and Rust's `to_lowercase` folds non-ASCII
    /// correctly where SQLite's LIKE only folds ASCII. `%`/`_` in the needle
    /// are literal characters. Recalled rows are skipped, results are newest
    /// first, capped at `limit`. This is the P1 search protocol's backing
    /// store; the P3 search-service replaces it with a Tantivy/Meilisearch
    /// index fed by events.
    pub fn search_messages(
        &self,
        needle: &str,
        conv_filter: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<StoredMessage>> {
        if conv_filter.is_empty() || needle.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, 100);
        let needle = needle.to_lowercase();
        let mut hits = Vec::new();
        for conv in conv_filter {
            for row in self.list_messages(conv)? {
                if row.withdrawn {
                    continue;
                }
                if row
                    .envelope
                    .body
                    .preview_text()
                    .to_lowercase()
                    .contains(&needle)
                {
                    hits.push(row);
                }
            }
        }
        hits.sort_by_key(|m| std::cmp::Reverse(m.seq));
        hits.truncate(limit);
        Ok(hits)
    }

    /// One message by `(conv, seq)` — the shape a search-index hit carries
    /// when it comes back for authoritative rendering.
    pub fn message_at(&self, conv_id: &str, seq: i64) -> anyhow::Result<Option<StoredMessage>> {
        let c = self.c();
        let mut stmt = c.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages WHERE conv_id = ?1 AND seq = ?2"
        ))?;
        Ok(stmt
            .query_row(params![conv_id, seq], row_to_message)
            .optional()?)
    }

    /// Who authored `msg_id` — the hub needs this to route a receipt back.
    pub fn sender_of(&self, msg_id: &MsgId) -> anyhow::Result<Option<String>> {
        Ok(self
            .c()
            .query_row(
                "SELECT sender FROM messages WHERE msg_id = ?1",
                params![msg_id.as_slice()],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    }

    pub fn has_message(&self, msg_id: &MsgId) -> anyhow::Result<bool> {
        Ok(self
            .c()
            .query_row(
                "SELECT 1 FROM messages WHERE msg_id = ?1",
                params![msg_id.as_slice()],
                |_| Ok(1i64),
            )
            .optional()?
            .is_some())
    }

    /// Recalls `target`, but only when `by_sender` wrote it. Unknown ids and
    /// "not yours to recall" both return false without touching the store —
    /// that is what makes a forged recall harmless (WP-03 A3).
    pub fn withdraw(&self, target: &MsgId, by_sender: &str) -> anyhow::Result<bool> {
        let placeholder = bincode::serialize(&MsgBody::recalled())?;
        let changed = self.c().execute(
            "UPDATE messages SET withdrawn = 1, body = ?2
             WHERE msg_id = ?1 AND sender = ?3 AND withdrawn = 0",
            params![target.as_slice(), placeholder, by_sender],
        )?;
        Ok(changed == 1)
    }

    /// Withdrawal check by `(conv, seq)` — the shape a search-index hit
    /// carries. Recall filtering stays authoritative: the index is never
    /// asked "is this visible", the store is.
    pub fn is_withdrawn_by_seq(&self, conv_id: &str, seq: i64) -> anyhow::Result<bool> {
        Ok(self
            .c()
            .query_row(
                "SELECT withdrawn FROM messages WHERE conv_id = ?1 AND seq = ?2",
                params![conv_id, seq],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            != 0)
    }

    pub fn is_withdrawn(&self, msg_id: &MsgId) -> anyhow::Result<bool> {
        Ok(self
            .c()
            .query_row(
                "SELECT withdrawn FROM messages WHERE msg_id = ?1",
                params![msg_id.as_slice()],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            != 0)
    }

    /// Messages in `conv_id` this device has not received yet. The cursor then
    /// advances to the last row returned.
    pub fn sync_since(&self, device_id: &str, conv_id: &str) -> anyhow::Result<Vec<StoredMessage>> {
        let (rows, last) = {
            let cursor = self.device_cursor(device_id, conv_id)?;
            let c = self.c();
            let mut stmt = c.prepare(&format!(
                "SELECT {MESSAGE_COLUMNS} FROM messages WHERE conv_id = ?1 AND seq > ?2 ORDER BY seq"
            ))?;
            let rows = stmt
                .query_map(params![conv_id, cursor], row_to_message)?
                .collect::<Result<Vec<_>, _>>()?;
            let last = rows.last().map(|r| r.seq);
            (rows, last)
        };
        if let Some(seq) = last {
            self.set_device_cursor(device_id, conv_id, seq)?;
        }
        Ok(rows)
    }

    pub fn device_cursor(&self, device_id: &str, conv_id: &str) -> anyhow::Result<i64> {
        Ok(self
            .c()
            .query_row(
                "SELECT last_seq FROM device_cursor WHERE device_id = ?1 AND conv_id = ?2",
                params![device_id, conv_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    pub fn set_device_cursor(
        &self,
        device_id: &str,
        conv_id: &str,
        last_seq: i64,
    ) -> anyhow::Result<()> {
        self.c().execute(
            "INSERT INTO device_cursor(device_id, conv_id, last_seq, updated_at) VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(device_id, conv_id) DO UPDATE SET last_seq = excluded.last_seq, updated_at = excluded.updated_at",
            params![device_id, conv_id, last_seq, crate::types::now_ms() as i64],
        )?;
        Ok(())
    }

    /// Records that a device saw this message. Receipts and legacy (zero-id)
    /// envelopes are ignored, so an ack can never corrupt a cursor.
    pub fn note_seen(&self, device_id: &str, env: &Envelope) -> anyhow::Result<()> {
        if env.receipt.is_some() || env.msg_id == NO_MSG_ID {
            return Ok(());
        }
        let seq: Option<i64> = self
            .c()
            .query_row(
                "SELECT seq FROM messages WHERE msg_id = ?1",
                params![env.msg_id.as_slice()],
                |r| r.get::<_, i64>(0),
            )
            .optional()?;
        match seq {
            Some(seq) => self.set_device_cursor(device_id, &env.conv_id, seq),
            None => Ok(()),
        }
    }

    pub fn list_conversations(&self) -> anyhow::Result<Vec<Conversation>> {
        let c = self.c();
        let mut stmt = c.prepare(
            "SELECT id, title, updated_at_ms FROM conversations ORDER BY updated_at_ms DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Conversation {
                id: r.get(0)?,
                title: r.get(1)?,
                updated_at_ms: r.get::<_, i64>(2)? as u64,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

impl StoredMessage {
    /// True when the original body is no longer retrievable.
    pub fn is_recalled(&self) -> bool {
        self.withdrawn
    }
}

/// Reads one `messages` row by column name, so changing a projection cannot
/// silently shift indices. Recalled rows come back as the placeholder only.
fn row_to_message(r: &rusqlite::Row<'_>) -> rusqlite::Result<StoredMessage> {
    let body_bytes: Vec<u8> = r.get("body")?;
    let recipients: String = r.get("recipients")?;
    let msg_id: Option<Vec<u8>> = r.get("msg_id")?;
    let quote_of: Option<Vec<u8>> = r.get("quote_of")?;
    let kind_tag: i64 = r.get("kind")?;
    let seq: i64 = r.get("seq")?;
    Ok(StoredMessage {
        seq,
        withdrawn: r.get::<_, i64>("withdrawn")? != 0,
        sealed: r.get::<_, i64>("sealed")? != 0,
        envelope: Envelope {
            version: PROTOCOL_VERSION,
            conv_id: r.get("conv_id")?,
            sender: r.get("sender")?,
            to: serde_json::from_str(&recipients).unwrap_or_default(),
            seq: seq as u64,
            kind: kind_from_tag(kind_tag)?,
            body: bincode::deserialize(&body_bytes).map_err(corrupt_body)?,
            sent_at_ms: r.get::<_, i64>("sent_at_ms")? as u64,
            msg_id: msg_id.as_deref().map(to_id).unwrap_or(NO_MSG_ID),
            quote_of: quote_of.as_deref().map(to_id),
            receipt: None,
        },
    })
}

fn to_id(bytes: &[u8]) -> MsgId {
    <[u8; 16]>::try_from(bytes).unwrap_or(NO_MSG_ID)
}

/// A body that cannot be decoded is a storage bug: surface it as an error
/// instead of letting it masquerade as a recalled message.
fn corrupt_body(err: bincode::Error) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
        Some(format!("undecodable message body: {err}")),
    )
}

fn kind_tag(k: crate::types::MsgKind) -> i64 {
    use crate::types::MsgKind;
    match k {
        MsgKind::Text => 0,
        MsgKind::Blob => 1,
        MsgKind::System => 2,
    }
}

fn kind_from_tag(t: i64) -> rusqlite::Result<crate::types::MsgKind> {
    Ok(match t {
        0 => crate::types::MsgKind::Text,
        1 => crate::types::MsgKind::Blob,
        2 => crate::types::MsgKind::System,
        _ => return Err(rusqlite::Error::IntegralValueOutOfRange(2, t)),
    })
}

fn is_unique(e: &rusqlite::Error) -> bool {
    match e {
        rusqlite::Error::SqliteFailure(_, Some(msg)) => msg.contains("UNIQUE"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MsgBody, MsgKind};

    #[test]
    fn message_persistence() {
        let store = Store::open(None).unwrap();
        store.upsert_conversation("c1", "Rust Dev").unwrap();
        let e = Envelope {
            version: 1,
            conv_id: "c1".into(),
            sender: "alice".into(),
            to: vec![],
            seq: 0,
            kind: MsgKind::Text,
            body: MsgBody::Text("saved".into()),
            sent_at_ms: 123,
            msg_id: NO_MSG_ID,
            quote_of: None,
            receipt: None,
        };
        let seq = store.append_message(&e, true).unwrap();
        assert!(seq > 0);
        let msgs = store.list_messages("c1").unwrap();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].sealed);
        assert_eq!(msgs[0].envelope.body, MsgBody::Text("saved".into()));
        assert_eq!(store.list_conversations().unwrap().len(), 1);
    }

    #[test]
    fn recipients_round_trip() {
        let store = Store::open(None).unwrap();
        store.upsert_conversation("c1", "t").unwrap();
        let e = Envelope::direct("c1", "alice", vec!["bob".into(), "carol".into()], 1, "hi");
        store.append(&e, false).unwrap();
        let got = store.list_messages("c1").unwrap();
        assert_eq!(
            got[0].envelope.to,
            vec!["bob".to_string(), "carol".to_string()]
        );
    }

    #[test]
    fn reopened_database_keeps_v2_columns() {
        let dir = std::env::temp_dir().join(format!("wxlike-store-{}", crate::types::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        {
            let store = Store::open(Some(path.to_str().unwrap())).unwrap();
            store.upsert_conversation("c1", "t").unwrap();
            let e = Envelope::text("c1", "alice", 1, "persisted");
            store.append(&e, false).unwrap();
            assert!(store.has_message(&e.msg_id).unwrap());
        }
        let store = Store::open(Some(path.to_str().unwrap())).unwrap();
        let msgs = store.list_messages("c1").unwrap();
        assert_eq!(msgs.len(), 1, "rows survive a reopen");
        assert_eq!(msgs[0].envelope.body, MsgBody::Text("persisted".into()));
        std::fs::remove_dir_all(&dir).ok();
    }
}
