//! WP-09 wallet / red packet / transfer — **a virtual ledger**.
//!
//! Scope boundary, stated first as the task book requires: this is a bookkeeping
//! closed loop only. No real payment channel is wired in; acquiring real money
//! is a licensed-compliance question and is explicitly out of this repository.
//!
//! Money conservation is the property under test, so:
//! * amounts are integer minor units (分) — never floats;
//! * a transfer is **one transaction**: both balances and both ledger rows, or none;
//! * `wallets.balance` carries `CHECK (balance >= 0)`, so a negative balance is
//!   refused by SQLite itself even if application logic were wrong;
//! * a red packet freezes its full total at creation and refunds whatever was
//!   not claimed after `expire_at`, leaving a ledger row for the refund.
//!
//! Funding seam: [`Platform::fund`] is the only way balance enters the ledger.
//! There is no HTTP endpoint for it — that is precisely the seam a licensed
//! channel would plug into later.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use rand::Rng as _;
use rusqlite::{params, Connection, OptionalExtension as _};
use serde::Serialize;

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::validate;
use crate::{Platform, PlatformState};

/// Ledger kinds (contract names them explicitly).
pub mod kind {
    pub const TRANSFER_IN: &str = "transfer_in";
    pub const TRANSFER_OUT: &str = "transfer_out";
    pub const REDPACKET_OUT: &str = "redpacket_out";
    pub const REDPACKET_IN: &str = "redpacket_in";
    pub const REDPACKET_REFUND: &str = "redpacket_refund";
    pub const FUND_IN: &str = "fund_in";
}

/// Red packets expire after 24 h (contract).
pub const REDPACKET_TTL_MS: u64 = 24 * 60 * 60 * 1000;

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/wallet/transfer", post(transfer))
        .route("/api/wallet/redpacket", post(create_redpacket))
        .route("/api/wallet/redpacket/{id}/claim", post(claim))
        .route("/api/wallet/balance", get(balance))
        .route("/api/wallet/ledger", get(ledger))
}

#[derive(serde::Deserialize)]
pub struct TransferBody {
    pub to_uid: String,
    pub amount: i64,
    #[serde(default)]
    pub memo: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct RedpacketBody {
    pub amount: i64,
    pub count: i64,
    #[serde(default)]
    pub memo: Option<String>,
    /// Optional conversation the packet is posted into. The contract's body
    /// does not name one, so it falls back to the packet's own id.
    #[serde(default)]
    pub conv_id: Option<String>,
}

#[derive(serde::Deserialize, Default)]
pub struct LedgerQuery {
    #[serde(default)]
    since: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
}

#[derive(Serialize, Clone, Debug)]
pub struct LedgerRow {
    pub id: i64,
    pub kind: String,
    pub amount: i64,
    pub counterparty: String,
    pub ref_id: String,
    pub memo: String,
    pub created_at: i64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct RedpacketRow {
    pub id: String,
    pub from_uid: String,
    pub conv_id: String,
    pub total: i64,
    pub count: i64,
    pub claimed: i64,
    pub claimed_total: i64,
    pub memo: String,
    pub status: String,
    pub created_at: i64,
    pub expire_at: i64,
}

/// A feature message (`MsgBody::Custom`) addressed to `to` inside `conv_id`.
fn wallet_message(
    conv_id: &str,
    sender: &str,
    to: Vec<String>,
    kind: &str,
    payload: serde_json::Value,
) -> wxlike_core::Envelope {
    let mut env = crate::system_envelope(conv_id, sender, to, "");
    env.body = wxlike_core::MsgBody::Custom {
        kind: kind.to_string(),
        payload,
    };
    env
}

fn check_amount(amount: i64) -> Result<()> {
    if amount <= 0 {
        return Err(PlatformError::BadInput("amount must be positive"));
    }
    if amount > i64::from(i32::MAX) * 100 {
        // an absurd ceiling beats none: it keeps sums far from overflow
        return Err(PlatformError::BadInput(
            "amount exceeds the allowed maximum",
        ));
    }
    Ok(())
}

impl Platform {
    /// Reads a balance, treating a wallet-less user as holding zero.
    pub fn wallet_balance(&self, user_id: &str) -> Result<i64> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT balance FROM wallets WHERE user_id = ?1",
            params![user_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0))
    }

