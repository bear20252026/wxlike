-- M018 断言：清理触发器和共享 sid 语义。
-- 只读写 `m018_assert` fixture，最后清理；可重复执行。
\set ON_ERROR_STOP on

DO $$
BEGIN
    IF to_regprocedure('public.identity_session_meta_cleanup()') IS NULL THEN
        RAISE EXCEPTION 'M018: 缺函数 identity_session_meta_cleanup()';
    END IF;
    IF NOT EXISTS (
        SELECT 1
        FROM pg_trigger
        WHERE tgname = 'identity_session_meta_cleanup'
          AND tgrelid = 'public.identity_session_token'::regclass
    ) THEN
        RAISE EXCEPTION 'M018: identity_session_token 上缺清理触发器';
    END IF;
END $$;

INSERT INTO identity_user(user_id, name, pw_hash, created_at)
VALUES ('m018_assert', 'm018_assert', 'phc', 1)
ON CONFLICT (user_id) DO NOTHING;

INSERT INTO identity_session_token(
    token, user_id, created_at, expires_at, sid,
    device_id, device_name, ip, last_seen, audience,
    auth_level, features, serial, issued_at
) VALUES
    ('m018_t1', 'm018_assert', 100, 200, 'm018_shared', 'd', '', '127.0.0.1', 100, 'user', 'auth', '', 0, 100),
    ('m018_t2', 'm018_assert', 100, 200, 'm018_shared', 'd', '', '127.0.0.1', 100, 'user', 'auth', '', 0, 100)
ON CONFLICT (token) DO NOTHING;

INSERT INTO identity_session_meta(
    sid, user_id, auth_level, features, serial, issued_at, expires_at
) VALUES ('m018_shared', 'm018_assert', 'auth', '', 0, 100, 200)
ON CONFLICT (sid) DO NOTHING;

DELETE FROM identity_session_token WHERE token = 'm018_t1';
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM identity_session_meta WHERE sid = 'm018_shared'
    ) THEN
        RAISE EXCEPTION 'M018: 同 sid 还有 token 时侧车被提前删除';
    END IF;
END $$;

DELETE FROM identity_session_token WHERE token = 'm018_t2';
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM identity_session_meta WHERE sid = 'm018_shared'
    ) THEN
        RAISE EXCEPTION 'M018: 最后一个 token 删除后侧车仍然存在';
    END IF;
END $$;

DELETE FROM identity_user WHERE user_id = 'm018_assert';
