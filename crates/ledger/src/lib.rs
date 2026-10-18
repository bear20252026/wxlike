//! `wxlike-ledger` — P4 复式记账内核（ADR-12/18，方案 b：SQLite 上按
//! cala-ledger 的 schema 不变量实现，P6 迁 PG 时换 cala-ledger，接口不变）。
//!
//! 铁律（ADR-18 四类账户命名空间 + 蓝图"余额=重放分录"）：
//! 1. **复式记账**：每笔交易在同一 journal 内写两条分录（借/贷），金额
//!    相反相抵——全表 `SUM(amount)` 恒等于 0（守恒不变量）；
//! 2. **余额只读**：余额=该账户分录之和，禁止原地 UPDATE 余额；
//! 3. **四类命名空间**：`wallet`（用户钱包）/ `advertiser` / `marketing`
//!    / `revenue`——跨类划转即分录，无特例；
//! 4. **并发安全**：转账在单事务内完成取号+两条分录，SQLite 互斥锁保证
//!    100 并发转账分毫不差。
//!
//! 与 ADR-12 的对应：本 crate = 账本核心层；obix 的 outbox 形状由
//! `pending_outbox` 表预留（P4 后段接）；dtmrs Saga 与 ChannelConnector
//! 是上层，不在此 crate。

use std::sync::{Mutex, MutexGuard, PoisonError};

use rusqlite::{params, Connection, OptionalExtension as _};

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("unknown account {0:?}")]
    UnknownAccount(String),
    #[error("insufficient funds in {0:?} (available {1})")]
    InsufficientFunds(String, i64),
    #[error("amount must be positive")]
    NonPositiveAmount,
    #[error("account {0:?} already exists")]
    DuplicateAccount(String),
    #[error("account_type must be wallet|advertiser|marketing|revenue")]
    BadAccountType,
    #[error("storage failure: {0}")]
    Storage(#[from] rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, LedgerError>;

/// ADR-18 四类账户命名空间。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountType {
    Wallet,
    Advertiser,
    Marketing,
    Revenue,
}

impl AccountType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Wallet => "wallet",
            Self::Advertiser => "advertiser",
            Self::Marketing => "marketing",
            Self::Revenue => "revenue",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "wallet" => Some(Self::Wallet),
            "advertiser" => Some(Self::Advertiser),
            "marketing" => Some(Self::Marketing),
            "revenue" => Some(Self::Revenue),
            _ => None,
        }
    }
}

/// 一条分录（journal 内不可变）。
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub journal: String,
    pub account: String,
    pub amount: i64,
    pub memo: String,
}

pub struct Ledger {
    conn: Mutex<Connection>,
}

