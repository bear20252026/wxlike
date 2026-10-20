//! 客户端发送队列（outbox）：消息发不丢。
//!
//! 现状的洞：`send_body` 一旦 `client.send()` 失败就直接返回错误，什么都没留下。
//! 用户在地铁里发一条消息、进程被系统杀掉，那条消息就消失了——没有"待发送"这个概念。
//!
//! 参照 `refs/mars/mars/stn/src/zombie_task_manager.{h,cc}`（腾讯 Mars，MIT）：
//! 它把任务 `SaveTask/StopTask/HasTask` 落盘，进程重启后重新投递，命名叫
//! "僵尸任务"。我们借它的**形状**（持久化 + 成本计时 + 重启续跑），
//! 但重试策略要偏离它一处：
//!
//! > Mars 的任务级重试只有 1 次（`DEF_TASK_RETRY_COUNT`），因为它还有
//! > `RetryTasks`——连接恢复时把所有在飞任务一起重投。我们还没有那个机制，
//! > 所以队列自己扛：最多 5 次、指数退避 1s→30s。
//!
//! 幂等靠 `task_id`：同一个任务重试时 `task_id` 不变，服务端用它派生 `msg_id`
//! （`POST /api/conv/{c}/send` 的 `idempotency_key`），重试不会变成两条消息。

use std::sync::{Mutex, PoisonError};

use anyhow::Context as _;
use rusqlite::{params, Connection, OptionalExtension};

/// 最多尝试几次（含首次）。超过就进入 `failed`，等用户点重试。
pub const MAX_ATTEMPTS: u32 = 5;
/// 首次重试延迟。与 Mars 的 `DEF_TASK_RETRY_INTERNAL`（1s）一致。
pub const BASE_DELAY_MS: u64 = 1_000;
/// 退避上限。再长用户就该看到"发送失败"了，而不是无限转圈。
pub const MAX_DELAY_MS: u64 = 30_000;

/// 一条待发送的消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxTask {
    pub task_id: String,
    pub conv_id: String,
    /// 业务类型：`text` / `image` / `redpacket` …（由调用方定义，队列不解释）
    pub kind: String,
    pub payload: serde_json::Value,
    pub attempts: u32,
    /// 毫秒时间戳；`due()` 只取 `<= now` 的行
    pub next_retry_at: i64,
    pub last_error: Option<String>,
}

/// 发送失败的原因，决定"重试"还是"放弃"。
///
/// 这个区分很关键：403（不是群成员）重试 5 次也不会成功，只会让用户等 30 秒
/// 才看到失败；而连接断开值得重试。把判断交给**知道错误语义的那一层**（调用方），
/// 队列只负责执行。
#[derive(Debug, Clone)]
pub struct SendFailure {
    pub message: String,
    pub retryable: bool,
    /// 被本地漏斗挡下：**不计入尝试次数**。
    ///
    /// 这一条很关键：限流不是"这条消息发不出去"，是"现在别发"。把它算成失败，
    /// 用户会看到"发送失败"——而实际上再等一秒就好了，而且重试 5 次就把额度烧光、
    /// 消息直接进 failed 状态。
    pub throttled: bool,
}

impl SendFailure {
    pub fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
            throttled: false,
        }
    }

    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
            throttled: false,
        }
    }

    /// 本地限流：这一轮到此为止，**不动 attempts**。
    pub fn throttled(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
            throttled: true,
        }
    }
}

/// 一轮 flush 的结果。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FlushReport {
    pub sent: u32,
    /// 失败但会重试
    pub retrying: u32,
    /// 失败且不再重试（非重试错误，或次数用尽）
    pub failed: u32,
    /// 被本地漏斗挡下（不计尝试次数，下一轮再来）
    pub throttled: u32,
}

pub struct Outbox {
    conn: Mutex<Connection>,
}

