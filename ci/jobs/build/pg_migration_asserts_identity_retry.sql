-- M015 断言：identity_retry 表、列与过期索引存在，且 upsert 语义可用。
-- 只读写一条 `m015_assert` fixture，最后删除；可重复执行。
\set ON_ERROR_STOP on

DO $$
DECLARE
    missing text;
BEGIN
    SELECT string_agg(wanted.name, ', ' ORDER BY wanted.name) INTO missing
    FROM (VALUES
        ('identity_retry'),
        ('idx_identity_retry_expiry')
    ) AS wanted(name)
    WHERE to_regclass('public.' || wanted.name) IS NULL;
    IF missing IS NOT NULL THEN
        RAISE EXCEPTION 'M015: 缺对象: %', missing;
    END IF;
END $$;

INSERT INTO identity_retry(bucket, attempts, created_at, updated_at, expires_at)
VALUES ('m015_assert', 1, 10, 10, 1000)
ON CONFLICT (bucket) DO UPDATE SET
    attempts = identity_retry.attempts + 1,
    updated_at = EXCLUDED.updated_at;

INSERT INTO identity_retry(bucket, attempts, created_at, updated_at, expires_at)
VALUES ('m015_assert', 1, 10, 20, 1000)
ON CONFLICT (bucket) DO UPDATE SET
    attempts = identity_retry.attempts + 1,
    updated_at = EXCLUDED.updated_at;

DO $$
BEGIN
    IF (SELECT attempts FROM identity_retry WHERE bucket = 'm015_assert') IS DISTINCT FROM 2 THEN
        RAISE EXCEPTION 'M015: upsert 未累加 attempts';
    END IF;
END $$;

DELETE FROM identity_retry WHERE bucket = 'm015_assert';
