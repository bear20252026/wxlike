-- M017 回填 fixture：在 M016 之后、M017 之前写一行旧侧车数据。
-- M017 的 `INSERT ... SELECT` 应把它搬进 `identity_session_meta`。
-- 幂等：重复执行不会叠加行；断言脚本负责清理。
\set ON_ERROR_STOP on

INSERT INTO identity_user(user_id, name, pw_hash, created_at)
VALUES ('m017_backfill', 'm017_backfill', 'phc', 1)
ON CONFLICT (user_id) DO NOTHING;

INSERT INTO identity_session_token(
    token, user_id, created_at, expires_at, sid,
    device_id, device_name, ip, last_seen, audience,
    auth_level, features, serial, issued_at
) VALUES (
    'm017_backfill_token', 'm017_backfill', 100, 200, 'm017_backfill_sid',
    'device', 'device', '127.0.0.1', 100, 'user',
    'root', 'V', 7, 100
)
ON CONFLICT (token) DO NOTHING;