impl Outbox {
    /// `None` = 内存库（测试用）。文件库与 `core::Store` 同路径，
    /// 各自一条连接：WAL + busy_timeout 下并发写是安全的。
    pub fn open(path: Option<&str>) -> anyhow::Result<Self> {
        let conn = match path {
            Some(p) => Connection::open(p).with_context(|| format!("open outbox db {p}"))?,
            None => Connection::open_in_memory()?,
        };
        // 与 LocalStore / MessageStore 共用同一个文件时，写锁竞争靠它等
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS outbox (
                task_id       TEXT PRIMARY KEY,
                conv_id       TEXT NOT NULL,
                kind          TEXT NOT NULL,
                payload       TEXT NOT NULL,
                attempts      INTEGER NOT NULL DEFAULT 0,
                next_retry_at INTEGER NOT NULL,
                created_at    INTEGER NOT NULL,
                last_error    TEXT,
                -- pending: 等待发送；sent: 已送达；failed: 不再自动重试
                state         TEXT NOT NULL
            );
            -- due() 的热路径：按状态筛 + 按时间排序
            CREATE INDEX IF NOT EXISTS outbox_ready ON outbox(state, next_retry_at);
            "#,
        )
        .context("outbox schema")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn c(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 入队，返回 `task_id`。**调用方要把这个 id 当作幂等键**：
    /// 重试用同一个 id，服务端就不会产生第二条消息。
    pub fn enqueue(
        &self,
        conv_id: &str,
        kind: &str,
        payload: &serde_json::Value,
    ) -> anyhow::Result<String> {
        let task_id = new_task_id();
        self.enqueue_with_id(&task_id, conv_id, kind, payload, wxlike_core_now())?;
        Ok(task_id)
    }

    /// 指定 id 入队（重试/重放时用）。已存在则**忽略**——同一条消息不该入队两次。
    pub fn enqueue_with_id(
        &self,
        task_id: &str,
        conv_id: &str,
        kind: &str,
        payload: &serde_json::Value,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        self.c().execute(
            "INSERT INTO outbox(task_id, conv_id, kind, payload, attempts, next_retry_at, created_at, state)
             VALUES(?1, ?2, ?3, ?4, 0, ?5, ?5, 'pending')
             ON CONFLICT(task_id) DO NOTHING",
            params![
                task_id,
                conv_id,
                kind,
                serde_json::to_string(payload).context("serialize payload")?,
                now_ms
            ],
        )?;
        Ok(())
    }

    /// 到点该发的任务，按"最早该发"优先。
    pub fn due(&self, now_ms: i64, limit: u32) -> anyhow::Result<Vec<OutboxTask>> {
        let c = self.c();
        let mut stmt = c.prepare(
            "SELECT task_id, conv_id, kind, payload, attempts, next_retry_at, last_error
             FROM outbox
             WHERE state = 'pending' AND next_retry_at <= ?1
             ORDER BY next_retry_at, created_at
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![now_ms, limit], row_to_task)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn mark_sent(&self, task_id: &str) -> anyhow::Result<()> {
        self.c().execute(
            "UPDATE outbox SET state = 'sent', last_error = NULL WHERE task_id = ?1",
            params![task_id],
        )?;
        Ok(())
    }

    /// 记一次失败。返回下一次重试的时间；`None` = 不再重试（已标记 failed）。
    pub fn record_failure(
        &self,
        task_id: &str,
        error: &str,
        now_ms: i64,
    ) -> anyhow::Result<Option<i64>> {
        let c = self.c();
        let attempts: Option<u32> = c
            .query_row(
                "SELECT attempts FROM outbox WHERE task_id = ?1",
                params![task_id],
                |r| r.get::<_, i64>(0).map(|v| v as u32),
            )
            .optional()?;
        let Some(attempts) = attempts else {
            return Ok(None);
        };
        let attempts = attempts + 1;
        if attempts >= MAX_ATTEMPTS {
            c.execute(
                "UPDATE outbox SET state = 'failed', attempts = ?2, last_error = ?3
                 WHERE task_id = ?1",
                params![task_id, attempts, error],
            )?;
            return Ok(None);
        }
        let next = now_ms + backoff_ms(attempts) as i64;
        c.execute(
            "UPDATE outbox SET attempts = ?2, next_retry_at = ?3, last_error = ?4
             WHERE task_id = ?1",
            params![task_id, attempts, next, error],
        )?;
        Ok(Some(next))
    }

    /// 直接判死（非重试错误：403、格式错误…）。重试只会浪费用户的时间。
    pub fn mark_failed(&self, task_id: &str, error: &str) -> anyhow::Result<()> {
        self.c().execute(
            "UPDATE outbox SET state = 'failed', last_error = ?2 WHERE task_id = ?1",
            params![task_id, error],
        )?;
        Ok(())
    }

    /// 用户点"重试"：清零计数、立刻可发。
    pub fn requeue(&self, task_id: &str, now_ms: i64) -> anyhow::Result<()> {
        self.c().execute(
            "UPDATE outbox SET state = 'pending', attempts = 0, next_retry_at = ?2
             WHERE task_id = ?1",
            params![task_id, now_ms],
        )?;
        Ok(())
    }

    /// 彻底删掉一条（用户点"删除"放弃它）。
    pub fn remove(&self, task_id: &str) -> anyhow::Result<bool> {
        Ok(self
            .c()
            .execute("DELETE FROM outbox WHERE task_id = ?1", params![task_id])?
            > 0)
    }

    pub fn pending_count(&self) -> anyhow::Result<u32> {
        Ok(self.c().query_row(
            "SELECT COUNT(*) FROM outbox WHERE state = 'pending'",
            [],
            |r| r.get::<_, i64>(0),
        )? as u32)
    }

    /// 失败的任务（UI 显示"发送失败，点击重试"）。
    pub fn failed(&self, limit: u32) -> anyhow::Result<Vec<OutboxTask>> {
        let c = self.c();
        let mut stmt = c.prepare(
            "SELECT task_id, conv_id, kind, payload, attempts, next_retry_at, last_error
             FROM outbox WHERE state = 'failed'
             ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], row_to_task)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 清掉已送达的老行。
    ///
    /// 按 `created_at` 而不是"送达时间"：队列不是日志——消息正文与状态在
    /// 本地消息库里有，这里只负责"还没发出去的"。所以送达即可删，不必留证据。
    pub fn purge_sent(&self, created_before_ms: i64) -> anyhow::Result<usize> {
        Ok(self.c().execute(
            "DELETE FROM outbox WHERE state = 'sent' AND created_at <= ?1",
            params![created_before_ms],
        )?)
    }

    /// 跑一轮：取到点的任务，逐条交给 `sender`。
    ///
    /// **遇到可重试的失败就停**，不再继续往下发。理由：这种失败通常意味着
    /// 连接已经不可用（`send` 报错、超时），继续发只会让每条都失败一遍、
    /// 各自 +1 次尝试——这就是 Mars `anti_avalanche` 要挡的东西，只是它按频率
    /// 算，我们按"同一轮内不重复撞墙"算。
    pub fn flush_with<F>(
        &self,
        now_ms: i64,
        limit: u32,
        mut sender: F,
    ) -> anyhow::Result<FlushReport>
    where
        F: FnMut(&OutboxTask) -> Result<(), SendFailure>,
    {
        let mut report = FlushReport::default();
        for task in self.due(now_ms, limit)? {
            match sender(&task) {
                Ok(()) => {
                    self.mark_sent(&task.task_id)?;
                    report.sent += 1;
                }
                // 本地限流：不记账、这一轮结束。连"失败"都不算——
                // 否则用户会看到"发送失败"，而实际只是等一下就好。
                Err(fail) if fail.throttled => {
                    report.throttled += 1;
                    break;
                }
                Err(fail) if fail.retryable => {
                    // 连接层大概率已经坏了：剩下的任务这一轮不再试
                    let next = self.record_failure(&task.task_id, &fail.message, now_ms)?;
                    if next.is_some() {
                        report.retrying += 1;
                    } else {
                        report.failed += 1;
                    }
                    break;
                }
                Err(fail) => {
                    self.mark_failed(&task.task_id, &fail.message)?;
                    report.failed += 1;
                }
            }
        }
        Ok(report)
    }
}

/// 指数退避：1s、2s、4s、8s、16s，上限 30s。
///
/// `attempts` 是**已经失败的次数**（首次失败传 1）。
pub fn backoff_ms(attempts: u32) -> u64 {
    let shift = attempts.saturating_sub(1).min(6);
    (BASE_DELAY_MS << shift).min(MAX_DELAY_MS)
}

fn row_to_task(r: &rusqlite::Row<'_>) -> rusqlite::Result<OutboxTask> {
    let payload: String = r.get(3)?;
    Ok(OutboxTask {
        task_id: r.get(0)?,
        conv_id: r.get(1)?,
        kind: r.get(2)?,
        // 坏 JSON 不该让整个队列卡死：退化成字符串，发送时会失败并被判死
        payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::String(payload)),
        attempts: r.get::<_, i64>(4)? as u32,
        next_retry_at: r.get(5)?,
        last_error: r.get(6)?,
    })
}

fn new_task_id() -> String {
    use rand::RngCore as _;
    let mut id = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut id);
    hex::encode(id)
}

