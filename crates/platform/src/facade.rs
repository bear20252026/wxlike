//! `Platform` 门面试点：先从 Account 的会话能力切出一个窄 trait。
//!
//! 这里不是“把 101 个 pub 方法一次搬进 8 个 trait”，那会把一个可评审的
//! 接口隔离动作变成大规模改签名。试点只做一条真实调用链：
//! `lookup_token` 是 HTTP/WS/媒体面共用的身份入口，`revoke_session` 是退出登录；
//! 两者都归 Account，但不需要调用方认识 `Platform` 的其余 100 多个方法。
//!
//! 约定：
//! - trait 方法必须对应一个真实跨 crate 调用点；没有调用点的“门面”不建。
//! - 实现只转调 `Platform` 的 `pub(crate)` 方法，不复制业务逻辑。
//! - 新方法先加到这里，再把 `Platform` 的同名方法收成 `pub(crate)`；调用方
//!   通过 `use wxlike_platform::AccountSessions` 拿到接口。

use crate::error::Result;
use crate::Platform;

/// Account 门面的会话切片：凭据换身份、退出登录。
pub trait AccountSessions {
    /// 凭据 → 用户 id。`None` 表示凭据无效、过期或已吊销。
    fn lookup_token(&self, token: &str) -> Result<Option<String>>;

    /// 吊销一条会话（按原始 token）。
    fn revoke_session(&self, token: &str) -> Result<()>;

    /// 列出该用户的会话；`current_token` 标出“当前会话”。
    fn sessions_of(&self, user_id: &str, current_token: &str) -> Result<Vec<crate::SessionRow>>;

    /// 按 `sid` 吊销一条会话；`false` = 本来就不存在/已吊销。
    fn revoke_session_by_sid(&self, user_id: &str, sid: &str) -> Result<bool>;

    /// 吊销除当前 token 外的全部会话，返回吊销条数。
    fn revoke_other_sessions(&self, user_id: &str, keep_token: &str) -> Result<usize>;
}

impl AccountSessions for Platform {
    fn lookup_token(&self, token: &str) -> Result<Option<String>> {
        Platform::lookup_token(self, token)
    }

    fn revoke_session(&self, token: &str) -> Result<()> {
        Platform::revoke_session(self, token)
    }

    fn sessions_of(&self, user_id: &str, current_token: &str) -> Result<Vec<crate::SessionRow>> {
        Platform::sessions_of(self, user_id, current_token)
    }

    fn revoke_session_by_sid(&self, user_id: &str, sid: &str) -> Result<bool> {
        Platform::revoke_session_by_sid(self, user_id, sid)
    }

    fn revoke_other_sessions(&self, user_id: &str, keep_token: &str) -> Result<usize> {
        Platform::revoke_other_sessions(self, user_id, keep_token)
    }
}

/// Governance 门面的管理面鉴权切片：管理令牌是否有效。
pub trait AdminAccess {
    fn admin_ok(&self, presented: Option<&str>) -> bool;
}

impl AdminAccess for Platform {
    fn admin_ok(&self, presented: Option<&str>) -> bool {
        Platform::admin_ok(self, presented)
    }
}

/// Messaging 门面的会话读权限切片：消息落库前的最后一道授权。
pub trait ConversationAccess {
    /// 该用户能否读这个会话（直聊参与者 / 群成员 / 频道可见性）。
    fn may_read_conv(&self, user_id: &str, conv_id: &str) -> bool;

    /// 这个会话是否开启秘密聊天（决定字段脱敏与密钥协商路径）。
    fn is_secret_conv(&self, conv_id: &str) -> Result<bool>;
}

impl ConversationAccess for Platform {
    fn may_read_conv(&self, user_id: &str, conv_id: &str) -> bool {
        Platform::may_read_conv(self, user_id, conv_id)
    }

    fn is_secret_conv(&self, conv_id: &str) -> Result<bool> {
        Platform::is_secret_conv(self, conv_id)
    }
}

/// Social 门面的群成员切片：给媒体面做"我属于哪些群"的事实查询。
pub trait GroupMembership {
    fn groups_of_user(&self, user_id: &str) -> Result<Vec<String>>;
}

impl GroupMembership for Platform {
    fn groups_of_user(&self, user_id: &str) -> Result<Vec<String>> {
        Platform::groups_of_user(self, user_id)
    }
}

/// Account 门面的设备目录切片：推送面要知道"这个人有哪些设备"。
pub trait AccountDevices {
    fn devices_of(&self, user_id: &str) -> Result<Vec<crate::DeviceRow>>;
}

impl AccountDevices for Platform {
    fn devices_of(&self, user_id: &str) -> Result<Vec<crate::DeviceRow>> {
        Platform::devices_of(self, user_id)
    }
}

/// Messaging 门面的免打扰切片：hub 决策层查会话级通知规则。
pub trait MessagingPrefs {
    fn notification_rule(&self, user: &str, conv: &str) -> Result<Option<crate::NotificationRule>>;
}

impl MessagingPrefs for Platform {
    fn notification_rule(&self, user: &str, conv: &str) -> Result<Option<crate::NotificationRule>> {
        Platform::notification_rule(self, user, conv)
    }
}

/// Social 门面的关系判定切片：投递策略问"这两个人能不能直发"。
pub trait SocialGraph {
    fn are_friends(&self, a: &str, b: &str) -> Result<bool>;
    fn stranger_accepted(&self, from: &str, to: &str) -> Result<bool>;
    fn is_blocked(&self, user: &str, by: &str) -> Result<bool>;
}

impl SocialGraph for Platform {
    fn are_friends(&self, a: &str, b: &str) -> Result<bool> {
        Platform::are_friends(self, a, b)
    }

    fn stranger_accepted(&self, from: &str, to: &str) -> Result<bool> {
        Platform::stranger_accepted(self, from, to)
    }

    fn is_blocked(&self, user: &str, by: &str) -> Result<bool> {
        Platform::is_blocked(self, user, by)
    }
}

/// Governance 门面的发送闸门切片：秘密会话 TTL 与内容审核裁决。
pub trait GovernanceGate {
    fn secret_state(&self, conv_id: &str) -> Result<crate::SecretState>;
    fn moderate_for_send(
        &self,
        sender: &str,
        conv_id: &str,
        text: &str,
    ) -> Result<crate::SendVerdict>;
}

impl GovernanceGate for Platform {
    fn secret_state(&self, conv_id: &str) -> Result<crate::SecretState> {
        Platform::secret_state(self, conv_id)
    }

    fn moderate_for_send(
        &self,
        sender: &str,
        conv_id: &str,
        text: &str,
    ) -> Result<crate::SendVerdict> {
        Platform::moderate_for_send(self, sender, conv_id, text)
    }
}

/// Social 门面的群禁言切片：投递策略问"这个人在群里被禁言了吗"。
pub trait GroupModeration {
    fn is_muted(&self, group: &str, user: &str) -> Result<bool>;
}

impl GroupModeration for Platform {
    fn is_muted(&self, group: &str, user: &str) -> Result<bool> {
        Platform::is_muted(self, group, user)
    }
}
