//! 账本的存储层。
//!
//! 这里的函数都接受 `&Connection`；调用方既可以给独立连接，也可以在
//! commerce 的同一个事务里调用——资金分录与业务记录因此能同库提交。
//! 所有写事务必须用 `IMMEDIATE` 开启（见 [`crate::Ledger`] 与 commerce），
//! 否则"先查可用余额再写锁额"会在并发下退化。

use std::collections::BTreeMap;

use rusqlite::{params, Connection, OptionalExtension as _};
use sha2::{Digest as _, Sha256};

use crate::model::{
    Account, AccountSpec, AccountType, ClosingBalance, ClosingReport, Currency, Entry, FundClass,
    Hold, HoldStatus, Journal, LedgerTransaction, Money, PostingLine, PostingRequest,
    StatementPage,
};
use crate::{LedgerError, Result};

/// 单页最大条数。
pub const MAX_PAGE: u32 = 200;

struct JournalRow {
    journal: Journal,
    digest: String,
}

/// 开户；同参数幂等，异参数拒绝。
pub fn create_account_in(conn: &Connection, spec: &AccountSpec, now: i64) -> Result<()> {
    validate_account(spec)?;
    if let Some(existing) = load_account(conn, &spec.name)? {
        let same = existing.kind == spec.kind
            && existing.currency == spec.currency
            && existing.fund_class == spec.fund_class
            && existing.owner_id == spec.owner_id;
        if same {
            return Ok(());
        }
        return Err(LedgerError::DuplicateAccount(spec.name.clone()));
    }
    conn.execute(
        "INSERT INTO ledger_accounts(name, kind, currency, fund_class, owner_id, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            spec.name,
            spec.kind.as_str(),
            spec.currency.as_str(),
            spec.fund_class.as_str(),
            spec.owner_id,
            now
        ],
    )?;
    Ok(())
}

fn validate_account(spec: &AccountSpec) -> Result<()> {
    if spec.name.is_empty() || spec.name.len() > 200 {
        return Err(LedgerError::BadInput("account name"));
    }
    if spec.owner_id.len() > 200 {
        return Err(LedgerError::BadInput("account owner"));
    }
    Ok(())
}

/// 读取账户元数据。
pub fn load_account(conn: &Connection, name: &str) -> Result<Option<Account>> {
    let row = conn
        .query_row(
            "SELECT kind, currency, fund_class, owner_id, created_at
             FROM ledger_accounts WHERE name = ?1",
            params![name],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((kind, currency, fund_class, owner_id, created_at)) = row else {
        return Ok(None);
    };
    Ok(Some(Account {
        name: name.to_string(),
        kind: AccountType::parse(&kind).ok_or(LedgerError::BadAccountType)?,
        currency: Currency::parse(&currency)?,
        fund_class: FundClass::parse(&fund_class).ok_or(LedgerError::BadInput("fund class"))?,
        owner_id,
        created_at,
    }))
}

fn require_account(conn: &Connection, name: &str) -> Result<Account> {
    load_account(conn, name)?.ok_or_else(|| LedgerError::UnknownAccount(name.to_string()))
}

/// 已入账余额（不含锁额）。
pub fn balance_minor(conn: &Connection, account: &str) -> Result<i64> {
    require_account(conn, account)?;
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(amount_minor), 0) FROM ledger_entries WHERE account = ?1",
        params![account],
        |r| r.get(0),
    )?)
}

/// 冻结中的金额。
pub fn locked_minor(conn: &Connection, account: &str) -> Result<i64> {
    require_account(conn, account)?;
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(amount_minor), 0) FROM ledger_holds
         WHERE account = ?1 AND status = 'held'",
        params![account],
        |r| r.get(0),
    )?)
}

/// 可用余额 = 已入账余额 - 冻结金额。
pub fn available_minor(conn: &Connection, account: &str) -> Result<i64> {
    Ok(balance_minor(conn, account)? - locked_minor(conn, account)?)
}

/// 记账。`business_key` 是唯一业务键；重复提交同参数只入账一次。
pub fn post_in(conn: &Connection, req: &PostingRequest, now: i64) -> Result<LedgerTransaction> {
    let digest = posting_digest(req)?;
    if let Some(row) = journal_by_key(conn, &req.business_key)? {
        if row.digest != digest {
            return Err(LedgerError::IdempotencyConflict);
        }
        return load_transaction(conn, &row.journal);
    }
    let accounts = validate_lines(conn, req)?;
    check_available(conn, &accounts)?;
    insert_journal(conn, req, &digest, now)?;
    insert_entries(conn, req, now)?;
    load_transaction_by_key(conn, &req.business_key)
}

