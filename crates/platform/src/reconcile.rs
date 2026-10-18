//! P4 钱包对账任务（合同 §3.10 / 蓝图待修项 6）。
//!
//! 有账本无对账等于没账本：单笔转账的守恒有测试兜底，但**长期运行后的
//! 存量一致性**只能靠定时核对发现（半提交、手工改库、并发缺陷、代码回滚）。
//!
//! 三条核对线，全部只读，任一条不符即告警：
//!
//! | # | 断言 | 抓什么 |
//! |---|---|---|
//! | 1 | `wallet.balance == SUM(带符号流水)`（逐用户） | 余额被绕过账本改动 |
//! | 2 | `SUM(wallet.balance) + escrow == SUM(fund_in)` | 系统凭空多钱/少钱 |
//! | 3 | 红包自身算术（`claimed <= count`、`claimed_total <= total`、状态与笔数一致） | 领取计数漂移 |
//!
//! 注意符号：`ledger.amount` 存的是**正数量**，方向由 `kind` 编码
//! （`transfer_out` / `redpacket_out` 为流出，其余为流入）。对账时必须按
//! kind 还原符号，否则"转出"会被算成"转入"——这正是本模块第一版被测试
//! 抓到的错误，保留这条注释以防回退。
//!
//! 第 2 条里的 `escrow` 是"已冻结未领取"（`total - claimed_total`），必须计入
//! 供给——否则每个未领完的红包都会被误判为丢钱。
//!
//! 对账结果落 `reconcile_runs`（每次一行），供 P6 运营看板画趋势；不通过时
//! 同时写审计日志——**对账异常属于安全事件，不只是运维指标**。

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, PlatformError, Result};
use crate::{Platform, PlatformState};

/// `reconcile_runs` 一行的读取形状（列多，别名避免 clippy::type_complexity）。
type RunRow = (i64, i64, i64, i64, i64, i64, String, i64);

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/admin/reconcile", get(latest).post(run))
        .route("/api/admin/reconcile/history", get(history))
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Mismatch {
    /// 余额与流水不符。
    Wallet {
        user: String,
        balance: i64,
        ledger_sum: i64,
    },
    /// 供给与发行量不符。
    Supply { supply: i64, funded_total: i64 },
    /// 红包自身算术/状态不符。
    Redpacket { id: String, reason: String },
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ReconcileReport {
    pub at_ms: i64,
    pub users_checked: usize,
    pub wallet_sum: i64,
    pub escrow: i64,
    pub supply: i64,
    pub funded_total: i64,
    pub mismatches: Vec<Mismatch>,
    pub ok: bool,
}

impl Platform {
    /// 执行一次全量对账（只读 + 落一行运行记录）。
    pub fn reconcile(&self) -> Result<ReconcileReport> {
        let at = wxlike_core::types::now_ms() as i64;
        let mut mismatches = Vec::new();

        let (wallet_sum, users_checked, per_user, escrow, funded_total, packets) = {
            let c = self.db().lock();
            let wallet_sum: i64 =
                c.query_row("SELECT COALESCE(SUM(balance), 0) FROM wallets", [], |r| {
                    r.get(0)
                })?;
            let per_user: Vec<(String, i64, i64)> = {
                let mut stmt = c.prepare(
                    "SELECT w.user_id, w.balance,
                            COALESCE((SELECT SUM(CASE
                                WHEN l.kind IN ('transfer_out', 'redpacket_out') THEN -l.amount
                                ELSE l.amount END)
                              FROM ledger l WHERE l.user_id = w.user_id), 0)
                     FROM wallets w",
                )?;
                let rows = stmt.query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?;
                rows.collect::<std::result::Result<Vec<_>, _>>()?
            };
            let escrow: i64 = c.query_row(
                "SELECT COALESCE(SUM(total - claimed_total), 0) FROM redpackets WHERE status = 'open'",
                [],
                |r| r.get(0),
            )?;
            let funded_total: i64 = c.query_row(
                "SELECT COALESCE(SUM(amount), 0) FROM ledger WHERE kind = 'fund_in'",
                [],
                |r| r.get(0),
            )?;
            let packets: Vec<(String, i64, i64, i64, i64, String)> = {
                let mut stmt = c.prepare(
                    "SELECT id, total, count, claimed, claimed_total, status FROM redpackets",
                )?;
                let rows = stmt.query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                })?;
                rows.collect::<std::result::Result<Vec<_>, _>>()?
            };
            (
                wallet_sum,
                per_user.len(),
                per_user,
                escrow,
                funded_total,
                packets,
            )
        };

        for (user, balance, ledger_sum) in per_user {
            if balance != ledger_sum {
                mismatches.push(Mismatch::Wallet {
                    user,
                    balance,
                    ledger_sum,
                });
            }
        }

        let supply = wallet_sum + escrow;
        if supply != funded_total {
            mismatches.push(Mismatch::Supply {
                supply,
                funded_total,
            });
        }

        for (id, total, count, claimed, claimed_total, status) in packets {
            let reason = if claimed_total > total {
                Some("claimed_total exceeds total")
            } else if claimed > count {
                Some("claimed count exceeds slots")
            } else if status == "finished" && claimed != count {
                Some("finished but slots remain")
            } else if status == "open" && claimed >= count {
                Some("open but no slots remain")
            } else if status == "expired" && claimed > count {
                Some("expired with impossible claim count")
            } else {
                None
            };
            if let Some(reason) = reason {
                mismatches.push(Mismatch::Redpacket {
                    id,
                    reason: reason.to_string(),
                });
            }
        }

        let report = ReconcileReport {
            at_ms: at,
            users_checked,
            wallet_sum,
            escrow,
            supply,
            funded_total,
            ok: mismatches.is_empty(),
            mismatches,
        };
        self.persist_reconcile(&report)?;
        if !report.ok {
            let detail = format!(
                "{} mismatch(es): {}",
                report.mismatches.len(),
                report
                    .mismatches
                    .first()
                    .map(|m| format!("{m:?}"))
                    .unwrap_or_default()
            );
            self.audit("system", "wallet.reconcile", "", &detail);
        }
        Ok(report)
    }

    fn persist_reconcile(&self, report: &ReconcileReport) -> Result<()> {
        let detail = serde_json::to_string(&report.mismatches)?;
        let c = self.db().lock();
        c.execute(
            "INSERT INTO reconcile_runs(at_ms, users_checked, wallet_sum, escrow, supply,
                                         funded_total, mismatches, detail, ok)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                report.at_ms,
                report.users_checked as i64,
                report.wallet_sum,
                report.escrow,
                report.supply,
                report.funded_total,
                report.mismatches.len() as i64,
                detail,
                i64::from(report.ok)
            ],
        )?;
        Ok(())
    }

    /// 最近一次对账（运营看板/告警查询用）。
    pub fn last_reconcile(&self) -> Result<Option<ReconcileReport>> {
        let c = self.db().lock();
        let row: Option<RunRow> = c
            .query_row(
                "SELECT at_ms, users_checked, wallet_sum, escrow, supply, funded_total, detail, ok
                 FROM reconcile_runs ORDER BY id DESC LIMIT 1",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                },
            )
            .ok();
        let Some((at_ms, users_checked, wallet_sum, escrow, supply, funded_total, detail, ok)) =
            row
        else {
            return Ok(None);
        };
        let mismatches: Vec<Mismatch> = serde_json::from_str(&detail).unwrap_or_default();
        Ok(Some(ReconcileReport {
            at_ms,
            users_checked: users_checked.max(0) as usize,
            wallet_sum,
            escrow,
            supply,
            funded_total,
            ok: ok != 0,
            mismatches,
        }))
    }

    /// 最近 N 次运行摘要 `(at_ms, ok, mismatches)`，最新在前。
    pub fn reconcile_history(&self, limit: usize) -> Result<Vec<(i64, bool, i64)>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT at_ms, ok, mismatches FROM reconcile_runs ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit.clamp(1, 200) as i64], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)? != 0,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

