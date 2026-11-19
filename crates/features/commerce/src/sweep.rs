//! 到期清扫的共用骨架（红包 / 转账）。
//!
//! 两处原本各抄了一份"选到期清单 → 逐条 IMMEDIATE 事务 → 复核状态 → 到期
//! 落库"的循环，质量闸门的跨文件重复窗口把它们抓了出来。语义只有一份：
//!
//! * 清单选取与逐条处理**分开的锁段**：选取不持事务，处理每条各起一个
//!   IMMEDIATE 事务，失败只跳过该条，不影响整轮；
//! * 事务内**复核**状态与到期时间（清单选取后可能已被并发推进），
//!   不复核就会把已领取/已确认的对象错误到期；
//! * 到期效果（退款分录/解冻）由调用方的 `expire` 闭包给出——红包退未领
//!   份额、转账解冻退回，各自的账本语义留在各自模块。

use std::sync::MutexGuard;

use rusqlite::{params, Connection, TransactionBehavior};

use crate::error::Result;

/// 单轮清扫的批量上限：到期是后台节拍作业，一轮不贪多，避免与大表扫描抢锁。
pub(crate) const SWEEP_BATCH: i64 = 200;

/// 选取到期 id 清单（一个短锁段，不起事务）。
pub(crate) fn due_ids(
    conn: &Connection,
    table: &str,
    status: &str,
    now: i64,
) -> Result<Vec<String>> {
    let sql = format!(
        "SELECT id FROM {table}
         WHERE status = ?1 AND expires_at <= ?2
         ORDER BY expires_at ASC, id ASC LIMIT {SWEEP_BATCH}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![status, now], |r| r.get::<_, String>(0))?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row?);
    }
    Ok(ids)
}

/// 逐条到期：每条一个 IMMEDIATE 事务；`load` 复核现状，`still_due` 判定
/// 是否仍到期，`expire` 落到期效果。任何一步失败/不再到期都只跳过该条。
/// 返回本轮成功到期的条数。
///
/// 锁守卫由调用方在循环里取（`self.lock()` 的生命周期 tied to self，
/// 泛型闭包返回守卫写不出成立的签名）；本函数只负责事务与复核骨架。
pub(crate) fn expire_due<T>(
    guard: &mut MutexGuard<'_, Connection>,
    due: Vec<String>,
    now: i64,
    load: impl Fn(&Connection, &str) -> Result<Option<T>>,
    still_due: impl Fn(&T, i64) -> bool,
    expire: impl Fn(&Connection, &T, i64) -> Result<()>,
) -> usize {
    let mut expired = 0usize;
    for id in due {
        let Ok(tx) = guard.transaction_with_behavior(TransactionBehavior::Immediate) else {
            continue;
        };
        let Ok(Some(item)) = load(&tx, &id) else {
            continue;
        };
        if !still_due(&item, now) {
            continue;
        }
        if expire(&tx, &item, now).is_ok() && tx.commit().is_ok() {
            expired += 1;
        }
    }
    expired
}
