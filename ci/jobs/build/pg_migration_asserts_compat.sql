-- G00-04 S2 / M011-M013 兼容层与账本回填断言（CI 可执行）。
--
-- 执行前提：CI 的 pg-migration 作业已连续执行 M010、M011、M012、M013 各两次。
-- 本文件只做只读断言 + 可重跑的最小 fixtures；不修改迁移 DDL。
--
-- 覆盖：
--   1. identity_* / media_* / migration_* 关键对象存在；
--   2. users / sessions / media 三个兼容视图的 INSERT/UPDATE/DELETE 触发器存在；
--   3. users 视图写入命中 identity_user，sessions 写入命中 identity_session_token；
--   4. media 视图普通写入命中 media_object，重复写入保留第一次 owner；
--   5. M013 staging + migrate_wallets_batch 能把 100 分迁移成 wallet_balances=100，
--      且二次 CALL 不重复记账。
\set ON_ERROR_STOP on

-- ---------------------------------------------------------------------------
-- 1. 对象存在
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    missing text;
BEGIN
    SELECT string_agg(wanted.name, ', ' ORDER BY wanted.name) INTO missing
    FROM (VALUES
        ('identity_user'), ('identity_session_token'), ('identity_refresh'),
        ('identity_user_governance'), ('identity_governance_audit'),
        ('identity_governance_label'), ('media_object'), ('media_ref'),
        ('migration_wallets'), ('migration_ledger')
    ) AS wanted(name)
    WHERE to_regclass('public.' || wanted.name) IS NULL;
    IF missing IS NOT NULL THEN
        RAISE EXCEPTION 'M011-M013: 缺表: %', missing;
    END IF;

    SELECT string_agg(wanted.name, ', ' ORDER BY wanted.name) INTO missing
    FROM (VALUES ('users'), ('sessions'), ('media'), ('wallet_balances')) AS wanted(name)
    WHERE to_regclass('public.' || wanted.name) IS NULL;
    IF missing IS NOT NULL THEN
        RAISE EXCEPTION 'M011-M013: 缺视图: %', missing;
    END IF;
END $$;

DO $$
DECLARE
    missing text;
BEGIN
    SELECT string_agg(wanted.name, ', ' ORDER BY wanted.name) INTO missing
    FROM (VALUES
        ('users_insert_trg'), ('users_update_trg'), ('users_delete_trg'),
        ('sessions_insert_trg'), ('sessions_update_trg'), ('sessions_delete_trg'),
        ('media_insert_trg'), ('media_update_trg'), ('media_delete_trg')
    ) AS wanted(name)
    WHERE NOT EXISTS (
        SELECT 1 FROM pg_trigger
        WHERE tgname = wanted.name AND NOT tgisinternal
    );
    IF missing IS NOT NULL THEN
        RAISE EXCEPTION 'M011-M013: 缺触发器: %', missing;
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_proc p
        JOIN pg_namespace n ON n.oid = p.pronamespace
        WHERE n.nspname = 'public' AND p.proname = 'migrate_wallets_batch'
    ) THEN
        RAISE EXCEPTION 'M013: 缺过程 migrate_wallets_batch';
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- 2. users 兼容视图 -> identity_user
-- ---------------------------------------------------------------------------
DELETE FROM sessions WHERE user_id = 'ci-compat-u1';
DELETE FROM users WHERE id = 'ci-compat-u1';

INSERT INTO users(id, name, pw_hash, created_at)
VALUES ('ci-compat-u1', 'ci-compat-alice', 'phc', 1);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM identity_user
        WHERE user_id = 'ci-compat-u1' AND name = 'ci-compat-alice'
    ) THEN
        RAISE EXCEPTION 'M011: users INSERT 未命中 identity_user';
    END IF;
END $$;

UPDATE users SET name = 'ci-compat-alice2', signature = 'sig'
WHERE id = 'ci-compat-u1';

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM identity_user
        WHERE user_id = 'ci-compat-u1' AND name = 'ci-compat-alice2'
          AND signature = 'sig'
    ) THEN
        RAISE EXCEPTION 'M011: users UPDATE 未命中 identity_user';
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- 3. sessions 兼容视图 -> identity_session_token
-- ---------------------------------------------------------------------------
DELETE FROM sessions WHERE token = 'ci-compat-t1';

INSERT INTO sessions(token, user_id, created_at, expires_at, sid,
                     device_id, device_name, ip, last_seen, audience)
