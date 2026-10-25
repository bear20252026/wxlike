//! 红包：金额由服务端预分配，领取同事务校验群成员与唯一份额。

use rusqlite::{named_params, params, Connection, OptionalExtension, TransactionBehavior};
use wxlike_ledger::{Currency, Money, PostingLine, PostingRequest};

use crate::error::{CommerceError, Result};
use crate::store;
use crate::Commerce;

/// 红包状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedPacketStatus {
    Active,
    Depleted,
    Expired,
}

impl RedPacketStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Depleted => "depleted",
            Self::Expired => "expired",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "active" => Ok(Self::Active),
            "depleted" => Ok(Self::Depleted),
            "expired" => Ok(Self::Expired),
            _ => Err(CommerceError::BadInput("stored red packet status".into())),
        }
    }
}

/// 红包。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedPacket {
    pub id: String,
    pub sender_id: String,
    pub conversation_id: String,
    pub total: Money,
    pub count: u32,
    pub status: RedPacketStatus,
    pub memo: String,
    pub created_at: i64,
    pub expires_at: i64,
}

/// 一次领取。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedPacketClaim {
    pub packet_id: String,
    pub user_id: String,
    pub amount: Money,
    pub claimed_at: i64,
}

/// 红包详情（含已领取记录）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedPacketView {
    pub packet: RedPacket,
    pub claims: Vec<RedPacketClaim>,
}

impl Commerce {
    /// 发红包：先扣款进托管账户，再预分配份额。
    ///
    /// # Errors
    /// 参数非法、非群成员、余额不足、幂等键冲突或存储失败。
    pub fn create_red_packet(
        &self,
        sender: &str,
        conversation: &str,
        total: Money,
        count: u32,
        memo: &str,
        business_key: &str,
    ) -> Result<RedPacket> {
        store::validate_id("sender", sender)?;
        store::validate_id("conversation", conversation)?;
        store::validate_key(business_key)?;
        store::validate_note(memo)?;
        store::check_limit(total, self.config.max_transfer_minor)?;
        if count == 0 || count > 100 {
            return Err(CommerceError::BadInput(
                "red packet count must be between 1 and 100".into(),
            ));
        }
        if total.minor() < i64::from(count) {
            return Err(CommerceError::BadInput(
                "total must cover at least one minor unit per share".into(),
            ));
        }
        if !self.is_member(conversation, sender)? {
            return Err(CommerceError::Forbidden);
        }
        let digest = store::digest_hex(&[
            sender,
            conversation,
            &total.minor().to_string(),
            total.currency().as_str(),
            &count.to_string(),
            memo,
        ]);
        let scoped_key = format!("redpacket:{business_key}");
        let now = self.now();
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = match store::replay(load_by_key(&tx, &scoped_key)?, &digest)? {
            Some(existing) => existing.id,
            None => self.fund_escrow(
                &tx,
                sender,
                conversation,
                total,
                count,
                memo,
                &scoped_key,
                now,
            )?,
        };
        let packet = load(&tx, &id)?.ok_or(CommerceError::NotFound)?;
        tx.commit()?;
        Ok(packet)
    }

    /// 建托管、预拆份额、落红包单并记账（钱包 → escrow）。
    #[allow(clippy::too_many_arguments)]
    fn fund_escrow(
        &self,
        tx: &rusqlite::Transaction<'_>,
        sender: &str,
        conversation: &str,
        total: Money,
        count: u32,
        memo: &str,
        scoped_key: &str,
        now: i64,
    ) -> Result<String> {
        let id = store::new_id(self.entropy());
        let wallet = self.ensure_wallet_conn(tx, sender, total.currency(), now)?;
        let escrow = store::ensure_escrow(
            tx,
            "redpacket",
            &id,
            total.currency(),
            self.config.fund_class,
            now,
        )?;
        let funding = PostingRequest::new(
            format!("redpacket:{id}:fund"),
            "redpacket.funded",
            total.currency(),
            vec![
                PostingLine::debit(wallet, total.minor()),
                PostingLine::credit(escrow, total.minor()),
            ],
        )
        .with_memo("red packet funded");
        wxlike_ledger::post_in(tx, &funding, now).map_err(store::map_ledger)?;
        let shares = split_shares(total.minor(), count, self.entropy());
        let expires_at = now + self.config.redpacket_ttl_ms;
        insert_packet_and_shares(
            tx,
            &id,
            scoped_key,
            sender,
            conversation,
            total,
            count,
            memo,
            shares,
            expires_at,
            now,
        )?;
        store::emit(
            tx,
            self.entropy(),
            "redpacket.created",
            &id,
            &serde_json::json!({
                "packet_id": id,
                "conversation_id": conversation,
                "count": count,
            }),
            now,
        )?;
        Ok(id)
    }

