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

/// 领取后多久没确认就重新可见（进程被杀 / 连接断在半路）。
///
/// 30 秒：够一次握手 + 发一批的时间，又不至于让"重连后要等很久才补发"。
pub const LEASE_MS: u64 = 30_000;

/// Parked-message store. Semantics: per-user FIFO, capped, oldest dropped past
/// the cap.
///
/// # 为什么是 claim/ack 而不是 drain（issue #10）
///
/// `drain` 在事务里"读全部 + 删全部"，然后才逐条发。发到一半连接断了，
/// 剩下的**已经不在表里**——事务保证的是读删原子，不是"发送与接收原子"。
/// 排队 100 条、只发出 1 条就断连，另外 99 条就永久丢了（源端归档还能靠
/// 拉历史找回，但"实时投递"这条承诺破了）。
///
/// 现在分成三步：`claim`（标记领取，不删）→ 逐条 `ack`（删）→ 失败 `release`
/// （立刻重新可见）。没 ack 的条目在 [`LEASE_MS`] 之后也会自己回来。
#[async_trait]
pub trait OfflineStore: Send + Sync + 'static {
    async fn enqueue(&self, user: &str, frame: Vec<u8>) -> anyhow::Result<()>;

    /// 领取最多 `limit` 条（**不删除**），返回 `(id, frame)`，最老的在前。
    ///
    /// 已被别人领取且未过期的条目不会被返回——同一用户的两次并发 Hello
    /// 不会重复发同一批。
    async fn claim(&self, user: &str, limit: usize) -> anyhow::Result<Vec<(i64, Vec<u8>)>>;

    /// 投递成功 → 删掉这一条。
    async fn ack(&self, id: i64) -> anyhow::Result<()>;

    /// 投递失败 → 立刻重新可见（不等 lease 到期）。
    async fn release(&self, id: i64) -> anyhow::Result<()>;

    /// 便捷封装：领取全部并立刻 ack（"拿走就不管"的调用方，主要是测试）。
    ///
    /// **不要**在真实投递路径上用它——那正是 #10 的 bug。
    async fn drain(&self, user: &str) -> anyhow::Result<Vec<Vec<u8>>> {
        let batch = self.claim(user, usize::MAX).await?;
        let mut out = Vec::with_capacity(batch.len());
        for (id, frame) in batch {
            self.ack(id).await?;
            out.push(frame);
        }
        Ok(out)
    }

    /// How many frames are waiting (metrics, tests).
    async fn depth(&self, user: &str) -> anyhow::Result<usize>;
}

/// 队列里的一条：id 给 ack/release 用，`claimed_until` 是 lease 到期时间（ms）。
struct MemEntry {
    id: i64,
    frame: Vec<u8>,
    claimed_until: u64,
}

/// Per-user FIFO with a cap (oldest dropped beyond cap).
pub struct MemoryOffline {
    inner: AsyncMutex<HashMap<String, VecDeque<MemEntry>>>,
    cap_per_user: usize,
    next_id: std::sync::atomic::AtomicI64,
}

impl MemoryOffline {
    pub fn new(cap_per_user: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: AsyncMutex::new(HashMap::new()),
            cap_per_user,
            next_id: std::sync::atomic::AtomicI64::new(1),
        })
    }
}

