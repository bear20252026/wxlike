//! commerce 的共享存储工具：ID/摘要/HMAC、账户命名、outbox。

use hmac::{Hmac, Mac as _};
use rusqlite::{params, Connection};
use sha2::Sha256;
use wxlike_ledger::{AccountSpec, AccountType, Currency, FundClass, Money};

use crate::error::{CommerceError, Result};
use crate::ports::Entropy;

type HmacSha256 = Hmac<Sha256>;

/// 打开 commerce 的连接并建表。
pub(crate) fn open_conn(path: Option<&str>) -> Result<Connection> {
    let conn = wxlike_ledger::open_connection(path).map_err(map_ledger)?;
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

/// 长度前缀的 SHA-256 摘要（与账本的 request_digest 同一套规范）。
pub(crate) fn digest_hex(parts: &[&str]) -> String {
    wxlike_ledger::request_digest_hex(parts)
}

/// 常量时间比较，避免签名比对泄漏时序。
pub(crate) fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// 幂等重放判定：同参命中返回既有结果，异参拒绝，未命中放行新建。
///
/// 摘要本身也要过形状检查：一条长度不对的存储摘要不配拿来"相等即放行"。
/// 这是 Formance 幂等分流的另一半——它比对的是固定宽度的 proposal 哈希
/// （`internal/infra/state/machine.go:1514-1526`），空值或截断值在哈希
/// 阶段就进不了比对。
pub(crate) fn replay<T>(loaded: Option<(String, T)>, digest: &str) -> Result<Option<T>> {
    if !is_canonical_digest(digest) {
        return Err(CommerceError::BadInput(
            "idempotency digest is not canonical".into(),
        ));
    }
    match loaded {
        Some((stored, value)) if stored == digest => Ok(Some(value)),
        Some((stored, _)) if !is_canonical_digest(&stored) => Err(CommerceError::BadInput(
            "stored idempotency digest is corrupt".into(),
        )),
        Some(_) => Err(CommerceError::IdempotencyConflict),
        None => Ok(None),
    }
}

/// 一次多腿操作的全部幂等腿。
///
/// 一条腿 = `(库里冻结的摘要, 本次请求的摘要)`；前者为 `None` 表示这条腿
/// 还没落库。
pub(crate) struct Leg<'a> {
    pub(crate) name: &'static str,
    pub(crate) stored: Option<&'a str>,
    pub(crate) digest: &'a str,
}