    /// 领取一个份额；同一用户重试返回第一次的结果。
    ///
    /// # Errors
    /// 非群成员、红包为空/过期或存储失败。
    pub fn claim_red_packet(&self, user: &str, packet_id: &str) -> Result<RedPacketClaim> {
        store::validate_id("user", user)?;
        let packet = {
            let conn = self.lock();
            load(&conn, packet_id)?.ok_or(CommerceError::NotFound)?
        };
        if !self.is_member(&packet.conversation_id, user)? {
            return Err(CommerceError::Forbidden);
        }
        let now = self.now();
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = load_claim(&tx, packet_id, user)? {
            return Ok(existing);
        }
        let packet = load(&tx, packet_id)?.ok_or(CommerceError::NotFound)?;
        if packet.status == RedPacketStatus::Expired {
            return Err(CommerceError::Expired);
        }
        if packet.status == RedPacketStatus::Depleted {
            return Err(CommerceError::Conflict("red packet is empty".into()));
        }
        if now >= packet.expires_at {
            expire_locked(&tx, &packet, now)?;
            tx.commit()?;
            return Err(CommerceError::Expired);
        }
        let Some((_slot, amount)) = grab_share(&tx, packet_id, user, now)? else {
            // 空红包的 depleted 标记也要落库，再以业务冲突告终。
            tx.commit()?;
            return Err(CommerceError::Conflict("red packet is empty".into()));
        };
        tx.execute(
            "INSERT INTO commerce_redpacket_claims(packet_id, user_id, amount_minor, claimed_at)
             VALUES(?1, ?2, ?3, ?4)",
            params![packet_id, user, amount, now],
        )?;
        self.post_claim(&tx, packet_id, user, amount, packet.total.currency(), now)?;
        tx.commit()?;
        Ok(RedPacketClaim {
            packet_id: packet_id.to_string(),
            user_id: user.to_string(),
            amount: Money::from_minor(amount, packet.total.currency()),
            claimed_at: now,
        })
    }

