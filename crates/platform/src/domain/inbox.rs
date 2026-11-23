//! G05-05 通知收件箱（`/api/notifications`）。
//!
//! 与 `notification.rs`（会话级免打扰规则）分开：那个回答"要不要推"，这个
//! 是**已发生的事**的持久收件箱。两条约束：
//!
//! * 通知由业务写**同事务**插入（`notify` 接收 `&Connection`，调用方在事务里
//!   直接调用），所以不会出现"内容删了通知还在"的跨事务窗口；
//! * `read_at` 是服务端列，已读状态跨端共享；`dedupe_key` 唯一索引让重复
//!   点赞/重试不会刷屏。
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::inbox`]。

use crate::error::{PlatformError, Result};
use crate::Platform;
use serde::Serialize;

use super::repo::inbox::{mark_id, mark_up_to, page, unread};

pub const MAX_PAGE: i64 = 50;
const MAX_READ_IDS: usize = 200;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Notification {
    pub seq: i64,
    pub id: String,
    pub kind: String,
    pub actor: String,
    pub target_kind: String,
    pub target_id: String,
    pub text: String,
    pub created_at: i64,
    pub read: bool,
}

impl Platform {
    pub(crate) fn notifications(
        &self,
        user: &str,
        before_seq: Option<i64>,
        limit: i64,
    ) -> Result<(Vec<Notification>, i64, bool)> {
        let limit = limit.clamp(1, MAX_PAGE);
        let before = before_seq.unwrap_or(i64::MAX);
        let c = self.db().lock();
        let mut rows = page(&c, user, before, limit + 1)?;
        let has_more = rows.len() as i64 > limit;
        if has_more {
            rows.pop();
        }
        let next = rows.last().map(|n| n.seq).unwrap_or(0);
        Ok((rows, next, has_more))
    }

    pub(crate) fn unread_notifications(&self, user: &str) -> Result<i64> {
        let c = self.db().lock();
        unread(&c, user)
    }

    /// 已读只能推进到 `up_to_seq`（含），或者按 id 列表。两者都限定本人。
    pub(crate) fn mark_notifications_read(
        &self,
        user: &str,
        up_to_seq: Option<i64>,
        ids: &[String],
    ) -> Result<usize> {
        if up_to_seq.is_none() && ids.is_empty() {
            return Err(PlatformError::BadInput("up_to_seq or ids is required"));
        }
        if ids.len() > MAX_READ_IDS {
            return Err(PlatformError::BadInput("too many notification ids"));
        }
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        let mut changed = 0usize;
        if let Some(up_to) = up_to_seq {
            changed += mark_up_to(&c, user, up_to, now)?;
        }
        for id in ids {
            let id = crate::validate::id(id)?;
            changed += mark_id(&c, user, id, now)?;
        }
        Ok(changed)
    }
}