fn posting_digest(req: &PostingRequest) -> Result<String> {
    if req.business_key.is_empty() || req.business_key.len() > 200 {
        return Err(LedgerError::BadInput("business key"));
    }
    if req.kind.is_empty() || req.kind.len() > 64 {
        return Err(LedgerError::BadInput("journal kind"));
    }
    if req.memo.len() > 500 {
        return Err(LedgerError::BadInput("memo"));
    }
    if req.lines.len() < 2 {
        return Err(LedgerError::TooFewLines);
    }
    let mut parts = vec![
        req.kind.as_str().to_string(),
        req.currency.as_str().to_string(),
        req.memo.clone(),
    ];
    for line in &req.lines {
        if line.amount == 0 {
            return Err(LedgerError::NonPositiveAmount);
        }
        parts.push(line.account.clone());
        parts.push(line.amount.to_string());
    }
    Ok(digest_parts(&parts))
}

fn digest_parts(parts: &[String]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    hex::encode(hasher.finalize())
}

fn validate_lines(
    conn: &Connection,
    req: &PostingRequest,
) -> Result<BTreeMap<String, (Account, i64)>> {
    let mut net: BTreeMap<String, (Account, i64)> = BTreeMap::new();
    let mut fund_class: Option<FundClass> = None;
    for line in &req.lines {
        let account = require_account(conn, &line.account)?;
        if account.currency != req.currency {
            return Err(LedgerError::CurrencyMismatch {
                left: account.currency,
                right: req.currency,
            });
        }
        if let Some(seen) = fund_class {
            if seen != account.fund_class {
                return Err(LedgerError::MixedFundClass);
            }
        }
        fund_class = Some(account.fund_class);
        let slot = net.entry(line.account.clone()).or_insert((account, 0));
        slot.1 = slot
            .1
            .checked_add(line.amount)
            .ok_or(LedgerError::Overflow)?;
    }
    let total = net.values().try_fold(0i64, |acc, (_, amount)| {
        acc.checked_add(*amount).ok_or(LedgerError::Overflow)
    })?;
    if total != 0 {
        return Err(LedgerError::Unbalanced);
    }
    Ok(net)
}

fn check_available(conn: &Connection, net: &BTreeMap<String, (Account, i64)>) -> Result<()> {
    for (name, (account, amount)) in net {
        if *amount >= 0 || !account.kind.is_asset() {
            continue;
        }
        let available = available_minor(conn, name)?;
        if available < -*amount {
            return Err(LedgerError::InsufficientFunds(name.clone(), available));
        }
    }
    Ok(())
}

fn insert_journal(conn: &Connection, req: &PostingRequest, digest: &str, now: i64) -> Result<()> {
    let id = journal_id(&req.business_key);
    conn.execute(
        "INSERT INTO ledger_journals(id, business_key, kind, currency, request_digest, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id,
            req.business_key,
            req.kind,
            req.currency.as_str(),
            digest,
            now
        ],
    )?;
    Ok(())
}

fn journal_id(business_key: &str) -> String {
    let digest = Sha256::digest(business_key.as_bytes());
    hex::encode(&digest[..16])
}

fn insert_entries(conn: &Connection, req: &PostingRequest, now: i64) -> Result<()> {
    let journal = journal_id(&req.business_key);
    for (index, line) in req.lines.iter().enumerate() {
        conn.execute(
            "INSERT INTO ledger_entries
                (journal_id, seq, account, amount_minor, currency, memo, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                journal,
                (index + 1) as i64,
                line.account,
                line.amount,
                req.currency.as_str(),
                req.memo,
                now
            ],
        )?;
    }
    Ok(())
}

