-- M011 — 身份兼容层（PostgreSQL）
--
-- 目的：在不改现有平台 SQL 的前提下，把 `users` / `sessions` 两个名字保留为
-- 兼容视图，底层落到 G02 的规范表：
--   * `users`    -> `identity_user`（账户根表）
--   * `sessions` -> `identity_session_token`（访问会话）+ `identity_refresh`（刷新链）
--
-- 这是 expand-only 迁移：只新增表/视图/函数/触发器，不删除、不改写任何既有对象。
-- 旧二进制仍然可以对 `users` / `sessions` 做 SELECT / INSERT / UPDATE / DELETE；
-- 新代码应直接写 `identity_user` / `identity_session_token`。
--
-- 重要约束：PostgreSQL 的外键不能指向视图。新表的 `REFERENCES` 必须写
-- `identity_user(user_id)`，不要再写 `REFERENCES users(id)`；`sessions` 同理。
--
-- 幂等：全部 IF NOT EXISTS / CREATE OR REPLACE / DROP TRIGGER IF EXISTS。
-- 回滚：先删 `users` / `sessions` 视图与触发器，再删三张 identity 表；不回滚数据
-- （数据在 identity 表里，删视图不丢数据）。见文末回滚说明。

BEGIN;

-- ---------------------------------------------------------------------------
-- 1. 账户根表：identity_user
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS identity_user (
    user_id            TEXT PRIMARY KEY,
    name               TEXT NOT NULL,
    pw_salt            TEXT NOT NULL DEFAULT '',
    pw_hash            TEXT NOT NULL,
    pw_rounds          INTEGER NOT NULL DEFAULT 0,
    pw_algo            TEXT NOT NULL DEFAULT 'argon2id',
    totp_secret        TEXT,
    totp_enabled       INTEGER NOT NULL DEFAULT 0,
    avatar             TEXT NOT NULL DEFAULT '',
    signature          TEXT NOT NULL DEFAULT '',
    created_at         BIGINT NOT NULL,
    deleted_at         BIGINT,
    suspended          INTEGER NOT NULL DEFAULT 0,
    disabled           INTEGER NOT NULL DEFAULT 0,
    suspended_until_ms BIGINT
);

-- 旧 `users.name` 是全表 UNIQUE（含已注销行）；保持一致，避免注销后重名
-- 在 PG 上突然变成可注册。
CREATE UNIQUE INDEX IF NOT EXISTS uq_identity_user_name
    ON identity_user(name);

CREATE INDEX IF NOT EXISTS idx_identity_user_deleted
    ON identity_user(deleted_at)
    WHERE deleted_at IS NOT NULL;

