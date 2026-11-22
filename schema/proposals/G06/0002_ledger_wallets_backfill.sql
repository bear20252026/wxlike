-- M013（草案）— wallet / ledger 存量数据迁移（PostgreSQL）
--
-- 目标：把 SQLite 的单边钱包流水（`wallets.balance` + `ledger` 行）导入
-- G06 的双分录账本，同时保留“余额只由分录聚合、不存余额列”的底线。
--
-- 分两段：
--   1. 导入器先把 SQLite 的 `wallets` / `ledger` 行装进 staging 表
--      `migration_wallets` / `migration_ledger`（见 deploy/runbooks 的
--      sqlite3 -csv + psql \copy 步骤；本文件不直接连 SQLite）。
--   2. 反复 CALL migrate_wallets_batch(批次大小)，每批一个事务；中断后重跑
--      只会跳过已完成的 journal（business_key 幂等），不会重复记账。
--
-- 记账口径（第一版，严格可核对）：
--   每一条 legacy `ledger` 行 -> 一笔独立 journal，两条分录：
--     * `wallet:<user_id>`：按 kind 取符号（transfer_out / redpacket_out 为负，
--       其余为正）；
--     * `migration:clearing`：金额取反，保证 SUM(amount_minor)=0。
--   这样每一行都能追到原 ledger.id，钱包余额 = 该用户全部分录之和。
--   不把 transfer_out / transfer_in 合并成一笔：合并会改变行数与对账口径，
--   第一阶段先保“逐行可追溯”，后续再做语义合并。
--
-- 期初差额：
--   若 `wallets.balance` 与 `ledger` 行之和不等，默认**中止**（严格模式）；
--   只有显式 p_allow_adjustment=true 才补一笔 `migration.opening_adjustment`
--   journal，并把 staging 行标成 adjusted。严禁静默改余额。
--
-- 回滚：staging 表与 migration_* 视图可整体删除；已写入 ledger 的 journal /
-- entries 是 append-only，不能回滚，必须先导出再人工冲正。正式执行前必须
-- 在空库/合成库跑过两次（第二次应为 0 新增）。

BEGIN;

-- ---------------------------------------------------------------------------
-- 1. staging（导入器写入；不参与线上查询）
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS migration_wallets (
    user_id          TEXT PRIMARY KEY,
    balance_minor    BIGINT NOT NULL,
    ledger_sum_minor BIGINT,
    status           TEXT NOT NULL DEFAULT 'pending'
                     CHECK (status IN ('pending', 'migrated', 'adjusted')),
    migrated_at      BIGINT
);
CREATE INDEX IF NOT EXISTS idx_migration_wallets_status
    ON migration_wallets(status, user_id);

