//! SQLite-backed storage for conversations and messages.

use crate::types::Envelope;
use anyhow::Context as _;
use rusqlite::Connection;

pub struct Store {
    conn: Connection,
}

pub struct StoredMessage {
    pub seq: i64,
    pub envelope: Envelope,
    pub sealed: bool,
}

#[derive(Clone, Debug)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub updated_at_ms: u64,
}

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
            "#,
        )?;
        Ok(Self { conn })
    }

    pub fn upsert_conversation(&self, id: &str, title: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO conversations(id, title, updated_at_ms) VALUES(?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET title=excluded.title",
            rusqlite::params![id, title, crate::types::now_ms()],
        )?;
        Ok(())
    }

    pub fn append_message(&self, env: &Envelope, sealed: bool) -> anyhow::Result<i64> {
        let body = bincode::serialize(&env.body)?;
        self.conn.execute(
            "INSERT INTO messages(conv_id, sender, kind, body, sent_at_ms, sealed)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                env.conv_id,
                env.sender,
                kind_tag(env.kind),
                body,
                env.sent_at_ms,
                sealed as i64
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn list_messages(&self, conv_id: &str) -> anyhow::Result<Vec<StoredMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, sender, kind, body, sent_at_ms, sealed
             FROM messages WHERE conv_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([conv_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Vec<u8>>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, sender, kind, body, sent_at_ms, sealed) = row?;
            out.append(&mut vec![StoredMessage {
                seq,
                envelope: Envelope {
                    version: 1,
                    conv_id: conv_id.to_string(),
                    sender,
                    to: vec![],
                    seq: seq as u64,
                    kind: kind_from_tag(kind)?,
                    body: bincode::deserialize(&body)?,
                    sent_at_ms: sent_at_ms as u64,
                },
                sealed: sealed != 0,
            }]);
        }
        Ok(out)
    }

    pub fn list_conversations(&self) -> anyhow::Result<Vec<Conversation>> {
        let mut stmt = self.conn.prepare(
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

fn kind_tag(k: crate::types::MsgKind) -> i64 {
    match k {
        crate::types::MsgKind::Text => 0,
        crate::types::MsgKind::Blob => 1,
        crate::types::MsgKind::System => 2,
    }
}

fn kind_from_tag(t: i64) -> anyhow::Result<crate::types::MsgKind> {
    Ok(match t {
        0 => crate::types::MsgKind::Text,
        1 => crate::types::MsgKind::Blob,
        2 => crate::types::MsgKind::System,
        _ => anyhow::bail!("unknown kind tag {t}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MsgBody;

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
            kind: crate::types::MsgKind::Text,
            body: MsgBody::Text("saved".into()),
            sent_at_ms: 123,
        };
        let seq = store.append_message(&e, true).unwrap();
        assert!(seq > 0);
        let msgs = store.list_messages("c1").unwrap();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].sealed);
        assert_eq!(msgs[0].envelope.body, MsgBody::Text("saved".into()));
        assert_eq!(store.list_conversations().unwrap().len(), 1);
    }
}
