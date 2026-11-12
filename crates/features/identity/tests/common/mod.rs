//! G02 身份域集成测试的公共基座。全部是**真实 SQLite**：真实 DDL、真实 SQL、
//! 真实令牌摘要，没有任何假仓储。
//!
//! 存储与重试原语在 `wxlike_core::test_support`（与关系域共用一份）；本模块
//! 只放身份域特有的东西：设备注册、登录、以及本域错误到 BUSY 判定的拆包。
//!
//! 并发用例必须是文件库的多连接——`open_in_memory()` 每次 `open` 是一个全新的库，
//! 两个线程各拿一个内存库并发改，测到的只是各自单线程重放。
#![allow(dead_code)]

use rusqlite::Connection;
use wxlike_core::test_support;
pub use wxlike_core::test_support::FileDb;
use wxlike_identity::{
    device::{self, Registration},
    migrate, Audience, IdError,
};

/// 单连接内存库。
pub fn db() -> Connection {
    test_support::memory_db(|conn| migrate(conn).expect("identity migrate"))
        .expect("in-memory sqlite")
}

/// 身份域的文件库：建表走 `wxlike_identity::migrate`。
pub fn file_db(tag: &str) -> FileDb {
    let db = FileDb::new(tag);
    let conn = db.connect().expect("connect file sqlite");
    migrate(&conn).expect("identity migrate on file");
    db
}

/// 造一台设备（真实注册流程）。
pub fn add_device(conn: &Connection, user: &str, device_id: &str) -> device::DeviceRow {
    let reg = Registration {
        platform: "desktop".to_string(),
        label: "本机".to_string(),
        os_version: "win11".to_string(),
        app_version: "1.0.0".to_string(),
        identity_key: "key-".to_string() + device_id,
    };
    device::register(conn, user, device_id, &reg, 1_000).expect("register device")
}

/// 造一台设备并开一个会话，返回访问令牌与刷新令牌。
pub fn login(
    conn: &Connection,
    user: &str,
    device_id: &str,
    audience: Audience,
) -> (String, String) {
    add_device(conn, user, device_id);
    let grant =
        wxlike_identity::session::open_default(conn, user, device_id, audience, "1.2.3.4", 1_000)
            .expect("open session");
    (grant.access_token, grant.refresh_token)
}

/// BUSY / LOCKED = "这次没排上锁"，与业务失败是两回事。
pub fn is_busy(err: &IdError) -> bool {
    matches!(err, IdError::Db(e) if test_support::is_busy(e))
}

/// 新开一个连接跑 `f`，只对锁竞争重试；其它结果原样交出。
pub fn retry<T>(db: &FileDb, f: impl Fn(&Connection) -> Result<T, IdError>) -> Result<T, IdError> {
    let conn = db.connect().expect("connect file sqlite");
    test_support::retry(&conn, is_busy, f)
}

/// `retry` 的"就当我确定它会成功"版本，用于准备数据。
pub fn setup<T>(db: &FileDb, f: impl Fn(&Connection) -> Result<T, IdError>) -> T {
    retry(db, f).expect("setup step must succeed")
}