impl Ledger {
    pub fn open(path: Option<&str>) -> Result<Self> {
        let conn = match path {
            Some(p) => {
                if let Some(dir) = std::path::Path::new(p).parent() {
                    if !dir.as_os_str().is_empty() {
                        std::fs::create_dir_all(dir)
                            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                    }
                }
                Connection::open(p)?
            }
            None => Connection::open_in_memory()?,
        };
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA busy_timeout = 5000;

            -- 账户：余额 = 分录之和（禁止余额列）
            CREATE TABLE IF NOT EXISTS accounts (
                name        TEXT PRIMARY KEY,
                kind        TEXT NOT NULL,
                tenant_id   TEXT NOT NULL DEFAULT 'default',
                created_at  INTEGER NOT NULL
            );

            -- 分录流水：journal 内借贷相抵；只 INSERT 不 UPDATE（事件溯源）
            CREATE TABLE IF NOT EXISTS entries (
                journal  TEXT NOT NULL,
                seq      INTEGER NOT NULL,
                account  TEXT NOT NULL REFERENCES accounts(name),
                amount   INTEGER NOT NULL,
                memo     TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                PRIMARY KEY (journal, seq)
            );
            CREATE INDEX IF NOT EXISTS idx_entries_account ON entries(account, seq);
            "#,
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn c(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 开户（幂等：同名同类型直接成功）。
    pub fn create_account(&self, name: &str, kind: AccountType) -> Result<()> {
        let r = self.c().execute(
            "INSERT OR IGNORE INTO accounts(name, kind, tenant_id, created_at)
             VALUES(?1, ?2, 'default', ?3)",
            params![name, kind.as_str(), wxlike_core_now()],
        )?;
        if r == 0 {
            // 已存在：类型必须一致
            let existing: String = self.c().query_row(
                "SELECT kind FROM accounts WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )?;
            if existing != kind.as_str() {
                return Err(LedgerError::DuplicateAccount(name.to_string()));
            }
        }
        Ok(())
    }

    pub fn account_exists(&self, name: &str) -> Result<bool> {
        Ok(self
            .c()
            .query_row(
                "SELECT 1 FROM accounts WHERE name = ?1",
                params![name],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// 余额 = 分录之和（重放）。未知账户报错。
    pub fn balance(&self, account: &str) -> Result<i64> {
        if !self.account_exists(account)? {
            return Err(LedgerError::UnknownAccount(account.to_string()));
        }
        let c = self.c();
        Ok(c.query_row(
            "SELECT COALESCE(SUM(amount), 0) FROM entries WHERE account = ?1",
            params![account],
            |r| r.get(0),
        )?)
    }

    /// 全账本守恒不变量：所有分录之和必须为 0（借贷相抵）。
    pub fn total_sum_is_zero(&self) -> Result<bool> {
        let c = self.c();
        Ok(c.query_row(
            "SELECT COALESCE(SUM(amount), 0) = 0 FROM entries",
            [],
            |r| r.get::<_, i64>(0),
        )? != 0)
    }

    /// 某命名空间类的余额总和（对账断言面：wallet 总额=发行量等）。
    pub fn type_total(&self, kind: AccountType) -> Result<i64> {
        let c = self.c();
        Ok(c.query_row(
            "SELECT COALESCE(SUM(e.amount), 0) FROM entries e
             JOIN accounts a ON a.name = e.account WHERE a.kind = ?1",
            params![kind.as_str()],
            |r| r.get(0),
        )?)
    }

    /// 双条转账：同一 journal（`journal_id` 幂等键）内两条相反分录，单事务。
    ///
    /// 余额不足在事务内拒绝并回滚（余额=重放值，与并发写互斥）。
    pub fn transfer(
        &self,
        journal_id: &str,
        from: &str,
        to: &str,
        amount: i64,
        memo: &str,
    ) -> Result<()> {
        if amount <= 0 {
            return Err(LedgerError::NonPositiveAmount);
        }
        if from == to {
            return Err(LedgerError::BadAccountType); // 复用：自转即配置错
        }
        let mut c = self.c();
        let tx = c.transaction()?;
        for acct in [from, to] {
            let exists: Option<String> = tx
                .query_row(
                    "SELECT name FROM accounts WHERE name = ?1",
                    params![acct],
                    |r| r.get(0),
                )
                .optional()?;
            if exists.is_none() {
                return Err(LedgerError::UnknownAccount(acct.to_string()));
            }
        }
        // 幂等：同 journal 已存在则跳过（at-least-once 投递安全）
        let dup: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM entries WHERE journal = ?1 LIMIT 1",
                params![journal_id],
                |r| r.get(0),
            )
            .optional()?;
        if dup.is_some() {
            return Ok(());
        }
        let from_balance: i64 = tx.query_row(
            "SELECT COALESCE(SUM(amount), 0) FROM entries WHERE account = ?1",
            params![from],
            |r| r.get(0),
        )?;
        // 非负约束只对资产类（wallet）成立：营销/权益/收入是内部账户，
        // 贷方余额（负数）是复式记账的正常形态（如权益注资为负）
        let from_kind: String = tx.query_row(
            "SELECT kind FROM accounts WHERE name = ?1",
            params![from],
            |r| r.get(0),
        )?;
        if from_kind == "wallet" && from_balance < amount {
            return Err(LedgerError::InsufficientFunds(
                from.to_string(),
                from_balance,
            ));
        }
        let now = wxlike_core_now();
        // 两条分录：from 负（借），to 正（贷），journal 内相抵
        tx.execute(
            "INSERT INTO entries(journal, seq, account, amount, memo, created_at)
             VALUES(?1, 1, ?2, ?3, ?4, ?5)",
            params![journal_id, from, -amount, memo, now],
        )?;
        tx.execute(
            "INSERT INTO entries(journal, seq, account, amount, memo, created_at)
             VALUES(?1, 2, ?2, ?3, ?4, ?5)",
            params![journal_id, to, amount, memo, now],
        )?;
        tx.commit()?;
        Ok(())
    }
}

fn wxlike_core_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
