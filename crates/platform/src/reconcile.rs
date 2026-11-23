//! **handler 层**（`reconcile`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::AdminAuth;
use crate::db::offload;
use crate::error::ApiError;
use crate::PlatformState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};

use crate::domain::reconcile::{ReconcileReport, RECONCILE_HISTORY_LIMIT};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/admin/reconcile", get(latest).post(run))
        .route("/api/admin/reconcile/history", get(history))
}

async fn run(
    State(platform): State<PlatformState>,
    _admin: AdminAuth,
) -> std::result::Result<(StatusCode, Json<ReconcileReport>), ApiError> {
    let report = offload(&platform, |p| p.reconcile()).await?;
    Ok((StatusCode::OK, Json(report)))
}

async fn latest(
    State(platform): State<PlatformState>,
    _admin: AdminAuth,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let (last, recent) = offload(&platform, |p| {
        let last = p.last_reconcile()?;
        let recent = p.reconcile_history(20)?;
        Ok((last, recent))
    })
    .await?;
    Ok(Json(serde_json::json!({
        "last": last,
        "history": recent,
    })))
}

async fn history(
    State(platform): State<PlatformState>,
    _admin: AdminAuth,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let runs = offload(&platform, |p| p.reconcile_history(RECONCILE_HISTORY_LIMIT)).await?;
    Ok(Json(serde_json::json!({ "runs": runs })))
}