CREATE TABLE IF NOT EXISTS migration_ledger (
    id           BIGINT PRIMARY KEY,
    user_id      TEXT NOT NULL,
    kind         TEXT NOT NULL
                 CHECK (kind IN ('fund_in', 'transfer_in', 'transfer_out',
                                 'redpacket_out', 'redpacket_in', 'redpacket_refund')),
    amount_minor BIGINT NOT NULL CHECK (amount_minor > 0),
    counterparty TEXT NOT NULL DEFAULT '',
    ref_id       TEXT NOT NULL DEFAULT '',
    memo         TEXT NOT NULL DEFAULT '',
    created_at   BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_migration_ledger_user
    ON migration_ledger(user_id, id);

-- ---------------------------------------------------------------------------
-- 2. 余额展示：实时聚合视图（不建物化表）
-- ---------------------------------------------------------------------------

CREATE OR REPLACE VIEW wallet_balances AS
SELECT
    a.owner_id AS user_id,
    COALESCE(SUM(e.amount_minor), 0)::bigint AS balance
FROM ledger_accounts a
LEFT JOIN ledger_entries e
       ON e.account = a.name
      AND e.currency = a.currency
WHERE a.kind = 'wallet'
  AND a.owner_id <> ''
GROUP BY a.owner_id;

COMMENT ON VIEW wallet_balances IS
    '实时余额 = ledger_entries.amount_minor 按 wallet 账户聚合；不存余额列。';

-- ---------------------------------------------------------------------------
-- 3. 批次迁移过程
-- ---------------------------------------------------------------------------

CREATE OR REPLACE PROCEDURE migrate_wallets_batch(
    p_batch_size INTEGER DEFAULT 1000,
    p_allow_adjustment BOOLEAN DEFAULT FALSE
)
LANGUAGE plpgsql
AS $$
DECLARE
    v_user       TEXT;
    v_balance    BIGINT;
    v_sum        BIGINT;
    v_adjust     BIGINT;
    v_row        RECORD;
    v_journal_id TEXT;
    v_inserted   TEXT;
    v_count      INTEGER;
    v_signed     BIGINT;
    v_account    TEXT;
    c_clearing   CONSTANT TEXT := 'migration:clearing';
    c_currency   CONSTANT TEXT := 'CNY';
    c_asset      CONSTANT TEXT := 'CNY/2';
BEGIN
    IF p_batch_size < 1 OR p_batch_size > 10000 THEN
        RAISE EXCEPTION 'p_batch_size must be between 1 and 10000';
    END IF;

    -- 清账账户只建一次；kind/fund_class 都在 G06 的 CHECK 集合内。
    INSERT INTO ledger_accounts (name, kind, currency, fund_class, owner_id, created_at)
    VALUES (c_clearing, 'clearing', c_currency, 'virtual', '', 0)
    ON CONFLICT (name) DO NOTHING;

    LOOP
        v_user := NULL;
        SELECT w.user_id, w.balance_minor
          INTO v_user, v_balance
          FROM migration_wallets w
         WHERE w.status = 'pending'
         ORDER BY w.user_id
         LIMIT p_batch_size;

        EXIT WHEN v_user IS NULL;

        SELECT COALESCE(SUM(
                   CASE
                       WHEN l.kind IN ('transfer_out', 'redpacket_out') THEN -l.amount_minor
                       ELSE l.amount_minor
                   END
               ), 0)
          INTO v_sum
          FROM migration_ledger l
         WHERE l.user_id = v_user;

        IF v_sum <> v_balance THEN
            IF NOT p_allow_adjustment THEN
                RAISE EXCEPTION
                    'wallet % strict check failed: balance=% ledger_sum=%',
                    v_user, v_balance, v_sum;
            END IF;
            v_adjust := v_balance - v_sum;
        ELSE
            v_adjust := 0;
        END IF;

        v_account := 'wallet:' || v_user;
        INSERT INTO ledger_accounts (name, kind, currency, fund_class, owner_id, created_at)
        VALUES (v_account, 'wallet', c_currency, 'virtual', v_user, 0)
        ON CONFLICT (name) DO NOTHING;

        -- 逐条 legacy ledger -> 一笔 journal + 两条分录。
        FOR v_row IN
            SELECT id, kind, amount_minor, memo, created_at
              FROM migration_ledger
             WHERE user_id = v_user
             ORDER BY id
        LOOP
            v_journal_id := 'mig:ledger:' || v_row.id::text;
            v_signed := CASE
                WHEN v_row.kind IN ('transfer_out', 'redpacket_out') THEN -v_row.amount_minor
                ELSE v_row.amount_minor
            END;

            v_inserted := NULL;
            INSERT INTO ledger_journals (
                id, business_key, kind, currency, asset, metadata,
                entry_count, request_digest, created_at
            ) VALUES (
                v_journal_id, v_journal_id, 'migration.ledger', c_currency, c_asset, '',
                2, md5(v_journal_id), v_row.created_at
            )
            ON CONFLICT (business_key) DO NOTHING
            RETURNING id INTO v_inserted;

            SELECT COUNT(*) INTO v_count
              FROM ledger_entries
             WHERE journal_id = v_journal_id;

            IF v_count = 0 THEN
                INSERT INTO ledger_entries (
                    journal_id, seq, account, amount_minor, currency, memo, created_at
                ) VALUES
                    (v_journal_id, 0, v_account, v_signed, c_currency,
                     v_row.kind || ':' || v_row.memo, v_row.created_at),
                    (v_journal_id, 1, c_clearing, -v_signed, c_currency,
                     'counterpart:' || v_row.kind, v_row.created_at);
            ELSIF v_count <> 2 THEN
                RAISE EXCEPTION
                    'journal % has % entries; expected 2', v_journal_id, v_count;
            END IF;
        END LOOP;

        -- 余额与流水不等的期初差额，必须显式授权才补。
        IF v_adjust <> 0 THEN
            v_journal_id := 'mig:wallet:' || v_user || ':opening';
            v_inserted := NULL;
            INSERT INTO ledger_journals (
                id, business_key, kind, currency, asset, metadata,
                entry_count, request_digest, created_at
            ) VALUES (
                v_journal_id, v_journal_id, 'migration.opening_adjustment',
                c_currency, c_asset, '', 2, md5(v_journal_id), 0
            )
            ON CONFLICT (business_key) DO NOTHING
            RETURNING id INTO v_inserted;

            SELECT COUNT(*) INTO v_count
              FROM ledger_entries
             WHERE journal_id = v_journal_id;

            IF v_count = 0 THEN
                INSERT INTO ledger_entries (
                    journal_id, seq, account, amount_minor, currency, memo, created_at
                ) VALUES
                    (v_journal_id, 0, v_account, v_adjust, c_currency,
                     'opening adjustment', 0),
                    (v_journal_id, 1, c_clearing, -v_adjust, c_currency,
                     'opening adjustment counterpart', 0);
            ELSIF v_count <> 2 THEN
                RAISE EXCEPTION
                    'journal % has % entries; expected 2', v_journal_id, v_count;
            END IF;
        END IF;

        UPDATE migration_wallets
           SET ledger_sum_minor = v_sum,
               status = CASE WHEN v_adjust <> 0 THEN 'adjusted' ELSE 'migrated' END,
               migrated_at = 0
         WHERE user_id = v_user;

        -- 每批一个事务。CALL 必须在 autocommit 的 psql 会话里执行；
        -- 不要在显式 BEGIN 块里 CALL。
        COMMIT;
    END LOOP;
END;
$$;

COMMENT ON PROCEDURE migrate_wallets_batch(INTEGER, BOOLEAN) IS
    '批量把 migration_ledger 导入 ledger_journals/entries；每批一个事务，business_key 幂等。';

COMMIT;

-- 调用示例（psql，autocommit）：
--   \copy migration_wallets FROM 'wallets.csv' CSV HEADER
--   \copy migration_ledger  FROM 'ledger.csv'  CSV HEADER
--   CALL migrate_wallets_batch(1000);              -- 严格模式，余额不等即中止
--   CALL migrate_wallets_batch(1000, true);        -- 允许补期初差额并标 adjusted
--   SELECT count(*) FROM migration_wallets WHERE status = 'pending';  -- 应为 0
--   SELECT * FROM wallet_balances ORDER BY user_id LIMIT 10;
--
-- 也可用导入器（SQLite 直读、CSV + COPY + CALL 一条链路）：
--   python schema/proposals/G06/import_ledger_wallets.py \
--     --sqlite /data/wxlike.db \
--     --dsn "$WXLIKE_PG_URL" \
--     --batch-size 1000
--   加 --allow-adjustment 走补差路径；加 --reset 先清 staging（不动已入账 journal）。

-- 回滚边界：
--   DROP PROCEDURE IF EXISTS migrate_wallets_batch(INTEGER, BOOLEAN);
--   DROP VIEW IF EXISTS wallet_balances;
--   DROP TABLE IF EXISTS migration_ledger;
--   DROP TABLE IF EXISTS migration_wallets;
-- 已写入的 ledger_journals/entries 不能删（append-only），只能另记反向分录。
