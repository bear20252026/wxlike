//! 身份域（G02）：账号会话、设备生命周期、验证码登录、扫码登录、设置与偏好。
//!
//! # 分层约定
//!
//! `crates/platform` 保留 HTTP 传输与路由；**规则**只在这里实现一次。表结构
//! 以 `schema/proposals/G02/identity.sql` 为准，迁移由 G00 串行合入（见
//! `integration-requests/G02/`）。
//!
//! # 公共约束
//!
//! * 所有写命令幂等（重复提交返回同一结果，不产生第二行）；
//! * 错误与日志不出现 token、密码、验证码、手机号明文；
//! * 失败封闭：任何"看起来像授权"的中间态（已扫码、已同意）都不等于会话。

use rand::RngCore as _;

pub mod consent;
pub mod device;
pub mod login_challenge;
pub mod preferences;
pub mod qr_login;
pub mod recovery;
pub mod refresh;
pub mod session;
pub mod step_up;
pub mod throttle;

/// 令牌随机字节数（32B = 256 bit，与平台 `auth::TOKEN_BYTES` 同口径）。
pub const TOKEN_BYTES: usize = 32;
/// 访问令牌默认有效期：2 小时。
pub const ACCESS_TTL_MS: i64 = 2 * 60 * 60 * 1000;
/// 刷新令牌默认有效期：30 天。
pub const REFRESH_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// step-up 授权有效期：5 分钟。
pub const STEP_UP_TTL_MS: i64 = 5 * 60 * 1000;
/// 设备标签/机型等自由文本上限。
pub const MAX_LABEL_CHARS: usize = 64;

