//! SQLite access for the platform data plane.
//!
//! 连接生命周期（打开/pragma/慢查询日志/探活/迁移）已抽到 `storage` 端口
//! （G00-04 S2）；`Db` 仍是平台侧句柄，`lock()` / `blocking()` 签名不变。
//!
//! One `Connection` behind a `Mutex` (the task book's required shape): callers
//! lock, run fully-synchronous SQL, and drop the guard before any `await`, so
//! the guard is never held across a suspension point. Every statement is
//! parameterized — string-built SQL is banned by the architecture rules.
//!
//! ADR-28 (#326) adds the second half of that rule: an `async fn` must not run
//! the synchronous SQL at all — not even without holding the guard across an
//! `await`, because `self.db().lock()` inside an axum handler occupies a tokio
//! worker for the whole transaction. Two entry points exist for that:
//!
//! * [`Db::blocking`] — for code that already has a `Db`: the closure gets
//!   `&mut Connection` **on a blocking thread**, and the whole transaction has
//!   to fit inside it;
//! * `offload`（本模块内）— for handlers that call an existing synchronous `Platform`
//!   method: the *call* moves to the blocking pool, the transaction boundary
//!   does not move at all. `offload_api` is the same thing for methods that
//!   return `ApiError` (their HTTP statuses are not expressible as
//!   `PlatformError`).
//!
//! Both deliberately hand the caller a *connection*, never a guard, so nothing
//! can escape the blocking task and no transaction can be split across two of
//! them.

use std::sync::{Arc, MutexGuard};

use rusqlite::Connection;

use crate::error::{ApiError, PlatformError, Result};
use crate::storage::{SqliteStorage, Storage, StorageConnection, StorageDialect, StorageProfile};

#[derive(Clone)]
pub struct Db {
    /// 连接生命周期（创建/pragma/探活）在 `storage` 端口；这里只持有它，
    /// 让 `lock()` / `blocking()` 的签名与全部调用方保持不变。
    storage: SqliteStorage,
}

impl Db {
    /// `None` opens an in-memory database (tests, single-process demos).
    ///
    /// 打开路径经过 [`StorageProfile`]：`WXLIKE_STORAGE=postgres` 或非法值
    /// 都会返回受控错误（G00-04 S2 只落 SQLite 实现），不 panic、不 `unwrap`。
    pub fn open(path: Option<&str>) -> Result<Self> {
        let profile = StorageProfile::from_env()?;
        let storage = SqliteStorage::open(profile, path)?;
        Ok(Self { storage })
    }

    /// 当前连接走哪种方言。业务层只在一处需要分支：media 的旧 upsert
    /// 不能作用在 PG 兼容视图上，SQLite 仍保留原语句。
    pub(crate) fn dialect(&self) -> StorageDialect {
        self.storage.profile().dialect()
    }

    /// A poisoned lock only happens if SQL panicked mid-statement; SQLite
    /// connections survive that, so recovering the guard beats crashing.
    ///
    /// 走端口的 [`Storage::acquire`]，让 `Db` 的连接入口确实经过存储端口。
    pub fn lock(&self) -> MutexGuard<'_, Connection> {
        match self.storage.acquire() {
            StorageConnection::Sqlite(guard) => guard,
        }
    }

    /// ADR-28: run one synchronous SQLite critical section on the blocking pool.
    ///
    /// The `Mutex<Connection>` guard is acquired *inside* the closure and
    /// dropped there, so a caller cannot carry a guard — or an open transaction
    /// — back across an `await`. The whole transaction has to fit in `f`, which
    /// is exactly the point: moving the thread must not move the atomic boundary.
    pub async fn blocking<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        self.storage.blocking(f).await
    }
}

/// ADR-28 offload for handlers: move an existing synchronous `Platform` call
/// onto the blocking pool.
///
/// Deliberately coarse — the *call* crosses the thread boundary, the
/// transaction inside it does not. `f` may use every synchronous method of the
/// platform unchanged (`db().lock()`, `with_tx`, the same ordering), so
/// converting a handler cannot split a transaction in half or reorder its
/// statements; it only stops the await context from owning the wait.
///
/// The `platform` argument is the shared handle the handler already holds: a
/// `&Platform` cannot be moved into a `'static` task, an `Arc<Platform>` can.
pub(crate) async fn offload<T, F>(platform: &crate::PlatformState, f: F) -> Result<T>
where
    F: FnOnce(&crate::Platform) -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let state = Arc::clone(platform);
    let ran = tokio::task::spawn_blocking(move || f(&state)).await;
    crate::storage::join_blocking(ran)
}

