//! **仓储层**（`wallet`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。
//!
//! 金额校验、份额分配、幂等语义和红包状态机留在
//! [`crate::domain::wallet`]；本文件只负责余额、账本、幂等记录与红包行的读写。

use crate::storage::StorageConnection;
use rusqlite::{params, OptionalExtension as _};

use crate::domain::wallet::{LedgerRow, RedpacketRow};
use crate::error::Result;

/// 领取前的红包快照。一次查询同时带回 owner 与 conv，避免同一事务里
/// 对同一行查两遍。
pub(in crate::domain) struct RedpacketState {
    pub from_uid: String,
    pub conv_id: String,
    pub total: i64,
    pub count: i64,
    pub claimed: i64,
    pub claimed_total: i64,
    pub claimers_json: String,
    pub status: String,
}

pub(in crate::domain) struct ExpiredPacket {
    pub id: String,
    pub from_uid: String,
    pub total: i64,
    pub claimed_total: i64,
}

/// 读余额。`None` = 没有钱包行；调用方按 0 处理，但不会把读取失败折成 0。
pub(in crate::domain) fn wallet_balance(
    conn: &StorageConnection<'_>,
    user_id: &str,
) -> Result<Option<i64>> {
    let conn = conn.sqlite()?;
    let balance = conn
        .query_row(
            "SELECT balance FROM wallets WHERE user_id = ?1",
            params![user_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()?;
    Ok(balance)
}

pub(in crate::domain) fn ensure_wallet(conn: &StorageConnection<'_>, user_id: &str) -> Result<()> {
    let conn = conn.sqlite()?;
    conn.execute(
        "INSERT INTO wallets(user_id, balance) VALUES(?1, 0)
         ON CONFLICT(user_id) DO NOTHING",
        params![user_id],
    )?;
    Ok(())
}

pub(in crate::domain) fn user_exists(conn: &StorageConnection<'_>, user_id: &str) -> Result<bool> {
    let conn = conn.sqlite()?;
    Ok(conn
        .query_row(
            "SELECT 1 FROM users WHERE id = ?1",
            params![user_id],
            |_| Ok(1i64),
        )
        .optional()?
        .is_some())
}

pub(in crate::domain) fn debit_if_sufficient(
    conn: &StorageConnection<'_>,
    user_id: &str,
    amount: i64,
) -> Result<usize> {
    let conn = conn.sqlite()?;
    let updated = conn.execute(
        "UPDATE wallets SET balance = balance - ?1 WHERE user_id = ?2 AND balance >= ?1",
        params![amount, user_id],
    )?;
    Ok(updated)
}

pub(in crate::domain) fn credit(
    conn: &StorageConnection<'_>,
    user_id: &str,
    amount: i64,
) -> Result<usize> {
    let conn = conn.sqlite()?;
    let updated = conn.execute(
        "UPDATE wallets SET balance = balance + ?1 WHERE user_id = ?2",
        params![amount, user_id],
    )?;
    Ok(updated)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::domain) fn log_ledger(
    conn: &StorageConnection<'_>,
    user_id: &str,
    kind: &str,
    amount: i64,
    counterparty: &str,
    ref_id: &str,
    memo: &str,
    now: i64,
) -> Result<()> {
    let conn = conn.sqlite()?;
    conn.execute(
        "INSERT INTO ledger(user_id, kind, amount, counterparty, ref_id, memo, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![user_id, kind, amount, counterparty, ref_id, memo, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn ledger_of(
    conn: &StorageConnection<'_>,
    user_id: &str,
    since: i64,
    limit: i64,
) -> Result<Vec<LedgerRow>> {
    let conn = conn.sqlite()?;
    let mut stmt = conn.prepare(
        "SELECT id, kind, amount, counterparty, ref_id, memo, created_at
         FROM ledger WHERE user_id = ?1 AND id > ?2 ORDER BY id ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![user_id, since, limit], |r| {
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
    let ledger = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(ledger)
}

pub(in crate::domain) fn ledger_head(conn: &StorageConnection<'_>, user_id: &str) -> Result<i64> {
    let conn = conn.sqlite()?;
    let head = conn.query_row(
        "SELECT COALESCE(MAX(id), 0) FROM ledger WHERE user_id = ?1",
        params![user_id],
        |r| r.get::<_, i64>(0),
    )?;
    Ok(head)
}

pub(in crate::domain) fn idem_lookup(
    conn: &StorageConnection<'_>,
    user_id: &str,
    key: &str,
) -> Result<Option<(String, String)>> {
    let conn = conn.sqlite()?;
    let row = conn
        .query_row(
            "SELECT request_hash, response FROM wallet_idem WHERE user_id = ?1 AND key = ?2",
            params![user_id, key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row)
}

pub(in crate::domain) fn idem_gc(conn: &StorageConnection<'_>, cutoff: i64) -> Result<()> {
    let conn = conn.sqlite()?;
    conn.execute(
        "DELETE FROM wallet_idem WHERE created_at < ?1",
        params![cutoff],
    )?;
    Ok(())
}

pub(in crate::domain) fn idem_insert(
    conn: &StorageConnection<'_>,
    user_id: &str,
    key: &str,
    request_hash: &str,
    response_json: &str,
    now: i64,
) -> Result<()> {
    let conn = conn.sqlite()?;
    conn.execute(
        "INSERT INTO wallet_idem(user_id, key, request_hash, response, created_at) \
         VALUES(?1, ?2, ?3, ?4, ?5) ON CONFLICT(user_id, key) DO NOTHING",
        params![user_id, key, request_hash, response_json, now],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(in crate::domain) fn insert_redpacket(
    conn: &StorageConnection<'_>,
    id: &str,
    from: &str,
    conv_id: &str,
    total: i64,
    count: i64,
    memo: &str,
    now: i64,
    expire_at: i64,
) -> Result<()> {
    let conn = conn.sqlite()?;
    conn.execute(
        "INSERT INTO redpackets(id, from_uid, conv_id, total, count, claimed, claimed_total, claimers, memo, status, created_at, expire_at)
         VALUES(?1, ?2, ?3, ?4, ?5, 0, 0, '[]', ?6, 'open', ?7, ?8)",
        params![id, from, conv_id, total, count, memo, now, expire_at],
    )?;
    Ok(())
}

pub(in crate::domain) fn redpacket(
    conn: &StorageConnection<'_>,
    id: &str,
) -> Result<Option<RedpacketRow>> {
    let conn = conn.sqlite()?;
    let row = conn
        .query_row(
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
        .optional()?;
    Ok(row)
}

pub(in crate::domain) fn redpacket_state(
    conn: &StorageConnection<'_>,
    id: &str,
) -> Result<Option<RedpacketState>> {
    let conn = conn.sqlite()?;
    let state = conn
        .query_row(
            "SELECT total, count, claimed, claimed_total, claimers, status, from_uid, conv_id
             FROM redpackets WHERE id = ?1",
            params![id],
            |r| {
                Ok(RedpacketState {
                    total: r.get(0)?,
                    count: r.get(1)?,
                    claimed: r.get(2)?,
                    claimed_total: r.get(3)?,
                    claimers_json: r.get(4)?,
                    status: r.get(5)?,
                    from_uid: r.get(6)?,
                    conv_id: r.get(7)?,
                })
            },
        )
        .optional()?;
    Ok(state)
}

pub(in crate::domain) fn update_claim(
    conn: &StorageConnection<'_>,
    id: &str,
    share: i64,
    claimers_json: &str,
) -> Result<usize> {
    let conn = conn.sqlite()?;
    let updated = conn.execute(
        "UPDATE redpackets SET claimed = claimed + 1, claimed_total = claimed_total + ?1,
                claimers = ?2, status = CASE WHEN claimed + 1 >= count THEN 'finished' ELSE status END
         WHERE id = ?3",
        params![share, claimers_json, id],
    )?;
    Ok(updated)
}

pub(in crate::domain) fn expired_packets(
    conn: &StorageConnection<'_>,
    now: i64,
) -> Result<Vec<ExpiredPacket>> {
    let conn = conn.sqlite()?;
    let mut stmt = conn.prepare(
        "SELECT id, from_uid, total, claimed_total FROM redpackets
         WHERE status = 'open' AND expire_at <= ?1",
    )?;
    let rows = stmt.query_map(params![now], |r| {
        Ok(ExpiredPacket {
            id: r.get(0)?,
            from_uid: r.get(1)?,
            total: r.get(2)?,
            claimed_total: r.get(3)?,
        })
    })?;
    let packets = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(packets)
}

pub(in crate::domain) fn mark_expired(conn: &StorageConnection<'_>, id: &str) -> Result<usize> {
    let conn = conn.sqlite()?;
    let updated = conn.execute(
        "UPDATE redpackets SET status = 'expired' WHERE id = ?1",
        params![id],
    )?;
    Ok(updated)
}

pub(in crate::domain) fn total_wallets(conn: &StorageConnection<'_>) -> Result<i64> {
    let conn = conn.sqlite()?;
    let total = conn.query_row("SELECT COALESCE(SUM(balance), 0) FROM wallets", [], |r| {
        r.get::<_, i64>(0)
    })?;
    Ok(total)
}

pub(in crate::domain) fn open_escrow(conn: &StorageConnection<'_>) -> Result<i64> {
    let conn = conn.sqlite()?;
    let escrow = conn.query_row(
        "SELECT COALESCE(SUM(total - claimed_total), 0) FROM redpackets WHERE status = 'open'",
        [],
        |r| r.get::<_, i64>(0),
    )?;
    Ok(escrow)
}
