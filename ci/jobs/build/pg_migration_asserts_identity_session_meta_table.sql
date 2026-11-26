-- M017 断言：identity_session_meta 表/索引存在，且旧访问表的侧车行可回填。
-- 只读写一条 `m017_assert` fixture，最后删除；可重复执行。
\set ON_ERROR_STOP on

DO $$
DECLARE
    missing text;
BEGIN
    SELECT string_agg(wanted.name, ', ' ORDER BY wanted.name) INTO missing
    FROM (VALUES
        ('identity_session_meta'),
        ('idx_identity_session_meta_user'),
        ('idx_identity_session_meta_expiry')
    ) AS wanted(name)
    WHERE to_regclass('public.' || wanted.name) IS NULL;
    IF missing IS NOT NULL THEN
        RAISE EXCEPTION 'M017: 缺对象: %', missing;
    END IF;
END $$;

INSERT INTO identity_user(user_id, name, pw_hash, created_at)
VALUES ('m017_assert', 'm017_assert', 'phc', 1)
ON CONFLICT (user_id) DO NOTHING;

INSERT INTO identity_session_meta(
    sid, user_id, auth_level, features, serial, issued_at, expires_at
)
VALUES ('m017_assert', 'm017_assert', 'auth', '', 0, 100, 200)
ON CONFLICT (sid) DO NOTHING;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM identity_session_meta WHERE sid = 'm017_assert'
    ) THEN
        RAISE EXCEPTION 'M017: 侧车表不可写';
    END IF;
    IF EXISTS (
        SELECT 1 FROM identity_session_token WHERE token = 'm017_backfill_token'
    ) AND NOT EXISTS (
        SELECT 1 FROM identity_session_meta
        WHERE sid = 'm017_backfill_sid'
          AND user_id = 'm017_backfill'
          AND auth_level = 'root'
          AND features = 'V'
          AND serial = 7
          AND issued_at = 100
          AND expires_at = 200
    ) THEN
        RAISE EXCEPTION 'M017: 旧访问表侧车行没有回填到 identity_session_meta';
    END IF;
END $$;

DELETE FROM identity_session_meta WHERE sid = 'm017_assert';
DELETE FROM identity_user WHERE user_id = 'm017_assert';
DELETE FROM identity_session_token WHERE token = 'm017_backfill_token';
DELETE FROM identity_user WHERE user_id = 'm017_backfill';
