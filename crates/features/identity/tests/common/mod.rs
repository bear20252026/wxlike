//! G02 身份域集成测试的公共基座。全部是**真实 SQLite**：真实 DDL、真实 SQL、
//! 真实令牌摘要，没有任何假仓储。
//!
//! 并发用例必须是文件库的多连接——`open_in_memory()` 每次 `open` 是一个全新的库，
//! 两个线程各拿一个内存库并发改，测到的只是各自单线程重放。
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use rusqlite::Connection;
use wxlike_identity::{
    device::{self, Registration},
    migrate, Audience,
};

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// 单连接内存库。
pub fn db() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory sqlite");
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .expect("fk pragma");
    migrate(&conn).expect("identity migrate");
    conn
}

/// 一个真实的数据库文件，可被多个线程各自 `open`。
pub struct FileDb {
    path: PathBuf,
}

impl FileDb {
    pub fn new(tag: &str) -> Self {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "wxlike-g02-identity-{tag}-{}-{n}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let me = Self { path };
        let conn = me.connect();
        migrate(&conn).expect("identity migrate on file");
        me
    }

    pub fn connect(&self) -> Connection {
        let conn = Connection::open(&self.path).expect("open file sqlite");
        conn.pragma_update(None, "journal_mode", "WAL")
            .expect("WAL pragma");
        conn.pragma_update(None, "busy_timeout", 8_000_i64)
            .expect("busy_timeout pragma");
        conn
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for FileDb {
    fn drop(&mut self) {
        let base = self.path.to_string_lossy().to_string();
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{base}{suffix}"));
        }
    }
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
fn is_busy(err: &wxlike_identity::IdError) -> bool {
    matches!(
        err,
        wxlike_identity::IdError::Db(rusqlite::Error::SqliteFailure(code, _))
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

/// 在当前线程新开连接跑 `f`，只对锁竞争重试；其它结果原样交出。
pub fn retry<T>(
    db: &FileDb,
    f: impl Fn(&Connection) -> Result<T, wxlike_identity::IdError>,
) -> Result<T, wxlike_identity::IdError> {
    let conn = db.connect();
    let mut last_busy = None;
    for _ in 0..300 {
        match f(&conn) {
            Ok(v) => return Ok(v),
            Err(e) if is_busy(&e) => {
                last_busy = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_busy.expect("retry loop records a busy error before giving up"))
}

/// `retry` 的"就当我确定它会成功"版本，用于准备数据。
pub fn setup<T>(db: &FileDb, f: impl Fn(&Connection) -> Result<T, wxlike_identity::IdError>) -> T {
    retry(db, f).expect("setup step must succeed")
}