fn journal_by_key(conn: &Connection, key: &str) -> Result<Option<JournalRow>> {
    let row = conn
        .query_row(
            "SELECT id, business_key, kind, currency, created_at, request_digest
             FROM ledger_journals WHERE business_key = ?1",
            params![key],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?;
    row.map(|(id, business_key, kind, currency, created_at, digest)| {
        Ok(JournalRow {
            journal: Journal {
                id,
                business_key,
                kind,
                currency: Currency::parse(&currency)?,
                created_at,
            },
            digest,
        })
    })
    .transpose()
}

fn load_transaction(conn: &Connection, journal: &Journal) -> Result<LedgerTransaction> {
    let mut stmt = conn.prepare(
        "SELECT id, seq, account, amount_minor, currency, memo, created_at
         FROM ledger_entries WHERE journal_id = ?1 ORDER BY seq ASC",
    )?;
    let rows = stmt.query_map(params![journal.id], |r| {
        Ok(Entry {
            id: r.get(0)?,
            journal_id: journal.id.clone(),
            seq: r.get(1)?,
            account: r.get(2)?,
            amount: r.get(3)?,
            currency: journal.currency,
            memo: r.get(5)?,
            created_at: r.get(6)?,
        })
    })?;
    let entries = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(LedgerTransaction {
        id: journal.id.clone(),
        business_key: journal.business_key.clone(),
        kind: journal.kind.clone(),
        currency: journal.currency,
        created_at: journal.created_at,
        entries,
    })
}

/// 按业务键读取已入账交易。
pub fn transaction_by_key(conn: &Connection, key: &str) -> Result<Option<LedgerTransaction>> {
    let Some(row) = journal_by_key(conn, key)? else {
        return Ok(None);
    };
    Ok(Some(load_transaction(conn, &row.journal)?))
}

fn load_transaction_by_key(conn: &Connection, key: &str) -> Result<LedgerTransaction> {
    transaction_by_key(conn, key)?.ok_or(LedgerError::BadInput("journal just inserted"))
}

/// 锁额。`available` 不足时拒绝。
pub fn hold_in(
    conn: &Connection,
    key: &str,
    account: &str,
    amount: Money,
    now: i64,
) -> Result<Hold> {
    if key.is_empty() || key.len() > 200 {
        return Err(LedgerError::BadInput("hold key"));
    }
    if !amount.is_positive() {
        return Err(LedgerError::NonPositiveAmount);
    }
    let digest = digest_parts(&[
        "hold".to_string(),
        account.to_string(),
        amount.currency().as_str().to_string(),
        amount.minor().to_string(),
    ]);
    if let Some(existing) = load_hold(conn, key)? {
        if existing.1 != digest {
            return Err(LedgerError::IdempotencyConflict);
        }
        return Ok(existing.0);
    }
    let meta = require_account(conn, account)?;
    if meta.currency != amount.currency() {
        return Err(LedgerError::CurrencyMismatch {
            left: meta.currency,
            right: amount.currency(),
        });
    }
    let available = available_minor(conn, account)?;
    if available < amount.minor() {
        return Err(LedgerError::InsufficientFunds(
            account.to_string(),
            available,
        ));
    }
    conn.execute(
        "INSERT INTO ledger_holds
            (business_key, account, amount_minor, currency, status, request_digest,
             created_at, updated_at)
         VALUES(?1, ?2, ?3, ?4, 'held', ?5, ?6, ?6)",
        params![
            key,
            account,
            amount.minor(),
            amount.currency().as_str(),
            digest,
            now
        ],
    )?;
    load_hold(conn, key)?
        .map(|row| row.0)
        .ok_or(LedgerError::UnknownHold(key.to_string()))
}

fn load_hold(conn: &Connection, key: &str) -> Result<Option<(Hold, String)>> {
    let row = conn
        .query_row(
            "SELECT account, amount_minor, currency, status, journal_id, created_at,
                    updated_at, request_digest
             FROM ledger_holds WHERE business_key = ?1",
            params![key],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, String>(7)?,
                ))
            },
        )
        .optional()?;
    let Some((account, minor, currency, status, journal_id, created_at, updated_at, digest)) = row
    else {
        return Ok(None);
    };
    Ok(Some((
        Hold {
            business_key: key.to_string(),
            account,
            amount: Money::from_minor(minor, Currency::parse(&currency)?),
            status: HoldStatus::parse(&status).ok_or(LedgerError::BadInput("hold status"))?,
            journal_id,
            created_at,
            updated_at,
        },
        digest,
    )))
}

/// 解冻；重复解冻幂等。
pub fn release_hold_in(conn: &Connection, key: &str, now: i64) -> Result<()> {
    let Some((hold, _)) = load_hold(conn, key)? else {
        return Err(LedgerError::UnknownHold(key.to_string()));
    };
    match hold.status {
        HoldStatus::Released => Ok(()),
        HoldStatus::Captured => Err(LedgerError::HoldAlreadyCaptured(key.to_string())),
        HoldStatus::Held => {
            conn.execute(
                "UPDATE ledger_holds SET status = 'released', updated_at = ?1
                 WHERE business_key = ?2 AND status = 'held'",
                params![now, key],
            )?;
            Ok(())
        }
    }
}

