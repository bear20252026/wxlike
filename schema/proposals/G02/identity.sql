-- G02-01 / G02-05 / G02-06 / G02-07 身份域迁移草稿
--
-- 执行者：G00（迁移编号与串行合入归 G00，本文件只是草稿）。
-- 幂等：全部 IF NOT EXISTS；对既有 `sessions` 表只做"补列"，不改既有列语义。
-- 回滚：见同目录 README.md「回滚兼容说明」。

-- ---------------------------------------------------------------------------
-- G02-01 设备注册表
-- ---------------------------------------------------------------------------
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

-- ---------------------------------------------------------------------------
-- G02-01 刷新令牌链（轮换 + 重放检测）
-- ---------------------------------------------------------------------------
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

-- ---------------------------------------------------------------------------
-- G02-01 step-up 授权（短时、单次、绑定会话与用途）
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS identity_step_up (
    user_id     TEXT NOT NULL,
    sid         TEXT NOT NULL,
    purpose     TEXT NOT NULL,
    granted_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    consumed_at INTEGER,
    PRIMARY KEY (user_id, sid, purpose)
);

-- ---------------------------------------------------------------------------
-- G02-01 限速桶（固定窗口）
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS identity_throttle (
    bucket        TEXT PRIMARY KEY,
    window_start  INTEGER NOT NULL,
    attempts      INTEGER NOT NULL,
    blocked_until INTEGER NOT NULL DEFAULT 0
);

-- ---------------------------------------------------------------------------
-- G02-01 找回令牌（对外不泄漏账号是否存在）
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS identity_reset_token (
    token_hash TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    used_at    INTEGER
);

-- ---------------------------------------------------------------------------
-- G02-06 短信挑战
-- ---------------------------------------------------------------------------
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

-- ---------------------------------------------------------------------------
-- G02-06 协议同意回执
-- ---------------------------------------------------------------------------
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

-- ---------------------------------------------------------------------------
-- G02-07 扫码登录 challenge
-- ---------------------------------------------------------------------------
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

-- ---------------------------------------------------------------------------
-- G02-05 服务端偏好（逐字段 + seq 作为跨端同步游标）
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS identity_pref (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id    TEXT NOT NULL,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    device_id  TEXT NOT NULL DEFAULT '',
    UNIQUE(user_id, key)
);

-- ---------------------------------------------------------------------------
-- G02-05 导出 / 注销后台作业
-- ---------------------------------------------------------------------------
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

-- ---------------------------------------------------------------------------
-- 既有表的补列（唯一一处改动平台表）
-- ---------------------------------------------------------------------------
-- 老库执行前先看 PRAGMA table_info(sessions)，缺列才 ALTER。
-- ALTER TABLE sessions ADD COLUMN audience TEXT NOT NULL DEFAULT 'user';
--
-- G02-05（#114）注销标记列：G00 执行 integration-requests/G02/G02-05.yaml 的
-- `schema` 项时补上这一句。**不占迁移编号**（`schema/registry/migrations.json`
-- 里没有它），因为它是"有列就跳过"的幂等补列，与 `sessions.audience` 同一手法：
-- 执行点是 `crates/platform/src/db.rs::migrate_account()`，用
-- `has_column("users", "deleted_at")` 守卫后再 ALTER。
-- 之所以不新增编号：G00-02 正在串行分配编号，占号会与之冲突
-- （冲突史：G00-02 的未提交稿曾重引入 G02 的表丢失）。
-- 回滚口径见 integration-requests/G02/G02-05-migration.md：列只增不删，
-- 旧二进制按显式列名查询不受影响；可回滚窗口只到注销作业执行之前。
-- ALTER TABLE users ADD COLUMN deleted_at INTEGER;
