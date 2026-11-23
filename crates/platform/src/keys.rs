//! **handler 层**（`keys`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::{offload, offload_api};
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::PlatformState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use wxlike_core::e2ee::PreKeyBundle;

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/keys", post(publish))
        .route("/api/keys", get(own))
        .route("/api/keys/{user_id}", get(claim))
}

async fn publish(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(bundle): ApiJson<PreKeyBundle>,
) -> std::result::Result<StatusCode, ApiError> {
    let me = authed.user_id.clone();
    offload(&platform, move |p| p.publish_keys(&me, &bundle)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn own(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<PreKeyBundle>, ApiError> {
    let me = authed.user_id.clone();
    let bundle = offload(&platform, move |p| p.own_keys(&me)).await?;
    Ok(Json(bundle))
}

async fn claim(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(user_id): Path<String>,
) -> std::result::Result<Json<PreKeyBundle>, ApiError> {
    // AR-2/H-4（第四轮审计）：认领一次性密钥此前只要登录，任何人都可把
    // 某用户的 OTK 库存循环领空（他人再也无法与其建立 Olm 会话），且每次
    // 认领伴随一次 DB 写。两道闸：
    // 1) 关系校验——只能领"好友或同群成员"的密钥（自己当然可以）；
    // 2) 风控限流——按 (caller, target) 计数，Throttle/Deny 落 429。
    //
    // 关系校验、风控、认领、成功计数四步收进同一次阻塞执行；顺序不变，
    // 也不在 await 上下文里直接调同步查库函数。
    let me = authed.user_id.clone();
    let target = user_id;
    let claimed = offload_api(&platform, move |p| {
        if me != target && !p.may_claim_keys_of(&me, &target)? {
            // 404 而非 403：不确认"这个用户存在"，与 moments 的陌生人策略同款
            return Err(ApiError::new(StatusCode::NOT_FOUND, "no such user"));
        }
        let risk_key = format!("{me}:{target}");
        p.risk_gate(wxlike_risk::Action::ClaimKeys, &risk_key, None)?;
        let claimed = p.claim_key(&target)?;
        // 成功的认领才计数：失败重试不该吃掉正常用户的配额
        if claimed.is_some() {
            p.risk_record(wxlike_risk::Action::ClaimKeys, &risk_key)?;
        }
        Ok(claimed)
    })
    .await?;
    match claimed {
        Some(bundle) => Ok(Json(bundle)),
        None => Err(ApiError::new(StatusCode::NOT_FOUND, "no such user")),
    }
}