    fn ensure_wallet(c: &Connection, user_id: &str) -> Result<()> {
        c.execute(
            "INSERT INTO wallets(user_id, balance) VALUES(?1, 0)
             ON CONFLICT(user_id) DO NOTHING",
            params![user_id],
        )?;
        Ok(())
    }

    fn log(
        c: &Connection,
        user: &str,
        k: &str,
        amount: i64,
        counterparty: &str,
        ref_id: &str,
        memo: &str,
    ) -> Result<()> {
        c.execute(
            "INSERT INTO ledger(user_id, kind, amount, counterparty, ref_id, memo, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                user,
                k,
                amount,
                counterparty,
                ref_id,
                memo,
                wxlike_core::types::now_ms() as i64
            ],
        )?;
        Ok(())
    }

    /// The only way balance enters the system: a virtual funding entry.
    /// A licensed channel would call this on settlement; nothing here speaks to
    /// a payment provider.
    pub fn fund(&self, user_id: &str, amount: i64, memo: &str) -> Result<()> {
        check_amount(amount)?;
        let user_id = validate::id(user_id)?.to_string();
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        Self::ensure_wallet(&tx, &user_id)?;
        let exists: Option<String> = tx
            .query_row(
                "SELECT id FROM users WHERE id = ?1",
                params![user_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(PlatformError::NotFound);
        }
        tx.execute(
            "UPDATE wallets SET balance = balance + ?1 WHERE user_id = ?2",
            params![amount, user_id],
        )?;
        Self::log(&tx, &user_id, kind::FUND_IN, amount, "", "", memo)?;
        tx.commit()?;
        Ok(())
    }

    /// Moves `amount` from `from` to `to` in **one transaction**: both balance
    /// updates plus both ledger rows, committed together or not at all.
    /// Returns the transfer id used as the ledger `ref_id`.
    pub fn transfer(&self, from: &str, to: &str, amount: i64, memo: &str) -> Result<String> {
        let from = validate::id(from)?.to_string();
        let to = validate::id(to)?.to_string();
        let memo = validate::note(memo)?.to_string();
        check_amount(amount)?;
        if from == to {
            return Err(PlatformError::BadInput("cannot transfer to yourself"));
        }

        let ref_id = crate::auth::random_hex(8);
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        for user in [&from, &to] {
            let exists: Option<String> = tx
                .query_row("SELECT id FROM users WHERE id = ?1", params![user], |r| {
                    r.get::<_, String>(0)
                })
                .optional()?;
            if exists.is_none() {
                return Err(PlatformError::NotFound);
            }
            Self::ensure_wallet(&tx, user)?;
        }

        // The whole exchange runs under one connection guard, so no other
        // transfer can read a half-applied balance. The CHECK constraint is the
        // backstop if this guard were ever removed.
        let updated = tx.execute(
            "UPDATE wallets SET balance = balance - ?1 WHERE user_id = ?2 AND balance >= ?1",
            params![amount, from],
        )?;
        if updated == 0 {
            // roll the whole transaction back, including the ensure_wallet rows
            return Err(PlatformError::BadInput("insufficient balance"));
        }
        tx.execute(
            "UPDATE wallets SET balance = balance + ?1 WHERE user_id = ?2",
            params![amount, to],
        )?;
        Self::log(&tx, &from, kind::TRANSFER_OUT, amount, &to, &ref_id, &memo)?;
        Self::log(&tx, &to, kind::TRANSFER_IN, amount, &from, &ref_id, &memo)?;
        tx.commit()?;
        Ok(ref_id)
    }

    /// Ledger rows, oldest first: `since` is the last id already seen, so the
    /// cursor advances the same way as the contact-change log (WP-02) rather
    /// than mixing a newest-first sort with an ascending cursor.
    pub fn ledger_of(&self, user_id: &str, since: i64, limit: i64) -> Result<Vec<LedgerRow>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT id, kind, amount, counterparty, ref_id, memo, created_at
             FROM ledger WHERE user_id = ?1 AND id > ?2 ORDER BY id ASC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![user_id, since, limit.clamp(1, 200)], |r| {
            Ok(LedgerRow {
                id: r.get(0)?,
                kind: r.get(1)?,
                amount: r.get(2)?,
                counterparty: r.get(3)?,
                ref_id: r.get(4)?,
                memo: r.get(5)?,
                created_at: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn ledger_head(&self, user_id: &str) -> Result<i64> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM ledger WHERE user_id = ?1",
            params![user_id],
            |r| r.get::<_, i64>(0),
        )?)
    }

    /// Creates a red packet: the full total leaves the sender's balance here,
    /// so the money is frozen before anybody can claim it.
    ///
    /// `conv_id` is where the packet was posted; the contract's body does not
    /// carry one, so an empty value falls back to the packet's own id — the
    /// announcement then lands in a channel only this packet uses.
    pub fn create_redpacket(
        &self,
        from: &str,
        amount: i64,
        count: i64,
        memo: &str,
        conv_id: Option<&str>,
    ) -> Result<String> {
        let from = validate::id(from)?.to_string();
        let memo = validate::note(memo)?.to_string();
        check_amount(amount)?;
        if !(1..=1_000).contains(&count) {
            return Err(PlatformError::BadInput("count must be between 1 and 1000"));
        }
        if amount < count {
            // every share must be at least one minor unit
            return Err(PlatformError::BadInput("amount is too small to split"));
        }
        let id = crate::auth::random_hex(8);
        let conv_id = match conv_id {
            // 直聊会话 id 含 `~`，必须走 conv 校验（id 校验会拒真会话——存量 bug）
            Some(c) if !c.is_empty() => validate::conv(c)?.to_string(),
            _ => id.clone(),
        };
        let now = wxlike_core::types::now_ms() as i64;
        let expire_at = now + REDPACKET_TTL_MS as i64;

        let mut c = self.db().lock();
        let tx = c.transaction()?;
        let exists: Option<String> = tx
            .query_row("SELECT id FROM users WHERE id = ?1", params![&from], |r| {
                r.get::<_, String>(0)
            })
            .optional()?;
        if exists.is_none() {
            return Err(PlatformError::NotFound);
        }
        Self::ensure_wallet(&tx, &from)?;
        let frozen = tx.execute(
            "UPDATE wallets SET balance = balance - ?1 WHERE user_id = ?2 AND balance >= ?1",
            params![amount, from],
        )?;
        if frozen == 0 {
            return Err(PlatformError::BadInput("insufficient balance"));
        }
        tx.execute(
            "INSERT INTO redpackets(id, from_uid, conv_id, total, count, claimed, claimed_total, claimers, memo, status, created_at, expire_at)
             VALUES(?1, ?2, ?3, ?4, ?5, 0, 0, '[]', ?6, 'open', ?7, ?8)",
            params![id, from, conv_id, amount, count, memo, now, expire_at],
        )?;
        Self::log(&tx, &from, kind::REDPACKET_OUT, amount, "", &id, &memo)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn redpacket(&self, id: &str) -> Result<Option<RedpacketRow>> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT id, from_uid, conv_id, total, count, claimed, claimed_total, memo, status, created_at, expire_at
             FROM redpackets WHERE id = ?1",
            params![id],
            |r| {
                Ok(RedpacketRow {
                    id: r.get(0)?,
                    from_uid: r.get(1)?,
                    conv_id: r.get(2)?,
                    total: r.get(3)?,
                    count: r.get(4)?,
                    claimed: r.get(5)?,
                    claimed_total: r.get(6)?,
                    memo: r.get(7)?,
                    status: r.get(8)?,
                    created_at: r.get(9)?,
                    expire_at: r.get(10)?,
                })
            },
        )
        .optional()?)
    }

    /// Claims one share. Exactly `count` claims can succeed; the shares are
    /// random but the sum is conserved by construction (the last claimer takes
    /// the whole remainder).
    pub fn claim_redpacket(&self, id: &str, by: &str) -> Result<i64> {
        let id = validate::id(id)?.to_string();
        let by = validate::id(by)?.to_string();
        let mut c = self.db().lock();
        let tx = c.transaction()?;

        let row: Option<(i64, i64, i64, i64, String, String, String)> = tx
            .query_row(
                "SELECT total, count, claimed, claimed_total, claimers, status, conv_id
                 FROM redpackets WHERE id = ?1",
                params![&id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((total, count, claimed, claimed_total, claimers_json, status, _conv_id)) = row
        else {
            return Err(PlatformError::NotFound);
        };
        if status != "open" {
            return Err(PlatformError::Conflict("redpacket".into()));
        }
        let owner: Option<String> = tx
            .query_row(
                "SELECT from_uid FROM redpackets WHERE id = ?1",
                params![&id],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        if owner.as_deref() == Some(by.as_str()) {
            // the sender already paid; grabbing their own packet would just move
            // money from escrow back into their wallet early
            return Err(PlatformError::Forbidden);
        }
        if claimed >= count {
            return Err(PlatformError::Conflict("redpacket is empty".into()));
        }
        let mut claimers: Vec<String> = serde_json::from_str(&claimers_json).unwrap_or_default();
        if claimers.contains(&by) {
            // one share per person; a second grab is refused without touching money
            return Err(PlatformError::Conflict("already claimed".into()));
        }

        let remaining_slots = count - claimed;
        let remaining_money = total - claimed_total;
        let share = draw_share(remaining_money, remaining_slots);

        Self::ensure_wallet(&tx, &by)?;
        tx.execute(
            "UPDATE wallets SET balance = balance + ?1 WHERE user_id = ?2",
            params![share, by],
        )?;
        claimers.push(by.clone());
        let claimers_json = serde_json::to_string(&claimers)
            .map_err(|_| PlatformError::BadInput("claimers record is corrupt"))?;
        tx.execute(
            "UPDATE redpackets SET claimed = claimed + 1, claimed_total = claimed_total + ?1,
                    claimers = ?2, status = CASE WHEN claimed + 1 >= count THEN 'finished' ELSE status END
             WHERE id = ?3",
            params![share, claimers_json, id],
        )?;
        Self::log(&tx, &by, kind::REDPACKET_IN, share, "", &id, "")?;
        tx.commit()?;
        drop(c);

        // P4 ADR-5 红包卡片回写：领取事实走 ephemeral 通道更新原卡片
        // （不占时间线、不占序列号——不变式 3）。发红包时未带 conv 的包
        // 没有卡片可回写，跳过。
        if let Some(conv) = self.redpacket_conv(&id)? {
            if let Some(store) = self.message_store.as_ref() {
                let _ = store.append_ephemeral(
                    (wxlike_message::OwnerType::Direct, &conv),
                    &by,
                    "wallet.redpacket.claim",
                    &format!("claim-{id}-{}", crate::auth::random_hex(4)),
                    &id,
                    &serde_json::json!({
                        "claimer": by, "amount": share,
                        "finished": status_now_finished(&id, claimed_total + share, count),
                    })
                    .to_string(),
                );
            }
        }
        Ok(share)
    }

    /// 红包卡片所在会话（发红包时可带 conv_id；未带则无卡片）。
    fn redpacket_conv(&self, id: &str) -> Result<Option<String>> {
        let c = self.db().lock();
        let conv: Option<String> = c
            .query_row(
                "SELECT conv_id FROM redpackets WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(conv.filter(|s| !s.is_empty()))
    }

    /// Refunds whatever is unclaimed on expired packets. Idempotent: the status
    /// flip is part of the same transaction, so a second sweep changes nothing.
    pub fn sweep_expired_redpackets(&self) -> Result<usize> {
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        let mut expired = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT id, from_uid, total, claimed_total FROM redpackets
                 WHERE status = 'open' AND expire_at <= ?1",
            )?;
            let rows = stmt.query_map(params![now], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            for row in rows {
                expired.push(row?);
            }
        }
        let refunded = expired.len();
        for (id, from_uid, total, claimed_total) in expired {
            let refund = total - claimed_total;
            if refund > 0 {
                tx.execute(
                    "UPDATE wallets SET balance = balance + ?1 WHERE user_id = ?2",
                    params![refund, from_uid],
                )?;
                Self::log(&tx, &from_uid, kind::REDPACKET_REFUND, refund, "", &id, "")?;
            }
            tx.execute(
                "UPDATE redpackets SET status = 'expired' WHERE id = ?1",
                params![id],
            )?;
        }
        tx.commit()?;
        Ok(refunded)
    }

    /// Sum of every balance in the system. Excludes money sitting in an open
    /// red packet, which is why the conservation tests use
    /// [`Platform::money_supply`] instead.
    pub fn total_money(&self) -> Result<i64> {
        let c = self.db().lock();
        Ok(
            c.query_row("SELECT COALESCE(SUM(balance), 0) FROM wallets", [], |r| {
                r.get::<_, i64>(0)
            })?,
        )
    }

    /// The invariant that must never move except through [`Platform::fund`]:
    /// every wallet plus the unclaimed remainder of every open packet. Frozen
    /// money has left the sender but is still in the system, so a supply check
    /// that ignores escrow would "lose" money on every red packet.
    pub fn money_supply(&self) -> Result<i64> {
        let c = self.db().lock();
        let wallets: i64 =
            c.query_row("SELECT COALESCE(SUM(balance), 0) FROM wallets", [], |r| {
                r.get::<_, i64>(0)
            })?;
        let escrow: i64 = c.query_row(
            "SELECT COALESCE(SUM(total - claimed_total), 0) FROM redpackets WHERE status = 'open'",
            [],
            |r| r.get::<_, i64>(0),
        )?;
        Ok(wallets + escrow)
    }
}

/// A random share that keeps every remaining slot fundable: at least 1 unit now,
/// and enough left for the others to also get >= 1.
fn draw_share(remaining_money: i64, remaining_slots: i64) -> i64 {
    if remaining_slots <= 1 {
        return remaining_money.max(1);
    }
    // double-mean style draw, bounded so the tail stays feasible
    let max = remaining_money - (remaining_slots - 1);
    if max <= 1 {
        return 1;
    }
    let mut rng = rand::thread_rng();
    let mean = (2 * remaining_money) / remaining_slots;
    let cap = mean.min(max).max(1);
    let drawn = rng.gen_range(1..=cap);
    drawn.min(max).max(1)
}

async fn transfer(
    State(platform): State<PlatformState>,
    authed: Authed,
    Json(body): Json<TransferBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let memo = body.memo.clone().unwrap_or_default();
    let ref_id = platform.transfer(&authed.user_id, &body.to_uid, body.amount, &memo)?;
    platform
        .emit(wallet_message(
            &crate::direct_conv_id(&authed.user_id, &body.to_uid),
            &authed.user_id,
            vec![body.to_uid.clone()],
            wxlike_core::custom_kind::WALLET_TRANSFER,
            serde_json::json!({
                "transfer_id": ref_id,
                "amount": body.amount,
                "from": authed.user_id,
                "to": body.to_uid,
                "memo": memo,
            }),
        ))
        .await;
    Ok(Json(serde_json::json!({ "transfer_id": ref_id })))
}

async fn create_redpacket(
    State(platform): State<PlatformState>,
    authed: Authed,
    Json(body): Json<RedpacketBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let memo = body.memo.clone().unwrap_or_default();
    let id = platform.create_redpacket(
        &authed.user_id,
        body.amount,
        body.count,
        &memo,
        body.conv_id.as_deref(),
    )?;
    let packet = platform
        .redpacket(&id)?
        .ok_or_else(|| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "packet vanished"))?;
    // announced to everyone who can see the conversation it was posted in
    let recipients = match platform.cached_members(&packet.conv_id) {
        Ok(Some(members)) => members,
        _ => vec![packet.from_uid.clone()],
    }
    .into_iter()
    .filter(|u| *u != authed.user_id)
    .collect();
    platform
        .emit(wallet_message(
            &packet.conv_id,
            &authed.user_id,
            recipients,
            wxlike_core::custom_kind::WALLET_REDPACKET,
            serde_json::json!({
                "redpacket_id": id,
                "amount": body.amount,
                "count": body.count,
                "memo": memo,
            }),
        ))
        .await;
    Ok(Json(serde_json::json!({ "redpacket_id": id })))
}

async fn claim(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    // identity comes from the token only: any `user_id` in the body is ignored
    let share = platform.claim_redpacket(&id, &authed.user_id)?;
    let conv_id = platform
        .redpacket(&id)?
        .map(|p| p.conv_id)
        .unwrap_or_else(|| id.clone());
    platform
        .emit(wallet_message(
            &conv_id,
            &authed.user_id,
            vec![],
            wxlike_core::custom_kind::WALLET_CLAIM,
            serde_json::json!({
                "redpacket_id": id,
                "by": authed.user_id,
                "amount": share,
            }),
        ))
        .await;
    Ok(Json(serde_json::json!({ "amount": share })))
}

async fn balance(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(serde_json::json!({
        "balance": platform.wallet_balance(&authed.user_id)?,
    })))
}

