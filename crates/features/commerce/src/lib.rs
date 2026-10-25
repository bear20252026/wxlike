//! 账本、支付、红包、卡券（G06）。
//!
//! 所有资金变更最终落到 `wxlike-ledger` 的不可变复式分录；业务表与分录
//! 共用一个 SQLite 连接，并在一个 `IMMEDIATE` 事务里提交。供应商网络调用
//! 永远发生在事务之外，失败只改状态，不"试试看"再扣一次钱。

pub mod accounts;
pub mod error;
pub mod infrastructure;
pub mod ports;

mod config;
mod schema;
mod store;
pub mod testkit;

use std::sync::{Mutex, MutexGuard, PoisonError};

use rusqlite::Connection;

pub use config::{CommerceConfig, ProviderCredentials, ProviderMode, SecretBytes};
pub use error::{CommerceError, Result};
pub use ports::{
    Clock, DenyAllMembership, Entropy, GroupMembership, OsEntropy, PaymentProvider, Ports,
    ProviderError, ProviderIntent, ProviderIntentRequest, ProviderPayment, ProviderPaymentStatus,
    ProviderPayout, ProviderPayoutRequest, ProviderPayoutStatus, ProviderRefund,
    ProviderRefundRequest, ProviderStatementRow, StatementKind, SystemClock,
};
pub use wxlike_ledger::{
    AccountType, ClosingReport, Currency, Entry, FundClass, Hold, LedgerError, Money, PostingLine,
    PostingRequest, StatementPage,
};

use crate::error::CommerceError as Error;

/// commerce 组合根。
pub struct Commerce {
    pub(crate) conn: Mutex<Connection>,
    pub(crate) config: CommerceConfig,
    pub(crate) ports: Ports,
}

impl Commerce {
    /// 打开数据库并校验配置/供应商模式。
    ///
    /// # Errors
    /// 配置非法、供应商模式与凭据不匹配、目录或建表失败。
    pub fn open(path: Option<&str>, config: CommerceConfig, ports: Ports) -> Result<Self> {
        config.validate()?;
        if ports.provider.mode() != config.credentials.mode {
            return Err(Error::BadInput(
                "provider implementation does not match configured mode".into(),
            ));
        }
        let conn = store::open_conn(path)?;
        Ok(Self {
            conn: Mutex::new(conn),
            config,
            ports,
        })
    }

    /// 内存库（测试/演示）。
    ///
    /// # Errors
    /// 配置非法或建表失败。
    pub fn in_memory(config: CommerceConfig, ports: Ports) -> Result<Self> {
        Self::open(None, config, ports)
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn now(&self) -> i64 {
        self.ports.clock.now_ms()
    }

    pub(crate) fn entropy(&self) -> &dyn Entropy {
        self.ports.entropy.as_ref()
    }

    /// 供应商稳定标识，用于账户名与对账。
    pub(crate) fn provider_name(&self) -> &'static str {
        match self.config.credentials.mode {
            ProviderMode::Sandbox => "sandbox",
            ProviderMode::Live => "live",
        }
    }

    /// 配置的只读视图。
    #[must_use]
    pub fn config(&self) -> &CommerceConfig {
        &self.config
    }

    /// 复核全账本：每笔分录自平衡、每个币种总和为零。
    ///
    /// # Errors
    /// 账本不一致或存储失败。
    pub fn verify_books(&self) -> Result<()> {
        let conn = self.lock();
        wxlike_ledger::verify_journals(&conn)?;
        if !wxlike_ledger::total_sum_is_zero(&conn)? {
            return Err(Error::Ledger(
                wxlike_ledger::LedgerError::ConservationViolated("currency total".into()),
            ));
        }
        Ok(())
    }
}

/// 账本 schema 版本（供 G00 迁移登记核对）。
#[must_use]
pub fn ledger_schema_version() -> i64 {
    wxlike_ledger::SCHEMA_VERSION
}

/// commerce schema 版本。
#[must_use]
pub fn commerce_schema_version() -> i64 {
    schema::SCHEMA_VERSION
}
