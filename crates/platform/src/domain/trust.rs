//! P4 审核管线接入面（ADR-20 第二层的调用边界）。
//!
//! 审核判定本身在 [`wxlike_moderation`]；这里做三件事：
//!
//! 1. **发送路径的闸门**：`moderate_for_send` 返回处置结论，`Hold` 时把消息
//!    放进 `moderation_queue`（先审后发），投递方拿不到可投递内容——挂起是
//!    真的挂起，不是"打个标记照发"。
//! 2. **运营复核**：管理令牌可查看队列、放行（release 返回原文，由调用方
//!    投递）与切换模式。
//! 3. **自查接口**：客户端发前自检（`/api/moderation/check`），降低用户
//!    因误触敏感词被挂起的挫败感。
//!
//! 多模态：`Pipeline` 支持注入 [`wxlike_moderation::MediaJudge`]，本层不
//! 直接依赖任何模型；没有注入时带媒体的消息按"未扫描"进复核（
//! `media_unscanned` 标签），绝不假装干净。
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::trust`]。

use crate::error::Result;
use crate::Platform;
use rusqlite::params;
use serde::Serialize;
use wxlike_moderation::{Decision, Mode};

/// 发送闸门的返回：结论 + 挂起队列 id（仅 Hold 时有值）。
#[derive(Clone, Debug, Serialize)]
pub struct SendVerdict {
    pub decision: Decision,
    pub queued_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct QueuedItem {
    pub id: String,
    pub conv_id: String,
    pub sender: String,
    pub text: String,
    pub labels: Vec<String>,
    pub created_at: i64,
}

impl Platform {
    /// 纯判定（不落库）。
    pub fn moderate(&self, text: &str) -> Decision {
        self.moderation().judge_text(text)
    }

    /// 发送闸门：判定 + （先审后发时）挂起。
    pub fn moderate_for_send(
        &self,
        sender: &str,
        conv_id: &str,
        text: &str,
    ) -> Result<SendVerdict> {
        let decision = self.moderation().judge_text(text);
        let mut queued_id = None;
        if decision.outcome == wxlike_moderation::Outcome::Hold {
            let id = crate::auth::random_hex(8);
            let labels = serde_json::to_string(&decision.labels)?;
            let c = self.db().lock();
            c.execute(
                "INSERT INTO moderation_queue(id, conv_id, sender, text, labels, decision, created_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id,
                    conv_id,
                    sender,
                    text,
                    labels,
                    serde_json::to_string(&decision)?,
                    wxlike_core::types::now_ms() as i64
                ],
            )?;
            queued_id = Some(id);
        }
        Ok(SendVerdict {
            decision,
            queued_id,
        })
    }

    pub fn moderation_mode(&self) -> Mode {
        self.moderation().mode()
    }

    /// 热切换模式（运营在"先审后发/先发后审"之间调整，无需重启）。
    pub fn set_moderation_mode(&self, mode: Mode) {
        self.moderation().set_mode(mode);
    }

    /// 挂起队列（最早在前：先挂起的先处理）。
    pub fn moderation_queue(&self, limit: usize) -> Result<Vec<QueuedItem>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT id, conv_id, sender, text, labels, created_at
             FROM moderation_queue ORDER BY created_at ASC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit.clamp(1, 200) as i64], |r| {
                let labels: String = r.get(4)?;
                Ok(QueuedItem {
                    id: r.get(0)?,
                    conv_id: r.get(1)?,
                    sender: r.get(2)?,
                    text: r.get(3)?,
                    labels: serde_json::from_str(&labels).unwrap_or_default(),
                    created_at: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 放行：从队列取出并返回原文（调用方负责真正投递）。
    pub fn release_moderated(&self, id: &str, actor: &str) -> Result<Option<QueuedItem>> {
        let item = {
            let c = self.db().lock();
            let row = c
                .query_row(
                    "SELECT id, conv_id, sender, text, labels, created_at
                     FROM moderation_queue WHERE id = ?1",
                    params![id],
                    |r| {
                        let labels: String = r.get(4)?;
                        Ok(QueuedItem {
                            id: r.get(0)?,
                            conv_id: r.get(1)?,
                            sender: r.get(2)?,
                            text: r.get(3)?,
                            labels: serde_json::from_str(&labels).unwrap_or_default(),
                            created_at: r.get(5)?,
                        })
                    },
                )
                .ok();
            if row.is_some() {
                c.execute("DELETE FROM moderation_queue WHERE id = ?1", params![id])?;
            }
            row
        };
        if item.is_some() {
            self.audit(actor, "moderation.release", id, "");
        }
        Ok(item)
    }

    /// 拒绝：从队列删除，不投递。
    pub fn reject_moderated(&self, id: &str, actor: &str) -> Result<bool> {
        let c = self.db().lock();
        let n = c.execute("DELETE FROM moderation_queue WHERE id = ?1", params![id])?;
        drop(c);
        if n > 0 {
            self.audit(actor, "moderation.reject", id, "");
        }
        Ok(n > 0)
    }
}
