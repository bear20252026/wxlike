//! G02 关系域集成测试的公共基座。
//!
//! 这里只提供**真实存储**：内存 SQLite（单连接的快路径）与文件 SQLite
//! （多个连接指向同一个数据库文件，才是真并发）。存储与重试原语在
//! `wxlike_core::test_support`（与身份域共用一份）；本模块只放关系域特有的
//! 东西：好友、群、以及本域错误到 BUSY 判定的拆包。
//!
//! 本域的规则本身就是 SQL 上的条件写，换成内存实现就测不到要测的东西。
#![allow(dead_code)]

use rusqlite::Connection;
use wxlike_core::test_support;
pub use wxlike_core::test_support::FileDb;
use wxlike_relations::{friend, group, group::Visibility, migrate, RelError};

/// 单连接内存库：跑真实 DDL 与真实 SQL。
pub fn db() -> Connection {
    test_support::memory_db(|conn| migrate(conn).expect("relations migrate"))
        .expect("in-memory sqlite")
}

/// 关系域的文件库：建表走 `wxlike_relations::migrate`。
pub fn file_db(tag: &str) -> FileDb {
    let db = FileDb::new(tag);
    let conn = db.connect().expect("connect file sqlite");
    migrate(&conn).expect("relations migrate on file");
    db
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

/// BUSY / LOCKED = "这次没排上锁"，与业务失败是两回事。
pub fn is_busy(err: &RelError) -> bool {
    matches!(err, RelError::Db(e) if test_support::is_busy(e))
}

/// 新开一个连接跑 `f`，只对锁竞争重试；其它结果原样交出。
pub fn retry<T>(
    db: &FileDb,
    f: impl Fn(&Connection) -> Result<T, RelError>,
) -> Result<T, RelError> {
    let conn = db.connect().expect("connect file sqlite");
    test_support::retry(&conn, is_busy, f)
}

/// `retry` 的"就当我确定它会成功"版本，用于准备数据。
pub fn setup<T>(db: &FileDb, f: impl Fn(&Connection) -> Result<T, RelError>) -> T {
    retry(db, f).expect("setup step must succeed")
}