/// 把锁额转成正式分录。记账请求里必须有一条借记该账户、金额等于锁额。
pub fn capture_hold_in(
    conn: &Connection,
    hold_key: &str,
    req: &PostingRequest,
    now: i64,
) -> Result<LedgerTransaction> {
    let digest = posting_digest(req)?;
    if let Some(row) = journal_by_key(conn, &req.business_key)? {
        if row.digest != digest {
            return Err(LedgerError::IdempotencyConflict);
        }
        return load_transaction(conn, &row.journal);
    }
    let Some((hold, _)) = load_hold(conn, hold_key)? else {
        return Err(LedgerError::UnknownHold(hold_key.to_string()));
    };
    match hold.status {
        HoldStatus::Captured => Err(LedgerError::HoldAlreadyCaptured(hold_key.to_string())),
        HoldStatus::Released => Err(LedgerError::HoldReleased(hold_key.to_string())),
        HoldStatus::Held => {
            require_capture_line(req, &hold)?;
            let changed = conn.execute(
                "UPDATE ledger_holds
                 SET status = 'captured', journal_id = ?1, updated_at = ?2
                 WHERE business_key = ?3 AND status = 'held'",
                params![journal_id(&req.business_key), now, hold_key],
            )?;
            if changed != 1 {
                return Err(LedgerError::HoldAlreadyCaptured(hold_key.to_string()));
            }
            post_in(conn, req, now)
        }
    }
}

fn require_capture_line(req: &PostingRequest, hold: &Hold) -> Result<()> {
    if req.currency != hold.amount.currency() {
        return Err(LedgerError::CurrencyMismatch {
            left: hold.amount.currency(),
            right: req.currency,
        });
    }
    let debit = req.lines.iter().find(|line| line.account == hold.account);
    match debit {
        Some(line) if line.amount == -hold.amount.minor() => Ok(()),
        _ => Err(LedgerError::BadInput("capture must debit the held amount")),
    }
}

/// 流水页；游标是第一页最后一条分录的 `id`。
pub fn statement_in(
    conn: &Connection,
    account: &str,
    cursor: Option<i64>,
    limit: u32,
) -> Result<StatementPage> {
    let meta = require_account(conn, account)?;
    let limit = limit.clamp(1, MAX_PAGE);
    let cursor = cursor.unwrap_or(0).max(0);
    let mut stmt = conn.prepare(
        "SELECT id, journal_id, seq, account, amount_minor, currency, memo, created_at
         FROM ledger_entries WHERE account = ?1 AND id > ?2
         ORDER BY id ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![account, cursor, i64::from(limit) + 1], |r| {
        Ok(Entry {
            id: r.get(0)?,
            journal_id: r.get(1)?,
            seq: r.get(2)?,
            account: r.get(3)?,
            amount: r.get(4)?,
            currency: meta.currency,
            memo: r.get(6)?,
            created_at: r.get(7)?,
        })
    })?;
    let mut items = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    let has_more = items.len() > limit as usize;
    if has_more {
        items.truncate(limit as usize);
    }
    Ok(StatementPage {
        next_cursor: items.last().map(|entry| entry.id),
        has_more,
        currency: meta.currency,
        items,
    })
}

