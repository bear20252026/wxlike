//! commerce 表结构。
//!
//! 与账本共用一个连接：`init_schema` 先建账本表，再建业务表。业务记录与
//! 分录因此在同一个事务里提交——钱和"为什么动钱"不会分家。
//! 连接 pragma（WAL/忙等/外键）归 [`wxlike_ledger::open_connection`] 管。

use rusqlite::{params, Connection, Result};

/// 当前 commerce schema 版本。
pub const SCHEMA_VERSION: i64 = 1;

/// 建表（幂等）。
///
/// # Errors
/// SQLite 执行失败。
pub fn init_schema(conn: &Connection) -> Result<()> {
    wxlike_ledger::init_schema(conn)?;
    init_payment_tables(conn)?;
    init_card_tables(conn)?;
    init_settlement_tables(conn)?;
    init_transfer_tables(conn)?;
    init_redpacket_tables(conn)?;
    init_entitlement_tables(conn)?;
    init_order_tables(conn)?;
    conn.execute(
        "INSERT INTO commerce_meta(key, value) VALUES('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}

/// 支付意图与回调事件。
fn init_payment_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS commerce_meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS commerce_payment_intents (
            id            TEXT PRIMARY KEY,
            business_key  TEXT NOT NULL UNIQUE,
            request_digest TEXT NOT NULL,
            direction     TEXT NOT NULL,
            actor_id      TEXT NOT NULL,
            payee_id      TEXT NOT NULL,
            amount_minor  INTEGER NOT NULL CHECK (amount_minor > 0),
            currency      TEXT NOT NULL,
            status        TEXT NOT NULL,
            provider      TEXT NOT NULL,
            provider_ref  TEXT,
            nonce_hash    TEXT NOT NULL,
            qr_token_hash TEXT NOT NULL,
            expires_at    INTEGER NOT NULL,
            refunded_minor INTEGER NOT NULL DEFAULT 0,
            version       INTEGER NOT NULL DEFAULT 0,
            created_at    INTEGER NOT NULL,
            updated_at    INTEGER NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_intents_provider_ref
            ON commerce_payment_intents(provider, provider_ref)
            WHERE provider_ref IS NOT NULL;

        CREATE TABLE IF NOT EXISTS commerce_payment_events (
            event_id      TEXT PRIMARY KEY,
            intent_id     TEXT NOT NULL,
            kind          TEXT NOT NULL,
            provider_ref  TEXT NOT NULL,
            amount_minor  INTEGER NOT NULL,
            currency      TEXT NOT NULL,
            payload_digest TEXT NOT NULL,
            received_at   INTEGER NOT NULL,
            applied       INTEGER NOT NULL,
            reason        TEXT NOT NULL DEFAULT ''
        );
        "#,
    )
}

/// 绑卡与充值单。
fn init_card_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS commerce_cards (
            id            TEXT PRIMARY KEY,
            business_key  TEXT NOT NULL UNIQUE,
            owner_id      TEXT NOT NULL,
            provider      TEXT NOT NULL,
            provider_token TEXT NOT NULL,
            brand         TEXT NOT NULL,
            last4         TEXT NOT NULL,
            exp_month     INTEGER NOT NULL,
            exp_year      INTEGER NOT NULL,
            status        TEXT NOT NULL,
            request_digest TEXT NOT NULL DEFAULT '',
            created_at    INTEGER NOT NULL,
            updated_at    INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_cards_owner ON commerce_cards(owner_id, status);

        CREATE TABLE IF NOT EXISTS commerce_topups (
            id            TEXT PRIMARY KEY,
            business_key  TEXT NOT NULL UNIQUE,
            actor_id      TEXT NOT NULL,
            intent_id     TEXT NOT NULL UNIQUE,
            amount_minor  INTEGER NOT NULL,
            currency      TEXT NOT NULL,
            status        TEXT NOT NULL,
            provider_ref  TEXT,
            created_at    INTEGER NOT NULL,
            updated_at    INTEGER NOT NULL
        );
        "#,
    )
}

/// 提现、退款与对账差异队列。
fn init_settlement_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS commerce_withdrawals (
            id            TEXT PRIMARY KEY,
            business_key  TEXT NOT NULL UNIQUE,
            actor_id      TEXT NOT NULL,
            amount_minor  INTEGER NOT NULL,
            currency      TEXT NOT NULL,
            status        TEXT NOT NULL,
            hold_key      TEXT NOT NULL UNIQUE,
            provider_ref  TEXT,
            account_ref   TEXT NOT NULL,
            created_at    INTEGER NOT NULL,
            updated_at    INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_withdrawals_actor
            ON commerce_withdrawals(actor_id, status);

        CREATE TABLE IF NOT EXISTS commerce_refunds (
            id            TEXT PRIMARY KEY,
            business_key  TEXT NOT NULL UNIQUE,
            intent_id     TEXT NOT NULL,
            actor_id      TEXT NOT NULL,
            amount_minor  INTEGER NOT NULL,
            currency      TEXT NOT NULL,
            status        TEXT NOT NULL,
            hold_key      TEXT NOT NULL DEFAULT '',
            provider_ref  TEXT,
            created_at    INTEGER NOT NULL,
            updated_at    INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_refunds_intent ON commerce_refunds(intent_id);

        CREATE TABLE IF NOT EXISTS commerce_reconciliation_exceptions (
            id            TEXT PRIMARY KEY,
            run_id        TEXT NOT NULL,
            provider_ref  TEXT NOT NULL,
            local_kind    TEXT NOT NULL,
            local_id      TEXT NOT NULL,
            amount_minor  INTEGER NOT NULL,
            currency      TEXT NOT NULL,
            reason        TEXT NOT NULL,
            status        TEXT NOT NULL,
            created_at    INTEGER NOT NULL,
            resolved_by   TEXT,
            approval_ref  TEXT,
            journal_key   TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_recon_run
            ON commerce_reconciliation_exceptions(run_id, status);
        "#,
    )
}

/// 转账与一次性 nonce。
fn init_transfer_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS commerce_transfers (
            id            TEXT PRIMARY KEY,
            business_key  TEXT NOT NULL UNIQUE,
            sender_id     TEXT NOT NULL,
            payee_id      TEXT NOT NULL,
            amount_minor  INTEGER NOT NULL,
            currency      TEXT NOT NULL,
            memo          TEXT NOT NULL,
            status        TEXT NOT NULL,
            confirmation_hash TEXT NOT NULL,
            nonce_hash    TEXT NOT NULL UNIQUE,
            hold_key      TEXT NOT NULL UNIQUE,
            expires_at    INTEGER NOT NULL,
            created_at    INTEGER NOT NULL,
            updated_at    INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_transfers_actor
            ON commerce_transfers(sender_id, payee_id, status);

        CREATE TABLE IF NOT EXISTS commerce_nonces (
            nonce_hash    TEXT PRIMARY KEY,
            purpose       TEXT NOT NULL,
            used_at       INTEGER NOT NULL
        );
        "#,
    )
}

