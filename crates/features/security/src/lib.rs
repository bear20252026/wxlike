//! 安全域（G02）：多设备密钥分发、本地密钥库、隐私策略。
//!
//! 三个模块各自独立，共同点是**失败封闭**：
//!
//! * [`device_keys`]：服务端只见公钥与密文；设备密钥变更必须走轮换，不允许
//!   静默替换（否则服务端可以伪装成新可信设备）；
//! * [`secrets`]：本地主密钥只由口令/恢复码/OS 密钥库三种方式之一包裹，
//!   明文密钥永不出内存、永不上云；
//! * [`privacy`]：可见性/通知策略是纯函数，输入是关系与偏好，输出是判定，
//!   不查库、不发 IO，方便逐条验。
//!
//! 加密算法统一由 [`wxlike_core::e2ee`] 提供（vodozemac + XChaCha20Poly1305
//! + Argon2id），本 crate 不引入第二套密码学。

pub mod device_keys;
pub mod privacy;
pub mod secrets;

/// 设备密钥条数上限（一台设备一次最多发布这么多一次性预密钥）。
pub const MAX_OTKS: usize = 256;
/// 设备身份公钥（base64）长度上限。
pub const MAX_KEY_CHARS: usize = 128;
/// 安全码字符串长度上限。
pub const MAX_SAFETY_CHARS: usize = 128;

#[derive(Debug, thiserror::Error)]
pub enum SecError {
    #[error("bad input: {0}")]
    BadInput(&'static str),
    #[error("forbidden: {0}")]
    Forbidden(&'static str),
    #[error("not found: {0}")]
    NotFound(&'static str),
    #[error("conflict: {0}")]
    Conflict(&'static str),
    #[error("corrupted: {0}")]
    Corrupted(&'static str),
    #[error("crypto: {0}")]
    Crypto(#[from] wxlike_core::e2ee::CryptoError),
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, SecError>;

pub fn now_ms() -> i64 {
    wxlike_core::types::now_ms() as i64
}

const SCHEMA: &str = r#"
-- 每台设备一份密钥包（平台旧表 prekey_bundles 是每用户一份，多设备下不够用）
CREATE TABLE IF NOT EXISTS security_device_key (
    device_id        TEXT PRIMARY KEY,
    user_id          TEXT NOT NULL,
    identity         TEXT NOT NULL,
    identity_signing TEXT NOT NULL,
    signature        TEXT NOT NULL,
    otks             TEXT NOT NULL DEFAULT '[]',
    epoch            INTEGER NOT NULL DEFAULT 0,
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL,
    revoked_at       INTEGER
);
CREATE INDEX IF NOT EXISTS idx_sdk_user ON security_device_key(user_id, revoked_at);

-- 设备验证记录：绑定当时的身份公钥，对方换设备后自动失效
CREATE TABLE IF NOT EXISTS security_device_verify (
    user_id       TEXT NOT NULL,
    peer_user     TEXT NOT NULL,
    peer_device   TEXT NOT NULL,
    peer_identity TEXT NOT NULL,
    safety_number TEXT NOT NULL,
    verified_at   INTEGER NOT NULL,
    PRIMARY KEY (user_id, peer_device)
);

-- 轮换审计：每次设备密钥变更留一条，便于解释"为什么旧会话解不开了"
CREATE TABLE IF NOT EXISTS security_rotation (
    seq       INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id   TEXT NOT NULL,
    device_id TEXT NOT NULL,
    epoch     INTEGER NOT NULL,
    reason    TEXT NOT NULL,
    at        INTEGER NOT NULL
);
"#;

/// 建表（幂等）。
pub fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod testing {
    use rusqlite::Connection;

    pub fn db() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        super::migrate(&conn).expect("migrate");
        conn
    }
}