-- 治理状态的权威表。`identity_user` 上的 suspended/disabled/suspended_until_ms
-- 是平台侧镜像列；域侧命令写这里，再由 mirror_user_state 回写 identity_user。
CREATE TABLE IF NOT EXISTS identity_user_governance (
    user_id            TEXT PRIMARY KEY,
    suspended          INTEGER NOT NULL DEFAULT 0,
    disabled           INTEGER NOT NULL DEFAULT 0,
    suspended_until_ms BIGINT,
    reason             TEXT NOT NULL DEFAULT '',
    actor              TEXT NOT NULL DEFAULT '',
    updated_at         BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS identity_governance_audit (
    seq              BIGSERIAL PRIMARY KEY,
    action           TEXT NOT NULL,
    target           TEXT NOT NULL,
    actor            TEXT NOT NULL,
    reason           TEXT NOT NULL,
    trace            TEXT NOT NULL DEFAULT '',
    before_json      TEXT NOT NULL DEFAULT '{}',
    after_json       TEXT NOT NULL DEFAULT '{}',
    sessions_revoked INTEGER NOT NULL DEFAULT 0,
    already          INTEGER NOT NULL DEFAULT 0,
    at               BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_identity_gov_audit_target
    ON identity_governance_audit(target, seq);

CREATE TABLE IF NOT EXISTS identity_governance_label (
    user_id    TEXT NOT NULL,
    label      TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    PRIMARY KEY (user_id, label)
);

-- ---------------------------------------------------------------------------
-- 2. 访问会话表：identity_session_token
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS identity_session_token (
    token       TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL REFERENCES identity_user(user_id) ON DELETE CASCADE,
    created_at  BIGINT NOT NULL,
    expires_at  BIGINT NOT NULL,
    sid         TEXT,
    device_id   TEXT NOT NULL DEFAULT '',
    device_name TEXT NOT NULL DEFAULT '',
    ip          TEXT NOT NULL DEFAULT '',
    last_seen   BIGINT NOT NULL DEFAULT 0,
    audience    TEXT NOT NULL DEFAULT 'user'
);
CREATE INDEX IF NOT EXISTS idx_identity_session_token_user
    ON identity_session_token(user_id);
CREATE INDEX IF NOT EXISTS idx_identity_session_token_sid
    ON identity_session_token(sid);
CREATE INDEX IF NOT EXISTS idx_identity_session_token_device
    ON identity_session_token(user_id, device_id);

-- ---------------------------------------------------------------------------
-- 3. 刷新链：identity_refresh（G02-01 的规范表；此前只有 SQLite 提案）
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS identity_refresh (
    token_hash  TEXT PRIMARY KEY,
    sid         TEXT NOT NULL,
    user_id     TEXT NOT NULL REFERENCES identity_user(user_id) ON DELETE CASCADE,
    device_id   TEXT NOT NULL,
    audience    TEXT NOT NULL DEFAULT 'user',
    generation  INTEGER NOT NULL,
    created_at  BIGINT NOT NULL,
    expires_at  BIGINT NOT NULL,
    used_at     BIGINT,
    revoked_at  BIGINT
);
CREATE INDEX IF NOT EXISTS idx_identity_refresh_sid
    ON identity_refresh(sid);
CREATE INDEX IF NOT EXISTS idx_identity_refresh_device
    ON identity_refresh(user_id, device_id);
CREATE INDEX IF NOT EXISTS idx_identity_refresh_expiry
    ON identity_refresh(expires_at)
    WHERE revoked_at IS NULL;

-- ---------------------------------------------------------------------------
-- 4. 兼容视图：users
-- ---------------------------------------------------------------------------

CREATE OR REPLACE VIEW users AS
SELECT
    user_id            AS id,
    name,
    pw_salt,
    pw_hash,
    pw_rounds,
    pw_algo,
    totp_secret,
    totp_enabled,
    avatar,
    signature,
    created_at,
    deleted_at,
    suspended,
    disabled,
    suspended_until_ms
FROM identity_user;

-- 视图列默认值：旧表 `users` 上带 DEFAULT 的列，在视图上也要有，
-- 否则 `INSERT ... RETURNING` 拿到的是 NULL（实际落库值却由触发器兜底），
-- 与 SQLite 的返回口径不一致。触发器里的 COALESCE 仍是最后一道防线。
ALTER VIEW users ALTER COLUMN pw_algo SET DEFAULT 'pbkdf2';
ALTER VIEW users ALTER COLUMN totp_enabled SET DEFAULT 0;
ALTER VIEW users ALTER COLUMN avatar SET DEFAULT '';
ALTER VIEW users ALTER COLUMN signature SET DEFAULT '';
ALTER VIEW users ALTER COLUMN suspended SET DEFAULT 0;
ALTER VIEW users ALTER COLUMN disabled SET DEFAULT 0;

-- 旧代码的 INSERT 只提供部分列；视图 NEW 行里缺省列为 NULL，这里统一兜底。
CREATE OR REPLACE FUNCTION identity_users_insert_fn()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO identity_user (
        user_id, name, pw_salt, pw_hash, pw_rounds, pw_algo,
        totp_secret, totp_enabled, avatar, signature, created_at,
        deleted_at, suspended, disabled, suspended_until_ms
    ) VALUES (
        NEW.id, NEW.name, COALESCE(NEW.pw_salt, ''),
        NEW.pw_hash, COALESCE(NEW.pw_rounds, 0),
        COALESCE(NEW.pw_algo, 'pbkdf2'),
        NEW.totp_secret, COALESCE(NEW.totp_enabled, 0),
        COALESCE(NEW.avatar, ''), COALESCE(NEW.signature, ''),
        NEW.created_at,
        NEW.deleted_at, COALESCE(NEW.suspended, 0),
        COALESCE(NEW.disabled, 0), NEW.suspended_until_ms
    );
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS users_insert_trg ON users;
CREATE TRIGGER users_insert_trg
    INSTEAD OF INSERT ON users
    FOR EACH ROW EXECUTE FUNCTION identity_users_insert_fn();

CREATE OR REPLACE FUNCTION identity_users_update_fn()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    UPDATE identity_user SET
        user_id            = NEW.id,
        name               = NEW.name,
        pw_salt            = COALESCE(NEW.pw_salt, ''),
        pw_hash            = NEW.pw_hash,
        pw_rounds          = COALESCE(NEW.pw_rounds, 0),
        pw_algo            = COALESCE(NEW.pw_algo, 'pbkdf2'),
        totp_secret        = NEW.totp_secret,
        totp_enabled       = COALESCE(NEW.totp_enabled, 0),
        avatar             = COALESCE(NEW.avatar, ''),
        signature          = COALESCE(NEW.signature, ''),
        created_at         = NEW.created_at,
        deleted_at         = NEW.deleted_at,
        suspended          = COALESCE(NEW.suspended, 0),
        disabled           = COALESCE(NEW.disabled, 0),
        suspended_until_ms = NEW.suspended_until_ms
    WHERE user_id = OLD.id;
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS users_update_trg ON users;
CREATE TRIGGER users_update_trg
    INSTEAD OF UPDATE ON users
    FOR EACH ROW EXECUTE FUNCTION identity_users_update_fn();

CREATE OR REPLACE FUNCTION identity_users_delete_fn()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    DELETE FROM identity_user WHERE user_id = OLD.id;
    RETURN OLD;
END;
$$;

DROP TRIGGER IF EXISTS users_delete_trg ON users;
CREATE TRIGGER users_delete_trg
    INSTEAD OF DELETE ON users
    FOR EACH ROW EXECUTE FUNCTION identity_users_delete_fn();

-- ---------------------------------------------------------------------------
-- 5. 兼容视图：sessions
-- ---------------------------------------------------------------------------
-- `rowid` 是 SQLite 遗留的批量过期清理用的伪列。PG 视图没有 rowid，这里用
-- `ctid` 的文本形式兜底；它只适合“同一语句内选中再删”的旧查询，不能作为
-- 稳定游标。新代码请直接按 token 删除。

CREATE OR REPLACE VIEW sessions AS
SELECT
    token, user_id, created_at, expires_at, sid,
    device_id, device_name, ip, last_seen, audience,
    ctid::text AS rowid
FROM identity_session_token;

-- 与 `users` 同理：旧 sessions 表只有四个非空列，触发器会给其余列补默认；
-- 视图列默认值让 `INSERT ... RETURNING` 与 SQLite 口径一致。
ALTER VIEW sessions ALTER COLUMN device_id SET DEFAULT '';
ALTER VIEW sessions ALTER COLUMN device_name SET DEFAULT '';
ALTER VIEW sessions ALTER COLUMN ip SET DEFAULT '';
ALTER VIEW sessions ALTER COLUMN last_seen SET DEFAULT 0;
ALTER VIEW sessions ALTER COLUMN audience SET DEFAULT 'user';

CREATE OR REPLACE FUNCTION identity_sessions_insert_fn()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO identity_session_token (
        token, user_id, created_at, expires_at, sid,
        device_id, device_name, ip, last_seen, audience
    ) VALUES (
        NEW.token, NEW.user_id, NEW.created_at, NEW.expires_at, NEW.sid,
        COALESCE(NEW.device_id, ''), COALESCE(NEW.device_name, ''),
        COALESCE(NEW.ip, ''), COALESCE(NEW.last_seen, 0),
        COALESCE(NEW.audience, 'user')
    );
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS sessions_insert_trg ON sessions;
CREATE TRIGGER sessions_insert_trg
    INSTEAD OF INSERT ON sessions
    FOR EACH ROW EXECUTE FUNCTION identity_sessions_insert_fn();

CREATE OR REPLACE FUNCTION identity_sessions_update_fn()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    UPDATE identity_session_token SET
        token       = NEW.token,
        user_id     = NEW.user_id,
        created_at  = NEW.created_at,
        expires_at  = NEW.expires_at,
        sid         = NEW.sid,
        device_id   = COALESCE(NEW.device_id, ''),
        device_name = COALESCE(NEW.device_name, ''),
        ip          = COALESCE(NEW.ip, ''),
        last_seen   = COALESCE(NEW.last_seen, 0),
        audience    = COALESCE(NEW.audience, 'user')
    WHERE token = OLD.token;
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS sessions_update_trg ON sessions;
CREATE TRIGGER sessions_update_trg
    INSTEAD OF UPDATE ON sessions
    FOR EACH ROW EXECUTE FUNCTION identity_sessions_update_fn();

CREATE OR REPLACE FUNCTION identity_sessions_delete_fn()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    DELETE FROM identity_session_token WHERE token = OLD.token;
    RETURN OLD;
END;
$$;

DROP TRIGGER IF EXISTS sessions_delete_trg ON sessions;
CREATE TRIGGER sessions_delete_trg
    INSTEAD OF DELETE ON sessions
    FOR EACH ROW EXECUTE FUNCTION identity_sessions_delete_fn();

COMMIT;

-- 回滚（手工，按顺序）：
--   DROP TRIGGER IF EXISTS users_insert_trg ON users;
--   DROP TRIGGER IF EXISTS users_update_trg ON users;
--   DROP TRIGGER IF EXISTS users_delete_trg ON users;
--   DROP TRIGGER IF EXISTS sessions_insert_trg ON sessions;
--   DROP TRIGGER IF EXISTS sessions_update_trg ON sessions;
--   DROP TRIGGER IF EXISTS sessions_delete_trg ON sessions;
--   DROP VIEW IF EXISTS users;
--   DROP VIEW IF EXISTS sessions;
--   DROP FUNCTION IF EXISTS identity_users_insert_fn();
--   DROP FUNCTION IF EXISTS identity_users_update_fn();
--   DROP FUNCTION IF EXISTS identity_users_delete_fn();
--   DROP FUNCTION IF EXISTS identity_sessions_insert_fn();
--   DROP FUNCTION IF EXISTS identity_sessions_update_fn();
--   DROP FUNCTION IF EXISTS identity_sessions_delete_fn();
--   DROP TABLE IF EXISTS identity_refresh;
--   DROP TABLE IF EXISTS identity_session_token;
--   DROP TABLE IF EXISTS identity_governance_label;
--   DROP TABLE IF EXISTS identity_governance_audit;
--   DROP TABLE IF EXISTS identity_user_governance;
--   DROP TABLE IF EXISTS identity_user;