/// [`offload`]，给返回 `ApiError` 的同步方法用。
///
/// 为什么单独一个而不是把 `offload` 改成泛型 `<T, E, F>`：`E` 一旦由调用点
/// 推断，每个以 `Ok(..)` 收尾的闭包都定不下来（`From<PlatformError>` 有
/// `PlatformError` / `DomainError` / `ApiError` 三个实现）——本 crate 里有 60
/// 处会当场编译失败。把 `E` 在签名里钉成 `ApiError` 就没这个问题：调用点
/// 一个字不用改，类型由函数自己确定。
///
/// 谁需要它：`qr_login` 的领域方法返回 `ApiError`，因为票过期要回 410 GONE，
/// 而 `PlatformError` 没有对应变体。若把它塞进 `PlatformError`，要么把 410
/// 降级成 500，要么同一个「票已过期」在 scan（410）与 exchange（410）之外
/// 再分裂出第三种口径。
///
/// 语义与 `offload` 逐字一致：只有「阻塞任务本身没跑起来」（panic / 取消）
/// 才折成 500，闭包里的业务错误原样透传。
pub(crate) async fn offload_api<T, F>(
    platform: &crate::PlatformState,
    f: F,
) -> std::result::Result<T, ApiError>
where
    F: FnOnce(&crate::Platform) -> std::result::Result<T, ApiError> + Send + 'static,
    T: Send + 'static,
{
    let state = Arc::clone(platform);
    let ran = tokio::task::spawn_blocking(move || f(&state)).await;
    match ran {
        Ok(inner) => inner,
        Err(err) => Err(ApiError::from(PlatformError::Internal(format!(
            "blocking db task failed: {err}"
        )))),
    }
}

/// `UPDATE` / `DELETE` 影响行数为 0 → `NotFound`：**唯一**的一处判定。
///
/// 写语句的 `WHERE` 里带着归属条件（`AND user_id = ?`、`AND author = ?`），
/// 所以"没这行"与"不是你的"落进同一个 0。这时候**必须**回同一个 404：
/// 分开回（403 vs 404）就等于告诉对方"这个 id 存在，只是不属于你"——
/// 一个可枚举的账号/内容id 列表就这么送出去了。
///
/// 这条规则原来是 `page::ensure_updated`，但它是**任何写操作**共用的，
/// 不是列表语义（`page` 里另外四件才是一回事）。搬到这里，
/// 让 `page` 只讲列表，写路径的判定只有一处。
///
/// 各 feature 里曾各自手抄一遍 `if changed == 0 { NotFound }`（17 处），
/// 抄错的形式不是编译失败而是**语义漂移**：把 0 行当成"冲突"或"禁止"，
/// 于是同一张表上出现两种 404/409 口径，客户端分流跟着分叉。
pub(crate) fn ensure_updated(changed: usize) -> Result<()> {
    if changed == 0 {
        return Err(PlatformError::NotFound);
    }
    Ok(())
}

#[cfg(test)]
mod adr28_evidence {
    //! #326 验收第 3、4 条：并发证据与改前/改后数字。
    //!
    //! 两种写法跑在**同一个 runtime、同一份内存库**里，所以"改前/改后"的 tick 数
    //! 来自同一次运行，不是两次环境对比。runtime 只给 1 个 worker：一条 250 ms 的
    //! 同步事务占住 worker 时，旁边的计时任务**拿不到调度**——这正是 ADR-26 里
    //! 那句"换 PostgreSQL 不会自动治好它"要治的东西（连接池同样会串行）。

    use super::Db;
    use crate::error::PlatformError;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use wxlike_core::tx::ConnectionTx as _;

    const HOLD: Duration = Duration::from_millis(250);
    const TICK: Duration = Duration::from_millis(10);
    /// `armed` 还没被 handler 写入时的哨兵值。
    const NOT_ARMED: u64 = u64::MAX;

