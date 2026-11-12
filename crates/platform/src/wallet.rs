//! **handler 层**（`wallet`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::PlatformState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::domain::wallet::{
    ensure_conv_postable, idem_key, replay_or_proceed, request_hash, wallet_message,
    LEDGER_DEFAULT_LIMIT,
};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/wallet/transfer", post(transfer))
        .route("/api/wallet/redpacket", post(create_redpacket))
        .route("/api/wallet/redpacket/{id}", get(redpacket_get))
        .route("/api/wallet/redpacket/{id}/claim", post(claim))
        .route("/api/wallet/balance", get(balance))
        .route("/api/wallet/ledger", get(ledger))
}

#[derive(serde::Deserialize)]
pub struct TransferBody {
    pub to_uid: String,
    pub amount: i64,
    #[serde(default)]
    pub memo: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct RedpacketBody {
    pub amount: i64,
    pub count: i64,
    #[serde(default)]
    pub memo: Option<String>,
    /// Optional conversation the packet is posted into. The contract's body
    /// does not name one, so it falls back to the packet's own id.
    #[serde(default)]
    pub conv_id: Option<String>,
}

#[derive(serde::Deserialize, Default)]
pub struct LedgerQuery {
    #[serde(default)]
    since: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
}

async fn transfer(
    State(platform): State<PlatformState>,
    authed: Authed,
    headers: HeaderMap,
    ApiJson(body): ApiJson<TransferBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let memo = body.memo.clone().unwrap_or_default();
    let key = idem_key(&headers);
    let hash = request_hash(&[
        &authed.user_id,
        &body.to_uid,
        &body.amount.to_string(),
        &memo,
    ]);
    if let Some(hit) = replay_or_proceed(&platform, &authed.user_id, key.as_deref(), &hash)? {
        return Ok(Json(hit));
    }
    let ref_id = platform.transfer(&authed.user_id, &body.to_uid, body.amount, &memo)?;
    platform
        .emit(wallet_message(
            &crate::direct_conv_id(&authed.user_id, &body.to_uid),
            &authed.user_id,
            vec![body.to_uid.clone()],
            wxlike_core::custom_kind::WALLET_TRANSFER,
            serde_json::json!({
                "transfer_id": ref_id,
                "amount": body.amount,
                "from": authed.user_id,
                "to": body.to_uid,
                "memo": memo,
            }),
        ))
        .await;
    let response = serde_json::json!({ "transfer_id": ref_id });
    if let Some(key) = key.as_deref() {
        platform.wallet_idem_store(&authed.user_id, key, &hash, &response)?;
    }
    Ok(Json(response))
}

async fn create_redpacket(
    State(platform): State<PlatformState>,
    authed: Authed,
    headers: HeaderMap,
    ApiJson(body): ApiJson<RedpacketBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let memo = body.memo.clone().unwrap_or_default();
    let key = idem_key(&headers);
    let hash = request_hash(&[
        &authed.user_id,
        &body.amount.to_string(),
        &body.count.to_string(),
        &memo,
        body.conv_id.as_deref().unwrap_or_default(),
    ]);
    if let Some(hit) = replay_or_proceed(&platform, &authed.user_id, key.as_deref(), &hash)? {
        return Ok(Json(hit));
    }
    ensure_conv_postable(&platform, &authed.user_id, body.conv_id.as_deref())?;
    let id = platform.create_redpacket(
        &authed.user_id,
        body.amount,
        body.count,
        &memo,
        body.conv_id.as_deref(),
    )?;
    announce_redpacket(
        &platform,
        &authed.user_id,
        &id,
        body.amount,
        body.count,
        &memo,
    )
    .await?;
    let response = serde_json::json!({ "redpacket_id": id });
    if let Some(key) = key.as_deref() {
        platform.wallet_idem_store(&authed.user_id, key, &hash, &response)?;
    }
    Ok(Json(response))
}

/// `GET /api/wallet/redpacket/{id}`：红包本体行。发红包的人或该会话成员
/// 可读；对别人一律 404（不确认存在性）。
async fn redpacket_get(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let packet = platform
        .redpacket(&id)?
        .ok_or_else(|| ApiError::not_found("no such redpacket"))?;
    if packet.from_uid != authed.user_id
        && !platform.may_read_conv(&authed.user_id, &packet.conv_id)
    {
        return Err(ApiError::not_found("no such redpacket"));
    }
    Ok(Json(serde_json::json!({
        "id": packet.id,
        "from_uid": packet.from_uid,
        "conv_id": packet.conv_id,
        "total": packet.total,
        "count": packet.count,
        "claimed": packet.claimed,
        "claimed_total": packet.claimed_total,
        "memo": packet.memo,
        "status": packet.status,
        "created_at": packet.created_at,
        "expire_at": packet.expire_at,
    })))
}

async fn claim(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    // identity comes from the token only: any `user_id` in the body is ignored
    let share = platform.claim_redpacket(&id, &authed.user_id)?;
    let conv_id = platform
        .redpacket(&id)?
        .map(|p| p.conv_id)
        .unwrap_or_else(|| id.clone());
    platform
        .emit(wallet_message(
            &conv_id,
            &authed.user_id,
            vec![],
            wxlike_core::custom_kind::WALLET_CLAIM,
            serde_json::json!({
                "redpacket_id": id,
                "by": authed.user_id,
                "amount": share,
            }),
        ))
        .await;
    Ok(Json(serde_json::json!({ "amount": share })))
}

async fn balance(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(serde_json::json!({
        "balance": platform.wallet_balance(&authed.user_id)?,
    })))
}

async fn ledger(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<LedgerQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let since = q.since.unwrap_or(0).max(0);
    let limit = q.limit.unwrap_or(LEDGER_DEFAULT_LIMIT);
    let rows = platform.ledger_of(&authed.user_id, since, limit)?;
    let head = platform.ledger_head(&authed.user_id)?;
    Ok(Json(serde_json::json!({
        "entries": rows,
        "cursor": head,
        // a full page means there may be more; anything less is the tail
        "has_more": rows.len() as i64 == limit.clamp(1, 200),
    })))
}

/// 红包公告扇出：发给能看见该会话的所有人（除发红包者自己）。
async fn announce_redpacket(
    platform: &PlatformState,
    user_id: &str,
    id: &str,
    amount: i64,
    count: i64,
    memo: &str,
) -> std::result::Result<(), ApiError> {
    let packet = platform
        .redpacket(id)?
        .ok_or_else(|| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "packet vanished"))?;
    let recipients = match platform.cached_members(&packet.conv_id) {
        Ok(Some(members)) => members,
        _ => vec![packet.from_uid.clone()],
    }
    .into_iter()
    .filter(|u| *u != user_id)
    .collect();
    platform
        .emit(wallet_message(
            &packet.conv_id,
            user_id,
            recipients,
            wxlike_core::custom_kind::WALLET_REDPACKET,
            serde_json::json!({
                "redpacket_id": id,
                "amount": amount,
                "count": count,
                "memo": memo,
            }),
        ))
        .await;
    Ok(())
}
