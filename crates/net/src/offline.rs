//! Offline store-and-forward: messages for users who are not connected are
//! parked and delivered when they say Hello.
//!
//! Two implementations of [`OfflineStore`]:
//! * [`MemoryOffline`] — per-user FIFO in RAM, capped; tests and single-node dev;
//! * [`SqliteOffline`] — the same FIFO on disk, so restarting a hub does not
//!   lose what was parked for someone who was away.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};
use tokio::sync::Mutex as AsyncMutex;

/// 领取后多久没确认就重新可见（进程被杀 / 连接断在半路）。
///
/// 30 秒：够一次握手 + 发一批的时间，又不至于让"重连后要等很久才补发"。
///
/// rusqlite 0.41 起 `u64` 不再实现 `ToSql`，SQLite 的 INTEGER 就是有符号
/// 64 位。lease 时长是正数且远小于 `i64::MAX`，用 `i64` 与数据库列类型
/// 直接对齐，避免在每个绑定处 `as i64`。
pub const LEASE_MS: i64 = 30_000;

/// 全局离线队列预算。按**所有用户合计**计，不是每用户上限的简单叠加：
/// 每用户 5,000 条乘以任意多账号仍可把磁盘吃光。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OfflineLimits {
    /// 所有用户合计的最多行数。
    pub max_total_rows: usize,
    /// 所有用户合计的帧字节上限（不含 SQLite 页/索引开销）。
    pub max_total_bytes: usize,
}

impl Default for OfflineLimits {
    fn default() -> Self {
        Self {
            max_total_rows: 100_000,
            max_total_bytes: 256 * 1024 * 1024,
        }
    }
}

/// 预算不足时的统一拒绝信息；调用方据此让投递任务保持 pending。
fn budget_error(rows: usize, bytes: usize, limits: OfflineLimits) -> anyhow::Error {
    anyhow::anyhow!(
        "offline queue budget exceeded: rows {rows}/{} bytes {bytes}/{}",
        limits.max_total_rows,
        limits.max_total_bytes
    )
}

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
/// 一次领取得到的行。`claim_token` 是这次领取的 ownership proof；
/// ack/release 必须带同一个 token，旧 claimant 不能动新 claimant 的行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimedFrame {
    pub id: i64,
    pub claim_token: String,
    pub frame: Vec<u8>,
}

#[async_trait]
pub trait OfflineStore: Send + Sync + 'static {
    async fn enqueue(&self, user: &str, frame: Vec<u8>) -> anyhow::Result<()>;

    /// 领取最多 `limit` 条（**不删除**），最老的在前。
    ///
    /// 已被别人领取且未过期的条目不会被返回——同一用户的两次并发 Hello
    /// 不会重复发同一批。每次领取生成新的 `claim_token`；lease 过期后
    /// 新 claimant 拿到的是新 token，旧 token 的 ack/release 影响 0 行。
    async fn claim(&self, user: &str, limit: usize) -> anyhow::Result<Vec<ClaimedFrame>>;

    /// 投递成功 → 删掉这一条。`claim_token` 不匹配时什么都不做。
    async fn ack(&self, id: i64, claim_token: &str) -> anyhow::Result<()>;

    /// 投递失败 → 立刻重新可见（不等 lease 到期）。token 不匹配时不动。
    async fn release(&self, id: i64, claim_token: &str) -> anyhow::Result<()>;

    /// 便捷封装：领取全部并立刻 ack（"拿走就不管"的调用方，主要是测试）。
    ///
    /// **不要**在真实投递路径上用它——那正是 #10 的 bug。
    async fn drain(&self, user: &str) -> anyhow::Result<Vec<Vec<u8>>> {
        let batch = self.claim(user, usize::MAX).await?;
        let mut out = Vec::with_capacity(batch.len());
        for claimed in batch {
            self.ack(claimed.id, &claimed.claim_token).await?;
            out.push(claimed.frame);
        }
        Ok(out)
    }

    /// How many frames are waiting (metrics, tests).
    async fn depth(&self, user: &str) -> anyhow::Result<usize>;
}

