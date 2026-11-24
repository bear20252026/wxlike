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
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::reconcile`]。

use crate::error::Result;
use crate::{Platform, PlatformState};
use serde::{Deserialize, Serialize};

use super::repo::reconcile::{
    escrow_sum, funded_total, history_rows, insert_run, last_run, redpacket_rows,
    wallet_balance_sum, wallet_ledger_rows, PacketRow, WalletRow,
};

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

fn wallet_mismatches(rows: Vec<WalletRow>) -> Vec<Mismatch> {
    rows.into_iter()
        .filter(|row| row.balance != row.ledger_sum)
        .map(|row| Mismatch::Wallet {
            user: row.user,
            balance: row.balance,
            ledger_sum: row.ledger_sum,
        })
        .collect()
}

fn redpacket_mismatches(packets: Vec<PacketRow>) -> Vec<Mismatch> {
    packets
        .into_iter()
        .filter_map(|packet| {
            packet_reason(&packet).map(|reason| Mismatch::Redpacket {
                id: packet.id,
                reason: reason.to_string(),
            })
        })
        .collect()
}

fn packet_reason(packet: &PacketRow) -> Option<&'static str> {
    if packet.claimed_total > packet.total {
        Some("claimed_total exceeds total")
    } else if packet.claimed > packet.count {
        Some("claimed count exceeds slots")
    } else if packet.status == "finished" && packet.claimed != packet.count {
        Some("finished but slots remain")
    } else if packet.status == "open" && packet.claimed >= packet.count {
        Some("open but no slots remain")
    } else if packet.status == "expired" && packet.claimed > packet.count {
        Some("expired with impossible claim count")
    } else {
        None
    }
}

impl Platform {
    /// 执行一次全量对账（只读 + 落一行运行记录）。
    pub fn reconcile(&self) -> Result<ReconcileReport> {
        let at = wxlike_core::types::now_ms() as i64;

        let (wallet_sum, per_user, escrow, funded_total, packets) = {
            let c = self.db().acquire();
            let wallet_sum = wallet_balance_sum(&c)?;
            let per_user = wallet_ledger_rows(&c)?;
            let escrow = escrow_sum(&c)?;
            let funded_total = funded_total(&c)?;
            let packets = redpacket_rows(&c)?;
            (wallet_sum, per_user, escrow, funded_total, packets)
        };
        let users_checked = per_user.len();

        let mut mismatches = wallet_mismatches(per_user);
        let supply = wallet_sum + escrow;
        if supply != funded_total {
            mismatches.push(Mismatch::Supply {
                supply,
                funded_total,
            });
        }
        mismatches.extend(redpacket_mismatches(packets));

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
        self.audit_reconcile(&report);
        Ok(report)
    }

    fn audit_reconcile(&self, report: &ReconcileReport) {
        if report.ok {
            return;
        }
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

    fn persist_reconcile(&self, report: &ReconcileReport) -> Result<()> {
        let detail = serde_json::to_string(&report.mismatches)?;
        let c = self.db().acquire();
        insert_run(
            &c,
            report.at_ms,
            report.users_checked as i64,
            report.wallet_sum,
            report.escrow,
            report.supply,
            report.funded_total,
            report.mismatches.len() as i64,
            &detail,
            report.ok,
        )
    }

    /// 最近一次对账（运营看板/告警查询用）。
    pub fn last_reconcile(&self) -> Result<Option<ReconcileReport>> {
        let c = self.db().acquire();
        let Some(row) = last_run(&c)? else {
            return Ok(None);
        };
        let mismatches: Vec<Mismatch> = serde_json::from_str(&row.detail).unwrap_or_default();
        Ok(Some(ReconcileReport {
            at_ms: row.at_ms,
            users_checked: row.users_checked.max(0) as usize,
            wallet_sum: row.wallet_sum,
            escrow: row.escrow,
            supply: row.supply,
            funded_total: row.funded_total,
            ok: row.ok != 0,
            mismatches,
        }))
    }

    /// 最近 N 次运行摘要 `(at_ms, ok, mismatches)`，最新在前。
    pub fn reconcile_history(&self, limit: usize) -> Result<Vec<(i64, bool, i64)>> {
        let c = self.db().acquire();
        history_rows(&c, limit.clamp(1, 200) as i64)
    }
}

/// 定时对账。与红包清扫器同形：`wxlike-server` 启动，测试直接调
/// `Platform::reconcile` 以保证确定性。
pub fn spawn_reconciler(
    state: PlatformState,
    every: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(every).await;
            report_reconcile(state.reconcile());
        }
    })
}

/// 一次对账结果落成日志，返回事件的档位名。
///
/// 抽出来是为了让失败分支可断言：spawn 出去的循环体在测试里永远跑不到，
/// 而"对账不平/对账报错必须告警"本身就是合同 §3.10 要求的行为。
fn report_reconcile(outcome: Result<ReconcileReport>) -> &'static str {
    match outcome {
        Ok(report) if report.ok => "ok",
        Ok(report) => {
            tracing::error!(
                mismatches = report.mismatches.len(),
                supply = report.supply,
                funded_total = report.funded_total,
                "reconcile FAILED"
            );
            "failed"
        }
        Err(err) => {
            tracing::error!(error = %err, "reconcile error");
            "error"
        }
    }
}

/// 运维界面展示的对账历史条数上限。
pub(crate) const RECONCILE_HISTORY_LIMIT: usize = 50;
#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{report_reconcile, spawn_reconciler, Mismatch, Platform, ReconcileReport};
    use crate::PlatformError;

    fn report(ok: bool, mismatches: Vec<Mismatch>) -> ReconcileReport {
        ReconcileReport {
            at_ms: 1,
            users_checked: 0,
            wallet_sum: 0,
            escrow: 0,
            supply: 0,
            funded_total: 0,
            mismatches,
            ok,
        }
    }

    #[test]
    fn flat_ledger_reports_without_alerting() {
        assert_eq!(report_reconcile(Ok(report(true, Vec::new()))), "ok");
    }

    #[test]
    fn mismatching_report_alerts_as_failed() {
        let drift = vec![Mismatch::Supply {
            supply: 1,
            funded_total: 2,
        }];
        assert_eq!(report_reconcile(Ok(report(false, drift))), "failed");
    }

    #[test]
    fn reconcile_error_alerts_as_error() {
        assert_eq!(report_reconcile(Err(PlatformError::NotFound)), "error");
    }

    /// 定时器本体：`server/src/lib.rs` 是唯一调用方，这里用真实短周期跑一轮，
    /// 证明"到点就对账并落日志"这条线不是死代码。
    #[tokio::test]
    async fn spawned_reconciler_reaches_the_report_path() {
        let state = Platform::in_memory().expect("in-memory platform").shared();
        let handle = spawn_reconciler(state, Duration::from_millis(5));
        tokio::time::sleep(Duration::from_millis(80)).await;
        handle.abort();
    }
}
