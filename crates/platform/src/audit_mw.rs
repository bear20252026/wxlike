//! 命令审计：**每个带 `X-Wxlike-Command` 的请求都落一条**。
//!
//! 为什么放在中间件而不是各 handler 里：写命令有十几个入口（建群、拉人、踢人…），
//! 每个都手写一次 `audit()` 一定会漏。中间件只有一个地方，漏不掉。
//!
//! 为什么用请求头而不是 URL 参数：命令 id 是调用方的语义，不是资源路径。
//! 同一个 `POST /api/groups` 既可能是人点按钮，也可能是 agent 执行
//! `group.create`——只有调用方知道。
//!
//! 审计记录形状：`action = "cmd.<id>"`，`target = 请求路径`，
//! `detail = "status=200 method=POST"`。actor 是 token 解析出的用户；
//! 没有有效 token 时记空串（请求本来就会被 401 挡掉，但留痕仍有用）。

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::PlatformState;

/// 调用方声明"这条请求是执行哪条命令"的头。
pub const COMMAND_HEADER: &str = "x-wxlike-command";

pub async fn command_audit(
    State(platform): State<PlatformState>,
    req: Request,
    next: Next,
) -> Response {
    let Some(command) = req
        .headers()
        .get(COMMAND_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
    else {
        return next.run(req).await;
    };

    let actor = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .and_then(|token| platform.lookup_token(token).ok().flatten())
        .unwrap_or_default();
    let path = req.uri().path().to_string();
    let method = req.method().to_string();

    let response = next.run(req).await;
    platform.audit(
        &actor,
        &format!("cmd.{command}"),
        &path,
        &format!("status={} method={method}", response.status().as_u16()),
    );
    response
}
