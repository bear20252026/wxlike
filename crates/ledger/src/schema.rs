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
//! * `ledger_journals.entry_count` 冻结了这笔交易的分录条数，事后往已有
//!   journal 追加一条分录会被触发器拒绝——否则"借贷相抵"只是写入时的一次
//!   性检查，历史可以被追加破坏；
//! * 日结是快照表，重复日结只能与既有快照一致，不能覆盖；
//! * 分录与分录头的 UPDATE/DELETE 由 `BEFORE ... RAISE(ABORT)` 触发器在
//!   数据库层拒绝——`init_schema` 幂等，旧库文件下次打开即获得该兜底。

use rusqlite::{Connection, Result};

/// 当前账本 schema 版本。
///
/// 2 起加了 `ledger_journals.asset` / `.metadata` / `.entry_count` 三列
/// 与"分录条数不能被事后撑大"的触发器。
pub const SCHEMA_VERSION: i64 = 2;

/// 建表（幂等）。
///
/// # Errors
/// SQLite 执行失败。
pub fn init_schema(conn: &Connection) -> Result<()> {
    init_posting_tables(conn)?;
    migrate_journal_columns(conn)?;
    install_append_only_triggers(conn)?;
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
            asset         TEXT NOT NULL DEFAULT '',
            metadata      TEXT NOT NULL DEFAULT '',
            entry_count   INTEGER NOT NULL DEFAULT 0,
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

/// 旧库补齐 journal 的三个新列。
///
/// `CREATE TABLE IF NOT EXISTS` 对已存在的表不会加列，所以升级路径必须
/// 显式 `ALTER TABLE`；SQLite 的 ADD COLUMN 带 `NOT NULL DEFAULT` 时既有行
/// 按默认值填。
///
/// 旧 journal 的 `entry_count` 保持默认 0，**不做回填**：
/// * 语义上正好——0 表示"这笔 journal 的分录集合已经冻结，不接受任何
///   追加"，与历史账目不可改写是同一条底线；
/// * 机制上必须——老库文件里 `trg_ledger_journals_no_update` 已经存在，
///   任何 `UPDATE ledger_journals` 都会被它当场拒绝，回填语句会把整个
///   升级路径变成一次打不开库的失败。
fn migrate_journal_columns(conn: &Connection) -> Result<()> {
    let existing = column_names(conn, "ledger_journals")?;
    for (column, ddl) in [
        (
            "asset",
            "ALTER TABLE ledger_journals ADD COLUMN asset TEXT NOT NULL DEFAULT ''",
        ),
        (
            "metadata",
            "ALTER TABLE ledger_journals ADD COLUMN metadata TEXT NOT NULL DEFAULT ''",
        ),
        (
            "entry_count",
            "ALTER TABLE ledger_journals ADD COLUMN entry_count INTEGER NOT NULL DEFAULT 0",
        ),
    ] {
        if !existing.iter().any(|name| name == column) {
            conn.execute_batch(ddl)?;
        }
    }
    Ok(())
}

fn column_names(conn: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    rows.collect()
}

/// 分录与分录头的 append-only 触发器。
///
/// 不可变性由数据库兜底，而不是一句注释：任何绕过 `post_in` 的
/// UPDATE/DELETE（人工改余额、脚本回填、误用生产 fund 接口）都会在写入的
/// 那一刻被拒绝，事务整体回滚。
///
/// `trg_ledger_entries_no_extra_row` 是把"多借多贷必须平衡"从 Rust 层
/// 的校验升级为数据库层的约束：journal 写入时就声明了分录条数，之后
/// 任何往同一 journal 追加第 N+1 条的动作都会被拒绝。Formance 用
/// "每笔 posting 必然一进一出 + 余额格只累加"来保证平衡
/// （`processor_posting.go:86-198`），本仓库的等价底线是"落库的那组
/// 分录从此不可增、不可改、不可删"。
fn install_append_only_triggers(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TRIGGER IF NOT EXISTS trg_ledger_entries_no_update
            BEFORE UPDATE ON ledger_entries
        BEGIN
            SELECT RAISE(ABORT, 'ledger_entries is append-only');
        END;
        CREATE TRIGGER IF NOT EXISTS trg_ledger_entries_no_delete
            BEFORE DELETE ON ledger_entries
        BEGIN
            SELECT RAISE(ABORT, 'ledger_entries is append-only');
        END;
        CREATE TRIGGER IF NOT EXISTS trg_ledger_journals_no_update
            BEFORE UPDATE ON ledger_journals
        BEGIN
            SELECT RAISE(ABORT, 'ledger_journals is append-only');
        END;
        CREATE TRIGGER IF NOT EXISTS trg_ledger_journals_no_delete
            BEFORE DELETE ON ledger_journals
        BEGIN
            SELECT RAISE(ABORT, 'ledger_journals is append-only');
        END;
        CREATE TRIGGER IF NOT EXISTS trg_ledger_entries_no_extra_row
            AFTER INSERT ON ledger_entries
        WHEN (
            SELECT COUNT(*) FROM ledger_entries WHERE journal_id = NEW.journal_id
        ) > (SELECT entry_count FROM ledger_journals WHERE id = NEW.journal_id)
        BEGIN
            SELECT RAISE(ABORT, 'journal entry set is frozen at posting time');
        END;
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

        -- 日结快照也只能追加：能改写历史快照，"日结余额与分录一致"就只是
        -- 一句口号。重复日结由 save_closing 先比对再决定是否写入。
        CREATE TRIGGER IF NOT EXISTS trg_ledger_closings_no_update
            BEFORE UPDATE ON ledger_closings
        BEGIN
            SELECT RAISE(ABORT, 'ledger_closings is an immutable snapshot');
        END;
        CREATE TRIGGER IF NOT EXISTS trg_ledger_closings_no_delete
            BEFORE DELETE ON ledger_closings
        BEGIN
            SELECT RAISE(ABORT, 'ledger_closings is an immutable snapshot');
        END;
        CREATE TRIGGER IF NOT EXISTS trg_ledger_closing_balances_no_update
            BEFORE UPDATE ON ledger_closing_balances
        BEGIN
            SELECT RAISE(ABORT, 'ledger_closing_balances is an immutable snapshot');
        END;
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

    /// 升级路径：v1 库文件（journal 表只有 6 列，而且**已经带着**
    /// `trg_ledger_journals_no_update`）必须能被新代码打开并补齐三列。
    ///
    /// 这条用例钉住的是"回填只能靠默认值，不能靠 UPDATE"：任何
    /// `UPDATE ledger_journals` 的迁移写法都会在老库上被既有触发器打死，
    /// 结果是数据库直接打不开。
    #[test]
    fn upgrading_a_v1_database_adds_columns_without_a_backfill_update() {
        let c = Connection::open_in_memory().expect("db");
        c.execute_batch(
            r#"
            CREATE TABLE ledger_journals (
                id             TEXT PRIMARY KEY,
                business_key   TEXT NOT NULL UNIQUE,
                kind           TEXT NOT NULL,
                currency       TEXT NOT NULL,
                request_digest TEXT NOT NULL,
                created_at     INTEGER NOT NULL
            );
            CREATE TRIGGER trg_ledger_journals_no_update
                BEFORE UPDATE ON ledger_journals
            BEGIN
                SELECT RAISE(ABORT, 'ledger_journals is append-only');
            END;
            INSERT INTO ledger_journals
                (id, business_key, kind, currency, request_digest, created_at)
             VALUES('j1', 'old-key', 'transfer', 'CNY', 'deadbeef', 1);
            "#,
        )
        .expect("v1 db");
        init_schema(&c).expect("migrate v1");
        init_schema(&c).expect("migrate v1 again");

        let columns = column_names(&c, "ledger_journals").expect("columns");
        for added in ["asset", "metadata", "entry_count"] {
            assert!(columns.iter().any(|name| name == added), "{added} missing");
        }
        // 历史行按默认值补齐，且一条都没被改写。
        let row: (String, i64) = c
            .query_row(
                "SELECT asset, entry_count FROM ledger_journals WHERE business_key = 'old-key'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("legacy row");
        assert_eq!(row, (String::new(), 0));
    }
}
