//! 资金账户：余额、流水与沙箱注资。
//!
//! 余额永远由不可变分录与冻结锁额导出，没有可直接改写的余额列。沙箱
//! 注资走"外部世界 → 钱包"的复式分录，只在虚拟资金模式下开放。

use rusqlite::{Connection, TransactionBehavior};
use wxlike_ledger::{Currency, Money, PostingLine, PostingRequest, StatementPage};

use crate::error::{CommerceError, Result};
use crate::store;
use crate::Commerce;

/// 钱包余额视图。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalletBalance {
    pub currency: Currency,
    pub total_minor: i64,
    pub locked_minor: i64,
    pub available_minor: i64,
}

impl WalletBalance {
    /// 总余额。
    #[must_use]
    pub fn total(&self) -> Money {
        Money::from_minor(self.total_minor, self.currency)
    }

    /// 可用余额。
    #[must_use]
    pub fn available(&self) -> Money {
        Money::from_minor(self.available_minor, self.currency)
    }

    /// 冻结金额。
    #[must_use]
    pub fn locked(&self) -> Money {
        Money::from_minor(self.locked_minor, self.currency)
    }
}

impl Commerce {
    /// 幂等建用户钱包（同一事务内调用）。
    pub(crate) fn ensure_wallet_conn(
        &self,
        conn: &Connection,
        owner: &str,
        currency: Currency,
        now: i64,
    ) -> Result<String> {
        store::ensure_wallet(conn, owner, currency, self.config.fund_class, now)
    }

    /// 查询余额；不存在的钱包视为 0，但不会隐式开户。
    ///
    /// # Errors
    /// 参数非法或存储失败。
    pub fn wallet_balance(&self, owner: &str, currency: Currency) -> Result<WalletBalance> {
        store::validate_id("owner", owner)?;
        let conn = self.lock();
        let name = store::wallet_account(owner, currency);
        let total = match wxlike_ledger::load_account(&conn, &name) {
            Ok(Some(_)) => wxlike_ledger::balance_minor(&conn, &name)?,
            Ok(None) => 0,
            Err(error) => return Err(error.into()),
        };
        let locked = if total == 0 {
            0
        } else {
            wxlike_ledger::locked_minor(&conn, &name)?
        };
        Ok(WalletBalance {
            currency,
            total_minor: total,
            locked_minor: locked,
            available_minor: total - locked,
        })
    }

    /// 钱包流水（按分录 ID 升序分页）。
    ///
    /// # Errors
    /// 参数非法或账户不存在。
    pub fn wallet_statement(
        &self,
        owner: &str,
        currency: Currency,
        cursor: Option<i64>,
        limit: u32,
    ) -> Result<StatementPage> {
        store::validate_id("owner", owner)?;
        let conn = self.lock();
        let name = store::wallet_account(owner, currency);
        if wxlike_ledger::load_account(&conn, &name)?.is_none() {
            return Ok(StatementPage {
                items: Vec::new(),
                next_cursor: None,
                has_more: false,
                currency,
            });
        }
        wxlike_ledger::statement_in(&conn, &name, cursor, limit).map_err(Into::into)
    }

    /// 沙箱注资：仅虚拟资金 + 沙箱供应商可用。
    ///
    /// # Errors
    /// 真实资金模式、参数非法、幂等键冲突或存储失败。
    pub fn sandbox_credit(
        &self,
        owner: &str,
        amount: Money,
        business_key: &str,
    ) -> Result<WalletBalance> {
        if self.config.fund_class != wxlike_ledger::FundClass::Virtual
            || self.config.credentials.mode != crate::ProviderMode::Sandbox
        {
            return Err(CommerceError::Forbidden);
        }
        store::validate_id("owner", owner)?;
        store::validate_key(business_key)?;
        store::check_limit(amount, self.config.max_payment_minor)?;
        let now = self.now();
        let provider = self.provider_name();
        let key = format!("grant:{owner}:{business_key}");
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let wallet = self.ensure_wallet_conn(&tx, owner, amount.currency(), now)?;
        let external = store::ensure_external(
            &tx,
            provider,
            amount.currency(),
            self.config.fund_class,
            now,
        )?;
        let req = PostingRequest::new(
            key,
            "wallet.grant",
            amount.currency(),
            vec![
                PostingLine::debit(external, amount.minor()),
                PostingLine::credit(wallet.clone(), amount.minor()),
            ],
        )
        .with_memo("sandbox credit");
        wxlike_ledger::post_in(&tx, &req, now)?;
        let total = wxlike_ledger::balance_minor(&tx, &wallet)?;
        let locked = wxlike_ledger::locked_minor(&tx, &wallet)?;
        tx.commit()?;
        Ok(WalletBalance {
            currency: amount.currency(),
            total_minor: total,
            locked_minor: locked,
            available_minor: total - locked,
        })
    }
}