/// 多腿重放判定 —— 借 Blnk 的 `replayInflightAction`
/// （`transaction_inflight.go:726-746`）：**只有每一条腿都已按下发的摘要
/// 落库，才算"这次是重放"**；任何一条腿还缺着，都不算完成，调用方要继续
/// 把效果做完。部分完成被当成重放是资金事故，部分完成被当成"还没做"才
/// 是 at-least-once 的正确姿势。
///
/// 与 Blnk 的两处刻意差异，都写在这里而不是藏在实现里：
/// * **不借** `findInflightActionByRef` 里"金额为 nil/0 视为与既有金额
///   相同"的宽松匹配（`transaction_inflight.go:708`）——本仓库要求逐字节
///   相等；
/// * 只有**主腿**（数组第一项）摘要不符才判幂等冲突。副腿不符只说明"这次
///   不是重放"，要把拒绝权交回它自己的去重闸门（事件表 UNIQUE →
///   [`crate::error::CommerceError::Replay`]），否则事件表那条更精确的错误
///   会被幂等冲突吃掉。
///
/// # Errors
/// 主腿摘要被改写（账本损坏）或主腿摘要不符（同键换参）。
pub(crate) fn replay_legs(legs: &[Leg<'_>]) -> Result<bool> {
    let Some((primary, rest)) = legs.split_first() else {
        return Ok(false);
    };
    if !is_canonical_digest(primary.digest) {
        return Err(CommerceError::BadInput(format!(
            "request digest for {} is not canonical",
            primary.name
        )));
    }
    let Some(stored) = primary.stored else {
        // 主腿都没落库，谈不上重放；副腿的情况由各自的闸门处理。
        return Ok(false);
    };
    if !is_canonical_digest(stored) {
        return Err(CommerceError::BadInput(format!(
            "stored digest for {} is corrupt",
            primary.name
        )));
    }
    if stored != primary.digest {
        return Err(CommerceError::IdempotencyConflict);
    }
    for leg in rest {
        if !is_canonical_digest(leg.digest) {
            return Err(CommerceError::BadInput(format!(
                "request digest for {} is not canonical",
                leg.name
            )));
        }
        if leg.stored != Some(leg.digest) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// SHA-256 的小写十六进制摘要恰好 64 位；除此以外的值都不是摘要。
fn is_canonical_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
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
///
/// 形状规则借 Formance `validateIdempotencyKey`
/// （`internal/application/admission/admission.go:1161-1199`：限长 256 +
/// 必须是合法 UTF-8）。本仓库在此基础上更紧：上限 200、非空、且不允许
/// 控制字符与空白——空白/换行的键在 SQLite 里与去空白后的键长得一样，
/// 留到查询层才炸。
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

/// 转账/红包备注：最多 20 个字符，不允许控制字符。
pub(crate) fn validate_note(note: &str) -> Result<()> {
    const MEMO_MAX_CHARS: usize = 20;
    let ok = note.chars().count() <= MEMO_MAX_CHARS && !note.chars().any(char::is_control);
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
    const MAX_INPUT_LEN: usize = 32;
    const FRACTION_DIGITS_MAX: usize = 2;
    const MINOR_PER_UNIT: i64 = 100;
    let text = raw.trim();
    if text.is_empty() || text.len() > MAX_INPUT_LEN {
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
    if parts.next().is_some()
        || whole.is_empty()
        || fraction.len() > FRACTION_DIGITS_MAX
        || text.ends_with('.')
    {
        return Err(CommerceError::BadInput(
            "amount must have at most two decimal places".into(),
        ));
    }
    let whole_minor = parse_digits(whole)?
        .checked_mul(MINOR_PER_UNIT)
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

    /// 借来的那套重放判定必须"更紧"，不能把既有幂等保证放软。
    #[test]
    fn replay_requires_a_canonical_digest() {
        let good = digest_hex(&["kind", "1"]);
        assert_eq!(good.len(), 64);
        // 未命中 → 放行新建
        assert!(replay::<u32>(None, &good).expect("fresh").is_none());
        // 命中同摘要 → 返回既有结果
        assert_eq!(
            replay(Some((good.clone(), 7_u32)), &good).expect("hit"),
            Some(7)
        );
        // 同键换参 → 冲突（这条是既有验收的底线，不能退）
        assert!(matches!(
            replay(Some((good.clone(), 7_u32)), &digest_hex(&["kind", "2"])),
            Err(CommerceError::IdempotencyConflict)
        ));
        // 库里的摘要被改写 → 不当成"相等即放行"
        assert!(matches!(
            replay(Some(("".to_string(), 7_u32)), &good),
            Err(CommerceError::BadInput(_))
        ));
        // 本次算出来的摘要不是摘要 → 直接拒绝，不进比对
        assert!(matches!(
            replay::<u32>(None, "not-a-digest"),
            Err(CommerceError::BadInput(_))
        ));
    }

    /// Blnk 式多腿判定：全部腿落库才算重放，部分完成要继续做完。
    #[test]
    fn replay_legs_only_replays_when_every_leg_landed() {
        fn pair<'a>(
            primary: (&'a str, Option<&'a str>),
            secondary: (&'a str, Option<&'a str>),
        ) -> Vec<Leg<'a>> {
            vec![
                Leg {
                    name: "primary",
                    stored: primary.1,
                    digest: primary.0,
                },
                Leg {
                    name: "secondary",
                    stored: secondary.1,
                    digest: secondary.0,
                },
            ]
        }
        let a = digest_hex(&["a"]);
        let b = digest_hex(&["b"]);
        let other = digest_hex(&["x"]);
        // 一条腿都没落库：首次执行，不是重放。
        assert!(!replay_legs(&pair((&a, None), (&b, None))).expect("fresh"));
        // 主腿在、副腿缺：部分完成 ≠ 重放
        assert!(!replay_legs(&pair((&a, Some(&a)), (&b, None))).expect("partial"));
        // 主腿缺、副腿在：也不是重放（拒绝权留给各自的去重闸门）
        assert!(!replay_legs(&pair((&a, None), (&b, Some(&b)))).expect("partial"));
        // 两条腿都按下发摘要落库：重放。
        assert!(replay_legs(&pair((&a, Some(&a)), (&b, Some(&b)))).expect("complete"));
        // 副腿摘要不符：不是重放，让事件表去重报它自己的错
        assert!(
            !replay_legs(&pair((&a, Some(&a)), (&b, Some(&other)))).expect("secondary mismatch")
        );
        // 主腿摘要不符：同键换参，照旧冲突
        assert!(matches!(
            replay_legs(&pair((&a, Some(&other)), (&b, Some(&b)))),
            Err(CommerceError::IdempotencyConflict)
        ));
        // 主腿摘要被改写：账本损坏，不能"相等即放行"
        assert!(matches!(
            replay_legs(&pair((&a, Some("")), (&b, Some(&b)))),
            Err(CommerceError::BadInput(_))
        ));
        // 空数组不构成任何断言
        assert!(!replay_legs(&[]).expect("no legs"));
    }
}
