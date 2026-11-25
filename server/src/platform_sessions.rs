//! 生产用的会话校验端口：media 与 service-catalog 两个挂载点共用一份。

use axum::http::HeaderMap;
use wxlike_media::http::SessionVerifier;
use wxlike_platform::{AccountSessions, Authed, PlatformState};

/// 包着平台的 `sessions` 表，没有任何“临时放行”分支。
pub struct PlatformSessions(PlatformState);

impl PlatformSessions {
    pub fn new(platform: PlatformState) -> Self {
        Self(platform)
    }

    fn verify_platform(&self, headers: &HeaderMap) -> Option<String> {
        let token = Authed::bearer_from(headers)?;
        match AccountSessions::lookup_token(self.0.as_ref(), token) {
            Ok(Some(user_id)) => Some(user_id),
            Ok(None) => None,
            Err(err) => {
                tracing::error!(error = %err, "lookup_token failed, rejecting");
                None
            }
        }
    }
}

impl SessionVerifier for PlatformSessions {
    fn verify(&self, headers: &HeaderMap) -> Option<String> {
        self.verify_platform(headers)
    }
}
