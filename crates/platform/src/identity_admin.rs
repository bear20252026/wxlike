//! **handler 层**（`identity_admin`）：G02-09 账户治理的管理端 HTTP 面。
//!
//! 与 `crates/admin/src/identity.rs::HttpPort` 的转发路径**逐字对应**（集成
//! 请求 REQ-G00-governance-http-mount）：adminx 经 `Upstream::call` 发
//! `x-admin-token` + `x-admin-actor`，这四个写端点 + 一个元数据视图就挂在这里。
//!
//! # 鉴权（与静态管理面的差别）
//!
//! `x-admin-token` 在这里**不是**静态密钥，而是 identity 域签发的
//! **Admin audience 会话令牌**：域函数内部的 `authenticate_operator` 走
//! `session::authenticate(.., Audience::Admin)`（G02-01 受众分离，普通用户
//! 令牌进不来）。四个 identity 端点把令牌原样交给域做服务端二次鉴权；
//! 群治理端点的域函数（relations）不带令牌参数，鉴权在本 handler 先行。
//! 与 `/api/admin/reports` 的 `AdminAuth`（静态 `admin_ok`）是两套口径，
//! 互不通用——静态密钥不是会话，会话不是静态密钥。
//!
//! # ADR-28
//!
//! 域 API 纯同步（`&Connection` 进、`Result` 出），整段包进
//! `spawn_blocking`；连接锁在闭包里拿、闭包里放，async handler 不跨 await
//! 持锁。端口（联动/计数）走平台侧第二条连接，见 `domain::governance`。
//!
//! # 错误形状
//!
//! 沿平台惯例：错误只回稳定分类与可行动消息（`ApiError`），不回显内部细节；
//! identity/relations 的错误映射随治理编排一起收进
//! `domain::identity_admin`，handler 只负责 HTTP 形状与 `spawn_blocking`。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::auth::AdminAuthSession;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::PlatformState;
use wxlike_identity::governance::{GovernCommand, GovernEffect};
use wxlike_relations::governance::{GroupGovernCommand, GroupGovernEffect};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/admin/identity/users/{id}/suspend", post(suspend_user))
        .route(
            "/api/admin/identity/users/{id}/unsuspend",
            post(unsuspend_user),
        )
        .route(
            "/api/admin/identity/devices/{id}/revoke",
            post(revoke_device),
        )
        .route("/api/admin/identity/groups/{id}/govern", post(govern_group))
        .route("/api/admin/identity/users/{id}", get(user_metadata_view))
}

/// spawn_blocking 的 JoinError：阻塞任务 panic/被取消时结论不可信，
/// 一律 500 并落日志（与 admin `offload` 同口径）。
fn join_err(err: tokio::task::JoinError) -> ApiError {
    tracing::error!(%err, "治理阻塞任务异常结束");
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "governance task aborted")
}

#[derive(Deserialize)]
pub struct SuspendBody {
    pub reason: String,
    #[serde(default)]
    pub until_ms: Option<i64>,
    pub actor: String,
    #[serde(default)]
    pub trace: String,
}

#[derive(Deserialize)]
pub struct ReasonBody {
    pub reason: String,
    pub actor: String,
    #[serde(default)]
    pub trace: String,
}

#[derive(Deserialize)]
pub struct GroupGovernBody {
    pub reason: String,
    pub actor: String,
    #[serde(default)]
    pub trace: String,
    /// `freeze` / `unfreeze` / `transfer` / `dissolve`（域侧封闭集合）。
    pub action: String,
    /// 仅 `transfer` 用：新群主 user_id（adminx 当前未透传，见集成请求 note）。
    #[serde(default)]
    pub new_owner: Option<String>,
}

fn command(target: String, body: SuspendBody) -> GovernCommand {
    GovernCommand {
        target,
        reason: body.reason,
        until_ms: body.until_ms,
        actor: body.actor,
        trace: body.trace,
        verb: None,
        new_owner: None,
    }
}

fn command_from_reason(target: String, body: ReasonBody) -> GovernCommand {
    GovernCommand {
        target,
        reason: body.reason,
        until_ms: None,
        actor: body.actor,
        trace: body.trace,
        verb: None,
        new_owner: None,
    }
}

