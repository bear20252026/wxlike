//! 关系域（G02-02）：好友、新朋友、群与隐私规则。
//!
//! # 为什么要有这个 crate
//!
//! 聊天域（G03）需要回答"这两个人能不能发消息""这个群有谁"，但它**不许**
//! 直接读写关系表——那会让两套规则各自演化。规则只在这里实现一次，对外只暴露
//! [`port::RelationsPort`]：跨域查询走端口，写操作只走本 crate 的命令。
//!
//! # 三条不变式
//!
//! 1. **拉黑优先**：任何申请/写入之前先看拉黑，被拉黑者拿不到任何"已存在"线索；
//! 2. **双向对称**：好友关系两行（各自持有备注/星标），接受一次只产生一对，
//!    并发接受不会产生第二对（主键 + 条件 UPDATE）；
//! 3. **群内越权失败**：角色、邀请、踢人、解散、可见性全部服务端判定，
//!    前端传什么角色都不作数。

use rand::RngCore as _;

pub mod friend;
pub mod group;
pub mod port;
pub mod prefs;

/// 备注、群名、标签等自由文本上限。
pub const MAX_TEXT_CHARS: usize = 64;
/// 申请附言上限。
pub const MAX_MESSAGE_CHARS: usize = 128;
/// 单群成员上限（与平台 `member_limit` 默认值同口径）。
pub const MAX_GROUP_MEMBERS: usize = 10_000;
/// 好友申请 id 的随机字节数。
pub const REQUEST_ID_BYTES: usize = 12;
/// 群 id 的随机字节数。
pub const GROUP_ID_BYTES: usize = 12;

/// 关系域错误。`Display` 只描述类别，不含用户输入内容。
#[derive(Debug, thiserror::Error)]
pub enum RelError {
    #[error("bad input: {0}")]
    BadInput(&'static str),
    #[error("forbidden: {0}")]
    Forbidden(&'static str),
    #[error("not found: {0}")]
    NotFound(&'static str),
    #[error("conflict: {0}")]
    Conflict(&'static str),
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, RelError>;

pub fn now_ms() -> i64 {
    wxlike_core::types::now_ms() as i64
}

pub fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

/// 用户输入的短文本校验：去空白、限长、拒绝控制字符。
pub fn check_text(raw: &str, max: usize, what: &'static str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.chars().count() > max || trimmed.chars().any(|c| c.is_control()) {
        return Err(RelError::BadInput(what));
    }
    Ok(trimmed.to_string())
}

const SCHEMA: &str = r#"
-- 规范形状（平台已有同名表；缺失时建出来给测试与新库）
CREATE TABLE IF NOT EXISTS friends (
    user_id    TEXT NOT NULL,
    friend_id  TEXT NOT NULL,
    remark     TEXT NOT NULL DEFAULT '',
    source     TEXT NOT NULL DEFAULT '',
    starred    INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, friend_id)
);
CREATE TABLE IF NOT EXISTS friend_requests (
    id         TEXT PRIMARY KEY,
    from_uid   TEXT NOT NULL,
    to_uid     TEXT NOT NULL,
    message    TEXT NOT NULL DEFAULT '',
    status     TEXT NOT NULL DEFAULT 'pending',
    created_at INTEGER NOT NULL,
    UNIQUE(from_uid, to_uid)
);
CREATE INDEX IF NOT EXISTS idx_fr_to ON friend_requests(to_uid, status);
CREATE TABLE IF NOT EXISTS blocks (
    user_id    TEXT NOT NULL,
    blocked_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, blocked_id)
);
CREATE TABLE IF NOT EXISTS groups (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    owner_uid  TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS group_members (
    group_id   TEXT NOT NULL,
    user_id    TEXT NOT NULL,
    role       TEXT NOT NULL DEFAULT 'member',
    my_remark  TEXT NOT NULL DEFAULT '',
    joined_at  INTEGER NOT NULL,
    PRIMARY KEY (group_id, user_id)
);
CREATE INDEX IF NOT EXISTS idx_gm_user ON group_members(user_id);

-- G02 新增：好友私有标签（每人一套，互不可见）
CREATE TABLE IF NOT EXISTS friend_tag (
    user_id    TEXT NOT NULL,
    friend_id  TEXT NOT NULL,
    tag        TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, friend_id, tag)
);

-- G02 新增：群可见性与解散标记
CREATE TABLE IF NOT EXISTS group_meta (
    group_id     TEXT PRIMARY KEY,
    visibility   TEXT NOT NULL DEFAULT 'public',
    dissolved_at INTEGER,
    dissolved_by TEXT NOT NULL DEFAULT ''
);

-- G02 新增：关系写操作审计（谁在什么时候动了哪条关系）
CREATE TABLE IF NOT EXISTS relations_audit (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    actor      TEXT NOT NULL,
    subject    TEXT NOT NULL,
    kind       TEXT NOT NULL,
    at         INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ra_actor ON relations_audit(actor, seq);
"#;

/// 建表 + 补列（幂等）。
pub fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;
    ensure_friends_starred(conn)
}

/// 老库补 `starred` 列。
fn ensure_friends_starred(conn: &rusqlite::Connection) -> Result<()> {
    let mut have = Vec::new();
    {
        let mut stmt = conn.prepare("PRAGMA table_info(friends)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        for name in rows {
            have.push(name?);
        }
    }
    if !have.iter().any(|name| name == "starred") {
        conn.execute_batch("ALTER TABLE friends ADD COLUMN starred INTEGER NOT NULL DEFAULT 0;")?;
    }
    Ok(())
}

/// 记一条关系审计。所有写命令都调它，便于"重复提交"与越权排查。
pub(crate) fn audit(
    conn: &rusqlite::Connection,
    actor: &str,
    subject: &str,
    kind: &str,
    at: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO relations_audit(actor, subject, kind, at) VALUES(?1, ?2, ?3, ?4)",
        rusqlite::params![actor, subject, kind, at],
    )?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod testing {
    use rusqlite::Connection;

    /// 真实 SQLite 内存库：跑真实 DDL 与 SQL。
    pub fn db() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        super::migrate(&conn).expect("migrate");
        conn
    }
}
