//! 账本的存储层。
//!
//! 这里的函数都接受 `&Connection`；调用方既可以给独立连接，也可以在
//! commerce 的同一个事务里调用——资金分录与业务记录因此能同库提交。
//! 所有写事务必须用 `IMMEDIATE` 开启（见 [`crate::Ledger`] 与 commerce），
//! 否则"先查可用余额再写锁额"会在并发下退化。

use std::collections::BTreeMap;

use rusqlite::{named_params, params, Connection, OptionalExtension as _};
use sha2::{Digest as _, Sha256};

use crate::model::{
    Account, AccountSpec, AccountType, ApplyOutcome, Asset, ClosingBalance, ClosingReport, Entry,
    FundClass, Hold, HoldStatus, Journal, LedgerTransaction, Metadata, PostReport, PostingLine,
    PostingRequest, StatementPage,
};
use crate::{LedgerError, Result};
use wxlike_types::{Currency, Money};

/// 单页最大条数。
pub const MAX_PAGE: u32 = 200;

/// 日志类别字符串上限。
const MAX_KIND_LEN: usize = 64;

/// 长度前缀的 SHA-256 摘要（对外稳定：commerce 的幂等键摘要同用一套规范）。
pub fn request_digest_hex(parts: &[&str]) -> String {
    let owned: Vec<String> = parts.iter().map(|part| (*part).to_string()).collect();
    digest_parts(&owned)
}

fn digest_parts(parts: &[String]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    hex::encode(hasher.finalize())
}

struct JournalRow {
    journal: Journal,
    digest: String,
}

