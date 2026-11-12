//! 集成测试公共基座：真实 SQLite（内存库 / 文件库）与锁竞争重试。
//!
//! 这里没有任何业务类型，只依赖 `rusqlite`，所以放在 `wxlike-core`——各 feature
//! crate 的**集成测试**（`tests/`）编译的是库的下游消费者，看不到 `#[cfg(test)]`
//! 里的东西，因此本模块无条件编译（同 `commerce::testkit` 的先例）。
//!
//! # 为什么并发用例必须用文件库
//!
//! `open_in_memory()` 每次 `open` 得到的是一个**全新的库**：两个线程各拿一个
//! 内存库"并发"，测到的只是各自单线程重放一遍。多连接指向同一个数据库文件，
//! 由 SQLite 的写锁真正裁决谁先落地，才是真并发。
//!
//! # 为什么不 `expect()`
//!
//! 本模块住在 `src/` 而不是 `tests/`，`scripts/count_unwraps.py` 会把它当生产
//! 代码计数（CI 里 `--fail-on-increase` 是硬闸门）。所以这里只提供**不 panic**
//! 的原语，`expect()` 留给 `tests/` 下的调用方。
//!
//! 注意 `retry` 与 [`crate::retry`] 无关：那个是退避策略（第 N 次失败等多久），
//! 这个是锁竞争重试（BUSY/LOCKED 就原样再试一次）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rusqlite::Connection;

/// 临时文件序号：同一进程里多个测试各建一个库，靠它区分。
static SEQ: AtomicUsize = AtomicUsize::new(0);

/// 单连接内存库：跑真实 DDL 与真实 SQL，不用假存储。
///
/// 快路径——只有一个连接，所以没有锁竞争。`migrate` 由调用方给（各域建表语句
/// 不同），它自己的错误在闭包里处理。
pub fn memory_db(migrate: impl FnOnce(&Connection)) -> rusqlite::Result<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    migrate(&conn);
    Ok(conn)
}

/// 一个真实的数据库**文件**，可被多个线程各自 `open`。
pub struct FileDb {
    path: PathBuf,
}

impl FileDb {
    /// 只算出路径（并清掉可能残留的同名文件），**不建表**。
    ///
    /// `tag` 只进临时文件名，方便崩溃后在 `%TEMP%` 里分辨是谁留下的。建表由
    /// 调用方在本域的 `file_db` 包装里做一次——那需要各域的 `migrate`。
    pub fn new(tag: &str) -> Self {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "wxlike-test-{tag}-{}-{n}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        Self { path }
    }

    /// 新开一个连接。每个连接独立持锁，互不共享语句缓存。
    pub fn connect(&self) -> rusqlite::Result<Connection> {
        let conn = Connection::open(&self.path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 8_000_i64)?;
        Ok(conn)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for FileDb {
    fn drop(&mut self) {
        // WAL 会留下 -wal / -shm 两个伴生文件，一起清掉，别在临时目录里堆垃圾。
        let base = self.path.to_string_lossy().to_string();
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{base}{suffix}"));
        }
    }
}

/// BUSY / LOCKED = "这次没排上锁"，与业务失败是两回事。
///
/// 只认 SQLite 层的错误码。各域的错误类型（`IdError::Db` / `RelError::Db`）
/// 在调用方拆包后交给它——判断标准只此一份。
pub fn is_busy(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

/// 只对锁竞争重试；其它结果原样交出。
///
/// 重试上限 300 次 × 2ms。到顶后再撞锁竞争就**把那次错误交出去**，而不是
/// panic——"确定会成功"的场景（准备数据）由调用方 `expect`。
pub fn retry<T, E>(
    conn: &Connection,
    busy: impl Fn(&E) -> bool,
    f: impl Fn(&Connection) -> Result<T, E>,
) -> Result<T, E> {
    let mut attempts = 0;
    loop {
        match f(conn) {
            Ok(v) => return Ok(v),
            Err(e) if busy(&e) && attempts < 300 => {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(e) => return Err(e),
        }
    }
}
