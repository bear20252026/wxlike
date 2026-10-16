//! Offline store-and-forward: messages for users who are not connected are
//! parked and delivered when they say Hello.
//!
//! Two implementations of [`OfflineStore`]:
//! * [`MemoryOffline`] — per-user FIFO in RAM, capped; tests and single-node dev;
//! * [`SqliteOffline`] — the same FIFO on disk, so restarting a hub does not
//!   lose what was parked for someone who was away.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use rusqlite::{params, Connection};
use tokio::sync::Mutex as AsyncMutex;

/// Parked-message store. Semantics: per-user FIFO, capped, oldest dropped past
/// the cap.
#[async_trait]
pub trait OfflineStore: Send + Sync + 'static {
    async fn enqueue(&self, user: &str, frame: Vec<u8>) -> anyhow::Result<()>;
    /// Remove and return all parked frames for `user`, oldest first.
    async fn drain(&self, user: &str) -> anyhow::Result<Vec<Vec<u8>>>;
    /// How many frames are waiting (metrics, tests).
    async fn depth(&self, user: &str) -> anyhow::Result<usize>;
}

/// Per-user FIFO with a cap (oldest dropped beyond cap).
pub struct MemoryOffline {
    inner: AsyncMutex<HashMap<String, VecDeque<Vec<u8>>>>,
    cap_per_user: usize,
}

impl MemoryOffline {
    pub fn new(cap_per_user: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: AsyncMutex::new(HashMap::new()),
            cap_per_user,
        })
    }
}

#[async_trait]
impl OfflineStore for MemoryOffline {
    async fn enqueue(&self, user: &str, frame: Vec<u8>) -> anyhow::Result<()> {
        let mut m = self.inner.lock().await;
        let dq = m.entry(user.to_string()).or_default();
        while dq.len() >= self.cap_per_user {
            dq.pop_front(); // drop oldest
        }
        dq.push_back(frame);
        Ok(())
    }

    async fn drain(&self, user: &str) -> anyhow::Result<Vec<Vec<u8>>> {
        let mut m = self.inner.lock().await;
        Ok(m.remove(user)
            .map(|dq| dq.into_iter().collect())
            .unwrap_or_default())
    }

    async fn depth(&self, user: &str) -> anyhow::Result<usize> {
        Ok(self
            .inner
            .lock()
            .await
            .get(user)
            .map(|dq| dq.len())
            .unwrap_or_default())
    }
}

/// Durable parked inbox, one row per frame.
///
/// `drain` reads and deletes inside a single transaction, so a crash between
/// the two cannot both deliver and lose a frame; the cost is that a frame
/// delivered into a connection that dies before reading it is gone — the same
/// trade the in-memory store makes, and the reason `ReliableQueue` (WP-10)
/// exists for the cross-node hop.
pub struct SqliteOffline {
    conn: Mutex<Connection>,
    cap_per_user: usize,
}

impl SqliteOffline {
    /// `None` uses a temporary on-disk database; `Some(path)` is `WXLIKE_DB`.
    pub fn open(path: Option<&str>, cap_per_user: usize) -> anyhow::Result<Arc<Self>> {
        let conn = match path {
            Some(p) => {
                if let Some(dir) = std::path::Path::new(p).parent() {
                    if !dir.as_os_str().is_empty() {
                        std::fs::create_dir_all(dir)?;
                    }
                }
                Connection::open(p)?
            }
            None => Connection::open_in_memory()?,
        };
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA busy_timeout = 5000;
            CREATE TABLE IF NOT EXISTS offline_inbox (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                user         TEXT NOT NULL,
                frame        BLOB NOT NULL,
                enqueued_at  INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_offline_user ON offline_inbox(user, id);
            "#,
        )?;
        Ok(Arc::new(Self {
            conn: Mutex::new(conn),
            cap_per_user: cap_per_user.max(1),
        }))
    }