    /// 过期未领份额退回发送者。
    ///
    /// # Errors
    /// 不存在、无权操作、尚未到期或状态不允许。
    pub fn expire_red_packet(&self, actor: &str, packet_id: &str) -> Result<RedPacket> {
        store::validate_id("actor", actor)?;
        let now = self.now();
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let packet = load(&tx, packet_id)?.ok_or(CommerceError::NotFound)?;
        if packet.sender_id != actor {
            return Err(CommerceError::Forbidden);
        }
        match packet.status {
            RedPacketStatus::Expired => return Ok(packet),
            RedPacketStatus::Depleted => {
                return Err(CommerceError::IllegalTransition(
                    "red packet is fully claimed".into(),
                ))
            }
            RedPacketStatus::Active => {}
        }
        if now < packet.expires_at {
            return Err(CommerceError::Conflict("red packet is not expired".into()));
        }
        expire_locked(&tx, &packet, now)?;
        let updated = load(&tx, packet_id)?.ok_or(CommerceError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }

    /// 查看红包：发送者或已领取者可见。
    ///
    /// # Errors
    /// 不存在或无权查看。
    pub fn red_packet(&self, viewer: &str, packet_id: &str) -> Result<RedPacketView> {
        store::validate_id("viewer", viewer)?;
        let conn = self.lock();
        let packet = load(&conn, packet_id)?.ok_or(CommerceError::NotFound)?;
        let mut claims = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT user_id, amount_minor, claimed_at FROM commerce_redpacket_claims
             WHERE packet_id = ?1 ORDER BY claimed_at ASC, user_id ASC",
        )?;
        let rows = stmt.query_map(params![packet_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (user, amount, claimed_at) = row?;
            claims.push(RedPacketClaim {
                packet_id: packet_id.to_string(),
                user_id: user,
                amount: Money::from_minor(amount, packet.total.currency()),
                claimed_at,
            });
        }
        if packet.sender_id != viewer && !claims.iter().any(|claim| claim.user_id == viewer) {
            return Err(CommerceError::Forbidden);
        }
        Ok(RedPacketView { packet, claims })
    }

    fn is_member(&self, conversation: &str, user: &str) -> Result<bool> {
        self.ports
            .membership
            .is_member(conversation, user)
            .map_err(|_| CommerceError::MembershipUnavailable)
    }

    /// 领取入账：escrow → 钱包，随后把最后一次领取标成 depleted 并发事件。
    fn post_claim(
        &self,
        tx: &rusqlite::Transaction<'_>,
        packet_id: &str,
        user: &str,
        amount: i64,
        currency: Currency,
        now: i64,
    ) -> Result<()> {
        let escrow = store::escrow_account("redpacket", packet_id, currency);
        let wallet = self.ensure_wallet_conn(tx, user, currency, now)?;
        let posting = PostingRequest::new(
            format!("redpacket:{packet_id}:claim:{user}"),
            "redpacket.claimed",
            currency,
            vec![
                PostingLine::debit(escrow, amount),
                PostingLine::credit(wallet, amount),
            ],
        )
        .with_memo("red packet claim");
        wxlike_ledger::post_in(tx, &posting, now)?;
        let remaining: i64 = tx.query_row(
            "SELECT COUNT(*) FROM commerce_redpacket_shares
             WHERE packet_id = ?1 AND claimed_by IS NULL",
            params![packet_id],
            |r| r.get(0),
        )?;
        if remaining == 0 {
            tx.execute(
                "UPDATE commerce_redpackets SET status = 'depleted'
                 WHERE id = ?1 AND status = 'active'",
                params![packet_id],
            )?;
        }
        store::emit(
            tx,
            self.entropy(),
            "redpacket.claimed",
            packet_id,
            &serde_json::json!({
                "packet_id": packet_id,
                "user_id": user,
                "amount_minor": amount.to_string(),
            }),
            now,
        )?;
        Ok(())
    }
}

/// 落红包单并预写全部份额（份额金额已由服务端拆好）。
#[allow(clippy::too_many_arguments)]
fn insert_packet_and_shares(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
    scoped_key: &str,
    sender: &str,
    conversation: &str,
    total: Money,
    count: u32,
    memo: &str,
    shares: Vec<i64>,
    expires_at: i64,
    now: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO commerce_redpackets
            (id, business_key, sender_id, conv_id, total_minor, count, currency, status,
             memo, created_at, expires_at)
         VALUES(:id, :business_key, :sender_id, :conv_id, :total_minor, :count,
                :currency, 'active', :memo, :now, :expires_at)",
        named_params! {
            ":id": id,
            ":business_key": scoped_key,
            ":sender_id": sender,
            ":conv_id": conversation,
            ":total_minor": total.minor(),
            ":count": i64::from(count),
            ":currency": total.currency().as_str(),
            ":memo": memo,
            ":now": now,
            ":expires_at": expires_at,
        },
    )?;
    for (slot, amount) in shares.iter().enumerate() {
        tx.execute(
            "INSERT INTO commerce_redpacket_shares(packet_id, slot, amount_minor)
             VALUES(?1, ?2, ?3)",
            params![id, slot as i64, amount],
        )?;
    }
    Ok(())
}

