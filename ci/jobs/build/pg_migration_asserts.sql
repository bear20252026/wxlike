-- G00-04 / M010 PostgreSQL 迁移冒烟断言（CI 可执行，只读）。
--
-- 执行前提：schema/proposals/G06/0001_ledger_commerce.sql 已被 psql 执行过
-- （CI 的 pg-migration 作业会连跑两次以验证可重入）。本文件不建表、不改结构，
-- 只做存在性与不可变性断言；任何一条失败都会 RAISE EXCEPTION，配合
-- `psql -v ON_ERROR_STOP=1` 让作业非零退出。
--
-- 断言口径：
--   1. ledger_* / commerce_* 关键表存在；
--   2. 四张不可变表的触发器与四个触发函数存在；
--   3. ledger_closing_balances 绑定 ledger_closings_immutable()（提案 :159-162）；
--   4. 不可变表的 UPDATE / DELETE 被触发器拒绝。行级触发器只在有行时触发，
--      所以第 0 步先插入最小 fixtures（重跑安全：全部 ON CONFLICT DO NOTHING）。

\set ON_ERROR_STOP on

-- ---------------------------------------------------------------------------
-- 0. 最小 fixtures
-- ---------------------------------------------------------------------------
INSERT INTO ledger_accounts(name, kind, currency, fund_class, created_at)
VALUES ('m010_assert_acct', 'wallet', 'CNY', 'virtual', 0)
ON CONFLICT (name) DO NOTHING;

INSERT INTO ledger_journals(id, business_key, kind, currency, request_digest, entry_count, created_at)
VALUES ('m010_assert_journal', 'm010_assert_bk', 'assert', 'CNY', 'm010_assert_digest', 2, 0)
ON CONFLICT (id) DO NOTHING;

INSERT INTO ledger_entries(journal_id, seq, account, amount_minor, currency, created_at)
VALUES ('m010_assert_journal', 1, 'm010_assert_acct', 100, 'CNY', 0)
ON CONFLICT (journal_id, seq) DO NOTHING;

INSERT INTO ledger_entries(journal_id, seq, account, amount_minor, currency, created_at)
VALUES ('m010_assert_journal', 2, 'm010_assert_acct', -100, 'CNY', 0)
ON CONFLICT (journal_id, seq) DO NOTHING;

INSERT INTO ledger_closings(day, currency, start_ms, end_ms, debit_minor, credit_minor,
                            entry_count, locked_minor, balance_total, checksum, closed_at)
VALUES ('1970-01-01', 'CNY', 0, 0, 100, -100, 2, 0, 0, 'm010_assert_checksum', 0)
ON CONFLICT (day, currency) DO NOTHING;

INSERT INTO ledger_closing_balances(day, currency, account, balance_minor)
VALUES ('1970-01-01', 'CNY', 'm010_assert_acct', 0)
ON CONFLICT (day, currency, account) DO NOTHING;

-- ---------------------------------------------------------------------------
-- 1. 关键表存在
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    missing text;
BEGIN
    SELECT string_agg(wanted.name, ', ' ORDER BY wanted.name) INTO missing
    FROM (VALUES
        ('ledger_accounts'), ('ledger_journals'), ('ledger_entries'),
        ('ledger_holds'), ('ledger_closings'), ('ledger_closing_balances'),
        ('commerce_meta'), ('commerce_payment_intents'), ('commerce_payment_events'),
        ('commerce_provider_callbacks'), ('commerce_cards'), ('commerce_topups'),
        ('commerce_withdrawals'), ('commerce_refunds'),
        ('commerce_reconciliation_exceptions'), ('commerce_transfers'),
        ('commerce_nonces'), ('commerce_redpackets'), ('commerce_redpacket_shares'),
        ('commerce_redpacket_claims'), ('commerce_entitlement_rules'),
        ('commerce_entitlements'), ('commerce_redemption_tokens'),
        ('commerce_redemptions'), ('commerce_orders'), ('commerce_outbox')
    ) AS wanted(name)
    WHERE to_regclass('public.' || wanted.name) IS NULL;
    IF missing IS NOT NULL THEN
        RAISE EXCEPTION 'M010: 缺表: %', missing;
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- 2. 触发器存在（用户触发器，不含内部触发器）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    missing text;
BEGIN
    SELECT string_agg(wanted.name, ', ' ORDER BY wanted.name) INTO missing
    FROM (VALUES
        ('trg_ledger_entries_immutable'),
        ('trg_ledger_entries_frozen_set'),
        ('trg_ledger_journals_immutable'),
        ('trg_ledger_closings_immutable'),
        ('trg_ledger_closing_balances_immutable')
    ) AS wanted(name)
    WHERE NOT EXISTS (
        SELECT 1 FROM pg_trigger
        WHERE tgname = wanted.name AND NOT tgisinternal
    );
    IF missing IS NOT NULL THEN
        RAISE EXCEPTION 'M010: 缺触发器: %', missing;
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- 3. 触发函数存在
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    missing text;
BEGIN
    SELECT string_agg(wanted.name, ', ' ORDER BY wanted.name) INTO missing
    FROM (VALUES
        ('ledger_entries_immutable'),
        ('ledger_entries_frozen_set'),
        ('ledger_journals_immutable'),
        ('ledger_closings_immutable')
    ) AS wanted(name)
    WHERE NOT EXISTS (
        SELECT 1 FROM pg_proc p
        JOIN pg_namespace n ON n.oid = p.pronamespace
        WHERE n.nspname = 'public' AND p.proname = wanted.name
    );
    IF missing IS NOT NULL THEN
        RAISE EXCEPTION 'M010: 缺函数: %', missing;
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- 4. ledger_closing_balances 复用 ledger_closings_immutable()
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    expected oid;
    actual oid;