    fn one_worker_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("single-worker tokio runtime")
    }

    fn fresh_db() -> Db {
        Db::open(None).expect("in-memory platform db")
    }

    /// 交给 handler 的"报到"句柄：`Window` 借不到 `'static`，所以单独给一份可克隆的。
    #[derive(Clone)]
    struct Arm(Arc<AtomicU64>, Instant);

    impl Arm {
        /// 记下"我从这一刻起占住 worker"。调用点与占用之间**不许有 await**，
        /// 否则计时任务能插进一次调度，窗口就又开早了。
        fn now(&self) {
            self.0
                .store(self.1.elapsed().as_millis() as u64, Ordering::SeqCst);
        }
    }

    /// 一次测量的现场：只在 handler 占用 worker 的那段窗口里数计时步。
    struct Window {
        ticks: Arc<AtomicUsize>,
        armed_at: Arc<AtomicU64>,
        start: Instant,
    }

    impl Window {
        fn new() -> Self {
            Self {
                ticks: Arc::new(AtomicUsize::new(0)),
                armed_at: Arc::new(AtomicU64::new(NOT_ARMED)),
                start: Instant::now(),
            }
        }

        fn arm(&self) -> Arm {
            Arm(Arc::clone(&self.armed_at), self.start)
        }

        /// 每 `TICK` 一跳，只数 `armed_at .. armed_at + HOLD`。
        ///
        /// 关键改动：窗口**不再**从"计时任务开工"算起，而是从 handler 自己报到的
        /// 那一刻算起。旧写法里 worker 被占死，计时任务在窗口内一次都排不上；等它
        /// 醒来窗口已经关掉 ⇒ `ticks == 0` 是**构造出来的**，不是调出来的阈值。
        ///
        /// 原实现把 deadline 取在计时任务自己开工时，于是它与 handler 抢同一个
        /// worker，谁先被调度是竞态——慢一点的 runner 上它能先攒 4~7 跳，
        /// `old_ticks <= 3` 就无端红了（main 的 ci run 42409122864，windows job）。
        /// 那类"把等时间改成等事件"的修法只能用注入延迟验证，加压复跑验不出来。
        fn start_driver(&self) -> tokio::task::JoinHandle<()> {
            let ticks = Arc::clone(&self.ticks);
            let armed_at = Arc::clone(&self.armed_at);
            let start = self.start;
            let hold = HOLD.as_millis() as u64;
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(TICK).await;
                    let now = start.elapsed().as_millis() as u64;
                    let armed = armed_at.load(Ordering::SeqCst);
                    // 失控护栏：handler 一直没报到也别把用例挂死。不参与改前/改后比较。
                    let base = if armed == NOT_ARMED { 0 } else { armed };
                    if now - base >= 4 * hold {
                        return;
                    }
                    if armed != NOT_ARMED && now - armed < hold {
                        ticks.fetch_add(1, Ordering::SeqCst);
                    }
                }
            })
        }

        fn ticks(&self) -> usize {
            self.ticks.load(Ordering::SeqCst)
        }
    }

    /// 把 handler 作为**任务**放上唯一的 worker——这才是 axum handler 的形状。
    /// （早先我把它写在 `block_on` 的 future 里，结果测的是调用线程，数字全是假的。）
    async fn old_case(db: Db) -> (i64, usize) {
        let window = Window::new();
        let driver = window.start_driver();
        let arm = window.arm();
        let handler = tokio::spawn(async move {
            arm.now();
            let c = db.lock();
            std::thread::sleep(HOLD);
            c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0)).unwrap()
        });
        let value = handler.await.unwrap();
        driver.await.unwrap();
        (value, window.ticks())
    }

    async fn new_case(db: Db) -> (i64, usize) {
        let window = Window::new();
        let driver = window.start_driver();
        let arm = window.arm();
        let handler = tokio::spawn(async move {
            arm.now();
            db.blocking(move |c| -> std::result::Result<i64, PlatformError> {
                std::thread::sleep(HOLD);
                c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))
                    .map_err(PlatformError::from)
            })
            .await
            .unwrap()
        });
        let value = handler.await.unwrap();
        driver.await.unwrap();
        (value, window.ticks())
    }

    #[test]
    fn the_blocking_entrypoint_frees_the_worker_that_sync_sql_occupied() {
        // 同一个 runtime、同一次运行里量两种写法 ⇒ "改前/改后"不跨环境比较。
        let rt = one_worker_runtime();
        let (old_value, old_ticks, new_value, new_ticks) = rt.block_on(async {
            let (v_old, t_old) = old_case(fresh_db()).await;
            let (v_new, t_new) = new_case(fresh_db()).await;
            (v_old, t_old, v_new, t_new)
        });
        println!(
            "ADR-28 并发证据：持有 {}ms / 计时步长 {}ms ⇒ 占用窗口内 tick：改前={old_ticks}，改后={new_ticks}",
            HOLD.as_millis(),
            TICK.as_millis()
        );
        assert_eq!((old_value, new_value), (1, 1), "两条路径必须读出同一个结果");
        assert_eq!(
            old_ticks, 0,
            "旧写法把唯一的 worker 占死，计时任务在占用窗口内一次都排不上；实测 {old_ticks} 跳 \
             ⇒ 窗口又开早了（计时与 handler 在抢同一个 worker）。这是测量形状的问题，\
             放宽阈值只会让这个用例不再测量它声称的东西"
        );
        // 名义上 HOLD/TICK 跳；留 5 倍调度余量。旧写法再怎么抖都到不了这个量级，
        // 因为窗口关着；所以这条断言仍然能抓住"blocking 入口退回 worker 上跑"的回归。
        let floor = HOLD.as_millis() as usize / TICK.as_millis() as usize / 5;
        assert!(
            new_ticks >= floor,
            "改后 worker 应当空出来、计时任务能跑：期望至少 {floor} 跳，实测 {new_ticks}"
        );
    }

    #[test]
    fn the_transaction_boundary_stays_inside_one_closure() {
        let rt = one_worker_runtime();
        let rows = rt
            .block_on(async {
                let db = fresh_db();
                db.blocking(|c| {
                    c.with_tx(|tx| -> std::result::Result<i64, PlatformError> {
                        tx.execute_batch(
                            "CREATE TABLE IF NOT EXISTS adr28_probe(x INTEGER PRIMARY KEY);
                             INSERT INTO adr28_probe VALUES (1), (2);",
                        )?;
                        Ok(tx.query_row("SELECT COUNT(*) FROM adr28_probe", [], |r| r.get(0))?)
                    })
                })
                .await
            })
            .expect("transaction on the blocking pool");
        assert_eq!(rows, 2);
    }

    #[test]
    fn a_panicking_critical_section_is_internal_not_a_leaked_panic() {
        let rt = one_worker_runtime();
        let outcome = rt.block_on(async {
            let db = fresh_db();
            db.blocking::<_, i64>(|_| panic!("boom: sql 里不该发生的崩溃"))
                .await
        });
        assert!(
            matches!(outcome, Err(PlatformError::Internal(_))),
            "阻塞池里的崩溃必须归成 Internal（可重试与否由调用方判），实际 {outcome:?}"
        );
    }
}

