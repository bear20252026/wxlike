//! **handler 层**（`report`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::{AdminAuth, Authed};
use crate::db::{offload, offload_api};
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::validate;
use crate::PlatformState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use wxlike_report::{Reason, Ticket};

use crate::domain::report::{ActBody, TicketView};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/reports", post(create))
        .route("/api/reports", get(mine))
        .route("/api/admin/reports", get(queue))
        .route("/api/admin/reports/{id}/triage", post(triage))
        .route("/api/admin/reports/{id}/act", post(act))
        .route("/api/admin/reports/{id}/close", post(close))
        .route("/api/admin/reports/{id}/reject", post(reject))
}

#[derive(serde::Deserialize)]
pub struct ReportBody {
    pub target_kind: String,
    pub target_id: String,
    pub reason: String,
    #[serde(default)]
    pub detail: String,
}

#[derive(serde::Deserialize)]
pub struct NoteBody {
    #[serde(default)]
    pub note: String,
}

#[derive(serde::Deserialize, Default)]
pub struct QueueQuery {
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

async fn create(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<ReportBody>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    // 举报轰炸过风控闸（Throttle/Deny 都落到 429）
    let actor = authed.user_id.clone();
    let reason = Reason::parse(&body.reason)
        .ok_or_else(|| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "unknown reason"))?;
    let target_kind = body.target_kind;
    let target_id = validate::id(&body.target_id)?.to_string();
    let detail = body.detail;
    let ticket = offload_api(&platform, move |p| {
        p.risk_gate(wxlike_risk::Action::Report, &actor, None)?;
        let ticket = p.create_report(&actor, &target_kind, &target_id, reason, &detail)?;
        p.risk_record(wxlike_risk::Action::Report, &actor)?;
        Ok(ticket)
    })
    .await?;
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({ "ticket_id": ticket.id, "state": ticket.state })),
    ))
}

async fn mine(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let rows = offload(&platform, move |p| p.reports_by(&actor, 50)).await?;
    Ok(Json(serde_json::json!({ "tickets": rows })))
}

async fn queue(
    State(platform): State<PlatformState>,
    _admin: AdminAuth,
    Query(q): Query<QueueQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let sla = platform.options().report_sla_ms;
    let now = wxlike_core::types::now_ms();
    let state = q.state;
    let limit = q.limit.unwrap_or(50);
    let tickets = offload(&platform, move |p| p.report_queue(state.as_deref(), limit)).await?;
    let views: Vec<TicketView> = tickets
        .iter()
        .map(|t| TicketView {
            ticket: t,
            overdue: t.is_overdue(now, sla),
        })
        .collect();
    Ok(Json(serde_json::json!({ "queue": views })))
}

async fn triage(
    State(platform): State<PlatformState>,
    admin: AdminAuth,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<NoteBody>,
) -> std::result::Result<Json<Ticket>, ApiError> {
    let actor = admin.actor;
    let note = body.note;
    let ticket = offload(&platform, move |p| p.triage_report(&id, &actor, &note)).await?;
    Ok(Json(ticket))
}

async fn act(
    State(platform): State<PlatformState>,
    admin: AdminAuth,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ActBody>,
) -> std::result::Result<Json<Ticket>, ApiError> {
    // 兼容分支的**观测点**：只有知道还有谁在用旧形状，才能决定何时删掉它。
    // 日志放在 handler 而不是 `Deserialize` 里：反序列化是纯函数，
    // 而这里能带上工单 id、操作人，也能进 tracing span。
    if body.legacy_shape {
        tracing::warn!(
            report_id = %id,
            actor = %admin.actor,
            "report.act 收到旧的嵌套 action 形状（{{\"action\":{{\"action\":\"…\"}}}}）；建议客户端改用平铺"
        );
    }
    let actor = admin.actor;
    let action = body.action;
    let note = body.note;
    let ticket = offload(&platform, move |p| {
        p.act_on_report(&id, &actor, action, &note)
    })
    .await?;
    Ok(Json(ticket))
}

async fn close(
    State(platform): State<PlatformState>,
    admin: AdminAuth,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<NoteBody>,
) -> std::result::Result<Json<Ticket>, ApiError> {
    let actor = admin.actor;
    let note = body.note;
    let ticket = offload(&platform, move |p| p.close_report(&id, &actor, &note)).await?;
    Ok(Json(ticket))
}

async fn reject(
    State(platform): State<PlatformState>,
    admin: AdminAuth,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<NoteBody>,
) -> std::result::Result<Json<Ticket>, ApiError> {
    let actor = admin.actor;
    let note = body.note;
    let ticket = offload(&platform, move |p| p.reject_report(&id, &actor, &note)).await?;
    Ok(Json(ticket))
}
