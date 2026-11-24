//! `Platform` 门面试点：先从 Account 的会话能力切出一个窄 trait。
//!
//! 这里不是“把 102 个 pub 方法一次搬进 8 个 trait”，那会把一个可评审的
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
