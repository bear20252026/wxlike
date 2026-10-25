//! 绑卡。只保存供应商令牌与掩码，任何路径都不落 PAN/CVV。

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::error::{CommerceError, Result};
use crate::store;
use crate::Commerce;

/// 卡状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardStatus {
    Active,
    Unbound,
}

impl CardStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Unbound => "unbound",
        }
    }
}

/// 已绑定的卡（掩码视图）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundCard {
    pub id: String,
    pub owner_id: String,
    pub brand: String,
    pub last4: String,
    pub exp_month: u32,
    pub exp_year: u32,
    pub status: CardStatus,
    pub created_at: i64,
}

/// 绑卡输入。只在本函数生命周期内存在，不落库。
pub struct CardInput<'a> {
    pub pan: &'a str,
    pub cvv: &'a str,
    pub brand: &'a str,
    pub exp_month: u32,
    pub exp_year: u32,
}

impl Commerce {
    /// 绑定银行卡。
    ///
    /// # Errors
    /// 卡号/CVV/有效期非法、已过期、幂等键冲突或存储失败。
    pub fn bind_card(
        &self,
        owner: &str,
        input: &CardInput<'_>,
        business_key: &str,
    ) -> Result<BoundCard> {
        store::validate_id("owner", owner)?;
        store::validate_key(business_key)?;
        validate_brand(input.brand)?;
        if !valid_pan(input.pan) {
            return Err(CommerceError::BadInput("card number is invalid".into()));
        }
        if !(input.cvv.len() == 3 || input.cvv.len() == 4)
            || !input.cvv.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(CommerceError::BadInput("cvv is invalid".into()));
        }
        let now = self.now();
        if !(1..=12).contains(&input.exp_month)
            || !valid_expiry(input.exp_month, input.exp_year, now)
        {
            return Err(CommerceError::BadInput(
                "card expiry is invalid or already past".into(),
            ));
        }
        let digest = store::digest_hex(&[
            owner,
            input.brand,
            input.pan,
            input.cvv,
            &input.exp_month.to_string(),
            &input.exp_year.to_string(),
        ]);
        let scoped_key = format!("card:{business_key}");
        let token = format!("tok_{}", store::random_hex(self.entropy(), 18));
        let last4 = input.pan[input.pan.len() - 4..].to_string();
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = load_by_key(&tx, &scoped_key)?;
        if let Some((stored_digest, card)) = existing {
            if stored_digest != digest {
                return Err(CommerceError::IdempotencyConflict);
            }
            return Ok(card);
        }
        let id = store::new_id(self.entropy());
        tx.execute(
            "INSERT INTO commerce_cards
                (id, business_key, owner_id, provider, provider_token, brand, last4,
                 exp_month, exp_year, status, request_digest, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'active', ?10, ?11, ?11)",
            params![
                id,
                scoped_key,
                owner,
                self.provider_name(),
                token,
                input.brand,
                last4,
                i64::from(input.exp_month),
                i64::from(input.exp_year),
                digest,
                now
            ],
        )?;
        let card = load_by_id(&tx, &id)?.ok_or(CommerceError::NotFound)?;
        tx.commit()?;
        Ok(card)
    }

    /// 当前用户名下的有效卡。
    ///
    /// # Errors
    /// 参数非法或存储失败。
    pub fn list_cards(&self, owner: &str) -> Result<Vec<BoundCard>> {
        store::validate_id("owner", owner)?;
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, owner_id, brand, last4, exp_month, exp_year, status, created_at
             FROM commerce_cards WHERE owner_id = ?1 AND status = 'active'
             ORDER BY created_at DESC, id DESC",
        )?;
        let rows = stmt.query_map(params![owner], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, i64>(7)?,
            ))
        })?;
        let mut cards = Vec::new();
        for row in rows {
            let (id, owner_id, brand, last4, month, year, status, created_at) = row?;
            cards.push(BoundCard {
                id,
                owner_id,
                brand,
                last4,
                exp_month: month as u32,
                exp_year: year as u32,
                status: parse_status(&status)?,
                created_at,
            });
        }
        Ok(cards)
    }

    /// 解绑；重复解绑幂等。
    ///
    /// # Errors
    /// 卡不存在或不属于调用方。
    pub fn unbind_card(&self, owner: &str, card_id: &str) -> Result<()> {
        store::validate_id("owner", owner)?;
        store::validate_id("card", card_id)?;
        let conn = self.lock();
        let owner_of: Option<String> = conn
            .query_row(
                "SELECT owner_id FROM commerce_cards WHERE id = ?1",
                params![card_id],
                |r| r.get(0),
            )
            .optional()?;
        match owner_of {
            Some(stored) if stored == owner => {}
            Some(_) => return Err(CommerceError::Forbidden),
            None => return Err(CommerceError::NotFound),
        }
        conn.execute(
            "UPDATE commerce_cards SET status = 'unbound', updated_at = ?1
             WHERE id = ?2 AND status = 'active'",
            params![self.now(), card_id],
        )?;
        Ok(())
    }
}

fn validate_brand(brand: &str) -> Result<()> {
    if brand.is_empty() || brand.len() > 32 || brand.chars().any(char::is_control) {
        return Err(CommerceError::BadInput("card brand is invalid".into()));
    }
    Ok(())
}

