//! **handler 层**（`wallet`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::{offload, offload_api};
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
    let actor = authed.user_id.clone();
    let to_uid = body.to_uid;
    let amount = body.amount;
    let key = idem_key(&headers);
    let hash = request_hash(&[&actor, &to_uid, &amount.to_string(), &memo]);
    let idem_key = key.clone();
    let hash_for_task = hash.clone();
    let actor_for_task = actor.clone();
    let to_uid_for_task = to_uid.clone();
    let memo_for_task = memo.clone();
    let (hit, ref_id) = offload_api(&platform, move |p| {
        if let Some(hit) =
            replay_or_proceed(p, &actor_for_task, idem_key.as_deref(), &hash_for_task)?
        {
            return Ok((Some(hit), None));
        }
        let ref_id = p.transfer(&actor_for_task, &to_uid_for_task, amount, &memo_for_task)?;
        Ok((None, Some(ref_id)))
    })
    .await?;
    let ref_id = if let Some(hit) = hit {
        return Ok(Json(hit));
    } else {
        ref_id.ok_or_else(|| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "transfer result vanished",
            )
        })?
    };
    platform
        .emit(wallet_message(
            &crate::direct_conv_id(&actor, &to_uid),
            &actor,
            vec![to_uid.clone()],
            wxlike_core::custom_kind::WALLET_TRANSFER,
            serde_json::json!({
                "transfer_id": &ref_id,
                "amount": amount,
                "from": &actor,
                "to": &to_uid,
                "memo": &memo,
            }),
        ))
        .await;
    let response = serde_json::json!({ "transfer_id": ref_id });
    if let Some(key) = key {
        let response_for_store = response.clone();
        let hash_for_store = hash;
        offload(&platform, move |p| {
            p.wallet_idem_store(&actor, &key, &hash_for_store, &response_for_store)
        })
        .await?;
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
    let actor = authed.user_id.clone();
    let amount = body.amount;
    let count = body.count;
    let conv_id = body.conv_id;
    let key = idem_key(&headers);
    let hash = request_hash(&[
        &actor,
        &amount.to_string(),
        &count.to_string(),
        &memo,
        conv_id.as_deref().unwrap_or_default(),
    ]);
    let idem_key = key.clone();
    let hash_for_task = hash.clone();
    let actor_for_task = actor.clone();
    let memo_for_task = memo.clone();
    let conv_id_for_task = conv_id.clone();
    let (hit, id) = offload_api(&platform, move |p| {
        if let Some(hit) =
            replay_or_proceed(p, &actor_for_task, idem_key.as_deref(), &hash_for_task)?
        {
            return Ok((Some(hit), None));
        }
        ensure_conv_postable(p, &actor_for_task, conv_id_for_task.as_deref())?;
        let id = p.create_redpacket(
            &actor_for_task,
            amount,
            count,
            &memo_for_task,
            conv_id_for_task.as_deref(),
        )?;
        Ok((None, Some(id)))
    })
    .await?;
    let id = if let Some(hit) = hit {
        return Ok(Json(hit));
    } else {
        id.ok_or_else(|| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "redpacket result vanished",
            )
        })?
    };
    announce_redpacket(&platform, &actor, &id, amount, count, &memo).await?;
    let response = serde_json::json!({ "redpacket_id": id });
    if let Some(key) = key {
        let response_for_store = response.clone();
        offload(&platform, move |p| {
            p.wallet_idem_store(&actor, &key, &hash, &response_for_store)
        })
        .await?;
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
    let actor = authed.user_id.clone();
    let packet = offload_api(&platform, move |p| {
        let packet = p
            .redpacket(&id)?
            .ok_or_else(|| ApiError::not_found("no such redpacket"))?;
        if packet.from_uid != actor && !p.may_read_conv(&actor, &packet.conv_id) {
            return Err(ApiError::not_found("no such redpacket"));
        }
        Ok(packet)
    })
    .await?;
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
    let actor = authed.user_id.clone();
    let id_for_task = id.clone();
    let (share, conv_id) = offload_api(&platform, move |p| {
        let share = p.claim_redpacket(&id_for_task, &actor)?;
        let conv_id = p
            .redpacket(&id_for_task)?
            .map(|p| p.conv_id)
            .unwrap_or_else(|| id_for_task.clone());
        Ok((share, conv_id))
    })
    .await?;
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
    let actor = authed.user_id.clone();
    let balance = offload(&platform, move |p| p.wallet_balance(&actor)).await?;
    Ok(Json(serde_json::json!({
        "balance": balance,
    })))
}

async fn ledger(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<LedgerQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let since = q.since.unwrap_or(0).max(0);
    let limit = q.limit.unwrap_or(LEDGER_DEFAULT_LIMIT);
    let actor = authed.user_id.clone();
    let (rows, head) = offload(&platform, move |p| {
        let rows = p.ledger_of(&actor, since, limit)?;
        let head = p.ledger_head(&actor)?;
        Ok((rows, head))
    })
    .await?;
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
    let actor = user_id.to_string();
    let actor_for_task = actor.clone();
    let id_for_task = id.to_string();
    let (conv_id, recipients) = offload_api(platform, move |p| {
        let packet = p
            .redpacket(&id_for_task)?
            .ok_or_else(|| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "packet vanished"))?;
        let recipients = match p.cached_members(&packet.conv_id) {
            Ok(Some(members)) => members,
            _ => vec![packet.from_uid.clone()],
        }
        .into_iter()
        .filter(|u| u.as_str() != actor_for_task.as_str())
        .collect::<Vec<String>>();
        Ok((packet.conv_id, recipients))
    })
    .await?;
    platform
        .emit(wallet_message(
            &conv_id,
            &actor,
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
