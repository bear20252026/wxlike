//! 长连接的**身份来源**：`Hello` 只带凭据，不带 `user_id`。
//!
//! # 为什么需要这一层
//!
//! 之前的 `WireFrame::Hello { user_id }` 让客户端**自报身份**：hub 收到就直接
//! 注册 peers、绑路由、`offline.drain(user_id)`。而 `user_id` 是 16 位 hex，
//! 出现在每条消息的 `sender` 字段里，不是秘密。知道它的人可以：
//!
//! 1. 冒充该用户接收实时消息；
//! 2. **取走并销毁**离线队列（`drain` 是删除式的，真正的收件人再连就空了）；
//! 3. 以该身份提交消息。
//!
//! HTTP 的 Bearer、密码、TOTP 都保护不了这条绕行路径——它根本不经过 axum。
//! 这是 issue #3（P0）。
//!
//! # 设计
//!
//! `wxlike-net` 不认识 session 表，也不该认识（它要能在客户端、hub、测试里
//! 复用）。所以这里只定义**一个函数式接口**，由装配层注入：
//!
//! ```ignore
//! // server/src/lib.rs
//! struct SessionAuth { platform: Arc<Platform> }
//! impl HubAuthenticator for SessionAuth {
//!     fn authenticate(&self, token: &str) -> Option<HubIdentity> {
//!         self.platform.lookup_token(token).ok().flatten().map(|user_id| HubIdentity {
//!             user_id,
//!             session_id: token.to_string(),
//!         })
//!     }
//! }
//! ```
//!
//! # fail closed
//!
//! [`MqHubOptions::auth`](crate::MqHubOptions::auth) 是 `Option`，但 **`None` 表示
//! "拒绝所有连接"，不是"不校验"**。没有认证器的 hub 是一个任何人都能冒充的
//! 服务，让它默默放行比让它拒绝更危险。测试与本地开发用 [`StaticAuth`]。

use std::sync::Arc;

/// 认证成功后得到的身份。**只有服务端能产出它**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubIdentity {
    /// 服务端从凭据解析出的用户 id。
    pub user_id: String,
    /// 这次连接所属的会话。HTTP 侧吊销它，长连接也应当随之失效
    /// （见 [`HubAuthenticator::is_revoked`]）。
    pub session_id: String,
}

/// 把凭据换成身份。实现方通常是装配层（`wxlike-server`）对 session 表的一层包装。
pub trait HubAuthenticator: Send + Sync {
    /// `None` = 拒绝：凭据缺失、无效、过期，或已被吊销。
    fn authenticate(&self, token: &str) -> Option<HubIdentity>;

    /// 长连接存续期间复查凭据是否仍然有效。
    ///
    /// 参数是**原始凭据**（hub 在内存里留着，不发给客户端），不是
    /// [`HubIdentity::session_id`]——后者是指纹，换不回 session 行。
    ///
    /// 默认 `false`（不主动踢）。实现了它，hub 就能在心跳时把"HTTP 已退出登录
    /// 但 QUIC 还连着"的连接关掉；否则吊销只对下一次 HTTP 请求生效，
    /// 而长连接可以一直收消息。
    fn is_revoked(&self, _token: &str) -> bool {
        false
    }
}

/// **仅供测试与本地开发**：把 token 原样当作 `user_id`。
///
/// 名字里带 `Static` 是故意的——在 `server/src/lib.rs` 里看到它就该警觉。
/// 生产装配必须用基于 session 表的实现（见模块文档）。
pub struct StaticAuth;

impl HubAuthenticator for StaticAuth {
    fn authenticate(&self, token: &str) -> Option<HubIdentity> {
        if token.is_empty() {
            return None;
        }
        Some(HubIdentity {
            user_id: token.to_string(),
            session_id: token.to_string(),
        })
    }
}

/// 拒绝一切。用于"这个 hub 不该被外部连"的场景（比如只跑内部 MQ 的节点）。
pub struct DenyAll;

impl HubAuthenticator for DenyAll {
    fn authenticate(&self, _token: &str) -> Option<HubIdentity> {
        None
    }
}

/// 便于装配层把闭包直接当认证器用（测试里尤其省事）。
impl<F> HubAuthenticator for F
where
    F: Fn(&str) -> Option<HubIdentity> + Send + Sync,
{
    fn authenticate(&self, token: &str) -> Option<HubIdentity> {
        self(token)
    }
}

/// 把 `Arc<Platform>` 这类东西包成 `Arc<dyn HubAuthenticator>` 的便捷函数。
pub fn shared<A: HubAuthenticator + 'static>(auth: A) -> Arc<dyn HubAuthenticator> {
    Arc::new(auth)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_auth_rejects_empty_token() {
        assert!(StaticAuth.authenticate("").is_none());
        assert_eq!(
            StaticAuth.authenticate("u1").map(|i| i.user_id),
            Some("u1".to_string())
        );
    }

    #[test]
    fn deny_all_rejects_everything() {
        assert!(DenyAll.authenticate("u1").is_none());
        assert!(DenyAll.authenticate("").is_none());
    }

    #[test]
    fn closures_are_authenticators() {
        let auth = |token: &str| {
            (token == "good").then(|| HubIdentity {
                user_id: "u1".into(),
                session_id: "s1".into(),
            })
        };
        assert!(auth.authenticate("good").is_some());
        assert!(auth.authenticate("bad").is_none());
    }
}
