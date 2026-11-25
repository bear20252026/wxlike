//! Bearer -> actor 的 HTTP 会话端口。
//!
//! 组合根（G02）注入真实实现，各 feature 的 HTTP 投影只要求
//! "bearer 头 -> 受信 actor id"，不自行解析令牌。这个端口原先在
//! `wxlike-media` 与 `wxlike-service-catalog` 各定义一份，连
//! `FnVerifier` 的 blanket impl 都逐字重复；放这里后两端共享同一形状，
//! 注入方也只实现一次。
//!
//! 这里只依赖 `axum::http::HeaderMap`，不引入任何业务类型。

use axum::http::HeaderMap;

/// 会话验证端口：从请求头取出受信 actor id。
pub trait SessionVerifier: Send + Sync {
    fn verify(&self, headers: &HeaderMap) -> Option<String>;
}

/// 用闭包实现 [`SessionVerifier`]，测试与轻量组合根用。
pub struct FnVerifier<F>(pub F);

impl<F> SessionVerifier for FnVerifier<F>
where
    F: Fn(&HeaderMap) -> Option<String> + Send + Sync,
{
    fn verify(&self, headers: &HeaderMap) -> Option<String> {
        (self.0)(headers)
    }
}
