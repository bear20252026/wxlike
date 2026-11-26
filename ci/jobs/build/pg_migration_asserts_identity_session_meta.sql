-- M016 断言：identity_session_token 的侧车四列存在，默认值可读。
-- 只读系统目录，不写业务行；可重复执行。
\set ON_ERROR_STOP on

DO $$
DECLARE
    missing text;
BEGIN
    SELECT string_agg(wanted.name, ', ' ORDER BY wanted.name) INTO missing
    FROM (VALUES
        ('auth_level'),
        ('features'),
        ('serial'),
        ('issued_at')
    ) AS wanted(name)
    WHERE NOT EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_schema = 'public'
          AND table_name = 'identity_session_token'
          AND column_name = wanted.name
    );
    IF missing IS NOT NULL THEN
        RAISE EXCEPTION 'M016: identity_session_token 缺列: %', missing;
    END IF;
END $$;

DO $$
DECLARE
    defaults_ok boolean;
BEGIN
    SELECT bool_and(
        column_default IS NOT NULL
        AND is_nullable = 'NO'
    ) INTO defaults_ok
    FROM information_schema.columns
    WHERE table_schema = 'public'
      AND table_name = 'identity_session_token'
      AND column_name IN ('auth_level', 'features', 'serial', 'issued_at');
    IF defaults_ok IS DISTINCT FROM true THEN
        RAISE EXCEPTION 'M016: 侧车列必须是 NOT NULL 且有默认值';
    END IF;
END $$;