/// 领取 token：每次 claim 一个，不按行复用。16 字节随机十六进制。
fn claim_token() -> String {
    use rand::RngCore as _;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// 队列里的一条：id 给 ack/release 用，`claimed_until` 是 lease 到期时间（ms）。
///
/// `i64`：SQLite 的 INTEGER 列就是有符号 64 位；rusqlite 0.41 不再给
/// `u64` 实现 `ToSql`，内存表示与磁盘表示统一成一种类型，比较和写入
/// 都不需要 `as` 转换。
struct MemEntry {
    id: i64,
    frame: Vec<u8>,
    claimed_until: i64,
    claim_token: String,
}

/// Per-user FIFO with a cap (oldest dropped beyond cap).
pub struct MemoryOffline {
    inner: AsyncMutex<HashMap<String, VecDeque<MemEntry>>>,
    cap_per_user: usize,
    limits: OfflineLimits,
    total_rows: AtomicUsize,
    total_bytes: AtomicUsize,
    next_id: std::sync::atomic::AtomicI64,
}

impl MemoryOffline {
    pub fn new(cap_per_user: usize) -> Arc<Self> {
        Self::with_limits(cap_per_user, OfflineLimits::default())
    }

    /// 测试与小规模部署可以收紧全局预算；生产走默认值。
    pub fn with_limits(cap_per_user: usize, limits: OfflineLimits) -> Arc<Self> {
        Arc::new(Self {
            inner: AsyncMutex::new(HashMap::new()),
            cap_per_user: cap_per_user.max(1),
            limits,
            total_rows: AtomicUsize::new(0),
            total_bytes: AtomicUsize::new(0),
            next_id: std::sync::atomic::AtomicI64::new(1),
        })
    }
}

#[async_trait]
impl OfflineStore for MemoryOffline {
    async fn enqueue(&self, user: &str, frame: Vec<u8>) -> anyhow::Result<()> {
        let mut m = self.inner.lock().await;
        let rows = self.total_rows.load(Ordering::Relaxed);
        let bytes = self.total_bytes.load(Ordering::Relaxed);
        if rows + 1 > self.limits.max_total_rows
            || bytes.saturating_add(frame.len()) > self.limits.max_total_bytes
        {
            return Err(budget_error(
                rows + 1,
                bytes.saturating_add(frame.len()),
                self.limits,
            ));
        }
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dq = m.entry(user.to_string()).or_default();
        let mut dropped_rows = 0usize;
        let mut dropped_bytes = 0usize;
        while dq.len() >= self.cap_per_user {
            if let Some(dropped) = dq.pop_front() {
                dropped_rows += 1;
                dropped_bytes = dropped_bytes.saturating_add(dropped.frame.len());
            }
        }
        let added_bytes = frame.len();
        dq.push_back(MemEntry {
            id,
            frame,
            claimed_until: 0,
            claim_token: String::new(),
        });
        self.total_rows.fetch_sub(dropped_rows, Ordering::Relaxed);
        self.total_bytes.fetch_sub(dropped_bytes, Ordering::Relaxed);
        self.total_rows.fetch_add(1, Ordering::Relaxed);
        self.total_bytes.fetch_add(added_bytes, Ordering::Relaxed);
        Ok(())
    }

    async fn claim(&self, user: &str, limit: usize) -> anyhow::Result<Vec<ClaimedFrame>> {
        let now = wxlike_core::types::now_ms() as i64;
        let token = claim_token();
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
                e.claim_token = token.clone();
                out.push(ClaimedFrame {
                    id: e.id,
                    claim_token: token.clone(),
                    frame: e.frame.clone(),
                });
            }
        }
        Ok(out)
    }

    async fn ack(&self, id: i64, claim_token: &str) -> anyhow::Result<()> {
        let mut m = self.inner.lock().await;
        for dq in m.values_mut() {
            if let Some(pos) = dq
                .iter()
                .position(|e| e.id == id && e.claim_token == claim_token)
            {
                let Some(removed) = dq.remove(pos) else {
                    break;
                };
                self.total_rows.fetch_sub(1, Ordering::Relaxed);
                self.total_bytes
                    .fetch_sub(removed.frame.len(), Ordering::Relaxed);
                break;
            }
        }
        Ok(())
    }

    async fn release(&self, id: i64, claim_token: &str) -> anyhow::Result<()> {
        let mut m = self.inner.lock().await;
        for dq in m.values_mut() {
            if let Some(e) = dq
                .iter_mut()
                .find(|e| e.id == id && e.claim_token == claim_token)
            {
                e.claimed_until = 0;
                e.claim_token.clear();
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
    limits: OfflineLimits,
    total_rows: AtomicUsize,
    total_bytes: AtomicUsize,
}

impl SqliteOffline {
    /// `None` uses a temporary on-disk database; `Some(path)` is `WXLIKE_DB`.
    pub fn open(path: Option<&str>, cap_per_user: usize) -> anyhow::Result<Arc<Self>> {
        Self::open_with_limits(path, cap_per_user, OfflineLimits::default())
    }

    /// 打开并应用全局预算；已有库也会按实际行数初始化计数。
    pub fn open_with_limits(
        path: Option<&str>,
        cap_per_user: usize,
        limits: OfflineLimits,
    ) -> anyhow::Result<Arc<Self>> {
        let conn = open_connection(path)?;
        init_schema(&conn)?;
        migrate_claimed_until(&conn)?;
        let (rows, bytes): (i64, i64) = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(frame)), 0) FROM offline_inbox",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(Arc::new(Self {
            conn: Mutex::new(conn),
            cap_per_user: cap_per_user.max(1),
            limits,
            total_rows: AtomicUsize::new(usize::try_from(rows).unwrap_or(usize::MAX)),
            total_bytes: AtomicUsize::new(usize::try_from(bytes).unwrap_or(usize::MAX)),
        }))
    }

    fn c(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// 打开配置的库；路径带父目录时先建目录。
fn open_connection(path: Option<&str>) -> anyhow::Result<Connection> {
    let Some(path) = path else {
        return Ok(Connection::open_in_memory()?);
    };
    if let Some(dir) = std::path::Path::new(path).parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    Ok(Connection::open(path)?)
}

fn init_schema(conn: &Connection) -> anyhow::Result<()> {
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
                claimed_until INTEGER NOT NULL DEFAULT 0,
                -- 本次领取的 ownership proof；ack/release 必须匹配，
                -- 防止 lease 过期后的旧 claimant 动新 claimant 的行。
                claim_token  TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS idx_offline_user ON offline_inbox(user, id);
            "#,
    )?;
    Ok(())
}

