-- M017：把 PG 的会话侧车拆成独立表 identity_session_meta。
--
-- 背景：M011 的 `identity_session_token` 是访问会话表（token 主键），
-- 同时被 platform repo 使用；SQLite 则把访问会话放在 `sessions`、把
-- auth_level/features/serial/issued_at 放在同名的 `identity_session_token`
-- 侧车（sid 主键）。两种形状不能共用一张表：侧车写入没有 access token，
-- 在 token 主键上会直接违反 NOT NULL。
--
-- 本迁移新建 `identity_session_meta`（sid 主键）并把 M016 已补列后的
-- `identity_session_token` 行按 sid 回填。identity crate 之后读写这张表；
-- platform 继续读写 `identity_session_token`，两张表通过 sid 关联。
--
-- expand-only、IF NOT EXISTS / ON CONFLICT DO NOTHING 可重入。
-- 回滚：DROP TABLE identity_session_meta；访问会话不受影响。

BEGIN;

CREATE TABLE IF NOT EXISTS identity_session_meta (
    sid        TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES identity_user(user_id) ON DELETE CASCADE,
    auth_level TEXT NOT NULL DEFAULT 'auth',
    features   TEXT NOT NULL DEFAULT '',
    serial     BIGINT NOT NULL DEFAULT 0,
    issued_at  BIGINT NOT NULL DEFAULT 0,
    expires_at BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_identity_session_meta_user
    ON identity_session_meta(user_id);
CREATE INDEX IF NOT EXISTS idx_identity_session_meta_expiry
    ON identity_session_meta(expires_at);

INSERT INTO identity_session_meta(
    sid, user_id, auth_level, features, serial, issued_at, expires_at
)
SELECT sid, user_id, auth_level, features, serial, issued_at, expires_at
FROM identity_session_token
WHERE sid IS NOT NULL
ON CONFLICT (sid) DO NOTHING;

COMMIT;
