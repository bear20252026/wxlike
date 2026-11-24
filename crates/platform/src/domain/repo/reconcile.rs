//! **仓储层**（`reconcile`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use crate::storage::StorageConnection;
use rusqlite::{params, OptionalExtension as _};

pub(in crate::domain) fn wallet_balance_sum(c: &StorageConnection<'_>) -> Result<i64> {
    let c = c.sqlite()?;
    Ok(
        c.query_row("SELECT COALESCE(SUM(balance), 0) FROM wallets", [], |r| {
            r.get(0)
        })?,
    )
}

pub(in crate::domain) struct WalletRow {
    pub(crate) user: String,
    pub(crate) balance: i64,
    pub(crate) ledger_sum: i64,
}

pub(in crate::domain) fn wallet_ledger_rows(c: &StorageConnection<'_>) -> Result<Vec<WalletRow>> {
    let c = c.sqlite()?;
    let mut stmt = c.prepare(
        "SELECT w.user_id, w.balance,
                COALESCE((SELECT SUM(CASE
                    WHEN l.kind IN ('transfer_out', 'redpacket_out') THEN -l.amount
                    ELSE l.amount END)
                  FROM ledger l WHERE l.user_id = w.user_id), 0)
         FROM wallets w",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(WalletRow {
            user: r.get(0)?,
            balance: r.get(1)?,
            ledger_sum: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub(in crate::domain) fn escrow_sum(c: &StorageConnection<'_>) -> Result<i64> {
    let c = c.sqlite()?;
    Ok(c.query_row(
        "SELECT COALESCE(SUM(total - claimed_total), 0) FROM redpackets WHERE status = 'open'",
        [],
        |r| r.get(0),
    )?)
}

pub(in crate::domain) fn funded_total(c: &StorageConnection<'_>) -> Result<i64> {
    let c = c.sqlite()?;
    Ok(c.query_row(
        "SELECT COALESCE(SUM(amount), 0) FROM ledger WHERE kind = 'fund_in'",
        [],
        |r| r.get(0),
    )?)
}

pub(in crate::domain) struct PacketRow {
    pub(crate) id: String,
    pub(crate) total: i64,
    pub(crate) count: i64,
    pub(crate) claimed: i64,
    pub(crate) claimed_total: i64,
    pub(crate) status: String,
}

pub(in crate::domain) fn redpacket_rows(c: &StorageConnection<'_>) -> Result<Vec<PacketRow>> {
    let c = c.sqlite()?;
    let mut stmt =
        c.prepare("SELECT id, total, count, claimed, claimed_total, status FROM redpackets")?;
    let rows = stmt.query_map([], |r| {
        Ok(PacketRow {
            id: r.get(0)?,
            total: r.get(1)?,
            count: r.get(2)?,
            claimed: r.get(3)?,
            claimed_total: r.get(4)?,
            status: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::domain) fn insert_run(
    c: &StorageConnection<'_>,
    at_ms: i64,
    users_checked: i64,
    wallet_sum: i64,
    escrow: i64,
    supply: i64,
    funded_total: i64,
    mismatches: i64,
    detail: &str,
    ok: bool,
) -> Result<()> {
    let c = c.sqlite()?;
    c.execute(
        "INSERT INTO reconcile_runs(at_ms, users_checked, wallet_sum, escrow, supply,
                                     funded_total, mismatches, detail, ok)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            at_ms,
            users_checked,
            wallet_sum,
            escrow,
            supply,
            funded_total,
            mismatches,
            detail,
            i64::from(ok)
        ],
    )?;
    Ok(())
}

pub(in crate::domain) struct RunRow {
    pub(crate) at_ms: i64,
    pub(crate) users_checked: i64,
    pub(crate) wallet_sum: i64,
    pub(crate) escrow: i64,
    pub(crate) supply: i64,
    pub(crate) funded_total: i64,
    pub(crate) detail: String,
    pub(crate) ok: i64,
}

pub(in crate::domain) fn last_run(c: &StorageConnection<'_>) -> Result<Option<RunRow>> {
    let c = c.sqlite()?;
    Ok(c.query_row(
        "SELECT at_ms, users_checked, wallet_sum, escrow, supply, funded_total, detail, ok
         FROM reconcile_runs ORDER BY id DESC LIMIT 1",
        [],
        |r| {
            Ok(RunRow {
                at_ms: r.get(0)?,
                users_checked: r.get(1)?,
                wallet_sum: r.get(2)?,
                escrow: r.get(3)?,
                supply: r.get(4)?,
                funded_total: r.get(5)?,
                detail: r.get(6)?,
                ok: r.get(7)?,
            })
        },
    )
    .optional()?)
}

pub(in crate::domain) fn history_rows(
    c: &StorageConnection<'_>,
    limit: i64,
) -> Result<Vec<(i64, bool, i64)>> {
    let c = c.sqlite()?;
    let mut stmt =
        c.prepare("SELECT at_ms, ok, mismatches FROM reconcile_runs ORDER BY id DESC LIMIT ?1")?;
    let rows = stmt.query_map(params![limit], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)? != 0,
            r.get::<_, i64>(2)?,
        ))
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}