BEGIN
    SELECT p.oid INTO expected
    FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'public' AND p.proname = 'ledger_closings_immutable';

    SELECT tgfoid INTO actual
    FROM pg_trigger
    WHERE tgname = 'trg_ledger_closing_balances_immutable'
      AND tgrelid = 'public.ledger_closing_balances'::regclass
      AND NOT tgisinternal;

    IF actual IS DISTINCT FROM expected THEN
        RAISE EXCEPTION 'M010: trg_ledger_closing_balances_immutable 未绑定 ledger_closings_immutable()';
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- 5. 不可变表：UPDATE / DELETE 必须被触发器拒绝
-- ---------------------------------------------------------------------------
DO $$
BEGIN
    BEGIN
        UPDATE ledger_entries SET memo = 'm010_tamper';
        RAISE EXCEPTION 'ledger_entries UPDATE 未被拒绝';
    EXCEPTION
        WHEN OTHERS THEN
            IF SQLERRM NOT LIKE '%ledger_entries is append-only%' THEN
                RAISE EXCEPTION 'ledger_entries UPDATE 被拒绝但报错不符: %', SQLERRM;
            END IF;
    END;
    BEGIN
        DELETE FROM ledger_entries;
        RAISE EXCEPTION 'ledger_entries DELETE 未被拒绝';
    EXCEPTION
        WHEN OTHERS THEN
            IF SQLERRM NOT LIKE '%ledger_entries is append-only%' THEN
                RAISE EXCEPTION 'ledger_entries DELETE 被拒绝但报错不符: %', SQLERRM;
            END IF;
    END;
END $$;

DO $$
BEGIN
    BEGIN
        UPDATE ledger_journals SET kind = 'm010_tamper';
        RAISE EXCEPTION 'ledger_journals UPDATE 未被拒绝';
    EXCEPTION
        WHEN OTHERS THEN
            IF SQLERRM NOT LIKE '%ledger_journals is append-only%' THEN
                RAISE EXCEPTION 'ledger_journals UPDATE 被拒绝但报错不符: %', SQLERRM;
            END IF;
    END;
    BEGIN
        DELETE FROM ledger_journals;
        RAISE EXCEPTION 'ledger_journals DELETE 未被拒绝';
    EXCEPTION
        WHEN OTHERS THEN
            IF SQLERRM NOT LIKE '%ledger_journals is append-only%' THEN
                RAISE EXCEPTION 'ledger_journals DELETE 被拒绝但报错不符: %', SQLERRM;
            END IF;
    END;
END $$;

DO $$
BEGIN
    BEGIN
        UPDATE ledger_closings SET checksum = 'm010_tamper';
        RAISE EXCEPTION 'ledger_closings UPDATE 未被拒绝';
    EXCEPTION
        WHEN OTHERS THEN
            IF SQLERRM NOT LIKE '%ledger_closings is an immutable snapshot%' THEN
                RAISE EXCEPTION 'ledger_closings UPDATE 被拒绝但报错不符: %', SQLERRM;
            END IF;
    END;
    BEGIN
        DELETE FROM ledger_closings;
        RAISE EXCEPTION 'ledger_closings DELETE 未被拒绝';
    EXCEPTION
        WHEN OTHERS THEN
            IF SQLERRM NOT LIKE '%ledger_closings is an immutable snapshot%' THEN
                RAISE EXCEPTION 'ledger_closings DELETE 被拒绝但报错不符: %', SQLERRM;
            END IF;
    END;
END $$;

DO $$
BEGIN
    BEGIN
        UPDATE ledger_closing_balances SET balance_minor = 1;
        RAISE EXCEPTION 'ledger_closing_balances UPDATE 未被拒绝';
    EXCEPTION
        WHEN OTHERS THEN
            IF SQLERRM NOT LIKE '%ledger_closings is an immutable snapshot%' THEN
                RAISE EXCEPTION 'ledger_closing_balances UPDATE 被拒绝但报错不符: %', SQLERRM;
            END IF;
    END;
    BEGIN
        DELETE FROM ledger_closing_balances;
        RAISE EXCEPTION 'ledger_closing_balances DELETE 未被拒绝';
    EXCEPTION
        WHEN OTHERS THEN
            IF SQLERRM NOT LIKE '%ledger_closings is an immutable snapshot%' THEN
                RAISE EXCEPTION 'ledger_closing_balances DELETE 被拒绝但报错不符: %', SQLERRM;
            END IF;
    END;
END $$;