fn valid_pan(pan: &str) -> bool {
    if !(12..=19).contains(&pan.len()) || !pan.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let mut sum = 0u32;
    let mut double = false;
    for byte in pan.bytes().rev() {
        let mut digit = u32::from(byte - b'0');
        if double {
            digit *= 2;
            if digit > 9 {
                digit -= 9;
            }
        }
        sum += digit;
        double = !double;
    }
    sum.is_multiple_of(10)
}

fn valid_expiry(month: u32, year: u32, now_ms: i64) -> bool {
    if !(2000..=2100).contains(&year) {
        return false;
    }
    let (now_year, now_month) = civil_from_ms(now_ms);
    year > now_year || (year == now_year && month >= now_month)
}

/// 毫秒时间戳 → (UTC 年, 月)，足够用于卡有效期判断。
fn civil_from_ms(ms: i64) -> (u32, u32) {
    let days = ms.div_euclid(86_400_000);
    let (year, month, _day) = civil_from_days(days);
    (year as u32, month)
}

/// Howard Hinnant 的 civil_from_days。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

fn parse_status(raw: &str) -> Result<CardStatus> {
    match raw {
        "active" => Ok(CardStatus::Active),
        "unbound" => Ok(CardStatus::Unbound),
        _ => Err(CommerceError::BadInput("stored card status".into())),
    }
}

fn load_by_key(conn: &Connection, key: &str) -> Result<Option<(String, BoundCard)>> {
    let raw = conn
        .query_row(
            "SELECT id, owner_id, brand, last4, exp_month, exp_year, status, created_at,
                    request_digest
             FROM commerce_cards WHERE business_key = ?1",
            params![key],
            scan_card,
        )
        .optional()?;
    raw.map(|(digest, card)| Ok((digest, card))).transpose()
}

fn load_by_id(conn: &Connection, id: &str) -> Result<Option<BoundCard>> {
    let raw = conn
        .query_row(
            "SELECT id, owner_id, brand, last4, exp_month, exp_year, status, created_at,
                    request_digest
             FROM commerce_cards WHERE id = ?1",
            params![id],
            scan_card,
        )
        .optional()?;
    raw.map(|(_, card)| Ok(card)).transpose()
}

fn scan_card(r: &rusqlite::Row<'_>) -> rusqlite::Result<(String, BoundCard)> {
    let status: String = r.get(6)?;
    Ok((
        r.get(8)?,
        BoundCard {
            id: r.get(0)?,
            owner_id: r.get(1)?,
            brand: r.get(2)?,
            last4: r.get(3)?,
            exp_month: r.get::<_, i64>(4)? as u32,
            exp_year: r.get::<_, i64>(5)? as u32,
            status: match status.as_str() {
                "active" => CardStatus::Active,
                "unbound" => CardStatus::Unbound,
                _ => {
                    return Err(rusqlite::Error::IntegralValueOutOfRange(6, 0));
                }
            },
            created_at: r.get(7)?,
        },
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::infrastructure::sandbox::SandboxProvider;
    use crate::testkit::{FixedEntropy, ManualClock, StaticMembership};
    use crate::{Commerce, CommerceConfig, Ports};

    fn commerce() -> Commerce {
        let ports = Ports::new(
            Arc::new(SandboxProvider::new()),
            Arc::new(StaticMembership::new()),
        )
        .with_clock_entropy(
            Arc::new(ManualClock::new(1_700_000_000_000)),
            Arc::new(FixedEntropy::default()),
        );
        Commerce::in_memory(CommerceConfig::sandbox(b"secret".to_vec()), ports).expect("commerce")
    }

    #[test]
    fn raw_row_never_contains_pan_or_cvv() {
        let commerce = commerce();
        let pan = "4242424242424242";
        let cvv = "737";
        commerce
            .bind_card(
                "alice",
                &super::CardInput {
                    pan,
                    cvv,
                    brand: "visa",
                    exp_month: 12,
                    exp_year: 2030,
                },
                "card-raw",
            )
            .expect("bind");

        let conn = commerce.lock();
        let mut stmt = conn
            .prepare("SELECT * FROM commerce_cards WHERE owner_id = 'alice'")
            .expect("prepare");
        let column_count = stmt.column_count();
        let mut rows = stmt.query([]).expect("query");
        let row = rows.next().expect("row").expect("some");
        for index in 0..column_count {
            let raw = row.get_ref(index).expect("column");
            let value = match raw {
                rusqlite::types::ValueRef::Null => String::new(),
                rusqlite::types::ValueRef::Integer(number) => number.to_string(),
                rusqlite::types::ValueRef::Real(number) => number.to_string(),
                rusqlite::types::ValueRef::Text(bytes) => {
                    String::from_utf8_lossy(bytes).into_owned()
                }
                rusqlite::types::ValueRef::Blob(bytes) => {
                    String::from_utf8_lossy(bytes).into_owned()
                }
            };
            assert!(!value.contains(pan), "column {index} leaked the PAN");
            assert_ne!(value, cvv, "column {index} stored the CVV verbatim");
        }
        // 3 位 CVV 可能偶然出现在 64 位十六进制摘要里，所以只校验摘要形态。
        let digest: String = row.get(10).expect("digest");
        assert_eq!(digest.len(), 64);
        assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()));
        let token: String = row.get(4).expect("token");
        assert!(token.starts_with("tok_"));
        let last4: String = row.get(6).expect("last4");
        assert_eq!(last4, "4242");
    }
}