/// 全账本按币种守恒：每个币种的分录和都必须为 0。
pub fn total_sum_is_zero(conn: &Connection) -> Result<bool> {
    let bad: Option<String> = conn
        .query_row(
            "SELECT currency FROM ledger_entries
             GROUP BY currency HAVING SUM(amount_minor) <> 0 LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(bad.is_none())
}

/// 某一类账户的余额合计。存在多个币种时拒绝隐式相加。
pub fn type_total(conn: &Connection, kind: AccountType) -> Result<i64> {
    let mut stmt = conn.prepare(
        "SELECT a.currency, COALESCE(SUM(e.amount_minor), 0)
         FROM ledger_accounts a
         LEFT JOIN ledger_entries e ON e.account = a.name
         WHERE a.kind = ?1 GROUP BY a.currency ORDER BY a.currency",
    )?;
    let rows = stmt.query_map(params![kind.as_str()], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    let mut total: Option<(String, i64)> = None;
    for row in rows {
        let (currency, sum) = row?;
        if let Some((seen, _)) = &total {
            return Err(LedgerError::MixedCurrency(format!("{seen}+{currency}")));
        }
        total = Some((currency, sum));
    }
    Ok(total.map_or(0, |(_, sum)| sum))
}

/// 校验每笔 journal 内部守恒。
pub fn verify_journals(conn: &Connection) -> Result<()> {
    let bad: Option<String> = conn
        .query_row(
            "SELECT journal_id FROM ledger_entries
             GROUP BY journal_id HAVING SUM(amount_minor) <> 0 LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    match bad {
        Some(id) => Err(LedgerError::ConservationViolated(id)),
        None => Ok(()),
    }
}

/// 日结：写入不可变快照；同一天重复日结参数必须完全一致。
pub fn close_range_in(
    conn: &Connection,
    day: &str,
    start_ms: i64,
    end_ms: i64,
    now: i64,
) -> Result<Vec<ClosingReport>> {
    validate_day(day, start_ms, end_ms)?;
    let currencies = currencies_in_range(conn, start_ms, end_ms)?;
    let mut reports = Vec::new();
    for currency in currencies {
        let report = build_closing(conn, day, currency, start_ms, end_ms, now)?;
        save_closing(conn, &report)?;
        reports.push(report);
    }
    Ok(reports)
}

fn validate_day(day: &str, start_ms: i64, end_ms: i64) -> Result<()> {
    if day.is_empty() || day.len() > 10 || start_ms >= end_ms {
        return Err(LedgerError::BadInput("closing day/range"));
    }
    Ok(())
}

fn currencies_in_range(conn: &Connection, start: i64, end: i64) -> Result<Vec<Currency>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT currency FROM ledger_entries
         WHERE created_at >= ?1 AND created_at < ?2 ORDER BY currency",
    )?;
    let rows = stmt.query_map(params![start, end], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(Currency::parse(&row?)?);
    }
    Ok(out)
}

fn build_closing(
    conn: &Connection,
    day: &str,
    currency: Currency,
    start: i64,
    end: i64,
    now: i64,
) -> Result<ClosingReport> {
    let (debits, credits, count, net) = period_totals(conn, currency, start, end)?;
    if net != 0 {
        return Err(LedgerError::ConservationViolated(day.to_string()));
    }
    let balances = balances_as_of(conn, currency, end)?;
    let balance_total = balances.iter().try_fold(0i64, |acc, row| {
        acc.checked_add(row.balance_minor)
            .ok_or(LedgerError::Overflow)
    })?;
    if balance_total != 0 {
        return Err(LedgerError::ConservationViolated(day.to_string()));
    }
    let checksum = balances_checksum(&balances);
    Ok(ClosingReport {
        day: day.to_string(),
        currency,
        start_ms: start,
        end_ms: end,
        debits,
        credits,
        entry_count: count,
        locked: locked_as_of(conn, currency, end)?,
        balance_total,
        checksum,
        closed_at: now,
    })
}

fn period_totals(
    conn: &Connection,
    currency: Currency,
    start: i64,
    end: i64,
) -> Result<(i64, i64, i64, i64)> {
    conn.query_row(
        "SELECT COALESCE(SUM(CASE WHEN amount_minor < 0 THEN -amount_minor ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN amount_minor > 0 THEN amount_minor ELSE 0 END), 0),
                COUNT(*), COALESCE(SUM(amount_minor), 0)
         FROM ledger_entries
         WHERE currency = ?1 AND created_at >= ?2 AND created_at < ?3",
        params![currency.as_str(), start, end],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .map_err(Into::into)
}

fn balances_as_of(conn: &Connection, currency: Currency, end: i64) -> Result<Vec<ClosingBalance>> {
    let mut stmt = conn.prepare(
        "SELECT a.name,
                COALESCE((SELECT SUM(e.amount_minor) FROM ledger_entries e
                          WHERE e.account = a.name AND e.created_at < ?2), 0)
         FROM ledger_accounts a WHERE a.currency = ?1 ORDER BY a.name",
    )?;
    let rows = stmt.query_map(params![currency.as_str(), end], |r| {
        Ok(ClosingBalance {
            account: r.get(0)?,
            balance_minor: r.get(1)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn locked_as_of(conn: &Connection, currency: Currency, end: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(amount_minor), 0) FROM ledger_holds
         WHERE currency = ?1 AND created_at < ?2
           AND (status = 'held' OR updated_at >= ?2)",
        params![currency.as_str(), end],
        |r| r.get(0),
    )?)
}

fn balances_checksum(balances: &[ClosingBalance]) -> String {
    let mut hasher = Sha256::new();
    for row in balances {
        let text = format!("{}|{}", row.account, row.balance_minor);
        hasher.update((text.len() as u64).to_be_bytes());
        hasher.update(text.as_bytes());
    }
    hex::encode(hasher.finalize())
}

fn save_closing(conn: &Connection, report: &ClosingReport) -> Result<()> {
    if let Some(existing) = closing_report(conn, &report.day, report.currency)? {
        if existing == *report {
            return Ok(());
        }
        return Err(LedgerError::ClosingConflict(
            report.day.to_string(),
            report.currency.to_string(),
        ));
    }
    conn.execute(
        "INSERT INTO ledger_closings
            (day, currency, start_ms, end_ms, debit_minor, credit_minor,
             entry_count, locked_minor, balance_total, checksum, closed_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            report.day,
            report.currency.as_str(),
            report.start_ms,
            report.end_ms,
            report.debits,
            report.credits,
            report.entry_count,
            report.locked,
            report.balance_total,
            report.checksum,
            report.closed_at
        ],
    )?;
    let balances = balances_as_of(conn, report.currency, report.end_ms)?;
    for row in balances {
        conn.execute(
            "INSERT INTO ledger_closing_balances(day, currency, account, balance_minor)
             VALUES(?1, ?2, ?3, ?4)",
            params![
                report.day,
                report.currency.as_str(),
                row.account,
                row.balance_minor
            ],
        )?;
    }
    Ok(())
}

fn closing_report(
    conn: &Connection,
    day: &str,
    currency: Currency,
) -> Result<Option<ClosingReport>> {
    conn.query_row(
        "SELECT start_ms, end_ms, debit_minor, credit_minor, entry_count,
                locked_minor, balance_total, checksum, closed_at
         FROM ledger_closings WHERE day = ?1 AND currency = ?2",
        params![day, currency.as_str()],
        |r| {
            Ok(ClosingReport {
                day: day.to_string(),
                currency,
                start_ms: r.get(0)?,
                end_ms: r.get(1)?,
                debits: r.get(2)?,
                credits: r.get(3)?,
                entry_count: r.get(4)?,
                locked: r.get(5)?,
                balance_total: r.get(6)?,
                checksum: r.get(7)?,
                closed_at: r.get(8)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// 某天的日结报告。
pub fn closings_in(conn: &Connection, day: &str) -> Result<Vec<ClosingReport>> {
    let mut stmt =
        conn.prepare("SELECT currency FROM ledger_closings WHERE day = ?1 ORDER BY currency")?;
    let rows = stmt.query_map(params![day], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        let currency = Currency::parse(&row?)?;
        if let Some(report) = closing_report(conn, day, currency)? {
            out.push(report);
        }
    }
    Ok(out)
}

/// 重新核对某天的快照与分录。任何漂移都视为账本损坏。
pub fn verify_closing_in(conn: &Connection, day: &str) -> Result<()> {
    for report in closings_in(conn, day)? {
        let balances = balances_as_of(conn, report.currency, report.end_ms)?;
        let checksum = balances_checksum(&balances);
        if checksum != report.checksum {
            return Err(LedgerError::ClosingMismatch(format!(
                "{} {}",
                report.day, report.currency
            )));
        }
        let (_, _, _, net) = period_totals(conn, report.currency, report.start_ms, report.end_ms)?;
        if net != 0 {
            return Err(LedgerError::ConservationViolated(report.day));
        }
    }
    Ok(())
}

/// 某天的账户余额快照。
pub fn closing_balances_in(
    conn: &Connection,
    day: &str,
    currency: Currency,
) -> Result<Vec<ClosingBalance>> {
    let mut stmt = conn.prepare(
        "SELECT account, balance_minor FROM ledger_closing_balances
         WHERE day = ?1 AND currency = ?2 ORDER BY account",
    )?;
    let rows = stmt.query_map(params![day, currency.as_str()], |r| {
        Ok(ClosingBalance {
            account: r.get(0)?,
            balance_minor: r.get(1)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// 便捷构造一条转账请求。
#[must_use]
pub fn transfer_request(
    business_key: impl Into<String>,
    from: &str,
    to: &str,
    amount: Money,
) -> PostingRequest {
    PostingRequest::new(
        business_key,
        "transfer",
        amount.currency(),
        vec![
            PostingLine::debit(from.to_string(), amount.minor()),
            PostingLine::credit(to.to_string(), amount.minor()),
        ],
    )
}
