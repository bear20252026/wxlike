//! 转账：确认令牌绑定收款人/金额/币种/备注/nonce，创建时冻结资金。

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use wxlike_ledger::{Currency, Money, PostingLine, PostingRequest};

use crate::error::{CommerceError, Result};
use crate::store;
use crate::Commerce;

/// 转账状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferStatus {
    Pending,
    Accepted,
    Rejected,
    Expired,
}

impl TransferStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "pending" => Ok(Self::Pending),
            "accepted" => Ok(Self::Accepted),
            "rejected" => Ok(Self::Rejected),
            "expired" => Ok(Self::Expired),
            _ => Err(CommerceError::BadInput("stored transfer status".into())),
        }
    }
}

/// 转账确认单。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferDraft {
    pub sender_id: String,
    pub payee_id: String,
    pub amount: Money,
    pub memo: String,
    pub nonce: String,
    pub expires_at: i64,
    pub confirmation_token: String,
}

/// 已创建的转账。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transfer {
    pub id: String,
    pub sender_id: String,
    pub payee_id: String,
    pub amount: Money,
    pub memo: String,
    pub status: TransferStatus,
    pub expires_at: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Commerce {
    /// 准备转账，返回一次性确认令牌。此时不动账。
    ///
    /// # Errors
    /// 参数非法、金额超限或给自己转账。
    pub fn prepare_transfer(
        &self,
        sender: &str,
        payee: &str,
        amount: Money,
        memo: &str,
    ) -> Result<TransferDraft> {
        store::validate_id("sender", sender)?;
        store::validate_id("payee", payee)?;
        store::validate_note(memo)?;
        store::check_limit(amount, self.config.max_transfer_minor)?;
        if sender == payee {
            return Err(CommerceError::BadInput(
                "cannot transfer to yourself".into(),
            ));
        }
        let now = self.now();
        let expires_at = now + self.config.transfer_ttl_ms;
        let nonce = store::random_hex(self.entropy(), 16);
        let token = self.transfer_token(sender, payee, amount, memo, &nonce, expires_at);
        Ok(TransferDraft {
            sender_id: sender.to_string(),
            payee_id: payee.to_string(),
            amount,
            memo: memo.to_string(),
            nonce,
            expires_at,
            confirmation_token: token,
        })
    }

    /// 用确认令牌创建转账并冻结资金。
    ///
    /// # Errors
    /// 令牌不符/过期、nonce 重放、余额不足、幂等键冲突或存储失败。
    pub fn create_transfer(
        &self,
        sender: &str,
        draft: &TransferDraft,
        confirmation_token: &str,
        business_key: &str,
    ) -> Result<Transfer> {
        store::validate_key(business_key)?;
        if draft.sender_id != sender {
            return Err(CommerceError::Forbidden);
        }
        store::validate_note(&draft.memo)?;
        store::check_limit(draft.amount, self.config.max_transfer_minor)?;
        let now = self.now();
        if now >= draft.expires_at {
            return Err(CommerceError::Expired);
        }
        let expected = self.transfer_token(
            &draft.sender_id,
            &draft.payee_id,
            draft.amount,
            &draft.memo,
            &draft.nonce,
            draft.expires_at,
        );
        if !constant_time_eq(&expected, confirmation_token) {
            return Err(CommerceError::InvalidSignature);
        }
        if !constant_time_eq(&draft.confirmation_token, confirmation_token) {
            return Err(CommerceError::InvalidSignature);
        }
        let nonce_hash = store::digest_hex(&["transfer-nonce", &draft.nonce]);
        let digest = store::digest_hex(&[
            sender,
            &draft.payee_id,
            &draft.amount.minor().to_string(),
            draft.amount.currency().as_str(),
            &draft.memo,
            &nonce_hash,
        ]);
        let scoped_key = format!("transfer:{business_key}");
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = load_by_key(&tx, &scoped_key)? {
            if existing.0 != digest {
                return Err(CommerceError::IdempotencyConflict);
            }
            return Ok(existing.1);
        }
        let reused: Option<String> = tx
            .query_row(
                "SELECT nonce_hash FROM commerce_nonces WHERE nonce_hash = ?1",
                params![nonce_hash],
                |r| r.get(0),
            )
            .optional()?;
        if reused.is_some() {
            return Err(CommerceError::Conflict(
                "confirmation nonce was already used".into(),
            ));
        }
        tx.execute(
            "INSERT INTO commerce_nonces(nonce_hash, purpose, used_at) VALUES(?1, 'transfer', ?2)",
            params![nonce_hash, now],
        )?;
        let id = store::new_id(self.entropy());
        let hold_key = format!("transfer:{id}:hold");
        let wallet = self.ensure_wallet_conn(&tx, sender, draft.amount.currency(), now)?;
        wxlike_ledger::hold_in(&tx, &hold_key, &wallet, draft.amount, now)
            .map_err(store::map_ledger)?;
        let confirmation_hash = store::digest_hex(&["confirmation", confirmation_token]);
        tx.execute(
            "INSERT INTO commerce_transfers
                (id, business_key, sender_id, payee_id, amount_minor, currency, memo, status,
                 confirmation_hash, nonce_hash, hold_key, expires_at, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?9, ?10, ?11, ?12, ?12)",
            params![
                id,
                scoped_key,
                sender,
                draft.payee_id,
                draft.amount.minor(),
                draft.amount.currency().as_str(),
                draft.memo,
                confirmation_hash,
                nonce_hash,
                hold_key,
                draft.expires_at,
                now
            ],
        )?;
        store::emit(
            &tx,
            self.entropy(),
            "transfer.created",
            &id,
            &serde_json::json!({
                "transfer_id": id,
                "amount_minor": draft.amount.minor().to_string(),
                "currency": draft.amount.currency().as_str(),
            }),
            now,
        )?;
        let transfer = load(&tx, &id)?.ok_or(CommerceError::NotFound)?;
        tx.commit()?;
        Ok(transfer)
    }

    /// 收款人确认收款。
    ///
    /// # Errors
    /// 不存在、无权操作、已过期或状态不允许。
    pub fn accept_transfer(&self, payee: &str, transfer_id: &str) -> Result<Transfer> {
        let now = self.now();
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let transfer = load(&tx, transfer_id)?.ok_or(CommerceError::NotFound)?;
        if transfer.payee_id != payee {
            return Err(CommerceError::Forbidden);
        }
        match transfer.status {
            TransferStatus::Accepted => return Ok(transfer),
            TransferStatus::Pending => {}
            TransferStatus::Rejected | TransferStatus::Expired => {
                return Err(CommerceError::IllegalTransition(
                    "transfer is already closed".into(),
                ))
            }
        }
        if now >= transfer.expires_at {
            expire_locked(&tx, &transfer, now)?;
            tx.commit()?;
            return Err(CommerceError::Expired);
        }
        let payee_wallet = self.ensure_wallet_conn(&tx, payee, transfer.amount.currency(), now)?;
        let sender_wallet = store::wallet_account(&transfer.sender_id, transfer.amount.currency());
        let request = PostingRequest::new(
            format!("transfer:{transfer_id}:accept"),
            "transfer.accepted",
            transfer.amount.currency(),
            vec![
                PostingLine::debit(sender_wallet, transfer.amount.minor()),
                PostingLine::credit(payee_wallet, transfer.amount.minor()),
            ],
        )
        .with_memo("transfer accepted");
        wxlike_ledger::capture_hold_in(&tx, &format!("transfer:{transfer_id}:hold"), &request, now)
            .map_err(store::map_ledger)?;
        tx.execute(
            "UPDATE commerce_transfers SET status = 'accepted', updated_at = ?1
             WHERE id = ?2 AND status = 'pending'",
            params![now, transfer_id],
        )?;
        store::emit(
            &tx,
            self.entropy(),
            "transfer.accepted",
            transfer_id,
            &serde_json::json!({ "transfer_id": transfer_id }),
            now,
        )?;
        let updated = load(&tx, transfer_id)?.ok_or(CommerceError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }

    /// 收款人拒收，资金解冻退回。
    ///
    /// # Errors
    /// 不存在、无权操作或状态不允许。
    pub fn reject_transfer(&self, payee: &str, transfer_id: &str) -> Result<Transfer> {
        let now = self.now();
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let transfer = load(&tx, transfer_id)?.ok_or(CommerceError::NotFound)?;
        if transfer.payee_id != payee {
            return Err(CommerceError::Forbidden);
        }
        match transfer.status {
            TransferStatus::Rejected => return Ok(transfer),
            TransferStatus::Pending => {}
            TransferStatus::Accepted | TransferStatus::Expired => {
                return Err(CommerceError::IllegalTransition(
                    "transfer is already closed".into(),
                ))
            }
        }
        wxlike_ledger::release_hold_in(&tx, &format!("transfer:{transfer_id}:hold"), now)?;
        tx.execute(
            "UPDATE commerce_transfers SET status = 'rejected', updated_at = ?1
             WHERE id = ?2 AND status = 'pending'",
            params![now, transfer_id],
        )?;
        let updated = load(&tx, transfer_id)?.ok_or(CommerceError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }

    /// 过期转账由任一方触发退回。
    ///
    /// # Errors
    /// 不存在、无权操作或尚未到期。
    pub fn expire_transfer(&self, actor: &str, transfer_id: &str) -> Result<Transfer> {
        let now = self.now();
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let transfer = load(&tx, transfer_id)?.ok_or(CommerceError::NotFound)?;
        if transfer.sender_id != actor && transfer.payee_id != actor {
            return Err(CommerceError::Forbidden);
        }
        match transfer.status {
            TransferStatus::Expired => return Ok(transfer),
            TransferStatus::Pending => {}
            TransferStatus::Accepted | TransferStatus::Rejected => {
                return Err(CommerceError::IllegalTransition(
                    "transfer is already closed".into(),
                ))
            }
        }
        if now < transfer.expires_at {
            return Err(CommerceError::Conflict("transfer is not expired".into()));
        }
        expire_locked(&tx, &transfer, now)?;
        let updated = load(&tx, transfer_id)?.ok_or(CommerceError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }

    /// 查询转账（收付双方可见）。
    ///
    /// # Errors
    /// 不存在或无权查看。
    pub fn transfer(&self, actor: &str, transfer_id: &str) -> Result<Transfer> {
        store::validate_id("actor", actor)?;
        let conn = self.lock();
        let transfer = load(&conn, transfer_id)?.ok_or(CommerceError::NotFound)?;
        if transfer.sender_id != actor && transfer.payee_id != actor {
            return Err(CommerceError::Forbidden);
        }
        Ok(transfer)
    }

    fn transfer_token(
        &self,
        sender: &str,
        payee: &str,
        amount: Money,
        memo: &str,
        nonce: &str,
        expires_at: i64,
    ) -> String {
        store::hmac_hex(
            self.config.confirmation_secret.as_slice(),
            &[
                "transfer",
                sender,
                payee,
                &amount.minor().to_string(),
                amount.currency().as_str(),
                memo,
                nonce,
                &expires_at.to_string(),
            ],
        )
    }
}

fn expire_locked(conn: &Connection, transfer: &Transfer, now: i64) -> Result<()> {
    wxlike_ledger::release_hold_in(conn, &format!("transfer:{}:hold", transfer.id), now)?;
    conn.execute(
        "UPDATE commerce_transfers SET status = 'expired', updated_at = ?1
         WHERE id = ?2 AND status = 'pending'",
        params![now, transfer.id],
    )?;
    Ok(())
}

fn load(conn: &Connection, id: &str) -> Result<Option<Transfer>> {
    let raw = conn
        .query_row(
            "SELECT id, sender_id, payee_id, amount_minor, currency, memo, status, expires_at,
                    created_at, updated_at
             FROM commerce_transfers WHERE id = ?1",
            params![id],
            scan,
        )
        .optional()?;
    raw.map(build).transpose()
}

fn load_by_key(conn: &Connection, key: &str) -> Result<Option<(String, Transfer)>> {
    let raw = conn
        .query_row(
            "SELECT id, sender_id, payee_id, amount_minor, currency, memo, status, expires_at,
                    created_at, updated_at, nonce_hash
             FROM commerce_transfers WHERE business_key = ?1",
            params![key],
            |r| {
                let mut row = scan(r)?;
                row.nonce_hash = r.get(10)?;
                Ok(row)
            },
        )
        .optional()?;
    raw.map(|raw| {
        let digest = store::digest_hex(&[
            &raw.sender_id,
            &raw.payee_id,
            &raw.amount_minor.to_string(),
            &raw.currency,
            &raw.memo,
            &raw.nonce_hash,
        ]);
        Ok((digest, build(raw)?))
    })
    .transpose()
}

struct RawTransfer {
    id: String,
    sender_id: String,
    payee_id: String,
    amount_minor: i64,
    currency: String,
    memo: String,
    status: String,
    expires_at: i64,
    created_at: i64,
    updated_at: i64,
    nonce_hash: String,
}

fn scan(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawTransfer> {
    Ok(RawTransfer {
        id: r.get(0)?,
        sender_id: r.get(1)?,
        payee_id: r.get(2)?,
        amount_minor: r.get(3)?,
        currency: r.get(4)?,
        memo: r.get(5)?,
        status: r.get(6)?,
        expires_at: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
        nonce_hash: String::new(),
    })
}

fn build(raw: RawTransfer) -> Result<Transfer> {
    Ok(Transfer {
        id: raw.id,
        sender_id: raw.sender_id,
        payee_id: raw.payee_id,
        amount: Money::from_minor(raw.amount_minor, Currency::parse(&raw.currency)?),
        memo: raw.memo,
        status: TransferStatus::parse(&raw.status)?,
        expires_at: raw.expires_at,
        created_at: raw.created_at,
        updated_at: raw.updated_at,
    })
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}
