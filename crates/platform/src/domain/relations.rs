//! P3 关系增强（合同 §3.2 / ADR-24）：拉黑、陌生人消息请求箱、星标/标签/分组。
//!
//! 不变式 9：**陌生人单聊必经 message_requests 流程**——非好友首条消息不产生
//! 会话，落请求箱；接受后发送方获得直发许可；拒绝即封该请求。是否允许陌生
//! 人发起是用户配置（`user_settings.allow_strangers`，默认允许），但流程本身
//! 必须存在、不可绕过。
//!
//! 拉黑是前置闸门：被拉黑者的好友申请与陌生人请求都直接拒绝（hub 侧的投递
//! 收件过滤随 P3-8 信箱接线落地，那里复用 `is_blocked`）。
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::relations`]。

use crate::error::{PlatformError, Result};
use crate::validate;
use crate::Platform;
use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};
use wxlike_relations::port::RelationsPort as _;

use super::repo::relations::row_request;

/// 关系域错误 → 平台错误。规则在 `wxlike-relations` 里，这里只做类型搬运；
/// HTTP 状态由既有的 `From<PlatformError> for ApiError` 统一决定。
impl From<wxlike_relations::RelError> for PlatformError {
    fn from(err: wxlike_relations::RelError) -> Self {
        use wxlike_relations::RelError as E;
        match err {
            E::BadInput(msg) => Self::BadInput(msg),
            E::Forbidden(_) => Self::Forbidden,
            E::NotFound(_) => Self::NotFound,
            E::Conflict(msg) => Self::Conflict(msg.to_string()),
            // 坏数据是服务端故障（500），不伪装成客户端错误：宁可失败，
            // 也不能让读不出的行折成"这条关系正常"。见 `RelError::Corrupt` 文档。
            E::Corrupt(msg) => Self::Internal(format!("corrupt relation row: {msg}")),
            E::Db(e) => Self::Sqlite(e),
            E::Json(e) => Self::Codec(e),
        }
    }
}

// ---------------------------------------------------------------------------
// blocklist
// ---------------------------------------------------------------------------

impl Platform {
    pub fn block_user(&self, user: &str, target: &str) -> Result<()> {
        let (user, target) = (validate::id(user)?, validate::id(target)?);
        if user == target {
            return Err(PlatformError::BadInput("cannot block yourself"));
        }
        let exists: Option<String> = self
            .db()
            .lock()
            .query_row("SELECT id FROM users WHERE id = ?1", params![target], |r| {
                r.get(0)
            })
            .optional()?;
        if exists.is_none() {
            return Err(PlatformError::NotFound);
        }
        self.db().lock().execute(
            "INSERT OR IGNORE INTO blocks(user_id, blocked_id, created_at) VALUES(?1, ?2, ?3)",
            params![user, target, wxlike_core::types::now_ms() as i64],
        )?;
        Ok(())
    }

    pub fn unblock_user(&self, user: &str, target: &str) -> Result<()> {
        let (user, target) = (validate::id(user)?, validate::id(target)?);
        let changed = self.db().lock().execute(
            "DELETE FROM blocks WHERE user_id = ?1 AND blocked_id = ?2",
            params![user, target],
        )?;
        crate::db::ensure_updated(changed)?;
        Ok(())
    }

    pub fn blocked_list(&self, user: &str) -> Result<Vec<String>> {
        let c = self.db().lock();
        let mut stmt =
            c.prepare("SELECT blocked_id FROM blocks WHERE user_id = ?1 ORDER BY created_at")?;
        let rows = stmt.query_map(params![user], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// 拉黑判定（好友申请闸门 + 陌生人请求闸门 + P3-8 hub 收件过滤复用）。
    pub fn is_blocked(&self, user: &str, by: &str) -> Result<bool> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT 1 FROM blocks WHERE user_id = ?1 AND blocked_id = ?2",
            params![by, user],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
    }
}

// ---------------------------------------------------------------------------
// 陌生人消息请求箱（ADR-24 / 不变式 9）
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct StrangerRequest {
    pub id: String,
    pub from_uid: String,
    pub to_uid: String,
    pub text: String,
    pub status: String,
    pub created_at: i64,
}

const MAX_STRANGER_TEXT: usize = 500;

/// 免打扰级别合法值（通知规则与陌生人开关共用校验面）。
pub const MUTE_LEVELS: [&str; 3] = ["all", "mention", "none"];

impl Platform {
    /// 陌生人首条消息：非好友、未拉黑、对方允许 → 请求箱。已好友直接放行
    /// （调用方返回 Accepted 语义）；重复 pending 请求幂等更新文本。
    pub fn send_stranger_message(
        &self,
        from: &str,
        to: &str,
        text: &str,
    ) -> std::result::Result<StrangerState, PlatformError> {
        let (from, to) = (validate::id(from)?, validate::id(to)?);
        if from == to {
            return Err(PlatformError::BadInput("cannot message yourself"));
        }
        if text.len() > MAX_STRANGER_TEXT {
            return Err(PlatformError::BadInput("text too long"));
        }
        if self.are_friends(from, to)? {
            return Ok(StrangerState::AlreadyFriends);
        }
        if self.is_blocked(from, to)? {
            return Err(PlatformError::Forbidden);
        }
        if !self.allow_strangers(to)? {
            return Err(PlatformError::Forbidden);
        }
        let c = self.db().lock();
        // 幂等：同 from→to 已有 pending 则更新文本与时间
        c.execute(
            "INSERT INTO message_requests(id, from_uid, to_uid, text, status, created_at)
             VALUES(?1, ?2, ?3, ?4, 'pending', ?5)
             ON CONFLICT(from_uid, to_uid, status) DO UPDATE SET
               text = excluded.text, created_at = excluded.created_at
             WHERE status = 'pending'",
            params![
                crate::auth::random_hex(8),
                from,
                to,
                text,
                wxlike_core::types::now_ms() as i64
            ],
        )?;
        Ok(StrangerState::Queued)
    }

    pub fn stranger_requests_in(&self, user: &str) -> Result<Vec<StrangerRequest>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT id, from_uid, to_uid, text, status, created_at FROM message_requests
             WHERE to_uid = ?1 AND status = 'pending' ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![user], row_request)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn stranger_requests_out(&self, user: &str) -> Result<Vec<StrangerRequest>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT id, from_uid, to_uid, text, status, created_at FROM message_requests
             WHERE from_uid = ?1 ORDER BY created_at DESC LIMIT 50",
        )?;
        let rows = stmt.query_map(params![user], row_request)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// 接受：状态置 accepted，发送方从此可直发（会话许可，不是好友）。
    pub fn accept_stranger_request(&self, user: &str, id: &str) -> Result<()> {
        self.decide_stranger_request(user, id, "accepted")
    }

    /// 拒绝：状态置 declined（该请求关闭；不等于拉黑，可再申请新请求）。
    pub fn decline_stranger_request(&self, user: &str, id: &str) -> Result<()> {
        self.decide_stranger_request(user, id, "declined")
    }

    fn decide_stranger_request(&self, user: &str, id: &str, status: &str) -> Result<()> {
        let id = validate::id(id)?;
        let changed = self.db().lock().execute(
            "UPDATE message_requests SET status = ?1
             WHERE id = ?2 AND to_uid = ?3 AND status = 'pending'",
            params![status, id, user],
        )?;
        crate::db::ensure_updated(changed)?;
        Ok(())
    }

    /// 直发许可：好友，或曾接受过来自 `from` 的请求（接受→正常会话语义）。
    pub fn stranger_accepted(&self, from: &str, to: &str) -> Result<bool> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT 1 FROM message_requests
             WHERE from_uid = ?1 AND to_uid = ?2 AND status = 'accepted'",
            params![from, to],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
    }

    // ---- 用户设置（是否允许陌生人发起） ----

    pub fn allow_strangers(&self, user: &str) -> Result<bool> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT allow_strangers FROM user_settings WHERE user_id = ?1",
            params![user],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .map(|v| v != 0)
        .unwrap_or(true))
    }

    pub fn set_allow_strangers(&self, user: &str, allow: bool) -> Result<()> {
        self.db().lock().execute(
            "INSERT INTO user_settings(user_id, allow_strangers) VALUES(?1, ?2)
             ON CONFLICT(user_id) DO UPDATE SET allow_strangers = excluded.allow_strangers",
            params![user, allow as i64],
        )?;
        Ok(())
    }

    // ---- 星标 / 标签 / 分组（friends 表的成员私有元数据） ----

    pub fn patch_friend_meta(
        &self,
        user: &str,
        friend: &str,
        starred: Option<bool>,
        tags: Option<Vec<String>>,
        group: Option<String>,
    ) -> Result<()> {
        let friend = validate::id(friend)?;
        if tags.as_ref().is_some_and(|t| t.len() > 16) {
            return Err(PlatformError::BadInput("too many tags"));
        }
        if group.as_ref().is_some_and(|g| g.chars().count() > 32) {
            return Err(PlatformError::BadInput("group name too long"));
        }
        let tags_json = tags.as_ref().map(serde_json::to_string).transpose()?;
        let c = self.db().lock();
        let changed = c.execute(
            "UPDATE friends SET
               starred = COALESCE(?3, starred),
               tags = COALESCE(?4, tags),
               friend_group = COALESCE(?5, friend_group)
             WHERE user_id = ?1 AND friend_id = ?2",
            params![user, friend, starred.map(|s| s as i64), tags_json, group],
        )?;
        crate::db::ensure_updated(changed)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrangerState {
    Queued,
    AlreadyFriends,
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// 跨域只读闸门（G02-02）：聊天/搜索不许自己查 friends / blocks / group_members
// ---------------------------------------------------------------------------

impl Platform {
    /// 这条消息能不能发。`Blocked` 不告诉调用方是哪一侧拉黑的。
    pub fn may_message(&self, from: &str, to: &str) -> Result<wxlike_relations::port::MessageGate> {
        let c = self.db().lock();
        let port = wxlike_relations::port::SqlRelations { conn: &c };
        Ok(port.may_message(from, to)?)
    }

    /// 两人共同的群 id（交集查询，非成员拿不到任何行）。
    pub fn shared_group_ids(&self, a: &str, b: &str) -> Result<Vec<String>> {
        let c = self.db().lock();
        let port = wxlike_relations::port::SqlRelations { conn: &c };
        Ok(port.shared_groups(a, b)?)
    }
}