/// 红包与其份额/领取记录。
fn init_redpacket_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS commerce_redpackets (
            id            TEXT PRIMARY KEY,
            business_key  TEXT NOT NULL UNIQUE,
            sender_id     TEXT NOT NULL,
            conv_id       TEXT NOT NULL,
            total_minor   INTEGER NOT NULL,
            count         INTEGER NOT NULL,
            currency      TEXT NOT NULL,
            status        TEXT NOT NULL,
            memo          TEXT NOT NULL,
            created_at    INTEGER NOT NULL,
            expires_at    INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_redpackets_expiry
            ON commerce_redpackets(status, expires_at);

        CREATE TABLE IF NOT EXISTS commerce_redpacket_shares (
            packet_id     TEXT NOT NULL,
            slot          INTEGER NOT NULL,
            amount_minor  INTEGER NOT NULL CHECK (amount_minor > 0),
            claimed_by    TEXT,
            claimed_at    INTEGER,
            PRIMARY KEY (packet_id, slot)
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_redpacket_share_claimer
            ON commerce_redpacket_shares(packet_id, claimed_by)
            WHERE claimed_by IS NOT NULL;

        CREATE TABLE IF NOT EXISTS commerce_redpacket_claims (
            packet_id     TEXT NOT NULL,
            user_id       TEXT NOT NULL,
            amount_minor  INTEGER NOT NULL,
            claimed_at    INTEGER NOT NULL,
            PRIMARY KEY (packet_id, user_id)
        );
        "#,
    )
}

/// 权益规则、权益实例与核销。
fn init_entitlement_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS commerce_entitlement_rules (
            issuer_id     TEXT NOT NULL,
            kind          TEXT NOT NULL,
            version       INTEGER NOT NULL,
            rule_json     TEXT NOT NULL,
            digest        TEXT NOT NULL,
            created_at    INTEGER NOT NULL,
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
            rule_version  INTEGER NOT NULL,
            status        TEXT NOT NULL,
            valid_from    INTEGER NOT NULL,
            valid_to      INTEGER NOT NULL,
            payload_json  TEXT NOT NULL,
            issued_at     INTEGER NOT NULL,
            redeemed_at   INTEGER
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_entitlement_owner_slot
            ON commerce_entitlements(id, owner_id);
        CREATE INDEX IF NOT EXISTS idx_entitlements_owner
            ON commerce_entitlements(owner_id, status);

        CREATE TABLE IF NOT EXISTS commerce_redemption_tokens (
            token_hash    TEXT PRIMARY KEY,
            entitlement_id TEXT NOT NULL,
            business_key  TEXT NOT NULL UNIQUE,
            expires_at    INTEGER NOT NULL,
            used_at       INTEGER,
            created_at    INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS commerce_redemptions (
            id            TEXT PRIMARY KEY,
            business_key  TEXT NOT NULL UNIQUE,
            entitlement_id TEXT NOT NULL,
            actor_id      TEXT NOT NULL,
            token_hash    TEXT NOT NULL UNIQUE,
            redeemed_at   INTEGER NOT NULL
        );
        "#,
    )
}

/// 订单与 outbox 事件。
fn init_order_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS commerce_orders (
            id            TEXT PRIMARY KEY,
            business_key  TEXT NOT NULL UNIQUE,
            actor_id      TEXT NOT NULL,
            payee_id      TEXT NOT NULL,
            quote_ref     TEXT NOT NULL,
            amount_minor  INTEGER NOT NULL,
            currency      TEXT NOT NULL,
            status        TEXT NOT NULL,
            payment_intent_id TEXT,
            provider_order_ref TEXT,
            version       INTEGER NOT NULL DEFAULT 0,
            created_at    INTEGER NOT NULL,
            updated_at    INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_orders_actor ON commerce_orders(actor_id, status);

        CREATE TABLE IF NOT EXISTS commerce_outbox (
            event_id      TEXT PRIMARY KEY,
            aggregate_id  TEXT NOT NULL,
            event_type    TEXT NOT NULL,
            payload_json  TEXT NOT NULL,
            created_at    INTEGER NOT NULL,
            published_at  INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_outbox_unpublished
            ON commerce_outbox(published_at, created_at);
        "#,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_applies_twice() {
        let c = Connection::open_in_memory().expect("db");
        init_schema(&c).expect("first");
        init_schema(&c).expect("second");
    }
}