/// 只读回某个业务键在库里冻结的摘要（不加载分录）。
///
/// 对应 Formance 在 FSM apply 之前"先按幂等键取回既有哈希"的那一步
/// （`internal/infra/state/machine.go:1514-1526`）：比对哈希不需要把整笔
/// 交易的载荷都读出来。也是 Blnk `TransactionExistsByRef` →
/// `GetTransactionByRef` 的两步式判定在本仓库的形状。
///
/// # Errors
/// 存储失败。
pub fn stored_request_digest(conn: &Connection, business_key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT request_digest FROM ledger_journals WHERE business_key = ?1",
            params![business_key],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
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
///
/// 减法也用 `checked_*`：借 Formance 的溢出即拒绝纪律
/// （`processor_posting.go:150-160`：会回绕的加法等于凭空造钱或烧钱）。
///
/// # Errors
/// 账户不存在、求和溢出或存储失败。
pub fn available_minor(conn: &Connection, account: &str) -> Result<i64> {
    let booked = balance_minor(conn, account)?;
    let held = locked_minor(conn, account)?;
    booked.checked_sub(held).ok_or(LedgerError::VolumeOverflow {
        account: account.to_string(),
        current: booked,
        amount: -held,
    })
}

/// 记账。`business_key` 是唯一业务键；重复提交同参数只入账一次。
pub fn post_in(conn: &Connection, req: &PostingRequest, now: i64) -> Result<LedgerTransaction> {
    Ok(post_checked_in(conn, req, now)?.transaction)
}

/// 记账，并回报这笔是"新入账"还是"幂等重放"。
///
/// 三条分支与 Formance FSM 的分流一致
/// （`internal/infra/state/machine.go:1514-1545`）：
/// * 键命中 + 摘要一致 → 直接返回既有结果，不再跑一遍校验管道；
/// * 键命中 + 摘要不符 → [`LedgerError::IdempotencyConflict`]（这是新的
///   一次拒绝，不是重放）；
/// * 键未命中 → 全量校验后入账。
///
/// # Errors
/// 见 [`crate::LedgerError`]。
pub fn post_checked_in(conn: &Connection, req: &PostingRequest, now: i64) -> Result<PostReport> {
    let digest = posting_digest(req)?;
    if let Some(row) = journal_by_key(conn, &req.business_key)? {
        if row.digest != digest {
            return Err(LedgerError::IdempotencyConflict);
        }
        return Ok(PostReport {
            transaction: load_transaction(conn, &row.journal)?,
            outcome: ApplyOutcome::Replayed,
        });
    }
    let accounts = validate_lines(conn, req)?;
    check_available(conn, &accounts)?;
    insert_journal(conn, req, &digest, now)?;
    insert_entries(conn, req, now)?;
    Ok(PostReport {
        transaction: load_transaction_by_key(conn, &req.business_key)?,
        outcome: ApplyOutcome::Applied,
    })
}

/// 本次请求的幂等摘要（含 `asset` 与规范化后的 `metadata`）。
///
/// 摘要片段顺序固定为：kind、currency、memo、(asset)、(metadata…)、
/// 逐条 (account, amount)。`asset` 未声明、`metadata` 为空时不产生任何
/// 片段，因此与升级前的摘要逐字节相同。
fn posting_digest(req: &PostingRequest) -> Result<String> {
    if req.business_key.is_empty() || req.business_key.len() > 200 {
        return Err(LedgerError::BadInput("business key"));
    }
    if req.kind.is_empty() || req.kind.len() > MAX_KIND_LEN {
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
    if let Some(asset) = req.asset {
        parts.push(format!("a{}", asset.canonical()));
    }
    parts.extend(req.metadata.digest_parts());
    for line in &req.lines {
        if line.amount == 0 {
            return Err(LedgerError::NonPositiveAmount);
        }
        parts.push(line.account.clone());
        parts.push(line.amount.to_string());
    }
    Ok(digest_parts(&parts))
}

/// 声明式 typed asset 与币种的一致性检查。
///
/// 借 Formance `validatePostings`（`processor_transaction.go:344-366`）的
/// 位置感：资产/账户形状的校验在幂等命中之后、写库之前跑一次，写进去的
/// 就一定是校验过的。`Asset::parse` 已保证精度是规范值，这里只需核对
/// 币种——不一致就是"同一笔钱按两种单位记"，直接拒绝。
fn validate_asset(req: &PostingRequest) -> Result<()> {
    let Some(asset) = req.asset else {
        return Ok(());
    };
    if asset.currency() != req.currency {
        return Err(LedgerError::AssetMismatch {
            declared: asset.canonical(),
            journal: req.currency.to_string(),
        });
    }
    Ok(())
}

fn validate_lines(
    conn: &Connection,
    req: &PostingRequest,
) -> Result<BTreeMap<String, (Account, i64)>> {
    validate_asset(req)?;
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

/// 资产类科目的透支检查。
///
/// 两条从 Formance 借来的规则：
/// 1. 只对"资产类"源科目检查，非资产科目（我们的 revenue/marketing，
///    Formance 的 `world`）允许为负——见 `processor_posting.go:120`；
/// 2. 累加必须用溢出即拒绝的加法，不能用会回绕的加法——见
///    `processor_posting.go:150-160` 的注释（"plain uint256.Add wraps
///    silently and would let an extreme posting … money silently created
///    or destroyed (#321)"）。本仓库金额是 `i64`，同一防线用 `checked_*`
///    实现，溢出回报 [`LedgerError::VolumeOverflow`] 并带上账户与两侧数值。
fn check_available(conn: &Connection, net: &BTreeMap<String, (Account, i64)>) -> Result<()> {
    for (name, (account, amount)) in net {
        if *amount >= 0 || !account.kind.is_asset() {
            continue;
        }
        let available = available_minor(conn, name)?;
        let needed = amount.checked_neg().ok_or(LedgerError::Overflow)?;
        if available < needed {
            return Err(LedgerError::InsufficientFunds(name.clone(), available));
        }
    }
    Ok(())
}

fn insert_journal(conn: &Connection, req: &PostingRequest, digest: &str, now: i64) -> Result<()> {
    let id = journal_id(&req.business_key);
    let asset = req.asset.map(|asset| asset.canonical()).unwrap_or_default();
    conn.execute(
        "INSERT INTO ledger_journals
            (id, business_key, kind, currency, asset, metadata, entry_count,
             request_digest, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            id,
            req.business_key,
            req.kind,
            req.currency.as_str(),
            asset,
            req.metadata.encode(),
            i64::try_from(req.lines.len()).map_err(|_| LedgerError::BadInput("line count"))?,
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
            "SELECT id, business_key, kind, currency, created_at, request_digest,
                    asset, metadata
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
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                ))
            },
        )
        .optional()?;
    row.map(|(id, business_key, kind, currency, created_at, digest, asset, metadata)| {
        Ok(JournalRow {
            journal: Journal {
                id,
                business_key,
                kind,
                currency: Currency::parse(&currency)?,
                asset: decode_asset(&asset)?,
                metadata: Metadata::decode(&metadata)?,
                created_at,
            },
            digest,
        })
    })
    .transpose()
}

/// 库里的资产列 → `Option<Asset>`。空串是"未声明"（旧行与不带 asset 的
/// 新行都走这一支），非空但解析不了就是账本被改写。
fn decode_asset(raw: &str) -> Result<Option<Asset>> {
    if raw.is_empty() {
        return Ok(None);
    }
    Ok(Some(Asset::parse(raw)?))
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
        asset: journal.asset,
        metadata: journal.metadata.clone(),
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
    let parts: Vec<String> = balances
        .iter()
        .map(|row| format!("{}|{}", row.account, row.balance_minor))
        .collect();
    digest_parts(&parts)
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
         VALUES(:day, :currency, :start_ms, :end_ms, :debit, :credit,
                :entry_count, :locked, :balance_total, :checksum, :closed_at)",
        named_params! {
            ":day": report.day,
            ":currency": report.currency.as_str(),
            ":start_ms": report.start_ms,
            ":end_ms": report.end_ms,
            ":debit": report.debits,
            ":credit": report.credits,
            ":entry_count": report.entry_count,
            ":locked": report.locked,
            ":balance_total": report.balance_total,
            ":checksum": report.checksum,
            ":closed_at": report.closed_at,
        },
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;

    const EQUITY: &str = "revenue:capital";
    const ALICE: &str = "wallet:alice";
    const BOB: &str = "wallet:bob";

    fn conn() -> Connection {
        let c = Connection::open_in_memory().expect("db");
        schema::init_schema(&c).expect("schema");
        for (name, kind) in [
            (EQUITY, AccountType::Revenue),
            (ALICE, AccountType::Wallet),
            (BOB, AccountType::Wallet),
        ] {
            create_account_in(
                &c,
                &AccountSpec::new(name, kind, Currency::CNY),
                1_700_000_000_000,
            )
            .expect("account");
        }
        c
    }

    fn fund(c: &Connection, minor: i64, key: &str) {
        post_in(
            c,
            &transfer_request(key, EQUITY, ALICE, Money::cny_minor(minor)),
            1_700_000_000_000,
        )
        .expect("fund");
    }

    fn request(key: &str, lines: Vec<PostingLine>) -> PostingRequest {
        PostingRequest::new(key, "transfer", Currency::CNY, lines)
    }

    /// 升级兼容：不带 asset/metadata 时，摘要必须与旧版逐字节一致，
    /// 否则历史重试会被误判成"参数变更"。
    #[test]
    fn digest_matches_the_pre_asset_format_when_undeclared() {
        let req = request(
            "k1",
            vec![
                PostingLine::debit(ALICE, 100),
                PostingLine::credit(BOB, 100),
            ],
        );
        let legacy = digest_parts(&[
            "transfer".to_string(),
            "CNY".to_string(),
            String::new(),
            ALICE.to_string(),
            (-100_i64).to_string(),
            BOB.to_string(),
            "100".to_string(),
        ]);
        assert_eq!(posting_digest(&req).expect("digest"), legacy);
    }

    #[test]
    fn asset_and_metadata_join_the_idempotency_digest() {
        let c = conn();
        fund(&c, 1_000, "j-fund");
        let plain = request(
            "j-pay",
            vec![
                PostingLine::debit(ALICE, 100),
                PostingLine::credit(BOB, 100),
            ],
        );
        post_in(&c, &plain, 1).expect("first");

        // 同键 + 补一个规范 asset：参数变了，必须拒。
        let with_asset = plain.clone().with_asset(Asset::new(Currency::CNY));
        assert!(matches!(
            post_in(&c, &with_asset, 2),
            Err(LedgerError::IdempotencyConflict)
        ));
        // 同键 + 补 metadata：同样拒。
        let with_meta = plain.clone().with_metadata(
            Metadata::from_pairs([("src".to_string(), "app".to_string())]).expect("m"),
        );
        assert!(matches!(
            post_in(&c, &with_meta, 3),
            Err(LedgerError::IdempotencyConflict)
        ));
        // 被拒的重试一个字都没写。
        assert_eq!(journal_rows(&c), 2);

        // 换一个新业务键声明 asset + metadata：入账一次，重放一次。
        let both = with_asset.with_metadata(
            Metadata::from_pairs([("src".to_string(), "app".to_string())]).expect("m"),
        );
        let declared = both
            .clone()
            .with_business_key("j-pay-declared")
            .with_memo("declared");
        let first = post_checked_in(&c, &declared, 4).expect("post");
        assert_eq!(first.outcome, ApplyOutcome::Applied);
        assert_eq!(first.transaction.entries.len(), 2);
        let again = post_checked_in(&c, &declared, 5).expect("replay");
        assert!(again.is_replay());
        assert_eq!(again.transaction, first.transaction);
        assert_eq!(again.transaction.asset, Some(Asset::new(Currency::CNY)));
        assert_eq!(again.transaction.metadata.get("src"), Some("app"));
        assert_eq!(journal_rows(&c), 3);
        // 只改 metadata 的一个值 → 同键换参。
        let changed = declared.clone().with_metadata(
            Metadata::from_pairs([("src".to_string(), "h5".to_string())]).expect("m"),
        );
        assert!(matches!(
            post_in(&c, &changed, 6),
            Err(LedgerError::IdempotencyConflict)
        ));
        assert_eq!(journal_rows(&c), 3);
    }

    fn journal_rows(c: &Connection) -> i64 {
        c.query_row("SELECT COUNT(*) FROM ledger_journals", [], |r| {
            r.get::<_, i64>(0)
        })
        .expect("count")
    }

    /// 借 Formance 的 typed asset：声明错了币种就是记错账。
    #[test]
    fn declared_asset_must_match_the_journal_currency() {
        let c = conn();
        let usd = Asset::parse("USD/2").expect("usd");
        let req = request(
            "j-asset",
            vec![
                PostingLine::debit(ALICE, 1),
                PostingLine::credit(BOB, 1),
            ],
        )
        .with_asset(usd);
        assert!(matches!(
            post_in(&c, &req, 1),
            Err(LedgerError::AssetMismatch { .. })
        ));
        // 非规范写法在进数据库之前就被拒。
        assert!(matches!(
            request("j-asset", vec![PostingLine::debit(ALICE, 1), PostingLine::credit(BOB, 1)])
                .try_with_asset("CNY/1"),
            Err(LedgerError::BadAsset(_))
        ));
    }

    /// 幂等分流要能区分"这次真入账"和"这次只是重放"。
    #[test]
    fn replay_is_reported_separately_from_a_fresh_post() {
        let c = conn();
        fund(&c, 1_000, "j-fund");
        let req = request(
            "j-move",
            vec![
                PostingLine::debit(ALICE, 300),
                PostingLine::credit(BOB, 300),
            ],
        );
        let first = post_checked_in(&c, &req, 1).expect("first");
        assert_eq!(first.outcome, ApplyOutcome::Applied);
        let second = post_checked_in(&c, &req, 2).expect("second");
        assert_eq!(second.outcome, ApplyOutcome::Replayed);
        assert_eq!(second.transaction, first.transaction);
        assert_eq!(balance_minor(&c, BOB).expect("bob"), 300);
        assert!(matches!(
            post_checked_in(&c, &request("j-move", req.lines.clone()).with_memo("改了附言"), 3),
            Err(LedgerError::IdempotencyConflict)
        ));
    }

    /// 分录集合在入账那一刻就定死：事后想给既有 journal 再补一条，
    /// 由数据库拒绝（把"借贷必须相抵"从一次性的 Rust 校验升级成约束）。
    #[test]
    fn appending_an_extra_entry_to_a_journal_is_refused_by_the_database() {
        let c = conn();
        fund(&c, 1_000, "j-fund");
        let error = c
            .execute(
                "INSERT INTO ledger_entries
                    (journal_id, seq, account, amount_minor, currency, memo, created_at)
                 SELECT id, 3, 'wallet:alice', -1, 'CNY', '多一条', 1
                 FROM ledger_journals WHERE business_key = 'j-fund'",
                [],
            )
            .expect_err("extra entry must be refused");
        assert!(
            error.to_string().contains("frozen"),
            "unexpected error: {error}"
        );
        verify_journals(&c).expect("still balanced");
    }

    #[test]
    fn stored_request_digest_reads_only_the_frozen_hash() {
        let c = conn();
        assert!(stored_request_digest(&c, "j-none").expect("query").is_none());
        fund(&c, 500, "j-fund");
        let digest = stored_request_digest(&c, "j-fund")
            .expect("query")
            .expect("present");
        assert_eq!(digest.len(), 64);
        assert_eq!(digest, stored_request_digest(&c, "j-fund").expect("q").expect("p2"));
    }

    #[test]
    fn metadata_survives_the_round_trip_through_the_journal_row() {
        let c = conn();
        fund(&c, 1_000, "j-fund");
        let meta = Metadata::from_pairs([
            ("z".to_string(), "最后".to_string()),
            ("a".to_string(), "第一".to_string()),
        ])
        .expect("pairs");
        let req = request(
            "j-meta",
            vec![
                PostingLine::debit(ALICE, 10),
                PostingLine::credit(BOB, 10),
            ],
        )
        .with_metadata(meta.clone());
        post_in(&c, &req, 1).expect("post");
        let tx = transaction_by_key(&c, "j-meta")
            .expect("load")
            .expect("present");
        assert_eq!(tx.metadata, meta);
        assert_eq!(tx.metadata.iter().next().map(|(k, _)| k.as_str()), Some("a"));
    }
}
