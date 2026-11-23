//! P3 审计日志（不变式 7）：独立审计表，只写面。
//!
//! 检索/留存策略归 P6 adminx；与运行日志（tracing → Loki）物理分离。本模块
//! 只保留时间戳、条数夹紧和失败留痕，SQL 在 [`crate::domain::repo::audit`]。

use crate::domain::repo::audit as audit_repo;
use crate::error::Result;
use crate::Platform;

impl Platform {
    /// 写一条审计。失败只留日志、不向调用方扩散：审计不能反过来阻断业务。
    pub fn audit(&self, actor: &str, action: &str, target: &str, detail: &str) {
        let r = {
            let c = self.db().lock();
            audit_repo::write(
                &c,
                wxlike_core::types::now_ms() as i64,
                actor,
                action,
                target,
                detail,
            )
        };
        if let Err(err) = r {
            eprintln!("[platform] audit write failed: {err}");
        }
    }

    /// 审计读取面（自查自己的记录；全量检索归 P6 adminx + RBAC）。
    pub fn audit_of(
        &self,
        actor: &str,
        limit: usize,
    ) -> Result<Vec<(i64, String, String, String)>> {
        let limit = limit.clamp(1, 200) as i64;
        let c = self.db().lock();
        audit_repo::list_for_actor(&c, actor, limit)
    }
}
