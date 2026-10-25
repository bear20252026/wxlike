//! 账本表结构。
//!
//! 同一份 DDL 给独立账本与 commerce 共用：commerce 在自己的连接里调用
//! [`init_schema`]，这样业务记录和分录可以落在同一个事务里。
//! 连接 pragma（WAL/忙等/外键）归 [`crate::open_connection`] 管，这里
//! 只管表。
//!
//! 不可变性的底线：
//! * `ledger_entries` 只 INSERT，没有余额列；
//! * `ledger_journals.business_key` 唯一，重放只能命中同一笔；
//! * 日结是快照表，重复日结只能与既有快照一致，不能覆盖。

use rusqlite::{Connection, Result};

/// 当前账本 schema 版本。
pub const SCHEMA_VERSION: i64 = 1;

/// 建表（幂等）。
///
/// # Errors
/// SQLite 执行失败。
pub fn init_schema(conn: &Connection) -> Result<()> {
    init_posting_tables(conn)?;
    init_hold_tables(conn)?;
    init_closing_tables(conn)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

/// 账户、日志与分录：append-only 的底线三张表。
fn init_posting_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS ledger_accounts (
            name         TEXT PRIMARY KEY,
            kind         TEXT NOT NULL,
            currency     TEXT NOT NULL,
            fund_class   TEXT NOT NULL,
            owner_id     TEXT NOT NULL DEFAULT '',
            created_at   INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_ledger_accounts_owner
            ON ledger_accounts(owner_id, currency);

        CREATE TABLE IF NOT EXISTS ledger_journals (
            id            TEXT PRIMARY KEY,
            business_key  TEXT NOT NULL UNIQUE,
            kind          TEXT NOT NULL,
            currency      TEXT NOT NULL,
            request_digest TEXT NOT NULL,
            created_at    INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS ledger_entries (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            journal_id  TEXT NOT NULL REFERENCES ledger_journals(id),
            seq         INTEGER NOT NULL,
            account     TEXT NOT NULL REFERENCES ledger_accounts(name),
            amount_minor INTEGER NOT NULL CHECK (amount_minor <> 0),
            currency    TEXT NOT NULL,
            memo        TEXT NOT NULL DEFAULT '',
            created_at  INTEGER NOT NULL,
            UNIQUE (journal_id, seq)
        );
        CREATE INDEX IF NOT EXISTS idx_ledger_entries_account
            ON ledger_entries(account, id);
        CREATE INDEX IF NOT EXISTS idx_ledger_entries_created
            ON ledger_entries(created_at, currency);
        "#,
    )
}

/// 锁额：余额之外的可用性扣减，不产生分录。
fn init_hold_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS ledger_holds (
            business_key TEXT PRIMARY KEY,
            account      TEXT NOT NULL REFERENCES ledger_accounts(name),
            amount_minor INTEGER NOT NULL CHECK (amount_minor > 0),
            currency     TEXT NOT NULL,
            status       TEXT NOT NULL,
            journal_id   TEXT,
            request_digest TEXT NOT NULL,
            created_at   INTEGER NOT NULL,
            updated_at   INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_ledger_holds_account
            ON ledger_holds(account, status);
        "#,
    )
}

/// 日结快照：同一天重复日结只能与既有快照一致。
fn init_closing_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS ledger_closings (
            day           TEXT NOT NULL,
            currency      TEXT NOT NULL,
            start_ms      INTEGER NOT NULL,
            end_ms        INTEGER NOT NULL,
            debit_minor   INTEGER NOT NULL,
            credit_minor  INTEGER NOT NULL,
            entry_count   INTEGER NOT NULL,
            locked_minor  INTEGER NOT NULL,
            balance_total INTEGER NOT NULL,
            checksum      TEXT NOT NULL,
            closed_at     INTEGER NOT NULL,
            PRIMARY KEY (day, currency)
        );

        CREATE TABLE IF NOT EXISTS ledger_closing_balances (
            day          TEXT NOT NULL,
            currency     TEXT NOT NULL,
            account      TEXT NOT NULL,
            balance_minor INTEGER NOT NULL,
            PRIMARY KEY (day, currency, account)
        );
        "#,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_idempotent() {
        let c = Connection::open_in_memory().expect("db");
        init_schema(&c).expect("first");
        init_schema(&c).expect("second");
    }
}