VALUES ('ci-compat-t1', 'ci-compat-u1', 1, 100, 'ci-compat-sid',
        'd1', 'phone', '127.0.0.1', 1, 'user');

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM identity_session_token
        WHERE token = 'ci-compat-t1' AND user_id = 'ci-compat-u1'
          AND sid = 'ci-compat-sid' AND audience = 'user'
    ) THEN
        RAISE EXCEPTION 'M011: sessions INSERT 未命中 identity_session_token';
    END IF;
END $$;

UPDATE sessions SET last_seen = 50 WHERE token = 'ci-compat-t1';

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM identity_session_token
        WHERE token = 'ci-compat-t1' AND last_seen = 50
    ) THEN
        RAISE EXCEPTION 'M011: sessions UPDATE 未命中 identity_session_token';
    END IF;
END $$;

DELETE FROM sessions WHERE token = 'ci-compat-t1';
DELETE FROM users WHERE id = 'ci-compat-u1';

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM identity_session_token WHERE token = 'ci-compat-t1') THEN
        RAISE EXCEPTION 'M011: sessions DELETE 未命中 identity_session_token';
    END IF;
    IF EXISTS (SELECT 1 FROM identity_user WHERE user_id = 'ci-compat-u1') THEN
        RAISE EXCEPTION 'M011: users DELETE 未命中 identity_user';
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- 4. media 兼容视图 -> media_object
-- ---------------------------------------------------------------------------
DELETE FROM media WHERE id = 'ci-compat-m1';

INSERT INTO media(id, size, content_type, uploader, conv_id, public, has_thumb, created_at)
VALUES ('ci-compat-m1', 12, 'image/png', 'ci-compat-u2', 'ci-compat-c1', 0, 0, 10);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM media_object
        WHERE object_id = 'ci-compat-m1' AND owner = 'ci-compat-u2'
          AND size = 12 AND mime = 'image/png'
    ) THEN
        RAISE EXCEPTION 'M012: media INSERT 未命中 media_object';
    END IF;
END $$;

-- 重复插入：只更新 size/mime，owner 保留第一次值。
INSERT INTO media(id, size, content_type, uploader, conv_id, public, has_thumb, created_at)
VALUES ('ci-compat-m1', 13, 'image/jpeg', 'ci-compat-u3', 'ci-compat-c2', 1, 0, 10);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM media_object
        WHERE object_id = 'ci-compat-m1' AND owner = 'ci-compat-u2'
          AND size = 13 AND mime = 'image/jpeg'
    ) THEN
        RAISE EXCEPTION 'M012: 重复写入未保留第一次 owner 或未更新 size/mime';
    END IF;
END $$;

DELETE FROM media WHERE id = 'ci-compat-m1';

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM media_object WHERE object_id = 'ci-compat-m1') THEN
        RAISE EXCEPTION 'M012: media DELETE 未命中 media_object';
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- 5. M013 分批回填
-- ---------------------------------------------------------------------------
DELETE FROM migration_ledger WHERE id = 9001;
DELETE FROM migration_wallets WHERE user_id = 'ci-compat-w1';

INSERT INTO migration_wallets(user_id, balance_minor)
VALUES ('ci-compat-w1', 100);

INSERT INTO migration_ledger(id, user_id, kind, amount_minor, counterparty, ref_id, memo, created_at)
VALUES (9001, 'ci-compat-w1', 'fund_in', 100, '', 'ci-r1', 'ci seed', 10);

CALL migrate_wallets_batch(10);

DO $$
BEGIN
    IF (SELECT balance FROM wallet_balances WHERE user_id = 'ci-compat-w1') IS DISTINCT FROM 100 THEN
        RAISE EXCEPTION 'M013: wallet_balances 不是 100';
    END IF;
    IF EXISTS (SELECT 1 FROM migration_wallets WHERE user_id = 'ci-compat-w1' AND status = 'pending') THEN
        RAISE EXCEPTION 'M013: 仍有 pending 钱包';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM ledger_journals WHERE business_key = 'mig:ledger:9001'
    ) THEN
        RAISE EXCEPTION 'M013: 缺 journal mig:ledger:9001';
    END IF;
END $$;

-- 二次 CALL 必须幂等：journal 仍只有一条。
CALL migrate_wallets_batch(10);

DO $$
BEGIN
    IF (SELECT count(*) FROM ledger_journals WHERE business_key = 'mig:ledger:9001') <> 1 THEN
        RAISE EXCEPTION 'M013: 二次 CALL 重复记账';
    END IF;
END $$;