/// 老库迁移：`CREATE TABLE IF NOT EXISTS` 不会给已有表加列。
fn migrate_claimed_until(conn: &Connection) -> anyhow::Result<()> {
    migrate_column(
        conn,
        "claimed_until",
        "ALTER TABLE offline_inbox ADD COLUMN claimed_until INTEGER NOT NULL DEFAULT 0",
    )?;
    migrate_column(
        conn,
        "claim_token",
        "ALTER TABLE offline_inbox ADD COLUMN claim_token TEXT NOT NULL DEFAULT ''",
    )?;
    Ok(())
}

fn migrate_column(conn: &Connection, name: &str, sql: &str) -> anyhow::Result<()> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('offline_inbox') WHERE name = ?1",
        params![name],
        |r| r.get(0),
    )?;
    if exists == 0 {
        conn.execute(sql, [])?;
    }
    Ok(())
}

#[async_trait]
impl OfflineStore for SqliteOffline {
    async fn enqueue(&self, user: &str, frame: Vec<u8>) -> anyhow::Result<()> {
        let c = self.c();
        let rows = self.total_rows.load(Ordering::Relaxed);
        let bytes = self.total_bytes.load(Ordering::Relaxed);
        if rows + 1 > self.limits.max_total_rows
            || bytes.saturating_add(frame.len()) > self.limits.max_total_bytes
        {
            return Err(budget_error(
                rows + 1,
                bytes.saturating_add(frame.len()),
                self.limits,
            ));
        }
        let added_bytes = frame.len();
        c.execute(
            "INSERT INTO offline_inbox(user, frame, enqueued_at) VALUES(?1, ?2, ?3)",
            params![user, frame, wxlike_core::types::now_ms() as i64],
        )?;
        // trim to the newest `cap` rows for this user
        let mut stmt = c.prepare(
            "DELETE FROM offline_inbox WHERE user = ?1 AND id NOT IN
             (SELECT id FROM offline_inbox WHERE user = ?1 ORDER BY id DESC LIMIT ?2)
             RETURNING LENGTH(frame)",
        )?;
        let removed: Vec<i64> = stmt
            .query_map(params![user, self.cap_per_user as i64], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let dropped_rows = removed.len();
        let dropped_bytes: usize = removed
            .into_iter()
            .map(|n| usize::try_from(n).unwrap_or(0))
            .sum();
        self.total_rows.fetch_sub(dropped_rows, Ordering::Relaxed);
        self.total_bytes.fetch_sub(dropped_bytes, Ordering::Relaxed);
        self.total_rows.fetch_add(1, Ordering::Relaxed);
        self.total_bytes.fetch_add(added_bytes, Ordering::Relaxed);
        Ok(())
    }

    async fn claim(&self, user: &str, limit: usize) -> anyhow::Result<Vec<ClaimedFrame>> {
        let now = wxlike_core::types::now_ms() as i64;
        // `usize::MAX`（drain 的便捷封装会传）不能直接塞进 SQLite 的 LIMIT
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let token = claim_token();
        let c = self.c();
        let mut stmt = c.prepare(
            r#"
            UPDATE offline_inbox SET claimed_until = ?2, claim_token = ?3
            WHERE id IN (
                SELECT id FROM offline_inbox
                WHERE user = ?1 AND claimed_until <= ?4
                ORDER BY id LIMIT ?5
            )
            RETURNING id, frame
            "#,
        )?;
        let rows = stmt
            .query_map(params![user, now + LEASE_MS, token, now, limit], |r| {
                Ok(ClaimedFrame {
                    id: r.get(0)?,
                    claim_token: token.clone(),
                    frame: r.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    async fn ack(&self, id: i64, claim_token: &str) -> anyhow::Result<()> {
        let c = self.c();
        let removed: Option<i64> = c
            .query_row(
                "DELETE FROM offline_inbox WHERE id = ?1 AND claim_token = ?2
                 RETURNING LENGTH(frame)",
                params![id, claim_token],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(bytes) = removed {
            self.total_rows.fetch_sub(1, Ordering::Relaxed);
            self.total_bytes
                .fetch_sub(usize::try_from(bytes).unwrap_or(0), Ordering::Relaxed);
        }
        Ok(())
    }

    async fn release(&self, id: i64, claim_token: &str) -> anyhow::Result<()> {
        self.c().execute(
            "UPDATE offline_inbox SET claimed_until = 0, claim_token = '' \
             WHERE id = ?1 AND claim_token = ?2",
            params![id, claim_token],
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
        assert_ne!(second[0].id, first[0].id, "第一条还在 lease 里");

        // ack 掉第一条 → depth 减一
        store.ack(first[0].id, &first[0].claim_token).await.unwrap();
        assert_eq!(store.depth("bob").await.unwrap(), 1);

        // release 第二条 → 立刻重新可见（不用等 30 秒）
        store
            .release(second[0].id, &second[0].claim_token)
            .await
            .unwrap();
        let third = store.claim("bob", 10).await.unwrap();
        assert_eq!(third.len(), 1, "release 之后应当立刻能再领");
        assert_eq!(third[0].id, second[0].id);
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
            !claimed.iter().any(|c| c.id == third[0].id),
            "已经在 lease 里的条目不该被重复领取"
        );

        store
            .ack(claimed[0].id, &claimed[0].claim_token)
            .await
            .unwrap();
        store
            .release(claimed[1].id, &claimed[1].claim_token)
            .await
            .unwrap();
        assert_eq!(
            store.depth("bob").await.unwrap(),
            2,
            "ack 删一条，release 不删"
        );

        // release 的那条立刻重新可见；第三条仍在 lease 里
        let again = store.claim("bob", 10).await.unwrap();
        assert_eq!(again.len(), 1, "只有 release 的那条重新可见");
        assert_eq!(again[0].id, claimed[1].id);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// H-31：lease 过期后新 claimant 拿到新 token；旧 claimant 的 ack/release
    /// 必须影响 0 行，不能删掉/释放别人刚领到的行。
    #[tokio::test]
    async fn stale_claimant_cannot_ack_or_release_the_reclaimed_row() {
        let store = SqliteOffline::open(None, 10).unwrap();
        store.enqueue("bob", b"m1".to_vec()).await.unwrap();

        let old = store.claim("bob", 1).await.unwrap();
        assert_eq!(old.len(), 1);
        // 模拟 30s lease 到期：直接把这行的 lease 归零，不睡 30 秒。
        store
            .c()
            .execute(
                "UPDATE offline_inbox SET claimed_until = 0 WHERE id = ?1",
                params![old[0].id],
            )
            .unwrap();

        let fresh = store.claim("bob", 1).await.unwrap();
        assert_eq!(fresh.len(), 1, "过期行必须能被新 claimant 领到");
        assert_eq!(fresh[0].id, old[0].id);
        assert_ne!(fresh[0].claim_token, old[0].claim_token);

        // 旧 claimant 的 ack/release 都是 no-op
        store.ack(old[0].id, &old[0].claim_token).await.unwrap();
        store.release(old[0].id, &old[0].claim_token).await.unwrap();
        assert_eq!(store.depth("bob").await.unwrap(), 1);
        let still_leased = store.claim("bob", 1).await.unwrap();
        assert!(
            still_leased.is_empty(),
            "旧 token 的 release 不能把新 lease 提前放出来"
        );

        // 新 claimant 才能真正退休这一行
        store.ack(fresh[0].id, &fresh[0].claim_token).await.unwrap();
        assert_eq!(store.depth("bob").await.unwrap(), 0);
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

    #[tokio::test]
    async fn memory_global_row_budget_rejects_and_recovers() {
        let store = MemoryOffline::with_limits(
            10,
            OfflineLimits {
                max_total_rows: 2,
                max_total_bytes: usize::MAX,
            },
        );
        store.enqueue("a", b"1".to_vec()).await.unwrap();
        store.enqueue("b", b"2".to_vec()).await.unwrap();
        let err = store
            .enqueue("c", b"3".to_vec())
            .await
            .expect_err("third row exceeds the global budget");
        assert!(err.to_string().contains("budget exceeded"), "{err}");
        assert_eq!(
            store.depth("c").await.unwrap(),
            0,
            "rejected row is not parked"
        );

        // 消费掉已有行后预算释放，新行可以重新入队
        assert_eq!(store.drain("a").await.unwrap(), vec![b"1".to_vec()]);
        store.enqueue("c", b"3".to_vec()).await.unwrap();
        assert_eq!(store.depth("c").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn memory_global_byte_budget_counts_every_user() {
        let store = MemoryOffline::with_limits(
            10,
            OfflineLimits {
                max_total_rows: 10,
                max_total_bytes: 4,
            },
        );
        store.enqueue("a", b"aa".to_vec()).await.unwrap();
        store.enqueue("b", b"bb".to_vec()).await.unwrap();
        let err = store
            .enqueue("c", b"cc".to_vec())
            .await
            .expect_err("fifth byte exceeds the global budget");
        assert!(err.to_string().contains("bytes 6/4"), "{err}");
    }

    #[tokio::test]
    async fn sqlite_global_budget_is_enforced_across_users() {
        let store = SqliteOffline::open_with_limits(
            None,
            10,
            OfflineLimits {
                max_total_rows: 2,
                max_total_bytes: 4,
            },
        )
        .unwrap();
        store.enqueue("a", b"aa".to_vec()).await.unwrap();
        store.enqueue("b", b"bb".to_vec()).await.unwrap();
        let err = store
            .enqueue("c", b"cc".to_vec())
            .await
            .expect_err("global caps apply across users");
        assert!(err.to_string().contains("budget exceeded"), "{err}");
        assert_eq!(store.depth("c").await.unwrap(), 0);

        store.drain("a").await.unwrap();
        store.enqueue("c", b"cc".to_vec()).await.unwrap();
        assert_eq!(store.depth("c").await.unwrap(), 1);
    }
}
