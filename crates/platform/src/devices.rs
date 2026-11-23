//! **handler 层**（`devices`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::offload;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::PlatformState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::domain::devices::{DeviceReg, DeviceRow};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/devices", post(register))
        .route("/api/devices", get(list))
}

async fn register(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(reg): ApiJson<DeviceReg>,
) -> std::result::Result<StatusCode, ApiError> {
    let me = authed.user_id.clone();
    offload(&platform, move |p| p.register_device(&me, &reg)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<Vec<DeviceRow>>, ApiError> {
    let me = authed.user_id.clone();
    let rows = offload(&platform, move |p| p.devices_of(&me)).await?;
    Ok(Json(rows))
}
