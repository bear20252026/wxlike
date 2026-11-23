//! `Platform` 门面试点：先从 Account 的会话能力切出一个窄 trait。
//!
//! 这里不是“把 107 个 pub 方法一次搬进 8 个 trait”，那会把一个可评审的
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
}

impl AccountSessions for Platform {
    fn lookup_token(&self, token: &str) -> Result<Option<String>> {
        Platform::lookup_token(self, token)
    }

    fn revoke_session(&self, token: &str) -> Result<()> {
        Platform::revoke_session(self, token)
    }
}
