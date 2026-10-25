use crate::error::{Result, ServiceError, ServiceErrorCode as Code};

/// 服务端再次鉴权：所有对外命令先换取可信 actor，绝不直接相信请求里的用户 id。
pub trait Authenticator: Send + Sync {
    fn authenticate(&self, session_token: &str) -> Result<String>;
}

const MAX_SESSION_TOKEN_LEN: usize = 512;

pub struct BearerAuthenticator<F> {
    lookup: F,
}

impl<F> BearerAuthenticator<F>
where
    F: Fn(&str) -> Option<String> + Send + Sync,
{
    pub fn new(lookup: F) -> Self {
        Self { lookup }
    }
}

impl<F> Authenticator for BearerAuthenticator<F>
where
    F: Fn(&str) -> Option<String> + Send + Sync,
{
    fn authenticate(&self, session_token: &str) -> Result<String> {
        if session_token.is_empty() || session_token.len() > MAX_SESSION_TOKEN_LEN {
            return Err(ServiceError::new(Code::Unauthorized, "missing session"));
        }
        (self.lookup)(session_token)
            .ok_or_else(|| ServiceError::new(Code::Unauthorized, "session rejected"))
    }
}
