//! AR-5（第四轮审计 #29）：写接口风控的**声明式**闸门。
//!
//! 此前风控只在 Login/Register/Reset/Report 四处由 handler 手工调用，
//! 好友请求、媒体上传、朋友圈、HTTP 发送全部裸奔——漏掉是"每个 handler
//! 自己记得调闸门"这一模式的结构性必然，不是某次疏忽。本模块把闸门从
//! handler 里拿出来：路由表声明哪个 (method, path) 属于哪个
//! [`wxlike_risk::Action`]，中间件统一计数、统一落 429。
//!
//! 语义约定（与手工调用点对齐）：
//! * 身份只认 `Authorization: Bearer`（与 `Authed` 同一来源）；未认证或
//!   令牌失效的请求**不进闸门**——handler 会给 401，认证失败不该吃掉
//!   写配额，也不该把风控键污染成空串；
//! * 计数口径是"处理过的请求"：2xx/4xx 都算（4xx 同样消耗了校验与
//!   写入尝试，轰炸者用 409 响应刷库也是攻击面），5xx 不算（服务端故障
//!   不惩罚客户端）。这与认领闸门（H-4，只计成功）刻意不同——认领是
//!   定向稀缺资源，这里是反轰炸；
//! * Challenge 不在本层产生：写接口没有 captcha 兑现面，Throttle/Deny
//!   都落到 429（`RateLimited`）。

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, Method, Request};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};

use crate::auth::Authed;
use crate::error::ApiError;
use crate::{PlatformState, Result as PlatformResult};

/// (method, path) → 受控行为。`None` = 不归这层闸管（读接口、钱包、
/// 预密钥认领等已有各自闸门的路径）。
fn action_for(method: &Method, path: &str) -> Option<wxlike_risk::Action> {
    use wxlike_risk::Action;
    match (method, path) {
        (&Method::POST, "/api/friends/requests") => Some(Action::FriendRequest),
        (&Method::POST, "/api/media") => Some(Action::MediaUpload),
        (&Method::POST, "/api/moments") => Some(Action::MomentCreate),
        // 点赞幂等，但刷赞同样消耗写入；评论同属朋友圈面
        (&Method::POST, p)
            if p.starts_with("/api/moments/")
                && (p.ends_with("/comments") || p.ends_with("/likes")) =>
        {
            Some(Action::MomentCreate)
        }
        // HTTP 发送面（hub 的 QUIC/WS 路径另有 send_gate 在投递时把关）
        (&Method::POST, p) if p.starts_with("/api/conv/") && p.ends_with("/send") => {
            Some(Action::Send)
        }
        _ => None,
    }
}

fn bearer_of(headers: &axum::http::HeaderMap) -> Option<String> {
    Authed::bearer_from(headers).map(str::to_string)
}

/// 统一写接口闸门。挂在 [`crate::Platform::router`] 最外层。
pub async fn gate(
    State(platform): State<PlatformState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Some(action) = action_for(request.method(), request.uri().path()) else {
        return next.run(request).await;
    };
    let Some(token) = bearer_of(request.headers()) else {
        return next.run(request).await;
    };
    let user_id = match platform.lookup_token(&token) {
        Ok(Some(user_id)) => user_id,
        _ => return next.run(request).await,
    };
    // 检查本身不计入（extra=1 只影响本次裁决）；被限流的请求不落 handler
    let gate: PlatformResult<()> = platform.risk_gate(action, &user_id, None);
    if let Err(err) = gate {
        return ApiError::from(err).into_response();
    }
    let response = next.run(request).await;
    if !response.status().is_server_error() {
        // 计数失败不影响响应：风控是增强，不是可用性单点
        let _ = platform.risk_record(action, &user_id);
    }
    response
}

/// 供测试与文档核对映射表：路径归属必须与 `routes()` 的注册保持一致，
/// 漂移等于静默失去闸门。
pub fn mapped_actions() -> &'static [(&'static str, &'static str, &'static str)] {
    &[
        ("POST", "/api/friends/requests", "friend_request"),
        ("POST", "/api/media", "media_upload"),
        ("POST", "/api/moments", "moment_create"),
        ("POST", "/api/moments/{id}/comments", "moment_create"),
        ("POST", "/api/moments/{id}/likes", "moment_create"),
        ("POST", "/api/conv/{conv}/send", "send"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Method;

    #[test]
    fn mapping_covers_the_documented_write_surfaces() {
        assert_eq!(
            action_for(&Method::POST, "/api/friends/requests"),
            Some(wxlike_risk::Action::FriendRequest)
        );
        assert_eq!(
            action_for(&Method::POST, "/api/media"),
            Some(wxlike_risk::Action::MediaUpload)
        );
        assert_eq!(
            action_for(&Method::POST, "/api/moments"),
            Some(wxlike_risk::Action::MomentCreate)
        );
        assert_eq!(
            action_for(&Method::POST, "/api/moments/m1/likes"),
            Some(wxlike_risk::Action::MomentCreate)
        );
        assert_eq!(
            action_for(&Method::POST, "/api/moments/m1/comments"),
            Some(wxlike_risk::Action::MomentCreate)
        );
        assert_eq!(
            action_for(&Method::POST, "/api/conv/d-a~b/send"),
            Some(wxlike_risk::Action::Send)
        );
        // 读接口与已有独立闸门的路径不重复计数
        assert_eq!(action_for(&Method::GET, "/api/moments/feed"), None);
        assert_eq!(action_for(&Method::GET, "/api/keys/u1"), None);
        assert_eq!(action_for(&Method::POST, "/api/conv/d-a~b/recall"), None);
        assert_eq!(action_for(&Method::DELETE, "/api/media/x"), None);
    }

    #[test]
    fn mapped_actions_table_matches_action_for() {
        for (method, path, action) in mapped_actions() {
            let m = Method::from_bytes(method.as_bytes()).unwrap();
            let concrete = path.replace("{id}", "x1").replace("{conv}", "d-a~b");
            let got = action_for(&m, &concrete).map(|a| a.as_str().to_string());
            assert_eq!(
                got.as_deref(),
                Some(action),
                "{method} {path} declared {action} but maps to {got:?}"
            );
        }
    }
}
