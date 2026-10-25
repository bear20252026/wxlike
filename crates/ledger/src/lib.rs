//! 不可变复式账本（G06-01）。
//!
//! 设计底线（全部有测试兜底）：
//!
//! 1. **金额只有整数最小单位 + 币种**，跨币种运算直接报错；
//! 2. **复式记账**：每笔交易在同一事务里写至少两条分录，借贷相抵；
//! 3. **余额只读**：余额 = 该账户分录之和，没有可被原地 UPDATE 的余额列；
//! 4. **可用与冻结分开**：`available = 已入账 - 冻结合计`，提现先锁额；
//! 5. **演示资金与真实资金隔离**：同一笔交易不能混用两种资金性质；
//! 6. **幂等**：业务键唯一，重复提交同参数只入账一次，换参数即冲突；
//! 7. **日结不可变**：同一天重复日结必须与既有快照完全一致。
//!
//! commerce 域不另建账本：它把 [`schema::init_schema`] 建在自己的连接上，
//! 并在同一个事务里调用 `store` 模块的 `*_in` 函数，业务记录与分录同库提交。

mod model;
mod schema;
mod store;

use std::sync::{Mutex, MutexGuard, PoisonError};

use rusqlite::{Connection, Transaction, TransactionBehavior};

pub use model::{
    Account, AccountSpec, AccountType, ClosingBalance, ClosingReport, Entry, FundClass, Hold,
    HoldStatus, Journal, LedgerTransaction, PostingLine, PostingRequest, StatementPage,
};
pub use schema::{init_schema, SCHEMA_VERSION};
pub use store::{
    available_minor, balance_minor, capture_hold_in, close_range_in, closing_balances_in,
    closings_in, create_account_in, hold_in, load_account, locked_minor, post_in, release_hold_in,
    request_digest_hex, statement_in, total_sum_is_zero, transaction_by_key, transfer_request,
    type_total, verify_closing_in, verify_journals, MAX_PAGE,
};
pub use wxlike_types::{Currency, Money, MoneyError};

/// 账本错误。所有变体都不携带密钥、密文或数据库连接串。
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("unknown account {0:?}")]
    UnknownAccount(String),
    #[error("insufficient funds in {0:?} (available {1})")]
    InsufficientFunds(String, i64),
    #[error("amount must be positive")]
    NonPositiveAmount,
    #[error("account {0:?} already exists with different parameters")]
    DuplicateAccount(String),
    #[error("unknown account type")]
    BadAccountType,
    #[error("bad currency {0:?}")]
    BadCurrency(String),
    #[error("currency mismatch: {left} vs {right}")]
    CurrencyMismatch { left: Currency, right: Currency },
    #[error("posting is not balanced")]
    Unbalanced,
    #[error("posting must have at least two lines")]
    TooFewLines,
    #[error("posting mixes virtual and real funds")]
    MixedFundClass,
    #[error("implicit cross-currency sum is forbidden: {0}")]
    MixedCurrency(String),
    #[error("amount overflow")]
    Overflow,
    #[error("idempotency key reused with different parameters")]
    IdempotencyConflict,
    #[error("invalid {0}")]
    BadInput(&'static str),
    #[error("unknown hold {0:?}")]
    UnknownHold(String),
    #[error("hold {0:?} already captured")]
    HoldAlreadyCaptured(String),
    #[error("hold {0:?} was released")]
    HoldReleased(String),
    #[error("conservation violated for {0}")]
    ConservationViolated(String),
    #[error("closing {0} {1} conflicts with the immutable snapshot")]
    ClosingConflict(String, String),
    #[error("closing snapshot no longer matches entries: {0}")]
    ClosingMismatch(String),
    #[error("storage failure: {0}")]
    Storage(#[from] rusqlite::Error),
}

impl From<MoneyError> for LedgerError {
    fn from(error: MoneyError) -> Self {
        match error {
            MoneyError::BadCurrency(raw) => Self::BadCurrency(raw),
            MoneyError::Overflow => Self::Overflow,
            MoneyError::CurrencyMismatch { left, right } => Self::CurrencyMismatch { left, right },
        }
    }
}

/// 账本结果。
pub type Result<T> = std::result::Result<T, LedgerError>;

/// 连接超时（毫秒）。WAL 模式下写锁短暂竞争时在这里排队。
const BUSY_TIMEOUT_MS: i64 = 5_000;

/// 打开一个带好 pragma 的连接：WAL、忙等、外键。
///
/// `None` 表示内存库；文件库会先创建父目录。
///
/// # Errors
/// 目录创建、SQLite 打开或 pragma 失败。
pub fn open_connection(path: Option<&str>) -> Result<Connection> {
    let conn = match path {
        Some(path) => {
            if let Some(dir) = std::path::Path::new(path).parent() {
                if !dir.as_os_str().is_empty() {
                    std::fs::create_dir_all(dir)
                        .map_err(|_| LedgerError::BadInput("database path"))?;
                }
            }
            Connection::open(path)?
        }
        None => Connection::open_in_memory()?,
    };
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;",
    )?;
    conn.pragma_update(None, "busy_timeout", BUSY_TIMEOUT_MS)?;
    Ok(conn)
}

