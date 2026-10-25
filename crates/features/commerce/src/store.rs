//! commerce 的共享存储工具：ID/摘要/HMAC、账户命名、outbox。

use hmac::{Hmac, Mac as _};
use rusqlite::{params, Connection};
use sha2::{Digest as _, Sha256};
use wxlike_ledger::{AccountSpec, AccountType, Currency, FundClass, Money};

use crate::error::{CommerceError, Result};
use crate::ports::Entropy;

type HmacSha256 = Hmac<Sha256>;

/// 打开 commerce 的连接并建表。
pub(crate) fn open_conn(path: Option<&str>) -> Result<Connection> {
    let conn = match path {
        Some(path) => {
            if let Some(dir) = std::path::Path::new(path).parent() {
                if !dir.as_os_str().is_empty() {
                    std::fs::create_dir_all(dir)
                        .map_err(|_| CommerceError::BadInput("commerce database path".into()))?;
                }
            }
            Connection::open(path)?
        }
        None => Connection::open_in_memory()?,
    };
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA busy_timeout = 5000;
         PRAGMA foreign_keys = ON;",
    )?;
    crate::schema::init_schema(&conn)?;
    Ok(conn)
}

/// 随机 ID（32 hex）。
pub(crate) fn new_id(entropy: &dyn Entropy) -> String {
    let mut bytes = [0u8; 16];
    entropy.fill(&mut bytes);
    hex::encode(bytes)
}

/// 随机字节的 hex。
pub(crate) fn random_hex(entropy: &dyn Entropy, len: usize) -> String {
    let mut bytes = vec![0u8; len];
    entropy.fill(&mut bytes);
    hex::encode(bytes)
}

/// 长度前缀的 SHA-256 摘要。
pub(crate) fn digest_hex(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    hex::encode(hasher.finalize())
}

/// HMAC-SHA256，输出 hex。
pub(crate) fn hmac_hex(secret: &[u8], parts: &[&str]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    for part in parts {
        mac.update(&(part.len() as u64).to_be_bytes());
        mac.update(part.as_bytes());
    }
    hex::encode(mac.finalize().into_bytes())
}

/// 常量时间比较 HMAC。
pub(crate) fn hmac_verify(secret: &[u8], parts: &[&str], presented_hex: &str) -> bool {
    let Ok(presented) = hex::decode(presented_hex) else {
        return false;
    };
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    for part in parts {
        mac.update(&(part.len() as u64).to_be_bytes());
        mac.update(part.as_bytes());
    }
    mac.verify_slice(&presented).is_ok()
}

/// 不透明 ID：非空、长度受限、不含控制字符/空白。
pub(crate) fn validate_id(field: &str, value: &str) -> Result<()> {
    let ok = !value.is_empty()
        && value.len() <= 200
        && !value.chars().any(|c| c.is_control() || c.is_whitespace());
    if ok {
        Ok(())
    } else {
        Err(CommerceError::BadInput(format!("{field} is invalid")))
    }
}

/// 幂等键。
pub(crate) fn validate_key(value: &str) -> Result<()> {
    let ok = !value.is_empty()
        && value.len() <= 200
        && !value.chars().any(|c| c.is_control() || c.is_whitespace());
    if ok {
        Ok(())
    } else {
        Err(CommerceError::BadInput("idempotency key is invalid".into()))
    }
}

/// 转账备注：最多 20 个字符，不允许控制字符。
pub(crate) fn validate_note(note: &str) -> Result<()> {
    let ok = note.chars().count() <= 20 && !note.chars().any(char::is_control);
    if ok {
        Ok(())
    } else {
        Err(CommerceError::BadInput(
            "note must be at most 20 printable characters".into(),
        ))
    }
}

/// 金额上限校验。
pub(crate) fn check_limit(amount: Money, max_minor: i64) -> Result<()> {
    if !amount.is_positive() {
        return Err(CommerceError::BadInput("amount must be positive".into()));
    }
    if amount.minor() > max_minor {
        return Err(CommerceError::AmountTooLarge);
    }
    Ok(())
}

/// 用户钱包账户名。
pub(crate) fn wallet_account(owner: &str, currency: Currency) -> String {
    format!("wallet:{owner}:{currency}")
}

/// 外部世界（供应商侧）账户名。
///
/// 这是一个非资产类科目，允许为负：钱包充值对应它记借方，提现/退款
/// 对应贷方，净额就是"供应商还欠我们多少"。把外部世界建成 Revenue
/// 而不是 Clearing，是为了不伪造一笔不存在的本地在途资产。
pub(crate) fn external_account(provider: &str, currency: Currency) -> String {
    format!("external:{provider}:{currency}")
}

/// 幂等建外部世界账户。
pub(crate) fn ensure_external(
    conn: &Connection,
    provider: &str,
    currency: Currency,
    fund_class: FundClass,
    now: i64,
) -> Result<String> {
    let name = external_account(provider, currency);
    let spec =
        AccountSpec::new(name.clone(), AccountType::Revenue, currency).with_fund_class(fund_class);
    wxlike_ledger::create_account_in(conn, &spec, now)?;
    Ok(name)
}

/// 托管账户名。
pub(crate) fn escrow_account(kind: &str, id: &str, currency: Currency) -> String {
    format!("escrow:{kind}:{id}:{currency}")
}

