//! 会员卡、券与票证：规则版本化、一次性核销令牌、票证条码不含凭据。

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::Value;

use crate::error::{CommerceError, Result};
use crate::store;
use crate::Commerce;

/// 规则 JSON 体积上限。
const MAX_RULE_JSON: usize = 8_192;
/// 权益 payload JSON 体积上限。
const MAX_PAYLOAD_JSON: usize = 4_096;
/// 有效期天数：规则可给 1..=3650，缺省一年。
const MAX_VALID_DAYS: i64 = 3_650;
const DEFAULT_VALID_DAYS: i64 = 365;
/// 一次性核销令牌长度上限（hex）。
const MAX_TOKEN_LEN: usize = 128;

/// 权益类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntitlementKind {
    Card,
    Coupon,
    Ticket,
}

impl EntitlementKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Card => "card",
            Self::Coupon => "coupon",
            Self::Ticket => "ticket",
        }
    }

    /// 解析类别。
    ///
    /// # Errors
    /// 未知类别。
    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "card" => Ok(Self::Card),
            "coupon" => Ok(Self::Coupon),
            "ticket" => Ok(Self::Ticket),
            _ => Err(CommerceError::BadInput("unknown entitlement kind".into())),
        }
    }
}

/// 已发布的规则版本。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntitlementRule {
    pub issuer_id: String,
    pub kind: EntitlementKind,
    pub version: i64,
    pub rule: Value,
    pub digest: String,
    pub created_at: i64,
}

/// 已发放的权益。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entitlement {
    pub id: String,
    pub issuer_id: String,
    pub owner_id: Option<String>,
    pub kind: EntitlementKind,
    pub rule_version: i64,
    pub status: EntitlementStatus,
    pub valid_from: i64,
    pub valid_to: i64,
    pub payload: Value,
    pub issued_at: i64,
    pub redeemed_at: Option<i64>,
}

/// 权益状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntitlementStatus {
    Active,
    Redeemed,
}

impl EntitlementStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Redeemed => "redeemed",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "active" => Ok(Self::Active),
            "redeemed" => Ok(Self::Redeemed),
            _ => Err(CommerceError::BadInput("stored entitlement status".into())),
        }
    }
}

impl Entitlement {
    /// 状态的稳定字符串。
    #[must_use]
    pub fn status_name(&self) -> &'static str {
        self.status.as_str()
    }
}

/// 一次性核销令牌。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedemptionToken {
    pub token: String,
    pub expires_at: i64,
}

/// 核销记录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redemption {
    pub id: String,
    pub entitlement_id: String,
    pub actor_id: String,
    pub redeemed_at: i64,
}

/// 票证条码：只含权益 ID 与短签名，无用户凭据。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Barcode {
    pub format: String,
    pub value: String,
}