    fn c(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[async_trait]
impl OfflineStore for SqliteOffline {
    async fn enqueue(&self, user: &str, frame: Vec<u8>) -> anyhow::Result<()> {
        let c = self.c();
        c.execute(
            "INSERT INTO offline_inbox(user, frame, enqueued_at) VALUES(?1, ?2, ?3)",
            params![user, frame, wxlike_core::types::now_ms() as i64],
        )?;
        // trim to the newest `cap` rows for this user
        c.execute(
            "DELETE FROM offline_inbox WHERE user = ?1 AND id NOT IN
             (SELECT id FROM offline_inbox WHERE user = ?1 ORDER BY id DESC LIMIT ?2)",
            params![user, self.cap_per_user as i64],
        )?;
        Ok(())
    }

    async fn drain(&self, user: &str) -> anyhow::Result<Vec<Vec<u8>>> {
        let mut c = self.c();
        let tx = c.transaction()?;
        let mut stmt = tx.prepare("SELECT frame FROM offline_inbox WHERE user = ?1 ORDER BY id")?;
        let frames = stmt
            .query_map(params![user], |r| r.get::<_, Vec<u8>>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        tx.execute("DELETE FROM offline_inbox WHERE user = ?1", params![user])?;
        tx.commit()?;
        Ok(frames)
    }

    async fn depth(&self, user: &str) -> anyhow::Result<usize> {
        let n: i64 = self.c().query_row(
            "SELECT COUNT(*) FROM offline_inbox WHERE user = ?1",
            params![user],
            |r| r.get(0),
        )?;
        Ok(n.max(0) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn park_and_drain() {
        let store = MemoryOffline::new(3);
        store.enqueue("bob", b"m1".to_vec()).await.unwrap();
        store.enqueue("bob", b"m2".to_vec()).await.unwrap();
        assert_eq!(
            store.drain("bob").await.unwrap(),
            vec![b"m1".to_vec(), b"m2".to_vec()]
        );
        assert!(
            store.drain("bob").await.unwrap().is_empty(),
            "drain empties"
        );
    }

    #[tokio::test]
    async fn cap_drops_oldest() {
        let store = MemoryOffline::new(2);
        for m in ["a", "b", "c"] {
            store.enqueue("u", m.as_bytes().to_vec()).await.unwrap();
        }
        assert_eq!(
            store.drain("u").await.unwrap(),
            vec![b"b".to_vec(), b"c".to_vec()]
        );
    }

    #[tokio::test]
    async fn sqlite_persists_across_reopen() {
        let dir = std::env::temp_dir().join(format!("wxlike-offline-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("inbox.db");
        let p = path.to_str().unwrap().to_string();

        let first = SqliteOffline::open(Some(&p), 10).unwrap();
        first.enqueue("bob", b"parked".to_vec()).await.unwrap();
        assert_eq!(first.depth("bob").await.unwrap(), 1);
        drop(first);

        // a brand new connection to the same file still holds the frame
        let after = SqliteOffline::open(Some(&p), 10).unwrap();
        assert_eq!(after.depth("bob").await.unwrap(), 1);
        assert_eq!(after.drain("bob").await.unwrap(), vec![b"parked".to_vec()]);
        assert_eq!(after.depth("bob").await.unwrap(), 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn sqlite_cap_keeps_newest() {
        let store = SqliteOffline::open(None, 3).unwrap();
        for i in 0..5u8 {
            store.enqueue("u", vec![b'x', i]).await.unwrap();
        }
        let got = store.drain("u").await.unwrap();
        assert_eq!(got.len(), 3, "cap enforced");
        assert_eq!(got[0], vec![b'x', 2], "oldest dropped");
        assert_eq!(got[2], vec![b'x', 4], "newest kept");
        assert!(store.drain("u").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn sqlite_users_are_isolated() {
        let store = SqliteOffline::open(None, 10).unwrap();
        store.enqueue("alice", b"a".to_vec()).await.unwrap();
        store.enqueue("bob", b"b".to_vec()).await.unwrap();
        assert_eq!(store.drain("alice").await.unwrap(), vec![b"a".to_vec()]);
        assert_eq!(store.depth("bob").await.unwrap(), 1);
    }
}