/// 原子抢占一个空份额：并发撞车时以冲突回滚，份额被抢光时标 depleted。
fn grab_share(
    tx: &Connection,
    packet_id: &str,
    user: &str,
    now: i64,
) -> Result<Option<(i64, i64)>> {
    let slot: Option<(i64, i64)> = tx
        .query_row(
            "SELECT slot, amount_minor FROM commerce_redpacket_shares
             WHERE packet_id = ?1 AND claimed_by IS NULL
             ORDER BY slot ASC LIMIT 1",
            params![packet_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((slot, amount)) = slot else {
        tx.execute(
            "UPDATE commerce_redpackets SET status = 'depleted'
             WHERE id = ?1 AND status = 'active'",
            params![packet_id],
        )?;
        return Ok(None);
    };
    let changed = tx.execute(
        "UPDATE commerce_redpacket_shares
         SET claimed_by = ?1, claimed_at = ?2
         WHERE packet_id = ?3 AND slot = ?4 AND claimed_by IS NULL",
        params![user, now, packet_id, slot],
    )?;
    if changed != 1 {
        return Err(CommerceError::Conflict(
            "red packet share was taken concurrently".into(),
        ));
    }
    Ok(Some((slot, amount)))
}

fn split_shares(total: i64, count: u32, entropy: &dyn crate::Entropy) -> Vec<i64> {
    let mut shares = Vec::with_capacity(count as usize);
    let mut remaining = total;
    let mut slots_left = i64::from(count);
    while slots_left > 1 {
        let max_now = remaining - (slots_left - 1);
        // 上限取"剩余均值两倍"，避免第一个份额吃掉大部分红包。
        let average = remaining / slots_left;
        let cap = max_now.min((average * 2).max(1)).max(1);
        let mut bytes = [0u8; 8];
        entropy.fill(&mut bytes);
        let draw = u64::from_be_bytes(bytes) % cap as u64;
        let amount = 1 + draw as i64;
        shares.push(amount);
        remaining -= amount;
        slots_left -= 1;
    }
    shares.push(remaining);
    shares
}

fn expire_locked(conn: &Connection, packet: &RedPacket, now: i64) -> Result<()> {
    let unclaimed: i64 = conn.query_row(
        "SELECT COALESCE(SUM(amount_minor), 0) FROM commerce_redpacket_shares
         WHERE packet_id = ?1 AND claimed_by IS NULL",
        params![packet.id],
        |r| r.get(0),
    )?;
    if unclaimed > 0 {
        let escrow = store::escrow_account("redpacket", &packet.id, packet.total.currency());
        let wallet = store::wallet_account(&packet.sender_id, packet.total.currency());
        let posting = PostingRequest::new(
            format!("redpacket:{}:refund", packet.id),
            "redpacket.expired",
            packet.total.currency(),
            vec![
                PostingLine::debit(escrow, unclaimed),
                PostingLine::credit(wallet, unclaimed),
            ],
        )
        .with_memo("red packet expired");
        wxlike_ledger::post_in(conn, &posting, now).map_err(store::map_ledger)?;
    }
    conn.execute(
        "UPDATE commerce_redpackets SET status = 'expired'
         WHERE id = ?1 AND status = 'active'",
        params![packet.id],
    )?;
    Ok(())
}

fn load(conn: &Connection, id: &str) -> Result<Option<RedPacket>> {
    let raw = conn
        .query_row(
            "SELECT id, sender_id, conv_id, total_minor, count, currency, status, memo,
                    created_at, expires_at
             FROM commerce_redpackets WHERE id = ?1",
            params![id],
            scan,
        )
        .optional()?;
    raw.map(build).transpose()
}

fn load_by_key(conn: &Connection, key: &str) -> Result<Option<(String, RedPacket)>> {
    let raw = conn
        .query_row(
            "SELECT id, sender_id, conv_id, total_minor, count, currency, status, memo,
                    created_at, expires_at
             FROM commerce_redpackets WHERE business_key = ?1",
            params![key],
            scan,
        )
        .optional()?;
    raw.map(|raw| {
        let digest = store::digest_hex(&[
            &raw.sender_id,
            &raw.conversation_id,
            &raw.total_minor.to_string(),
            &raw.currency,
            &raw.count.to_string(),
            &raw.memo,
        ]);
        Ok((digest, build(raw)?))
    })
    .transpose()
}

struct RawPacket {
    id: String,
    sender_id: String,
    conversation_id: String,
    total_minor: i64,
    count: i64,
    currency: String,
    status: String,
    memo: String,
    created_at: i64,
    expires_at: i64,
}

fn scan(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawPacket> {
    Ok(RawPacket {
        id: r.get(0)?,
        sender_id: r.get(1)?,
        conversation_id: r.get(2)?,
        total_minor: r.get(3)?,
        count: r.get(4)?,
        currency: r.get(5)?,
        status: r.get(6)?,
        memo: r.get(7)?,
        created_at: r.get(8)?,
        expires_at: r.get(9)?,
    })
}

fn build(raw: RawPacket) -> Result<RedPacket> {
    Ok(RedPacket {
        id: raw.id,
        sender_id: raw.sender_id,
        conversation_id: raw.conversation_id,
        total: Money::from_minor(raw.total_minor, Currency::parse(&raw.currency)?),
        count: raw.count as u32,
        status: RedPacketStatus::parse(&raw.status)?,
        memo: raw.memo,
        created_at: raw.created_at,
        expires_at: raw.expires_at,
    })
}

fn load_claim(conn: &Connection, packet_id: &str, user: &str) -> Result<Option<RedPacketClaim>> {
    let raw = conn
        .query_row(
            "SELECT amount_minor, claimed_at FROM commerce_redpacket_claims
             WHERE packet_id = ?1 AND user_id = ?2",
            params![packet_id, user],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?;
    let Some((amount, claimed_at)) = raw else {
        return Ok(None);
    };
    let packet = load(conn, packet_id)?.ok_or(CommerceError::NotFound)?;
    Ok(Some(RedPacketClaim {
        packet_id: packet_id.to_string(),
        user_id: user.to_string(),
        amount: Money::from_minor(amount, packet.total.currency()),
        claimed_at,
    }))
}