impl Commerce {
    /// 发布一版规则；旧版本保留且不可改。
    ///
    /// # Errors
    /// 参数非法、规则不是对象或存储失败。
    pub fn publish_entitlement_rule(
        &self,
        issuer: &str,
        kind: EntitlementKind,
        rule: &Value,
    ) -> Result<EntitlementRule> {
        store::validate_id("issuer", issuer)?;
        if !rule.is_object() {
            return Err(CommerceError::BadInput(
                "entitlement rule must be a JSON object".into(),
            ));
        }
        let rule_json = rule.to_string();
        if rule_json.len() > MAX_RULE_JSON {
            return Err(CommerceError::BadInput("rule is too large".into()));
        }
        let now = self.now();
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.query_row(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM commerce_entitlement_rules
             WHERE issuer_id = ?1 AND kind = ?2",
            params![issuer, kind.as_str()],
            |r| r.get(0),
        )?;
        let digest = store::digest_hex(&[issuer, kind.as_str(), &version.to_string(), &rule_json]);
        tx.execute(
            "INSERT INTO commerce_entitlement_rules
                (issuer_id, kind, version, rule_json, digest, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![issuer, kind.as_str(), version, rule_json, digest, now],
        )?;
        tx.commit()?;
        Ok(EntitlementRule {
            issuer_id: issuer.to_string(),
            kind,
            version,
            rule: rule.clone(),
            digest,
            created_at: now,
        })
    }

    /// 发放权益；`owner` 为空表示未认领。
    ///
    /// # Errors
    /// 没有已发布规则、参数非法、幂等键冲突或存储失败。
    pub fn issue_entitlement(
        &self,
        issuer: &str,
        owner: Option<&str>,
        kind: EntitlementKind,
        payload: &Value,
        business_key: &str,
    ) -> Result<Entitlement> {
        store::validate_id("issuer", issuer)?;
        if let Some(owner) = owner {
            store::validate_id("owner", owner)?;
        }
        store::validate_key(business_key)?;
        if !payload.is_object() {
            return Err(CommerceError::BadInput(
                "entitlement payload must be a JSON object".into(),
            ));
        }
        let payload_json = payload.to_string();
        if payload_json.len() > MAX_PAYLOAD_JSON {
            return Err(CommerceError::BadInput("payload is too large".into()));
        }
        let owner_key = owner.unwrap_or("");
        let digest = store::digest_hex(&[issuer, owner_key, kind.as_str(), &payload_json]);
        let now = self.now();
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = store::replay(load_by_key(&tx, business_key)?, &digest)? {
            return Ok(existing);
        }
        let (rule_version, rule) = latest_rule(&tx, issuer, kind)?;
        let valid_to = now + validity_days(&rule)? * 24 * 60 * 60 * 1000;
        let id = store::new_id(self.entropy());
        insert_entitlement(
            &tx,
            self,
            &id,
            business_key,
            issuer,
            owner,
            kind,
            rule_version,
            valid_to,
            &payload_json,
            now,
        )?;
        let entitlement = load(&tx, &id)?.ok_or(CommerceError::NotFound)?;
        tx.commit()?;
        Ok(entitlement)
    }

    /// 认领一张未分配属主的权益。
    ///
    /// # Errors
    /// 不存在、已被他人认领或过期。
    pub fn claim_entitlement(&self, owner: &str, entitlement_id: &str) -> Result<Entitlement> {
        store::validate_id("owner", owner)?;
        let now = self.now();
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let entitlement = load(&tx, entitlement_id)?.ok_or(CommerceError::NotFound)?;
        if let Some(existing) = entitlement.owner_id.as_deref() {
            if existing == owner {
                return Ok(entitlement);
            }
            return Err(CommerceError::Forbidden);
        }
        if now > entitlement.valid_to {
            return Err(CommerceError::Expired);
        }
        tx.execute(
            "UPDATE commerce_entitlements SET owner_id = ?1
             WHERE id = ?2 AND owner_id IS NULL",
            params![owner, entitlement_id],
        )?;
        let updated = load(&tx, entitlement_id)?.ok_or(CommerceError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }

    /// 读取权益（属主或发行方可见）。
    ///
    /// # Errors
    /// 不存在或无权查看。
    pub fn entitlement(&self, viewer: &str, entitlement_id: &str) -> Result<Entitlement> {
        store::validate_id("viewer", viewer)?;
        let conn = self.lock();
        let entitlement = load(&conn, entitlement_id)?.ok_or(CommerceError::NotFound)?;
        let owner = entitlement.owner_id.as_deref();
        if owner != Some(viewer) && entitlement.issuer_id != viewer {
            return Err(CommerceError::Forbidden);
        }
        Ok(entitlement)
    }

    /// 申请一次性核销令牌（短期有效，库存只保存哈希）。
    ///
    /// # Errors
    /// 无权申请、权益已核销/过期或存储失败。
    pub fn request_redemption_token(
        &self,
        owner: &str,
        entitlement_id: &str,
    ) -> Result<RedemptionToken> {
        store::validate_id("owner", owner)?;
        let now = self.now();
        let token = store::random_hex(self.entropy(), 24);
        let token_hash = store::digest_hex(&["redemption-token", &token]);
        let expires_at = now + self.config.redemption_token_ttl_ms;
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let entitlement = load(&tx, entitlement_id)?.ok_or(CommerceError::NotFound)?;
        if entitlement.owner_id.as_deref() != Some(owner) {
            return Err(CommerceError::Forbidden);
        }
        if entitlement.status != EntitlementStatus::Active {
            return Err(CommerceError::Conflict(
                "entitlement already redeemed".into(),
            ));
        }
        if now < entitlement.valid_from || now > entitlement.valid_to {
            return Err(CommerceError::Expired);
        }
        let business_key = format!("token:{token_hash}");
        tx.execute(
            "INSERT INTO commerce_redemption_tokens
                (token_hash, entitlement_id, business_key, expires_at, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![token_hash, entitlement_id, business_key, expires_at, now],
        )?;
        tx.commit()?;
        Ok(RedemptionToken { token, expires_at })
    }

    /// 在线核销。同一令牌只能成功一次；同一业务键重试返回同一结果。
    ///
    /// # Errors
    /// 令牌无效/已用/过期、权益状态或所有权不符、幂等键冲突。
    pub fn redeem_entitlement(
        &self,
        actor: &str,
        token: &str,
        business_key: &str,
    ) -> Result<Redemption> {
        store::validate_id("actor", actor)?;
        store::validate_key(business_key)?;
        if token.is_empty() || token.len() > MAX_TOKEN_LEN {
            return Err(CommerceError::BadInput("redemption token".into()));
        }
        let token_hash = store::digest_hex(&["redemption-token", token]);
        let now = self.now();
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(done) = replay_redemption(&tx, business_key, &token_hash, actor)? {
            return Ok(done);
        }
        let entitlement_id = load_unused_token(&tx, &token_hash, now)?;
        let entitlement = load(&tx, &entitlement_id)?.ok_or(CommerceError::NotFound)?;
        ensure_redeemable(&entitlement, actor, now)?;
        let id = store::new_id(self.entropy());
        tx.execute(
            "INSERT INTO commerce_redemptions
                (id, business_key, entitlement_id, actor_id, token_hash, redeemed_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, business_key, entitlement_id, actor, token_hash, now],
        )?;
        mark_token_used(&tx, &token_hash, now)?;
        mark_redeemed(&tx, self, &entitlement_id, now, &id)?;
        tx.commit()?;
        Ok(Redemption {
            id,
            entitlement_id,
            actor_id: actor.to_string(),
            redeemed_at: now,
        })
    }

    /// 离线核销未实现：必须联网做服务端原子确认。
    ///
    /// # Errors
    /// 永远返回明确的"需要联网"错误，避免静默双花。
    pub fn redeem_offline(&self, _actor: &str, _token: &str) -> Result<Redemption> {
        Err(CommerceError::BadInput(
            "offline redemption is not supported; connect to the server".into(),
        ))
    }

    /// 票证条码：派生短签名，不含用户 ID 或永久凭据。
    ///
    /// # Errors
    /// 无权查看或权益不存在。
    pub fn ticket_barcode(&self, viewer: &str, entitlement_id: &str) -> Result<Barcode> {
        let entitlement = self.entitlement(viewer, entitlement_id)?;
        if entitlement.kind != EntitlementKind::Ticket {
            return Err(CommerceError::BadInput(
                "barcode is only available for tickets".into(),
            ));
        }
        let signature = store::hmac_hex(
            self.config.confirmation_secret.as_slice(),
            &["barcode", &entitlement.id],
        );
        Ok(Barcode {
            format: "wxlike.ticket.v1".to_string(),
            value: format!("wx1.{}.{}", entitlement.id, &signature[..16]),
        })
    }
}

/// 同业务键重试：同令牌同人返回第一次结果，否则视为冲突。
fn replay_redemption(
    conn: &Connection,
    business_key: &str,
    token_hash: &str,
    actor: &str,
) -> Result<Option<Redemption>> {
    let existing: Option<(String, String, String, i64)> = conn
        .query_row(
            "SELECT id, entitlement_id, actor_id, redeemed_at FROM commerce_redemptions
             WHERE business_key = ?1",
            params![business_key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((id, entitlement_id, actor_id, redeemed_at)) = existing else {
        return Ok(None);
    };
    let stored_hash: String = conn.query_row(
        "SELECT token_hash FROM commerce_redemptions WHERE id = ?1",
        params![id],
        |r| r.get(0),
    )?;
    if stored_hash != token_hash || actor_id != actor {
        return Err(CommerceError::IdempotencyConflict);
    }
    Ok(Some(Redemption {
        id,
        entitlement_id,
        actor_id,
        redeemed_at,
    }))
}

/// 令牌必须存在、未使用且未过期；返回对应权益 ID。
fn load_unused_token(conn: &Connection, token_hash: &str, now: i64) -> Result<String> {
    let record: Option<(String, i64, Option<i64>)> = conn
        .query_row(
            "SELECT entitlement_id, expires_at, used_at FROM commerce_redemption_tokens
             WHERE token_hash = ?1",
            params![token_hash],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((entitlement_id, expires_at, used_at)) = record else {
        return Err(CommerceError::NotFound);
    };
    if used_at.is_some() {
        return Err(CommerceError::Conflict("token already used".into()));
    }
    if now >= expires_at {
        return Err(CommerceError::Expired);
    }
    Ok(entitlement_id)
}

/// 核销资格：属主本人、状态有效、在有效期内。
fn ensure_redeemable(entitlement: &Entitlement, actor: &str, now: i64) -> Result<()> {
    if entitlement.owner_id.as_deref() != Some(actor) {
        return Err(CommerceError::Forbidden);
    }
    if entitlement.status != EntitlementStatus::Active {
        return Err(CommerceError::Conflict(
            "entitlement already redeemed".into(),
        ));
    }
    if now < entitlement.valid_from || now > entitlement.valid_to {
        return Err(CommerceError::Expired);
    }
    Ok(())
}

/// 一次性标记令牌已用；并发下抢不到就拒绝。
fn mark_token_used(conn: &Connection, token_hash: &str, now: i64) -> Result<()> {
    let changed = conn.execute(
        "UPDATE commerce_redemption_tokens SET used_at = ?1
         WHERE token_hash = ?2 AND used_at IS NULL",
        params![now, token_hash],
    )?;
    if changed != 1 {
        return Err(CommerceError::Conflict("token already used".into()));
    }
    Ok(())
}

/// 权益转 redeemed 并发核销事件。
fn mark_redeemed(
    conn: &Connection,
    commerce: &Commerce,
    entitlement_id: &str,
    now: i64,
    redemption_id: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE commerce_entitlements
         SET status = 'redeemed', redeemed_at = ?1
         WHERE id = ?2 AND status = 'active'",
        params![now, entitlement_id],
    )?;
    store::emit(
        conn,
        commerce.entropy(),
        "entitlement.redeemed",
        entitlement_id,
        &serde_json::json!({
            "entitlement_id": entitlement_id,
            "redemption_id": redemption_id,
        }),
        now,
    )?;
    Ok(())
}

/// 落权益实例并发发行事件（rule_issuer/rule_kind 与发行方同源）。
#[allow(clippy::too_many_arguments)]
fn insert_entitlement(
    conn: &Connection,
    commerce: &Commerce,
    id: &str,
    business_key: &str,
    issuer: &str,
    owner: Option<&str>,
    kind: EntitlementKind,
    rule_version: i64,
    valid_to: i64,
    payload_json: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO commerce_entitlements
            (id, business_key, issuer_id, owner_id, kind, rule_issuer, rule_kind,
             rule_version, status, valid_from, valid_to, payload_json, issued_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?3, ?5, ?6, 'active', ?7, ?8, ?9, ?7)",
        params![
            id,
            business_key,
            issuer,
            owner,
            kind.as_str(),
            rule_version,
            now,
            valid_to,
            payload_json
        ],
    )?;
    store::emit(
        conn,
        commerce.entropy(),
        "entitlement.issued",
        id,
        &serde_json::json!({
            "entitlement_id": id,
            "kind": kind.as_str(),
            "issuer_id": issuer,
        }),
        now,
    )?;
    Ok(())
}

/// 取该 (issuer, kind) 的最新已发布规则；没有规则就拒绝发放。
fn latest_rule(conn: &Connection, issuer: &str, kind: EntitlementKind) -> Result<(i64, Value)> {
    let (version, rule_json): (i64, String) = conn
        .query_row(
            "SELECT version, rule_json FROM commerce_entitlement_rules
             WHERE issuer_id = ?1 AND kind = ?2 ORDER BY version DESC LIMIT 1",
            params![issuer, kind.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| {
            CommerceError::BadInput("no published rule for this entitlement kind".into())
        })?;
    let rule: Value = serde_json::from_str(&rule_json)
        .map_err(|_| CommerceError::BadInput("stored rule is not JSON".into()))?;
    Ok((version, rule))
}

/// 规则里的 `valid_days`（1..=3650，缺省一年）。
fn validity_days(rule: &Value) -> Result<i64> {
    Ok(rule
        .get("valid_days")
        .and_then(Value::as_i64)
        .filter(|days| (1..=MAX_VALID_DAYS).contains(days))
        .unwrap_or(DEFAULT_VALID_DAYS))
}

fn load(conn: &Connection, id: &str) -> Result<Option<Entitlement>> {
    let raw = conn
        .query_row(
            "SELECT id, issuer_id, owner_id, kind, rule_version, status, valid_from, valid_to,
                    payload_json, issued_at, redeemed_at
             FROM commerce_entitlements WHERE id = ?1",
            params![id],
            scan,
        )
        .optional()?;
    raw.map(build).transpose()
}

fn load_by_key(conn: &Connection, key: &str) -> Result<Option<(String, Entitlement)>> {
    let raw = conn
        .query_row(
            "SELECT id, issuer_id, owner_id, kind, rule_version, status, valid_from, valid_to,
                    payload_json, issued_at, redeemed_at
             FROM commerce_entitlements WHERE business_key = ?1",
            params![key],
            scan,
        )
        .optional()?;
    raw.map(|raw| {
        let digest = store::digest_hex(&[
            &raw.issuer_id,
            raw.owner_id.as_deref().unwrap_or(""),
            &raw.kind,
            &raw.payload_json,
        ]);
        Ok((digest, build(raw)?))
    })
    .transpose()
}

struct RawEntitlement {
    id: String,
    issuer_id: String,
    owner_id: Option<String>,
    kind: String,
    rule_version: i64,
    status: String,
    valid_from: i64,
    valid_to: i64,
    payload_json: String,
    issued_at: i64,
    redeemed_at: Option<i64>,
}

fn scan(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawEntitlement> {
    Ok(RawEntitlement {
        id: r.get(0)?,
        issuer_id: r.get(1)?,
        owner_id: r.get(2)?,
        kind: r.get(3)?,
        rule_version: r.get(4)?,
        status: r.get(5)?,
        valid_from: r.get(6)?,
        valid_to: r.get(7)?,
        payload_json: r.get(8)?,
        issued_at: r.get(9)?,
        redeemed_at: r.get(10)?,
    })
}

fn build(raw: RawEntitlement) -> Result<Entitlement> {
    Ok(Entitlement {
        id: raw.id,
        issuer_id: raw.issuer_id,
        owner_id: raw.owner_id,
        kind: EntitlementKind::parse(&raw.kind)?,
        rule_version: raw.rule_version,
        status: EntitlementStatus::parse(&raw.status)?,
        valid_from: raw.valid_from,
        valid_to: raw.valid_to,
        payload: serde_json::from_str(&raw.payload_json)
            .map_err(|_| CommerceError::BadInput("stored entitlement payload".into()))?,
        issued_at: raw.issued_at,
        redeemed_at: raw.redeemed_at,
    })
}
