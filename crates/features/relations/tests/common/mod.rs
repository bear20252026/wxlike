//! G02 关系域集成测试的公共基座。
//!
//! 这里只提供**真实存储**：内存 SQLite（单连接的快路径）与文件 SQLite
//! （多个连接指向同一个数据库文件，才是真并发）。没有任何假仓储——本域的
//! 规则本身就是 SQL 上的条件写，换成内存实现就测不到要测的东西。
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use rusqlite::Connection;
use wxlike_relations::{friend, group, group::Visibility, migrate};

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// 单连接内存库：跑真实 DDL 与真实 SQL。
pub fn db() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory sqlite");
    migrate(&conn).expect("relations migrate");
    conn
}

/// 一个真实的数据库**文件**。
///
/// 并发用例必须是多连接：`open_in_memory()` 每次 `open` 得到的是一个全新的库，
/// 两个线程各拿一个内存库"并发"，测到的只是各自单线程重放一遍。
pub struct FileDb {
    path: PathBuf,
}

impl FileDb {
    pub fn new(tag: &str) -> Self {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "wxlike-g02-relations-{tag}-{}-{n}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let me = Self { path };
        // 建表只在第一个连接上做一次，后面每个连接只读已建好的库。
        let conn = me.connect();
        migrate(&conn).expect("relations migrate on file");
        me
    }

    /// 新开一个连接。每个连接独立持锁，互不共享语句缓存。
    pub fn connect(&self) -> Connection {
        let conn = Connection::open(&self.path).expect("open file sqlite");
        conn.pragma_update(None, "journal_mode", "WAL")
            .expect("WAL");
        conn.pragma_update(None, "busy_timeout", 8_000_i64)
            .expect("busy_timeout");
        conn
    }

    pub fn path(&self) -> &std::path::Path {
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

/// 造一对好友（走真实申请状态机，不是直接 INSERT）。
pub fn befriend(conn: &Connection, a: &str, b: &str) {
    friend::send(conn, a, b, "", 1_000).expect("a -> b");
    friend::send(conn, b, a, "", 1_100).expect("b -> a");
    assert!(friend::are_friends(conn, a, b).expect("are_friends"));
}

/// 造一个群，owner + 若干 member。
pub fn make_group(conn: &Connection, owner: &str, members: &[&str], vis: Visibility) -> String {
    let list: Vec<String> = members.iter().map(|s| s.to_string()).collect();
    let g = group::create(conn, owner, "测试群", &list, vis, 1_000).expect("create group");
    g.id
}