/// 一个独立的账本连接。
pub struct Ledger {
    conn: Mutex<Connection>,
}

impl Ledger {
    /// 打开账本；`None` 表示内存库。
    ///
    /// # Errors
    /// 目录创建、SQLite 打开或建表失败。
    pub fn open(path: Option<&str>) -> Result<Self> {
        let conn = open_connection(path)?;
        schema::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn c(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn write<T>(&self, op: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut guard = self.c();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = op(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// 开户（兼容旧 API：人民币、虚拟资金）。
    ///
    /// # Errors
    /// 同名账户参数不一致或存储失败。
    pub fn create_account(&self, name: &str, kind: AccountType) -> Result<()> {
        self.create_account_with(&AccountSpec::new(name, kind, Currency::CNY))
    }

    /// 按完整参数开户；同参数幂等。
    ///
    /// # Errors
    /// 同名账户参数不一致或存储失败。
    pub fn create_account_with(&self, spec: &AccountSpec) -> Result<()> {
        self.write(|tx| store::create_account_in(tx, spec, now_ms()))
    }

    /// 读取账户元数据。
    ///
    /// # Errors
    /// 存储失败。
    pub fn account(&self, name: &str) -> Result<Option<Account>> {
        store::load_account(&self.c(), name)
    }

    /// 已入账余额（最小单位）。
    ///
    /// # Errors
    /// 账户不存在或存储失败。
    pub fn balance(&self, account: &str) -> Result<i64> {
        store::balance_minor(&self.c(), account)
    }

    /// 已入账余额（带币种）。
    ///
    /// # Errors
    /// 账户不存在或存储失败。
    pub fn money_balance(&self, account: &str) -> Result<Money> {
        let conn = self.c();
        let meta = store::load_account(&conn, account)?
            .ok_or_else(|| LedgerError::UnknownAccount(account.to_string()))?;
        Ok(Money::from_minor(
            store::balance_minor(&conn, account)?,
            meta.currency,
        ))
    }

    /// 可用余额（已入账 - 冻结）。
    ///
    /// # Errors
    /// 账户不存在或存储失败。
    pub fn available(&self, account: &str) -> Result<Money> {
        let conn = self.c();
        let meta = store::load_account(&conn, account)?
            .ok_or_else(|| LedgerError::UnknownAccount(account.to_string()))?;
        Ok(Money::from_minor(
            store::available_minor(&conn, account)?,
            meta.currency,
        ))
    }

    /// 冻结金额。
    ///
    /// # Errors
    /// 账户不存在或存储失败。
    pub fn locked(&self, account: &str) -> Result<Money> {
        let conn = self.c();
        let meta = store::load_account(&conn, account)?
            .ok_or_else(|| LedgerError::UnknownAccount(account.to_string()))?;
        Ok(Money::from_minor(
            store::locked_minor(&conn, account)?,
            meta.currency,
        ))
    }

    /// 记一笔账。
    ///
    /// # Errors
    /// 参数非法、余额不足、币种/资金性质混用或存储失败。
    pub fn post(&self, req: &PostingRequest) -> Result<LedgerTransaction> {
        self.post_at(req, now_ms())
    }

    /// 指定入账时间（日结/重放测试用）。
    ///
    /// # Errors
    /// 同 [`Ledger::post`]。
    pub fn post_at(&self, req: &PostingRequest, at_ms: i64) -> Result<LedgerTransaction> {
        self.write(|tx| store::post_in(tx, req, at_ms))
    }

    /// 兼容旧 API 的转账。
    ///
    /// # Errors
    /// 同 [`Ledger::post`]。
    pub fn transfer(&self, key: &str, from: &str, to: &str, amount: i64, memo: &str) -> Result<()> {
        let amount = Money::cny_minor(amount);
        let req = store::transfer_request(key, from, to, amount).with_memo(memo);
        self.post(&req).map(|_| ())
    }

    /// 带币种的转账。
    ///
    /// # Errors
    /// 同 [`Ledger::post`]。
    pub fn transfer_money(
        &self,
        key: &str,
        from: &str,
        to: &str,
        amount: Money,
        memo: &str,
    ) -> Result<LedgerTransaction> {
        self.post(&store::transfer_request(key, from, to, amount).with_memo(memo))
    }

    /// 冻结一笔可用余额。
    ///
    /// # Errors
    /// 余额不足、账户不存在或参数非法。
    pub fn hold(&self, key: &str, account: &str, amount: Money) -> Result<Hold> {
        self.write(|tx| store::hold_in(tx, key, account, amount, now_ms()))
    }

    /// 解冻。
    ///
    /// # Errors
    /// 锁额不存在或已转正式分录。
    pub fn release_hold(&self, key: &str) -> Result<()> {
        self.write(|tx| store::release_hold_in(tx, key, now_ms()))
    }

    /// 把锁额转正式分录。
    ///
    /// # Errors
    /// 锁额状态不对、记账参数非法或存储失败。
    pub fn capture_hold(&self, hold_key: &str, req: &PostingRequest) -> Result<LedgerTransaction> {
        self.write(|tx| store::capture_hold_in(tx, hold_key, req, now_ms()))
    }

    /// 账户流水页。
    ///
    /// # Errors
    /// 账户不存在或存储失败。
    pub fn statement(
        &self,
        account: &str,
        cursor: Option<i64>,
        limit: u32,
    ) -> Result<StatementPage> {
        store::statement_in(&self.c(), account, cursor, limit)
    }

    /// 全账本按币种守恒。
    ///
    /// # Errors
    /// 存储失败。
    pub fn total_sum_is_zero(&self) -> Result<bool> {
        store::total_sum_is_zero(&self.c())
    }

    /// 按账户类别汇总（多币种时拒绝隐式相加）。
    ///
    /// # Errors
    /// 多币种或存储失败。
    pub fn type_total(&self, kind: AccountType) -> Result<i64> {
        store::type_total(&self.c(), kind)
    }

    /// 校验每笔交易借贷相抵。
    ///
    /// # Errors
    /// 发现不平衡交易。
    pub fn verify_journals(&self) -> Result<()> {
        store::verify_journals(&self.c())
    }

    /// 日结。
    ///
    /// # Errors
    /// 区间非法、账本不守恒或快照冲突。
    pub fn close_range(&self, day: &str, start_ms: i64, end_ms: i64) -> Result<Vec<ClosingReport>> {
        self.write(|tx| store::close_range_in(tx, day, start_ms, end_ms, now_ms()))
    }

    /// 读取某天的日结报告。
    ///
    /// # Errors
    /// 存储失败。
    pub fn closings(&self, day: &str) -> Result<Vec<ClosingReport>> {
        store::closings_in(&self.c(), day)
    }

    /// 核对某天的日结快照。
    ///
    /// # Errors
    /// 快照与分录不一致。
    pub fn verify_closing(&self, day: &str) -> Result<()> {
        store::verify_closing_in(&self.c(), day)
    }

    /// 某天的账户余额快照。
    ///
    /// # Errors
    /// 存储失败。
    pub fn closing_balances(&self, day: &str, currency: Currency) -> Result<Vec<ClosingBalance>> {
        store::closing_balances_in(&self.c(), day, currency)
    }
}

/// 当前时间（UTC 毫秒）。
#[must_use]
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