/// `POST /api/admin/identity/users/{id}/suspend`：封禁（`until_ms` 缺省永久）。
/// 域内即时撤销该用户全部会话；平台镜像与事件经端口执行。
async fn suspend_user(
    State(platform): State<PlatformState>,
    Path(id): Path<String>,
    operator: AdminAuthSession,
    ApiJson(body): ApiJson<SuspendBody>,
) -> Result<Json<GovernEffect>, ApiError> {
    let cmd = command(id, body);
    let ran =
        tokio::task::spawn_blocking(move || platform.admin_suspend(&operator.token, &cmd)).await;
    match ran {
        Ok(Ok(effect)) => Ok(Json(effect)),
        Ok(Err(err)) => Err(err),
        Err(err) => Err(join_err(err)),
    }
}

/// `POST /api/admin/identity/users/{id}/unsuspend`：解封（可逆，留痕）。
async fn unsuspend_user(
    State(platform): State<PlatformState>,
    Path(id): Path<String>,
    operator: AdminAuthSession,
    ApiJson(body): ApiJson<ReasonBody>,
) -> Result<Json<GovernEffect>, ApiError> {
    let cmd = command_from_reason(id, body);
    let ran =
        tokio::task::spawn_blocking(move || platform.admin_unsuspend(&operator.token, &cmd)).await;
    match ran {
        Ok(Ok(effect)) => Ok(Json(effect)),
        Ok(Err(err)) => Err(err),
        Err(err) => Err(join_err(err)),
    }
}

/// `POST /api/admin/identity/devices/{id}/revoke`：撤销设备
/// （同事务作废该设备的会话/刷新链/step-up）。
async fn revoke_device(
    State(platform): State<PlatformState>,
    Path(id): Path<String>,
    operator: AdminAuthSession,
    ApiJson(body): ApiJson<ReasonBody>,
) -> Result<Json<GovernEffect>, ApiError> {
    let cmd = command_from_reason(id, body);
    let ran =
        tokio::task::spawn_blocking(move || platform.admin_revoke_device(&operator.token, &cmd))
            .await;
    match ran {
        Ok(Ok(effect)) => Ok(Json(effect)),
        Ok(Err(err)) => Err(err),
        Err(err) => Err(join_err(err)),
    }
}

/// `POST /api/admin/identity/groups/{id}/govern`：群治理
/// （freeze/unfreeze/transfer/dissolve）。audience 鉴权、域调用与群缓存失效
/// 都在 `domain::identity_admin`，handler 只负责 HTTP 形状与阻塞边界。
async fn govern_group(
    State(platform): State<PlatformState>,
    Path(id): Path<String>,
    operator: AdminAuthSession,
    ApiJson(body): ApiJson<GroupGovernBody>,
) -> Result<Json<GroupGovernEffect>, ApiError> {
    let action = wxlike_relations::governance::GroupAction::parse(&body.action)
        .ok_or_else(|| ApiError::unprocessable("bad input: unknown action"))?;
    let cmd = GroupGovernCommand {
        group_id: id,
        action,
        reason: body.reason,
        actor: body.actor,
        trace: body.trace,
        new_owner: body.new_owner,
    };
    let ran =
        tokio::task::spawn_blocking(move || platform.admin_govern_group(&operator.token, &cmd))
            .await;
    match ran {
        Ok(Ok(effect)) => Ok(Json(effect)),
        Ok(Err(err)) => Err(err),
        Err(err) => Err(join_err(err)),
    }
}

/// `GET /api/admin/identity/users/{id}`：管理员元数据视图（白名单投影，
/// 绝不含消息正文）。群数/举报数是跨域计数，经 `UserMetadataPort` 注入。
async fn user_metadata_view(
    State(platform): State<PlatformState>,
    Path(id): Path<String>,
    operator: AdminAuthSession,
) -> Result<Json<wxlike_identity::governance::AdminUserView>, ApiError> {
    let ran =
        tokio::task::spawn_blocking(move || platform.admin_user_view(&operator.token, &id)).await;
    match ran {
        Ok(Ok(view)) => Ok(Json(view)),
        Ok(Err(err)) => Err(err),
        Err(err) => Err(join_err(err)),
    }
}
