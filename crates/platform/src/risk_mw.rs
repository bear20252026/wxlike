//! AR-5（第四轮审计 #29）：写接口风控的**声明式**闸门。
//!
//! 此前风控只在 Login/Register/Reset/Report 四处由 handler 手工调用，
//! 好友请求、媒体上传、朋友圈、HTTP 发送全部裸奔——漏掉是"每个 handler
//! 自己记得调闸门"这一模式的结构性必然，不是某次疏忽。本模块把闸门从
//! handler 里拿出来：路由表声明哪个 (method, path) 属于哪个
//! [`wxlike_risk::Action`]，中间件统一计数、统一落 429。
//!
//! T-B2 把映射补到四个写面：群管理（建群/拉人/踢人）、社区发帖（建社/
//! 建频道/建话题）、开播、通话信令 POST。边界同样重要：直播推流回调
//! （`/api/live/ingest/authorize`）走机器凭证、频道聊天消息属于日常聊天、
//! 通话开房/加入是低频控制动作——这些都不进用户风控闸门。
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
use axum::http::{Method, Request};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};

use crate::auth::Authed;
use crate::error::ApiError;
use crate::{PlatformState, Result as PlatformResult};

/// (method, path) → 受控行为。`None` = 不归这层闸管（读接口、预密钥认领
/// 等已有各自闸门的路径，以及直播推流回调）。
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
        // 审计第 5 轮：钱包资金写（转账 / 发红包）也是写面——轰炸者拿
        // 重试刷转账与红包公告和刷消息是同一类消耗。领取是定向稀缺资源
        // （每人一份、包空即止），不进反轰炸闸门。
        (&Method::POST, "/api/wallet/transfer") => Some(Action::Send),
        (&Method::POST, "/api/wallet/redpacket") => Some(Action::Send),
        // T-B2：群管理（建群 / 拉人 / 踢人）。群公告、静音、邀请链接等
        // 后台操作不在用户风控面内（低频且已有管理员权限闸门）。
        (&Method::POST, "/api/groups") => Some(Action::GroupManage),
        (&Method::POST, p) if p.starts_with("/api/groups/") && p.ends_with("/members") => {
            Some(Action::GroupManage)
        }
        (&Method::DELETE, p) if p.starts_with("/api/groups/") && p.contains("/members/") => {
            Some(Action::GroupManage)
        }
        // T-B2：社区发帖面（建社 / 建频道 / 建话题）。频道聊天消息不在此列：
        // 正常聊天不能按"发帖"配额限流。
        (&Method::POST, "/api/communities") => Some(Action::CommunityPost),
        (&Method::POST, p) if p.starts_with("/api/communities/") && p.ends_with("/channels") => {
            Some(Action::CommunityPost)
        }
        (&Method::POST, p) if p.starts_with("/api/community/") && p.ends_with("/threads") => {
            Some(Action::CommunityPost)
        }
        (&Method::POST, p) if p.starts_with("/api/channels/") && p.ends_with("/threads") => {
            Some(Action::CommunityPost)
        }
        // T-B2：开播。推流回调 `/api/live/ingest/authorize` 是 ingest 节点
        // 的机器凭证校验，**不能**闸（会打断直播）。
        (&Method::POST, p) if p.starts_with("/api/live/") && p.ends_with("/start") => {
            Some(Action::LiveStart)
        }
        // T-B2：通话信令 POST。GET 取信令是读，不进闸。
        (&Method::POST, p) if p.starts_with("/api/call/") && p.ends_with("/signal") => {
            Some(Action::CallSignal)
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

/// 供测试核对映射表：路径归属必须与 `routes()` 的注册保持一致，
/// 漂移等于静默失去闸门。
#[cfg(test)]
fn mapped_actions() -> &'static [(&'static str, &'static str, &'static str)] {
    &[
        ("POST", "/api/friends/requests", "friend_request"),
        ("POST", "/api/media", "media_upload"),
        ("POST", "/api/moments", "moment_create"),
        ("POST", "/api/moments/{id}/comments", "moment_create"),
        ("POST", "/api/moments/{id}/likes", "moment_create"),
        ("POST", "/api/conv/{conv}/send", "send"),
        ("POST", "/api/wallet/transfer", "send"),
        ("POST", "/api/wallet/redpacket", "send"),
        ("POST", "/api/groups", "group_manage"),
        ("POST", "/api/groups/{group_id}/members", "group_manage"),
        (
            "DELETE",
            "/api/groups/{group_id}/members/{user_id}",
            "group_manage",
        ),
        ("POST", "/api/communities", "community_post"),
        ("POST", "/api/communities/{id}/channels", "community_post"),
        ("POST", "/api/community/{id}/threads", "community_post"),
        ("POST", "/api/channels/{id}/threads", "community_post"),
        ("POST", "/api/live/{channel}/start", "live_start"),
        ("POST", "/api/call/{conv}/signal", "call_signal"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Method;

    fn assert_maps(method: Method, path: &str, action: wxlike_risk::Action) {
        assert_eq!(
            action_for(&method, path),
            Some(action),
            "{method} {path} should map"
        );
    }

    fn assert_ungated(method: Method, path: &str) {
        assert_eq!(action_for(&method, path), None, "{method} {path}");
    }

    #[test]
    fn mapping_covers_the_documented_write_surfaces() {
        assert_maps(
            Method::POST,
            "/api/friends/requests",
            wxlike_risk::Action::FriendRequest,
        );
        assert_maps(Method::POST, "/api/media", wxlike_risk::Action::MediaUpload);
        assert_maps(
            Method::POST,
            "/api/moments",
            wxlike_risk::Action::MomentCreate,
        );
        assert_maps(
            Method::POST,
            "/api/moments/m1/likes",
            wxlike_risk::Action::MomentCreate,
        );
        assert_maps(
            Method::POST,
            "/api/moments/m1/comments",
            wxlike_risk::Action::MomentCreate,
        );
        assert_maps(
            Method::POST,
            "/api/conv/d-a~b/send",
            wxlike_risk::Action::Send,
        );

        // T-B2：群管理三入口
        assert_maps(
            Method::POST,
            "/api/groups",
            wxlike_risk::Action::GroupManage,
        );
        assert_maps(
            Method::POST,
            "/api/groups/g1/members",
            wxlike_risk::Action::GroupManage,
        );
        assert_maps(
            Method::DELETE,
            "/api/groups/g1/members/u1",
            wxlike_risk::Action::GroupManage,
        );

        // T-B2：社区发帖面
        assert_maps(
            Method::POST,
            "/api/communities",
            wxlike_risk::Action::CommunityPost,
        );
        assert_maps(
            Method::POST,
            "/api/communities/c1/channels",
            wxlike_risk::Action::CommunityPost,
        );
        assert_maps(
            Method::POST,
            "/api/community/c1/threads",
            wxlike_risk::Action::CommunityPost,
        );
        assert_maps(
            Method::POST,
            "/api/channels/ch1/threads",
            wxlike_risk::Action::CommunityPost,
        );

        // T-B2：开播与通话信令
        assert_maps(
            Method::POST,
            "/api/live/ch1/start",
            wxlike_risk::Action::LiveStart,
        );
        assert_maps(
            Method::POST,
            "/api/call/d-a~b/signal",
            wxlike_risk::Action::CallSignal,
        );
    }

    #[test]
    fn read_and_callback_surfaces_stay_ungated() {
        // 读接口与已有独立闸门的路径不重复计数
        assert_ungated(Method::GET, "/api/moments/feed");
        assert_ungated(Method::GET, "/api/keys/u1");
        assert_ungated(Method::POST, "/api/conv/d-a~b/recall");
        assert_ungated(Method::DELETE, "/api/media/x");

        // 群：离开/备注/公告不是"建群拉人踢人"
        assert_ungated(Method::POST, "/api/groups/g1/leave");
        assert_ungated(Method::PUT, "/api/groups/g1/remark-mine");
        assert_ungated(Method::PUT, "/api/groups/g1/announcement");

        // 社区：加入与聊天消息不按"发帖"限流
        assert_ungated(Method::POST, "/api/communities/c1/join");
        assert_ungated(Method::POST, "/api/channels/ch1/messages");
        assert_ungated(Method::POST, "/api/threads/t1/messages");
        assert_ungated(Method::PUT, "/api/channels/ch1");

        // 直播：推流回调绝不能闸；观众动作也是低频写
        assert_ungated(Method::POST, "/api/live/ingest/authorize");
        assert_ungated(Method::POST, "/api/live/ch1/join");
        assert_ungated(Method::POST, "/api/live/ch1/leave");
        assert_ungated(Method::POST, "/api/live/ch1/stop");
        assert_ungated(Method::POST, "/api/live/ch1/danmaku");
        assert_ungated(Method::GET, "/api/live/ch1");

        // 通话：只有信令 POST 进闸，开房/加入/管理动作不进
        assert_ungated(Method::GET, "/api/call/d-a~b/signal");
        assert_ungated(Method::POST, "/api/call/d-a~b/start");
        assert_ungated(Method::POST, "/api/call/d-a~b/join");
        assert_ungated(Method::POST, "/api/call/d-a~b/mute");
    }

    fn concrete(path: &str) -> String {
        path.replace("{conv}", "d-a~b")
            .replace("{group_id}", "g1")
            .replace("{user_id}", "u1")
            .replace("{channel}", "ch1")
            .replace("{id}", "x1")
    }

    #[test]
    fn mapped_actions_table_matches_action_for() {
        for (method, path, action) in mapped_actions() {
            let m = Method::from_bytes(method.as_bytes()).unwrap();
            let got = action_for(&m, &concrete(path)).map(|a| a.as_str().to_string());
            assert_eq!(
                got.as_deref(),
                Some(*action),
                "{method} {path} declared {action} but maps to {got:?}"
            );
        }
    }

    /// 反向核对：表里声明的每条写路径都必须能落到一个真实 Action 上，
    /// 且字符串与 `as_str()` 一致——防止表与枚举各改一半。
    #[test]
    fn mapped_actions_table_uses_real_action_names() {
        for (method, path, action) in mapped_actions() {
            let m = Method::from_bytes(method.as_bytes()).unwrap();
            let mapped = action_for(&m, &concrete(path)).unwrap_or_else(|| {
                panic!("{method} {path} is declared as {action} but maps to nothing")
            });
            assert_eq!(mapped.as_str(), *action, "{method} {path}");
        }
    }
}
