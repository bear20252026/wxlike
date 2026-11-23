//! P3 群管理补齐（合同 §3.3）：公告、禁言（全员/单人/定时解除）、
//! 入群审批 + 邀请 token（带过期）、@所有人（owner/admin 限定）。
//!
//! 与 groups.rs 的关系：同一个 `Platform` 的扩展 impl（模块化单体的 crate
//! 内分文件），路由独立注册。禁言的**发送侧拦截**由 hub 接线复用
//! [`Platform::is_muted`]（P3-8）；此处先落存储面与 HTTP 面。
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::groups_admin`]。

use crate::error::{PlatformError, Result};
use crate::validate;
use crate::Platform;
use serde::{Deserialize, Serialize};

use super::repo::groups::{
    group_exists as repo_group_exists, members as repo_members, role_of as repo_role_of,
};
use super::repo::groups_admin::{
    all_muted, announcement, delete_member_mute, insert_invite, insert_member_direct, invite,
    join_approval, join_requests, member_count, mute_until, set_all_muted, set_join_approval,
    set_join_request_status, upsert_announcement, upsert_join_request, upsert_member_mute,
    user_exists,
};

// ---------------------------------------------------------------------------
// 公告
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct GroupAnnouncement {
    pub text: String,
    pub media_id: String,
    pub pinned_by: String,
    pub updated_at: i64,
}

impl Platform {
    pub fn set_announcement(
        &self,
        group: &str,
        actor: &str,
        text: &str,
        media_id: &str,
    ) -> Result<()> {
        let group = validate::id(group)?;
        self.assert_manager(group, actor)?;
        if text.chars().count() > 2_000 {
            return Err(PlatformError::BadInput("announcement too long"));
        }
        upsert_announcement(
            &self.db().lock(),
            group,
            text,
            media_id,
            actor,
            wxlike_core::types::now_ms() as i64,
        )?;
        Ok(())
    }

    pub fn announcement(&self, group: &str) -> Result<Option<GroupAnnouncement>> {
        let group = validate::id(group)?;
        Ok(
            announcement(&self.db().lock(), group)?.map(|r| GroupAnnouncement {
                text: r.text,
                media_id: r.media_id,
                pinned_by: r.pinned_by,
                updated_at: r.updated_at,
            }),
        )
    }
}

// ---------------------------------------------------------------------------
// 禁言（全员 + 单人定时）
// ---------------------------------------------------------------------------

impl Platform {
    pub fn set_member_mute(
        &self,
        group: &str,
        actor: &str,
        target: &str,
        until_ms: u64,
    ) -> Result<()> {
        let (group, target) = (validate::id(group)?, validate::id(target)?);
        self.assert_manager(group, actor)?;
        if !self.is_member(group, target)? {
            return Err(PlatformError::NotFound);
        }
        let role = self.role_of(group, target)?;
        if role == "owner" {
            return Err(PlatformError::BadInput("the owner cannot be muted"));
        }
        let c = self.db().lock();
        upsert_member_mute(&c, group, target, until_ms as i64)?;
        Ok(())
    }

    pub fn clear_member_mute(&self, group: &str, actor: &str, target: &str) -> Result<()> {
        let (group, target) = (validate::id(group)?, validate::id(target)?);
        self.assert_manager(group, actor)?;
        if !delete_member_mute(&self.db().lock(), group, target)? {
            return Err(PlatformError::NotFound);
        }
        Ok(())
    }

    pub fn set_all_mute(&self, group: &str, actor: &str, on: bool) -> Result<()> {
        let group = validate::id(group)?;
        self.assert_manager(group, actor)?;
        set_all_muted(&self.db().lock(), group, on)
    }

    /// 全员禁言的当前状态（#318 遗留缺口的服务端一半）：写入侧只有
    /// `set_all_mute`，读取侧此前只有发送裁决内部查得到，命令面与
    /// 客户端都无从读——这里给一个与公告读取对称的域方法。
    pub fn all_muted(&self, group: &str) -> Result<bool> {
        let group = validate::id(group)?;
        all_muted(&self.db().lock(), group)
    }

    /// 发送侧裁决（hub 接线复用）：全员禁言对 owner/admin 无效；单人禁言
    /// 按 until_ms 定时解除（到点自然放行，无需 sweeper）。
    pub fn is_muted(&self, group: &str, user: &str) -> Result<bool> {
        let group = validate::id(group)?;
        let role = self.role_of(group, user)?;
        if role == "owner" || role == "admin" {
            return Ok(false);
        }
        if self.all_muted(group)? {
            return Ok(true);
        }
        let until = mute_until(&self.db().lock(), group, user)?;
        Ok(until
            .map(|u| u > wxlike_core::types::now_ms() as i64)
            .unwrap_or(false))
    }
}

// ---------------------------------------------------------------------------
// 邀请 token + 入群审批
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct GroupInvite {
    pub token: String,
    pub group_id: String,
    pub created_by: String,
    pub expires_at: i64,
}

impl Platform {
    /// token/审批路径的成员直入：与 add_members 同一套不变量（存在性、上限、
    /// 缓存失效），但**不做 actor 管理员断言**——授权来自有效邀请/批准本身。
    fn add_member_direct(&self, group: &str, user: &str) -> Result<()> {
        let (group, user) = (validate::id(group)?, validate::id(user)?);
        if !self.group_exists(group)? {
            return Err(PlatformError::NotFound);
        }
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        if member_count(&c, group)? as usize >= crate::domain::groups::MAX_GROUP_MEMBERS {
            return Err(PlatformError::BadInput("group member limit exceeded"));
        }
        if !user_exists(&c, user)? {
            return Err(PlatformError::NotFound);
        }
        insert_member_direct(&c, group, user, crate::domain::groups::ROLE_MEMBER, now)?;
        drop(c);
        self.invalidate_group(group);
        Ok(())
    }

    /// 审批开关（owner/admin 切换；审批开的群 token 入群转请求）。
    pub fn set_join_approval(&self, group: &str, actor: &str, on: bool) -> Result<()> {
        let group = validate::id(group)?;
        self.assert_manager(group, actor)?;
        set_join_approval(&self.db().lock(), group, on)
    }

    pub fn create_invite(&self, group: &str, actor: &str, ttl_ms: u64) -> Result<GroupInvite> {
        let group = validate::id(group)?;
        self.assert_manager(group, actor)?;
        let token = crate::auth::random_hex(16);
        let expires = wxlike_core::types::now_ms() as i64 + ttl_ms as i64;
        insert_invite(&self.db().lock(), &token, group, actor, expires)?;
        Ok(GroupInvite {
            token,
            group_id: group.to_string(),
            created_by: actor.to_string(),
            expires_at: expires,
        })
    }

    /// token 入群：审批开 → pending 请求；审批关 → 直接入群。
    pub fn join_by_token(&self, token: &str, user: &str) -> Result<&'static str> {
        let token = validate::id(token)?;
        let user = validate::id(user)?;
        // scope the guard: is_member/add_members re-lock the same non-reentrant
        // Mutex — holding it across those calls deadlocked the caller's thread
        let row = {
            let c = self.db().lock();
            invite(&c, token)?
        };
        let Some(invite) = row else {
            return Err(PlatformError::NotFound);
        };
        let (group, expires_at) = (invite.group_id, invite.expires_at);
        if expires_at <= wxlike_core::types::now_ms() as i64 {
            return Err(PlatformError::BadInput("invite expired"));
        }
        if self.is_member(&group, user)? {
            return Ok("already_member");
        }
        let approval = join_approval(&self.db().lock(), &group)?;
        if approval == 0 {
            self.add_member_direct(&group, user)?;
            Ok("joined")
        } else {
            upsert_join_request(
                &self.db().lock(),
                &group,
                user,
                wxlike_core::types::now_ms() as i64,
            )?;
            Ok("pending")
        }
    }

    pub fn join_requests(&self, group: &str, actor: &str) -> Result<Vec<(String, String)>> {
        let group = validate::id(group)?;
        self.assert_manager(group, actor)?;
        Ok(join_requests(&self.db().lock(), group)?
            .into_iter()
            .map(|r| (r.user_id, r.requested_at.to_string()))
            .collect())
    }

    pub fn decide_join_request(
        &self,
        group: &str,
        actor: &str,
        user: &str,
        approve: bool,
    ) -> Result<()> {
        let (group, user) = (validate::id(group)?, validate::id(user)?);
        self.assert_manager(group, actor)?;
        if approve {
            self.add_member_direct(group, user)?;
        }
        let status = if approve { "approved" } else { "rejected" };
        if !set_join_request_status(&self.db().lock(), group, user, status)? {
            return Err(PlatformError::NotFound);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// @所有人（owner/admin 限定）
// ---------------------------------------------------------------------------

impl Platform {
    pub fn can_mention_all(&self, group: &str, user: &str) -> Result<bool> {
        let role = self.role_of(group, user)?;
        Ok(role == "owner" || role == "admin")
    }

    /// @所有人：manager 专属。构造 mention Custom 帧并经 EventSink 扇给全体
    /// 成员（未指定 user_ids 语义 = all），帧内带 all 标记。
    pub async fn mention_all(&self, group: &str, actor: &str, text: &str) -> Result<String> {
        let group = validate::id(group)?.to_string();
        let group_for_task = group.clone();
        let actor_for_task = actor.to_string();
        self.db()
            .blocking(move |c| {
                let role = repo_role_of(c, &group_for_task, &actor_for_task)?
                    .ok_or(PlatformError::Forbidden)?;
                if role == "owner" || role == "admin" {
                    Ok(())
                } else {
                    Err(PlatformError::Forbidden)
                }
            })
            .await?;
        if text.is_empty() || text.chars().count() > 2_000 {
            return Err(PlatformError::BadInput("text required"));
        }
        let group_for_task = group.clone();
        let cache = std::sync::Arc::clone(&self.group_cache);
        let members = self
            .db()
            .blocking(move |c| {
                if !repo_group_exists(c, &group_for_task)? {
                    return Ok(Vec::new());
                }
                let mut guard = cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(hit) = guard.get(&group_for_task) {
                    return Ok(hit.clone());
                }
                let members: Vec<String> = repo_members(c, &group_for_task)?
                    .into_iter()
                    .map(|m| m.user_id)
                    .collect();
                guard.insert(group_for_task, members.clone());
                Ok(members)
            })
            .await?;
        let mut env = crate::system_envelope(&group, actor, members.clone(), "");
        env.body = wxlike_core::MsgBody::Custom {
            kind: "mention".into(),
            payload: serde_json::json!({ "text": text, "all": true, "user_ids": members }),
        };
        self.emit(env).await;
        Ok(text.to_string())
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------