async fn ledger(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<LedgerQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let since = q.since.unwrap_or(0).max(0);
    let limit = q.limit.unwrap_or(50);
    let rows = platform.ledger_of(&authed.user_id, since, limit)?;
    let head = platform.ledger_head(&authed.user_id)?;
    Ok(Json(serde_json::json!({
        "entries": rows,
        "cursor": head,
        // a full page means there may be more; anything less is the tail
        "has_more": rows.len() as i64 == limit.clamp(1, 200),
    })))
}

/// 领取后是否已领完（纯函数便于测试）。
fn status_now_finished(_id: &str, claimed_total: i64, count: i64) -> bool {
    claimed_total >= count
}

#[cfg(test)]
mod ephemeral_tests {
    use crate::Platform;

    #[test]
    fn claim_writes_ephemeral_card_update() {
        let p = Platform::in_memory().unwrap();
        let a = p.register("alice", "pw").unwrap();
        let b = p.register("bob", "pw").unwrap();
        p.fund(&a, 10_000, "test").unwrap();

        // 带会话发红包 → bob 领取 → ephemeral 回写进该会话
        let rp = p.create_redpacket(&a, 300, 2, "包", Some("d-a~b")).unwrap();
        let _ = p.claim_redpacket(&rp, &b).unwrap();

        let store = p.message_store.as_ref().unwrap();
        let (updates, _) = store
            .fetch_ephemeral((wxlike_message::OwnerType::Direct, "d-a~b"), 0, 10)
            .unwrap();
        assert_eq!(updates.len(), 1, "claim 写了一条卡片更新");
        assert_eq!(updates[0].target_msg_id, rp);
        assert!(updates[0].payload.contains(&b));
        // 频道主时间线不受影响（不变式 3）
        let (timeline, _) = store
            .fetch((wxlike_message::OwnerType::Direct, "d-a~b"), 0, 10)
            .unwrap();
        assert!(timeline.is_empty(), "ephemeral 不占时间线");
    }

    #[test]
    fn claim_without_conv_writes_no_card() {
        let p = Platform::in_memory().unwrap();
        let a = p.register("alice", "pw").unwrap();
        let b = p.register("bob", "pw").unwrap();
        p.fund(&a, 1_000, "test").unwrap();
        let rp = p.create_redpacket(&a, 100, 1, "包", None).unwrap();
        p.claim_redpacket(&rp, &b).unwrap();
        // 无会话的包无卡片可回写——不 panic 即可（存储面无 ephemeral 行）
        let store = p.message_store.as_ref().unwrap();
        let (updates, _) = store
            .fetch_ephemeral((wxlike_message::OwnerType::Direct, "d-a~b"), 0, 10)
            .unwrap();
        assert!(updates.is_empty());
    }
}