#[async_trait]
impl OfflineStore for MemoryOffline {
    async fn enqueue(&self, user: &str, frame: Vec<u8>) -> anyhow::Result<()> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut m = self.inner.lock().await;
        let dq = m.entry(user.to_string()).or_default();
        while dq.len() >= self.cap_per_user {
            dq.pop_front(); // drop oldest
        }
        dq.push_back(MemEntry {
            id,
            frame,
            claimed_until: 0,
        });
        Ok(())
    }

    async fn claim(&self, user: &str, limit: usize) -> anyhow::Result<Vec<(i64, Vec<u8>)>> {
        let now = wxlike_core::types::now_ms();
        let mut m = self.inner.lock().await;
        let Some(dq) = m.get_mut(user) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for e in dq.iter_mut() {
            if out.len() >= limit {
                break;
            }
            if e.claimed_until <= now {
                e.claimed_until = now + LEASE_MS;
                out.push((e.id, e.frame.clone()));
            }
        }
        Ok(out)
    }

    async fn ack(&self, id: i64) -> anyhow::Result<()> {
        let mut m = self.inner.lock().await;
        for dq in m.values_mut() {
            if let Some(pos) = dq.iter().position(|e| e.id == id) {
                dq.remove(pos);
                break;
            }
        }
        Ok(())
    }

    async fn release(&self, id: i64) -> anyhow::Result<()> {
        let mut m = self.inner.lock().await;
        for dq in m.values_mut() {
            if let Some(e) = dq.iter_mut().find(|e| e.id == id) {
                e.claimed_until = 0;
                break;
            }
        }
        Ok(())
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
/// 投递用 `claim`（标记 `claimed_until`，不删）→ `ack`（删）/ `release`（立刻可见）。
/// 以前是"事务里读全部 + 删全部再发"，发一半断连就把剩下的丢了（issue #10）。
/// 未 ack 的条目在 [`LEASE_MS`] 后自动重新可见，所以进程被杀也不会吞消息。
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
                enqueued_at  INTEGER NOT NULL,
                -- 0 = 未领取；否则是 lease 到期时间（ms）。老库靠下面的
                -- ALTER 补这一列，不能只改 CREATE TABLE（已存在的表不会重建）。
                claimed_until INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_offline_user ON offline_inbox(user, id);
            "#,
        )?;
        // 老库迁移：`CREATE TABLE IF NOT EXISTS` 不会给已有表加列。
        let has_claimed: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('offline_inbox') WHERE name = 'claimed_until'",
            [],
            |r| r.get(0),
        )?;
        if has_claimed == 0 {
            conn.execute(
                "ALTER TABLE offline_inbox ADD COLUMN claimed_until INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
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

    async fn claim(&self, user: &str, limit: usize) -> anyhow::Result<Vec<(i64, Vec<u8>)>> {
        let now = wxlike_core::types::now_ms();
        // `usize::MAX`（drain 的便捷封装会传）不能直接塞进 SQLite 的 LIMIT
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let c = self.c();
        let mut stmt = c.prepare(
            r#"
            UPDATE offline_inbox SET claimed_until = ?2
            WHERE id IN (
                SELECT id FROM offline_inbox
                WHERE user = ?1 AND claimed_until <= ?3
                ORDER BY id LIMIT ?4
            )
            RETURNING id, frame
            "#,
        )?;
        let rows = stmt
            .query_map(params![user, now + LEASE_MS, now, limit], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    async fn ack(&self, id: i64) -> anyhow::Result<()> {
        self.c()
            .execute("DELETE FROM offline_inbox WHERE id = ?1", params![id])?;
        Ok(())
    }

    async fn release(&self, id: i64) -> anyhow::Result<()> {
        self.c().execute(
            "UPDATE offline_inbox SET claimed_until = 0 WHERE id = ?1",
            params![id],
        )?;
        Ok(())
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

    /// issue #10：**claim 不删除，ack 才删除**。
    ///
    /// 旧 `drain` 是"读全部 + 删全部再发"，发一半断连就丢。这里钉住新语义：
    /// 领取后 depth 不变；lease 未过期时不会被重复领取；release 后立刻可见。
    #[tokio::test]
    async fn claim_does_not_delete_until_ack() {
        let store = MemoryOffline::new(10);
        store.enqueue("bob", b"m1".to_vec()).await.unwrap();
        store.enqueue("bob", b"m2".to_vec()).await.unwrap();

        let first = store.claim("bob", 1).await.unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(
            store.depth("bob").await.unwrap(),
            2,
            "领取之后不能从队列里消失（否则断连就丢了）"
        );

        // lease 未过期：同一条不会被再次领取
        let second = store.claim("bob", 10).await.unwrap();
        assert_eq!(second.len(), 1, "只剩第二条可领");
        assert_ne!(second[0].0, first[0].0, "第一条还在 lease 里");

        // ack 掉第一条 → depth 减一
        store.ack(first[0].0).await.unwrap();
        assert_eq!(store.depth("bob").await.unwrap(), 1);

        // release 第二条 → 立刻重新可见（不用等 30 秒）
        store.release(second[0].0).await.unwrap();
        let third = store.claim("bob", 10).await.unwrap();
        assert_eq!(third.len(), 1, "release 之后应当立刻能再领");
        assert_eq!(third[0].0, second[0].0);
    }

    /// 同一个语义在 SQLite 上也要成立（生产用的是它）。
    #[tokio::test]
    async fn sqlite_claim_and_release_round_trip() {
        let dir = std::env::temp_dir().join(format!(
            "wxlike-offline-lease-{}-{}",
            std::process::id(),
            wxlike_core::types::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("inbox.db");
        let store = SqliteOffline::open(path.to_str(), 10).unwrap();

        for m in ["a", "b", "c"] {
            store.enqueue("bob", m.as_bytes().to_vec()).await.unwrap();
        }
        let claimed = store.claim("bob", 2).await.unwrap();
        assert_eq!(claimed.len(), 2);
        assert_eq!(store.depth("bob").await.unwrap(), 3, "claim 不删行");

        // 没 ack 的两条在 lease 里，只剩第三条可领
        let third = store.claim("bob", 10).await.unwrap();
        assert_eq!(third.len(), 1, "只剩第三条（前两条还在 lease 里）");
        assert!(
            !claimed.iter().any(|(id, _)| *id == third[0].0),
            "已经在 lease 里的条目不该被重复领取"
        );

        store.ack(claimed[0].0).await.unwrap();
        store.release(claimed[1].0).await.unwrap();
        assert_eq!(
            store.depth("bob").await.unwrap(),
            2,
            "ack 删一条，release 不删"
        );

        // release 的那条立刻重新可见；第三条仍在 lease 里
        let again = store.claim("bob", 10).await.unwrap();
        assert_eq!(again.len(), 1, "只有 release 的那条重新可见");
        assert_eq!(again[0].0, claimed[1].0);

        let _ = std::fs::remove_dir_all(&dir);
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