/// 身份域错误。**不含**任何凭据值：`Display` 只描述类别，便于直接进日志。
#[derive(Debug, thiserror::Error)]
pub enum IdError {
    #[error("bad input: {0}")]
    BadInput(&'static str),
    #[error("unauthorized: {0}")]
    Unauthorized(&'static str),
    #[error("forbidden: {0}")]
    Forbidden(&'static str),
    #[error("not found: {0}")]
    NotFound(&'static str),
    #[error("conflict: {0}")]
    Conflict(&'static str),
    #[error("expired")]
    Expired,
    #[error("rate limited, retry after {retry_after_ms} ms")]
    RateLimited { retry_after_ms: i64 },
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, IdError>;

/// 令牌/凭据的受众。管理面与用户面**不共用**令牌。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Audience {
    User,
    Admin,
}

impl Audience {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Admin => "admin",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "user" => Some(Self::User),
            "admin" => Some(Self::Admin),
            _ => None,
        }
    }
}

/// 当前毫秒时间戳（与平台同源，测试可注入固定时钟）。
pub fn now_ms() -> i64 {
    wxlike_core::types::now_ms() as i64
}

/// `n` 字节熵的十六进制串。
pub fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

/// 令牌落库前的摘要：库里只有摘要，泄露库文件不等于泄露可用凭据。
pub fn digest(token: &str) -> String {
    wxlike_core::msgmeta::sha256_hex(token.as_bytes())
}

/// 定长比较，避免把匹配位置泄漏给本地计时观察。
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// 自由文本字段的长度与字符检查（设备名、备注等用户输入）。
pub fn check_label(raw: &str, what: &'static str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.chars().count() > MAX_LABEL_CHARS {
        return Err(IdError::BadInput(what));
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err(IdError::BadInput(what));
    }
    Ok(trimmed.to_string())
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS identity_device (
    device_id     TEXT PRIMARY KEY,
    user_id       TEXT NOT NULL,
    platform      TEXT NOT NULL,
    label         TEXT NOT NULL DEFAULT '',
    os_version    TEXT NOT NULL DEFAULT '',
    app_version   TEXT NOT NULL DEFAULT '',
    identity_key  TEXT NOT NULL DEFAULT '',
    trust         TEXT NOT NULL DEFAULT 'unverified',
    created_at    INTEGER NOT NULL,
    last_seen     INTEGER NOT NULL,
    revoked_at    INTEGER
);
CREATE INDEX IF NOT EXISTS idx_identity_device_user
    ON identity_device(user_id, revoked_at);

CREATE TABLE IF NOT EXISTS identity_refresh (
    token_hash  TEXT PRIMARY KEY,
    sid         TEXT NOT NULL,
    user_id     TEXT NOT NULL,
    device_id   TEXT NOT NULL,
    audience    TEXT NOT NULL DEFAULT 'user',
    generation  INTEGER NOT NULL,
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    used_at     INTEGER,
    revoked_at  INTEGER
);
CREATE INDEX IF NOT EXISTS idx_identity_refresh_sid ON identity_refresh(sid);
CREATE INDEX IF NOT EXISTS idx_identity_refresh_device
    ON identity_refresh(user_id, device_id);

CREATE TABLE IF NOT EXISTS identity_step_up (
    user_id     TEXT NOT NULL,
    sid         TEXT NOT NULL,
    purpose     TEXT NOT NULL,
    granted_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    consumed_at INTEGER,
    PRIMARY KEY (user_id, sid, purpose)
);

CREATE TABLE IF NOT EXISTS identity_throttle (
    bucket        TEXT PRIMARY KEY,
    window_start  INTEGER NOT NULL,
    attempts      INTEGER NOT NULL,
    blocked_until INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS identity_challenge (
    id            TEXT PRIMARY KEY,
    phone_e164    TEXT NOT NULL,
    phone_hash    TEXT NOT NULL,
    purpose       TEXT NOT NULL,
    device_id     TEXT NOT NULL DEFAULT '',
    code_hash     TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    expires_at    INTEGER NOT NULL,
    resend_after  INTEGER NOT NULL,
    attempts      INTEGER NOT NULL DEFAULT 0,
    max_attempts  INTEGER NOT NULL,
    consumed_at   INTEGER
);
CREATE INDEX IF NOT EXISTS idx_identity_challenge_phone
    ON identity_challenge(phone_hash, purpose, created_at);

CREATE TABLE IF NOT EXISTS identity_consent (
    id        TEXT PRIMARY KEY,
    user_id   TEXT NOT NULL,
    doc       TEXT NOT NULL,
    version   TEXT NOT NULL,
    purpose   TEXT NOT NULL,
    locale    TEXT NOT NULL DEFAULT '',
    agreed_at INTEGER NOT NULL,
    ip_hash   TEXT NOT NULL DEFAULT '',
    UNIQUE(user_id, doc, version)
);

CREATE TABLE IF NOT EXISTS identity_qr (
    id          TEXT PRIMARY KEY,
    secret_hash TEXT NOT NULL,
    state       TEXT NOT NULL DEFAULT 'pending',
    pkce_hash   TEXT NOT NULL DEFAULT '',
    device_id   TEXT NOT NULL DEFAULT '',
    platform    TEXT NOT NULL DEFAULT '',
    label       TEXT NOT NULL DEFAULT '',
    ip_hint     TEXT NOT NULL DEFAULT '',
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    scanned_by  TEXT NOT NULL DEFAULT '',
    scanned_at  INTEGER,
    decided_at  INTEGER,
    consumed_at INTEGER
);

CREATE TABLE IF NOT EXISTS identity_pref (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id    TEXT NOT NULL,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    device_id  TEXT NOT NULL DEFAULT '',
    UNIQUE(user_id, key)
);

CREATE TABLE IF NOT EXISTS identity_job (
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL,
    kind         TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'queued',
    requested_at INTEGER NOT NULL,
    started_at   INTEGER,
    finished_at  INTEGER,
    artifact     TEXT NOT NULL DEFAULT '',
    bytes        INTEGER NOT NULL DEFAULT 0,
    error        TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_identity_job_user ON identity_job(user_id, requested_at);

CREATE TABLE IF NOT EXISTS identity_export_link (
    token_hash TEXT PRIMARY KEY,
    job_id     TEXT NOT NULL,
    user_id    TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    used_at    INTEGER
);

CREATE TABLE IF NOT EXISTS identity_reset_token (
    token_hash TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    used_at    INTEGER
);
"#;

/// 访问会话表的**规范形状**（平台 `sessions` + G02 新增 `audience` 列）。
///
/// 真实库里这张表由平台建；这里只在缺失时建出来（测试与新建库），
/// 已存在时只补列——迁移编号与正式执行归 G00。
const SESSIONS: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    token       TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    sid         TEXT,
    device_id   TEXT NOT NULL DEFAULT '',
    device_name TEXT NOT NULL DEFAULT '',
    ip          TEXT NOT NULL DEFAULT '',
    last_seen   INTEGER NOT NULL DEFAULT 0,
    audience    TEXT NOT NULL DEFAULT 'user'
);
CREATE INDEX IF NOT EXISTS idx_sessions_user ON sessions(user_id);
"#;

/// 建表 + 补列。可重复执行（幂等），失败不改动已有数据。
pub fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(SESSIONS)?;
    conn.execute_batch(SCHEMA)?;
    ensure_sessions_audience(conn)
}

/// 老库补 `audience` 列：存在才补，缺表就跳过（由平台建表时自带）。
fn ensure_sessions_audience(conn: &rusqlite::Connection) -> Result<()> {
    let mut have = Vec::new();
    {
        let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        for name in rows {
            have.push(name?);
        }
    }
    if !have.iter().any(|name| name == "audience") {
        conn.execute_batch(
            "ALTER TABLE sessions ADD COLUMN audience TEXT NOT NULL DEFAULT 'user';",
        )?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod testing {
    use rusqlite::Connection;

    /// 每个测试一份真实 SQLite（内存库）：跑真实 DDL、真实 SQL，不用假存储。
    pub fn db() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .expect("pragma");
        super::migrate(&conn).expect("migrate");
        conn
    }
}