#[cfg(test)]
mod zero_rows_rule {
    //! [`super::ensure_updated`] 的规则钉桩：**0 行只能有一种解释**。
    //!
    //! 这条规则的价值全在"统一"上——调用方的 `WHERE ... AND owner = ?` 让
    //! "没这行"和"不是你的"落进同一个 0，所以 0 必须**无条件**回 `NotFound`。
    //! 一旦某个 feature 改成 `Conflict` / `Forbidden`，同一张表上就出现了两种
    //! 口径，客户端分流跟着分叉，而 id 存在与否也随之可枚举。
    //!
    //! 这里只守映射本身；"两种情况确实都返回 0"由调用方的 SQL 与各 feature
    //! 的端到端用例覆盖。

    use super::ensure_updated;
    use crate::error::PlatformError;

    #[test]
    fn zero_rows_is_always_not_found() {
        assert!(
            matches!(ensure_updated(0), Err(PlatformError::NotFound)),
            "0 行必须回 NotFound；回 Conflict/Forbidden 等于承认这个 id 存在"
        );
    }

    #[test]
    fn non_zero_rows_passes() {
        assert!(ensure_updated(1).is_ok(), "影响 1 行是成功");
        assert!(ensure_updated(7).is_ok(), "影响多行同样是成功，不是错误");
    }
}
