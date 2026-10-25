-- G06 迁移草稿：不可变账本 + commerce 业务表（PostgreSQL）
--
-- 本文件是无编号提案，由 G00 分配唯一迁移编号后串行落主干。
-- 采用 expand -> backfill -> switch -> contract；本提案只包含 expand 阶段：
-- 全部为新增表/索引，不修改既有表，旧二进制可继续运行。
--
-- 与 SQLite 实现（crates/ledger/src/schema.rs、crates/features/commerce/src/schema.rs）
-- 字段一一对应。金额/时间为 BIGINT：金额是最小单位整数，时间是 Unix 毫秒。
-- 不在数据库里存任何余额列；余额只能由 ledger_entries 聚合导出。

BEGIN;

-- ---------------------------------------------------------------------------
-- 账本（G06-01）
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS ledger_accounts (
    name          TEXT PRIMARY KEY,
    kind          TEXT NOT NULL CHECK (kind IN ('wallet', 'advertiser', 'marketing', 'revenue', 'escrow', 'clearing')),
    currency      TEXT NOT NULL,
    fund_class    TEXT NOT NULL CHECK (fund_class IN ('virtual', 'real')),
    owner_id      TEXT NOT NULL DEFAULT '',
    created_at    BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ledger_accounts_owner
    ON ledger_accounts(owner_id, currency);

CREATE TABLE IF NOT EXISTS ledger_journals (
    id             TEXT PRIMARY KEY,
    business_key   TEXT NOT NULL UNIQUE,
    kind           TEXT NOT NULL,
    currency       TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    created_at     BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS ledger_entries (
    id           BIGSERIAL PRIMARY KEY,
    journal_id   TEXT NOT NULL REFERENCES ledger_journals(id),
    seq          INTEGER NOT NULL,
    account      TEXT NOT NULL REFERENCES ledger_accounts(name),
    amount_minor BIGINT NOT NULL CHECK (amount_minor <> 0),
    currency     TEXT NOT NULL,
    memo         TEXT NOT NULL DEFAULT '',
    created_at   BIGINT NOT NULL,
    UNIQUE (journal_id, seq)
);
CREATE INDEX IF NOT EXISTS idx_ledger_entries_account
    ON ledger_entries(account, id);
CREATE INDEX IF NOT EXISTS idx_ledger_entries_created
    ON ledger_entries(created_at, currency);

-- 分录不可编辑：纠错只能追加反向分录。
CREATE OR REPLACE FUNCTION ledger_entries_immutable() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'ledger_entries is append-only';
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_ledger_entries_immutable ON ledger_entries;
CREATE TRIGGER trg_ledger_entries_immutable
    BEFORE UPDATE OR DELETE ON ledger_entries
    FOR EACH ROW EXECUTE FUNCTION ledger_entries_immutable();

CREATE TABLE IF NOT EXISTS ledger_holds (
    business_key   TEXT PRIMARY KEY,
    account        TEXT NOT NULL REFERENCES ledger_accounts(name),
    amount_minor   BIGINT NOT NULL CHECK (amount_minor > 0),
    currency       TEXT NOT NULL,
    status         TEXT NOT NULL,
    journal_id     TEXT,
    request_digest TEXT NOT NULL,
    created_at     BIGINT NOT NULL,
    updated_at     BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ledger_holds_account
    ON ledger_holds(account, status);

CREATE TABLE IF NOT EXISTS ledger_closings (
    day            TEXT NOT NULL,
    currency       TEXT NOT NULL,
    start_ms       BIGINT NOT NULL,
    end_ms         BIGINT NOT NULL,
    debit_minor    BIGINT NOT NULL,
    credit_minor   BIGINT NOT NULL,
    entry_count    BIGINT NOT NULL,
    locked_minor   BIGINT NOT NULL,
    balance_total  BIGINT NOT NULL,
    checksum       TEXT NOT NULL,
    closed_at      BIGINT NOT NULL,
    PRIMARY KEY (day, currency)
);

CREATE TABLE IF NOT EXISTS ledger_closing_balances (
    day           TEXT NOT NULL,
    currency      TEXT NOT NULL,
    account       TEXT NOT NULL,
    balance_minor BIGINT NOT NULL,
    PRIMARY KEY (day, currency, account)
);

-- ---------------------------------------------------------------------------
-- commerce（G06-02 .. G06-05）
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS commerce_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS commerce_payment_intents (
    id             TEXT PRIMARY KEY,
    business_key   TEXT NOT NULL UNIQUE,
    request_digest TEXT NOT NULL,
    direction      TEXT NOT NULL CHECK (direction IN ('order', 'topup')),
    actor_id       TEXT NOT NULL,
    payee_id       TEXT NOT NULL,
    amount_minor   BIGINT NOT NULL CHECK (amount_minor > 0),
    currency       TEXT NOT NULL,
    status         TEXT NOT NULL,
    provider       TEXT NOT NULL,
    provider_ref   TEXT,
    nonce_hash     TEXT NOT NULL,
    qr_token_hash  TEXT NOT NULL,
    expires_at     BIGINT NOT NULL,
    refunded_minor BIGINT NOT NULL DEFAULT 0 CHECK (refunded_minor >= 0),
    version        BIGINT NOT NULL DEFAULT 0,
    created_at     BIGINT NOT NULL,
    updated_at     BIGINT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_intents_provider_ref
    ON commerce_payment_intents(provider, provider_ref)
    WHERE provider_ref IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_intents_payee
    ON commerce_payment_intents(payee_id, status);

CREATE TABLE IF NOT EXISTS commerce_payment_events (
    event_id       TEXT PRIMARY KEY,
    intent_id      TEXT NOT NULL REFERENCES commerce_payment_intents(id),
    kind           TEXT NOT NULL,
    provider_ref   TEXT NOT NULL,
    amount_minor   BIGINT NOT NULL,
    currency       TEXT NOT NULL,
    payload_digest TEXT NOT NULL,
    received_at    BIGINT NOT NULL,
    applied        BOOLEAN NOT NULL,
    reason         TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS commerce_cards (
    id             TEXT PRIMARY KEY,
    business_key   TEXT NOT NULL UNIQUE,
    owner_id       TEXT NOT NULL,
    provider       TEXT NOT NULL,
    provider_token TEXT NOT NULL,
    brand          TEXT NOT NULL,
    last4          TEXT NOT NULL CHECK (last4 ~ '^[0-9]{4}$'),
    exp_month      INTEGER NOT NULL CHECK (exp_month BETWEEN 1 AND 12),
    exp_year       INTEGER NOT NULL,
    status         TEXT NOT NULL,
    request_digest TEXT NOT NULL DEFAULT '',
    created_at     BIGINT NOT NULL,
    updated_at     BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_cards_owner ON commerce_cards(owner_id, status);

CREATE TABLE IF NOT EXISTS commerce_topups (
    id           TEXT PRIMARY KEY,
    business_key TEXT NOT NULL UNIQUE,
    actor_id     TEXT NOT NULL,
    intent_id    TEXT NOT NULL UNIQUE REFERENCES commerce_payment_intents(id),
    amount_minor BIGINT NOT NULL,
    currency     TEXT NOT NULL,
    status       TEXT NOT NULL,
    provider_ref TEXT,
    created_at   BIGINT NOT NULL,
    updated_at   BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS commerce_withdrawals (
    id           TEXT PRIMARY KEY,
    business_key TEXT NOT NULL UNIQUE,
    actor_id     TEXT NOT NULL,
    amount_minor BIGINT NOT NULL,
    currency     TEXT NOT NULL,
    status       TEXT NOT NULL,
    hold_key     TEXT NOT NULL UNIQUE,
    provider_ref TEXT,
    account_ref  TEXT NOT NULL,
    created_at   BIGINT NOT NULL,
    updated_at   BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_withdrawals_actor
    ON commerce_withdrawals(actor_id, status);

CREATE TABLE IF NOT EXISTS commerce_refunds (
    id           TEXT PRIMARY KEY,
    business_key TEXT NOT NULL UNIQUE,
    intent_id    TEXT NOT NULL REFERENCES commerce_payment_intents(id),
    actor_id     TEXT NOT NULL,
    amount_minor BIGINT NOT NULL,
    currency     TEXT NOT NULL,
    status       TEXT NOT NULL,
    hold_key     TEXT NOT NULL DEFAULT '',
    provider_ref TEXT,
    created_at   BIGINT NOT NULL,
    updated_at   BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_refunds_intent ON commerce_refunds(intent_id);

CREATE TABLE IF NOT EXISTS commerce_reconciliation_exceptions (
    id           TEXT PRIMARY KEY,
    run_id       TEXT NOT NULL,
    provider_ref TEXT NOT NULL,
    local_kind   TEXT NOT NULL,
    local_id     TEXT NOT NULL,
    amount_minor BIGINT NOT NULL,
    currency     TEXT NOT NULL,
    reason       TEXT NOT NULL,
    status       TEXT NOT NULL,
    created_at   BIGINT NOT NULL,
    resolved_by  TEXT,
    approval_ref TEXT,
    journal_key  TEXT
);
CREATE INDEX IF NOT EXISTS idx_recon_run
    ON commerce_reconciliation_exceptions(run_id, status);

CREATE TABLE IF NOT EXISTS commerce_transfers (
    id                TEXT PRIMARY KEY,
    business_key      TEXT NOT NULL UNIQUE,
    sender_id         TEXT NOT NULL,
    payee_id          TEXT NOT NULL,
    amount_minor      BIGINT NOT NULL,
    currency          TEXT NOT NULL,
    memo              TEXT NOT NULL,
    status            TEXT NOT NULL,
    confirmation_hash TEXT NOT NULL,
    nonce_hash        TEXT NOT NULL UNIQUE,
    hold_key          TEXT NOT NULL UNIQUE,
    expires_at        BIGINT NOT NULL,
    created_at        BIGINT NOT NULL,
    updated_at        BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_transfers_actor
    ON commerce_transfers(sender_id, payee_id, status);

CREATE TABLE IF NOT EXISTS commerce_nonces (
    nonce_hash TEXT PRIMARY KEY,
    purpose    TEXT NOT NULL,
    used_at    BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS commerce_redpackets (
    id           TEXT PRIMARY KEY,
    business_key TEXT NOT NULL UNIQUE,
    sender_id    TEXT NOT NULL,
    conv_id      TEXT NOT NULL,
    total_minor  BIGINT NOT NULL CHECK (total_minor > 0),
    count        INTEGER NOT NULL CHECK (count BETWEEN 1 AND 100),
    currency     TEXT NOT NULL,
    status       TEXT NOT NULL,
    memo         TEXT NOT NULL,
    created_at   BIGINT NOT NULL,
    expires_at   BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_redpackets_expiry
    ON commerce_redpackets(status, expires_at);

CREATE TABLE IF NOT EXISTS commerce_redpacket_shares (
    packet_id    TEXT NOT NULL REFERENCES commerce_redpackets(id),
    slot         INTEGER NOT NULL,
    amount_minor BIGINT NOT NULL CHECK (amount_minor > 0),
    claimed_by   TEXT,
    claimed_at   BIGINT,
    PRIMARY KEY (packet_id, slot)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_redpacket_share_claimer
    ON commerce_redpacket_shares(packet_id, claimed_by)
    WHERE claimed_by IS NOT NULL;

CREATE TABLE IF NOT EXISTS commerce_redpacket_claims (
    packet_id    TEXT NOT NULL REFERENCES commerce_redpackets(id),
    user_id      TEXT NOT NULL,
    amount_minor BIGINT NOT NULL,
    claimed_at   BIGINT NOT NULL,
    PRIMARY KEY (packet_id, user_id)
);

CREATE TABLE IF NOT EXISTS commerce_entitlement_rules (
    issuer_id  TEXT NOT NULL,
    kind       TEXT NOT NULL,
    version    BIGINT NOT NULL,
    rule_json  TEXT NOT NULL,
    digest     TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    PRIMARY KEY (issuer_id, kind, version)
);

CREATE TABLE IF NOT EXISTS commerce_entitlements (
    id            TEXT PRIMARY KEY,
    business_key  TEXT NOT NULL UNIQUE,
    issuer_id     TEXT NOT NULL,
    owner_id      TEXT,
    kind          TEXT NOT NULL,
    rule_issuer   TEXT NOT NULL,
    rule_kind     TEXT NOT NULL,
    rule_version  BIGINT NOT NULL,
    status        TEXT NOT NULL,
    valid_from    BIGINT NOT NULL,
    valid_to      BIGINT NOT NULL,
    payload_json  TEXT NOT NULL,
    issued_at     BIGINT NOT NULL,
    redeemed_at   BIGINT
);
CREATE INDEX IF NOT EXISTS idx_entitlements_owner
    ON commerce_entitlements(owner_id, status);

CREATE TABLE IF NOT EXISTS commerce_redemption_tokens (
    token_hash     TEXT PRIMARY KEY,
    entitlement_id TEXT NOT NULL REFERENCES commerce_entitlements(id),
    business_key   TEXT NOT NULL UNIQUE,
    expires_at     BIGINT NOT NULL,
    used_at        BIGINT,
    created_at     BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS commerce_redemptions (
    id             TEXT PRIMARY KEY,
    business_key   TEXT NOT NULL UNIQUE,
    entitlement_id TEXT NOT NULL REFERENCES commerce_entitlements(id),
    actor_id       TEXT NOT NULL,
    token_hash     TEXT NOT NULL UNIQUE,
    redeemed_at    BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS commerce_orders (
    id                 TEXT PRIMARY KEY,
    business_key       TEXT NOT NULL UNIQUE,
    actor_id           TEXT NOT NULL,
    payee_id           TEXT NOT NULL,
    quote_ref          TEXT NOT NULL,
    amount_minor       BIGINT NOT NULL,
    currency           TEXT NOT NULL,
    status             TEXT NOT NULL,
    payment_intent_id  TEXT REFERENCES commerce_payment_intents(id),
    provider_order_ref TEXT,
    version            BIGINT NOT NULL DEFAULT 0,
    created_at         BIGINT NOT NULL,
    updated_at         BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_orders_actor ON commerce_orders(actor_id, status);

CREATE TABLE IF NOT EXISTS commerce_outbox (
    event_id     TEXT PRIMARY KEY,
    aggregate_id TEXT NOT NULL,
    event_type   TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at   BIGINT NOT NULL,
    published_at BIGINT
);
CREATE INDEX IF NOT EXISTS idx_outbox_unpublished
    ON commerce_outbox(published_at, created_at);

INSERT INTO commerce_meta(key, value)
VALUES ('schema_version', '1')
ON CONFLICT (key) DO NOTHING;

COMMIT;

-- ---------------------------------------------------------------------------
-- 回填与切换（本提案不需要，但保留检查项给 G00）
-- ---------------------------------------------------------------------------
-- 1. 新库直接执行本文件；旧库无 commerce 表，不存在数据回填。
-- 2. ledger_entries 一旦有数据即不可 UPDATE/DELETE；如需纠错，追加反向分录。
-- 3. 切换阶段由 G00 校验：
--      SELECT currency FROM ledger_entries GROUP BY currency HAVING SUM(amount_minor) <> 0;
--    必须零行。
-- 4. 不提供破坏性 down migration；回滚策略见 ROLLBACK.md。
