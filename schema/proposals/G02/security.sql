-- G02-03 安全域迁移草稿
--
-- 执行者：G00。幂等；不修改平台既有 `prekey_bundles`（保留为单设备兼容路径，
-- 多设备走新表；两者的退役顺序见 README.md）。
-- 回滚：见同目录 README.md「回滚兼容说明」。

-- ---------------------------------------------------------------------------
-- 每设备密钥包（旧表 prekey_bundles 是每用户一份，多设备下不够用）
-- ---------------------------------------------------------------------------
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

-- ---------------------------------------------------------------------------
-- 设备验证记录（绑定验证时的身份公钥；对方轮换后自动失效）
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS security_device_verify (
    user_id       TEXT NOT NULL,
    peer_user     TEXT NOT NULL,
    peer_device   TEXT NOT NULL,
    peer_identity TEXT NOT NULL,
    safety_number TEXT NOT NULL,
    verified_at   INTEGER NOT NULL,
    PRIMARY KEY (user_id, peer_device)
);

-- ---------------------------------------------------------------------------
-- 轮换审计
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS security_rotation (
    seq       INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id   TEXT NOT NULL,
    device_id TEXT NOT NULL,
    epoch     INTEGER NOT NULL,
    reason    TEXT NOT NULL,
    at        INTEGER NOT NULL
);