fn wxlike_core_now() -> i64 {
    crate::types::now_ms() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now() -> i64 {
        wxlike_core_now()
    }

    #[test]
    fn enqueue_is_due_immediately() {
        let ob = Outbox::open(None).unwrap();
        let id = ob.enqueue("d-a~b", "text", &json!({"text": "hi"})).unwrap();
        assert_eq!(ob.pending_count().unwrap(), 1);

        let due = ob.due(now(), 10).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].task_id, id);
        assert_eq!(due[0].payload["text"], "hi");
        assert_eq!(due[0].attempts, 0);
    }

    #[test]
    fn backoff_doubles_then_caps() {
        assert_eq!(backoff_ms(1), 1_000);
        assert_eq!(backoff_ms(2), 2_000);
        assert_eq!(backoff_ms(3), 4_000);
        assert_eq!(backoff_ms(4), 8_000);
        assert_eq!(backoff_ms(5), 16_000);
        assert_eq!(backoff_ms(6), 30_000, "16s 之后到上限 30s");
        assert_eq!(backoff_ms(99), 30_000, "再多次也不溢出");
    }

    #[test]
    fn failure_backs_off_then_gives_up_after_max_attempts() {
        let ob = Outbox::open(None).unwrap();
        let t0 = now();
        let id = ob.enqueue("c", "text", &json!({})).unwrap();

        let mut clock = t0;
        for attempt in 1..MAX_ATTEMPTS {
            let next = ob
                .record_failure(&id, "boom", clock)
                .unwrap()
                .expect("还没到上限，应当安排重试");
            assert_eq!(next, clock + backoff_ms(attempt) as i64);
            clock = next;
            // 未到点不发
            assert!(ob.due(clock - 1, 10).unwrap().is_empty());
            assert_eq!(ob.due(clock, 10).unwrap().len(), 1);
        }

        // 第 MAX_ATTEMPTS 次失败 → 判死
        let next = ob.record_failure(&id, "boom", clock).unwrap();
        assert!(next.is_none(), "次数用尽后不该再安排重试");
        assert!(ob.due(clock + 1_000_000, 10).unwrap().is_empty());
        assert_eq!(ob.pending_count().unwrap(), 0);
        let failed = ob.failed(10).unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].attempts, MAX_ATTEMPTS);
        assert_eq!(failed[0].last_error.as_deref(), Some("boom"));
    }

    #[test]
    fn requeue_resets_attempts_for_a_user_initiated_retry() {
        let ob = Outbox::open(None).unwrap();
        let id = ob.enqueue("c", "text", &json!({})).unwrap();
        ob.mark_failed(&id, "403 not a member").unwrap();
        assert!(ob.due(now(), 10).unwrap().is_empty());

        ob.requeue(&id, now()).unwrap();
        let due = ob.due(now(), 10).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].attempts, 0, "用户重试要重新给满次数");
    }

    #[test]
    fn flush_marks_sent_and_stops_at_the_first_retryable_failure() {
        let ob = Outbox::open(None).unwrap();
        ob.enqueue("c", "text", &json!({"n": 1})).unwrap();
        ob.enqueue("c", "text", &json!({"n": 2})).unwrap();
        ob.enqueue("c", "text", &json!({"n": 3})).unwrap();

        let mut seen = 0;
        let report = ob
            .flush_with(now(), 10, |task| {
                seen += 1;
                if task.payload["n"] == 2 {
                    Err(SendFailure::retryable("connection reset"))
                } else {
                    Ok(())
                }
            })
            .unwrap();

        assert_eq!(report.sent, 1);
        assert_eq!(report.retrying, 1);
        assert_eq!(report.failed, 0);
        // 第 3 条这一轮**没被尝试**：连接坏了，接着撞墙只会把每条都 +1
        assert_eq!(seen, 2, "遇到可重试失败必须停下，而不是硬跑完");
        assert_eq!(ob.pending_count().unwrap(), 2);
    }

    #[test]
    fn permanent_failure_does_not_waste_four_retries() {
        let ob = Outbox::open(None).unwrap();
        ob.enqueue("c", "text", &json!({})).unwrap();

        let report = ob
            .flush_with(now(), 10, |_| {
                Err(SendFailure::permanent("403 not a member"))
            })
            .unwrap();
        assert_eq!(report.failed, 1);
        assert_eq!(report.retrying, 0);
        // 一次就判死：403 重试 5 次也不会变成 200
        assert!(ob.due(now() + 10_000, 10).unwrap().is_empty());
    }

    #[test]
    fn same_task_id_is_enqueued_once() {
        let ob = Outbox::open(None).unwrap();
        let t = now();
        ob.enqueue_with_id("fixed", "c", "text", &json!({"v": 1}), t)
            .unwrap();
        // 重放（比如进程重启后把本地日志里的未确认消息重新入队）
        ob.enqueue_with_id("fixed", "c", "text", &json!({"v": 2}), t)
            .unwrap();
        let due = ob.due(t, 10).unwrap();
        assert_eq!(due.len(), 1, "同 task_id 不该出现两行");
        assert_eq!(due[0].payload["v"], 1, "先入队的为准，不被覆盖");
    }

    /// 被本地漏斗挡下**不能**算一次失败：否则等 1 秒就能发出去的消息，
    /// 会先被记 5 次失败直接进 failed 状态。
    #[test]
    fn a_throttled_send_does_not_burn_an_attempt() {
        let ob = Outbox::open(None).unwrap();
        let id = ob.enqueue("c", "text", &json!({})).unwrap();

        for _ in 0..3 {
            let report = ob
                .flush_with(now(), 10, |_| Err(SendFailure::throttled("本地限流")))
                .unwrap();
            assert_eq!(report.throttled, 1);
            assert_eq!(report.failed, 0);
            assert_eq!(report.retrying, 0);
        }

        // 仍是 0 次尝试、仍然 pending、仍然到点可发
        let due = ob.due(now() + 1, 10).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].task_id, id);
        assert_eq!(due[0].attempts, 0, "限流不该累加 attempts");
    }

    /// 进程重启续跑：文件库关掉再打开，pending 还在。
    #[test]
    fn pending_tasks_survive_a_restart() {
        let path = std::env::temp_dir().join(format!("wxlike-outbox-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let p = path.to_string_lossy().to_string();

        let id = {
            let ob = Outbox::open(Some(&p)).unwrap();
            ob.enqueue("d-a~b", "text", &json!({"text": "在地铁里发的"}))
                .unwrap()
        };

        // 模拟进程被杀：连接随 drop 关闭，重新打开
        let ob = Outbox::open(Some(&p)).unwrap();
        let due = ob.due(now() + 1_000, 10).unwrap();
        assert_eq!(due.len(), 1, "重启后未发送的消息必须还在");
        assert_eq!(due[0].task_id, id);
        assert_eq!(due[0].payload["text"], "在地铁里发的");

        let _ = std::fs::remove_file(&path);
    }
}