/// 幂等建钱包。
pub(crate) fn ensure_wallet(
    conn: &Connection,
    owner: &str,
    currency: Currency,
    fund_class: FundClass,
    now: i64,
) -> Result<String> {
    validate_id("owner", owner)?;
    let name = wallet_account(owner, currency);
    let spec = AccountSpec::new(name.clone(), AccountType::Wallet, currency)
        .with_owner(owner)
        .with_fund_class(fund_class);
    wxlike_ledger::create_account_in(conn, &spec, now)?;
    Ok(name)
}

/// 幂等建托管账户。
pub(crate) fn ensure_escrow(
    conn: &Connection,
    kind: &str,
    id: &str,
    currency: Currency,
    fund_class: FundClass,
    now: i64,
) -> Result<String> {
    let name = escrow_account(kind, id, currency);
    let spec =
        AccountSpec::new(name.clone(), AccountType::Escrow, currency).with_fund_class(fund_class);
    wxlike_ledger::create_account_in(conn, &spec, now)?;
    Ok(name)
}

/// 对账差异的挂账科目；非资产类，允许为正负。
pub(crate) fn ensure_suspense(
    conn: &Connection,
    currency: Currency,
    fund_class: FundClass,
    now: i64,
) -> Result<String> {
    let name = format!("reconciliation:suspense:{currency}");
    let spec =
        AccountSpec::new(name.clone(), AccountType::Revenue, currency).with_fund_class(fund_class);
    wxlike_ledger::create_account_in(conn, &spec, now)?;
    Ok(name)
}

/// 写 outbox 事件。事件只带业务对象 ID 和最小状态，不广播用户资料。
pub(crate) fn emit(
    conn: &Connection,
    entropy: &dyn Entropy,
    event_type: &str,
    aggregate_id: &str,
    payload: &serde_json::Value,
    now: i64,
) -> Result<String> {
    let event_id = new_id(entropy);
    conn.execute(
        "INSERT INTO commerce_outbox
            (event_id, aggregate_id, event_type, payload_json, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5)",
        params![event_id, aggregate_id, event_type, payload.to_string(), now],
    )?;
    Ok(event_id)
}

/// 把余额不足映射成业务错误。
pub(crate) fn map_ledger(error: wxlike_ledger::LedgerError) -> CommerceError {
    match error {
        wxlike_ledger::LedgerError::InsufficientFunds(..) => CommerceError::InsufficientFunds,
        other => CommerceError::Ledger(other),
    }
}

/// 解析金额字符串为最小单位整数（币种小数位固定为 2）。
///
/// 拒绝：空串、正负号、科学计数法、千分位、超过 2 位小数、前导点、
/// 溢出以及超过 `max_minor`。
pub(crate) fn parse_decimal_minor(raw: &str, max_minor: i64) -> Result<i64> {
    let text = raw.trim();
    if text.is_empty() || text.len() > 32 {
        return Err(CommerceError::BadInput("amount is empty".into()));
    }
    if !text.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return Err(CommerceError::BadInput(
            "amount must be plain decimal digits".into(),
        ));
    }
    let mut parts = text.split('.');
    let whole = parts.next().unwrap_or("");
    let fraction = parts.next().unwrap_or("");
    if parts.next().is_some() || whole.is_empty() || fraction.len() > 2 || text.ends_with('.') {
        return Err(CommerceError::BadInput(
            "amount must have at most two decimal places".into(),
        ));
    }
    let whole_minor = parse_digits(whole)?
        .checked_mul(100)
        .ok_or(CommerceError::AmountTooLarge)?;
    let fraction_minor = match fraction.len() {
        0 => 0,
        1 => parse_digits(fraction)?
            .checked_mul(10)
            .ok_or(CommerceError::AmountTooLarge)?,
        _ => parse_digits(fraction)?,
    };
    let minor = whole_minor
        .checked_add(fraction_minor)
        .ok_or(CommerceError::AmountTooLarge)?;
    if minor <= 0 {
        return Err(CommerceError::BadInput("amount must be positive".into()));
    }
    if minor > max_minor {
        return Err(CommerceError::AmountTooLarge);
    }
    Ok(minor)
}

fn parse_digits(raw: &str) -> Result<i64> {
    if raw.is_empty() {
        return Ok(0);
    }
    raw.parse::<i64>()
        .map_err(|_| CommerceError::AmountTooLarge)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_amount_parsing_is_strict() {
        assert_eq!(parse_decimal_minor("88.88", 1_000_000).expect("ok"), 8888);
        assert_eq!(parse_decimal_minor("0.01", 1_000_000).expect("ok"), 1);
        assert_eq!(parse_decimal_minor("12", 1_000_000).expect("ok"), 1200);
        for bad in [
            "", "-1", "+1", "1e3", "1,000", "1.234", ".5", "1.", " 1 2 ", "0",
        ] {
            assert!(parse_decimal_minor(bad, 1_000_000).is_err(), "{bad}");
        }
        assert!(parse_decimal_minor("10000.01", 1_000_000).is_err());
    }

    #[test]
    fn note_is_bounded_in_characters_not_bytes() {
        assert!(validate_note("展览门票 AA").is_ok());
        assert!(validate_note(&"字".repeat(20)).is_ok());
        assert!(validate_note(&"字".repeat(21)).is_err());
        assert!(validate_note("a\nb").is_err());
    }

    #[test]
    fn hmac_rejects_tampering() {
        let tag = hmac_hex(b"secret", &["a", "1"]);
        assert!(hmac_verify(b"secret", &["a", "1"], &tag));
        assert!(!hmac_verify(b"secret", &["a", "2"], &tag));
        assert!(!hmac_verify(b"other", &["a", "1"], &tag));
        assert!(!hmac_verify(b"secret", &["a", "1"], "zz"));
    }
}
