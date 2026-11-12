//! **handler 层**（`report`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::{AdminAuth, Authed};
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
    platform.risk_gate(wxlike_risk::Action::Report, &authed.user_id, None)?;
    let reason = Reason::parse(&body.reason)
        .ok_or_else(|| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "unknown reason"))?;
    let target_id = validate::id(&body.target_id)?;
    let ticket = platform.create_report(
        &authed.user_id,
        &body.target_kind,
        target_id,
        reason,
        &body.detail,
    )?;
    platform.risk_record(wxlike_risk::Action::Report, &authed.user_id)?;
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({ "ticket_id": ticket.id, "state": ticket.state })),
    ))
}

async fn mine(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let rows = platform.reports_by(&authed.user_id, 50)?;
    Ok(Json(serde_json::json!({ "tickets": rows })))
}

async fn queue(
    State(platform): State<PlatformState>,
    _admin: AdminAuth,
    Query(q): Query<QueueQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let sla = platform.options().report_sla_ms;
    let now = wxlike_core::types::now_ms();
    let tickets = platform.report_queue(q.state.as_deref(), q.limit.unwrap_or(50))?;
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
    Ok(Json(platform.triage_report(
        &id,
        &admin.actor,
        &body.note,
    )?))
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
    Ok(Json(platform.act_on_report(
        &id,
        &admin.actor,
        body.action,
        &body.note,
    )?))
}

async fn close(
    State(platform): State<PlatformState>,
    admin: AdminAuth,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<NoteBody>,
) -> std::result::Result<Json<Ticket>, ApiError> {
    Ok(Json(platform.close_report(
        &id,
        &admin.actor,
        &body.note,
    )?))
}

async fn reject(
    State(platform): State<PlatformState>,
    admin: AdminAuth,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<NoteBody>,
) -> std::result::Result<Json<Ticket>, ApiError> {
    Ok(Json(platform.reject_report(
        &id,
        &admin.actor,
        &body.note,
    )?))
}