/// 定时对账。与红包清扫器同形：`wxlike-server` 启动，测试直接调
/// [`Platform::reconcile`] 以保证确定性。
pub fn spawn_reconciler(
    state: PlatformState,
    every: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(every).await;
            match state.reconcile() {
                Ok(r) if r.ok => {}
                Ok(r) => eprintln!(
                    "[platform] reconcile FAILED: {} mismatch(es), supply={} funded={}",
                    r.mismatches.len(),
                    r.supply,
                    r.funded_total
                ),
                Err(err) => eprintln!("[platform] reconcile error: {err}"),
            }
        }
    })
}

fn require_admin(platform: &Platform, headers: &HeaderMap) -> std::result::Result<(), ApiError> {
    let presented = headers.get("x-admin-token").and_then(|v| v.to_str().ok());
    if platform.admin_ok(presented) {
        Ok(())
    } else {
        Err(PlatformError::AdminRequired.into())
    }
}

async fn run(
    State(platform): State<PlatformState>,
    headers: HeaderMap,
) -> std::result::Result<(StatusCode, Json<ReconcileReport>), ApiError> {
    require_admin(&platform, &headers)?;
    let report = platform.reconcile()?;
    Ok((StatusCode::OK, Json(report)))
}

async fn latest(
    State(platform): State<PlatformState>,
    headers: HeaderMap,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    require_admin(&platform, &headers)?;
    Ok(Json(serde_json::json!({
        "last": platform.last_reconcile()?,
        "history": platform.reconcile_history(20)?,
    })))
}

async fn history(
    State(platform): State<PlatformState>,
    headers: HeaderMap,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    require_admin(&platform, &headers)?;
    Ok(Json(serde_json::json!({
        "runs": platform.reconcile_history(50)?,
    })))
}
