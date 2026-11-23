//! **handler 层**（`communities`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::{offload, offload_api};
use crate::error::{ApiError, PlatformError};
use crate::extract::ApiJson;
use crate::PlatformState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::Deserialize;

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/communities", post(create_community))
        .route("/api/communities/mine", get(my_communities))
        .route("/api/communities/{id}/join", post(join_community))
        .route("/api/communities/{id}/members", get(community_members))
        .route("/api/communities/{id}/channels", post(create_channel))
        .route("/api/communities/{id}/channels", get(list_channels))
        .route("/api/community/{id}/threads", get(community_threads))
        .route(
            "/api/community/{id}/threads",
            post(create_thread_in_community),
        )
        .route("/api/channels/{id}/messages", post(send_channel_message))
        .route("/api/channels/{id}/messages", get(fetch_channel_messages))
        .route(
            "/api/channels/{id}/messages/recall",
            post(recall_channel_message),
        )
        .route("/api/channels/{id}/threads", post(create_thread))
        .route("/api/channels/{id}/threads", get(list_threads))
        .route("/api/channel/{id}/roles", get(list_channel_roles))
        .route("/api/channel/{id}/roles", post(set_channel_role))
        .route("/api/channel/{id}/roles/{user}", delete(reset_channel_role))
        .route("/api/threads/{id}/messages", post(send_thread_reply))
        .route("/api/threads/{id}/messages", get(fetch_thread_messages))
        .route("/api/threads/{id}/subscribe", post(subscribe_thread))
        .route("/api/threads/{id}/subscribe", put(set_thread_subscription))
        .route("/api/threads/{id}/subscribe", delete(unsubscribe_thread))
        .route("/api/threads/{id}/subscribers", get(thread_subscribers))
}

#[derive(Deserialize)]
struct CommunityBody {
    name: String,
    #[serde(default)]
    member_limit: i64,
}

async fn create_community(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<CommunityBody>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let actor = authed.user_id.clone();
    let name = body.name;
    let member_limit = body.member_limit;
    let id = offload(&platform, move |p| {
        p.create_community(&actor, &name, member_limit)
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "community_id": id })),
    ))
}

async fn my_communities(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let rows = offload(&platform, move |p| p.my_communities(&actor)).await?;
    Ok(Json(serde_json::json!({
        "communities": rows.into_iter().map(|(c, role)| serde_json::json!({
            "id": c.id, "name": c.name, "owner_uid": c.owner_uid,
            "member_limit": c.member_limit, "role": role,
        })).collect::<Vec<_>>()
    })))
}

async fn join_community(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let state = offload(&platform, move |p| p.join_community(&id, &actor)).await?;
    Ok(Json(serde_json::json!({ "state": state })))
}

async fn community_members(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let rows = offload(&platform, move |p| p.community_members(&id, &actor)).await?;
    Ok(Json(serde_json::json!({
        "members": rows.into_iter().map(|(user, role)| serde_json::json!({
            "user_id": user, "role": role
        })).collect::<Vec<_>>()
    })))
}

#[derive(Deserialize)]
struct ChannelBody {
    name: String,
    kind: String,
}

async fn create_channel(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ChannelBody>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let actor = authed.user_id.clone();
    let name = body.name;
    let kind = body.kind;
    let channel = offload(&platform, move |p| {
        p.create_channel(&id, &actor, &name, &kind)
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "channel_id": channel })),
    ))
}

async fn list_channels(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let rows = offload(&platform, move |p| p.list_channels(&id, &actor)).await?;
    Ok(Json(serde_json::json!({ "channels": rows })))
}

#[derive(Deserialize)]
struct ChannelMessageBody {
    text: String,
    /// 消息类型（信箱注册表里的 normal 类）；缺省 `text`。
    #[serde(default)]
    kind: Option<String>,
}

async fn send_channel_message(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ChannelMessageBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    // ADR-28 样板 ①「消息写」：鉴权读 + 信箱事务整段进阻塞池，handler 只 `await`
    // 它的结果——tokio worker 不再替 SQLite 等待。
    let actor = authed.user_id.clone();
    let kind = body
        .kind
        .clone()
        .unwrap_or_else(|| "text".to_string())
        .to_string();
    let text = body.text.clone();
    let (seq, msg_id) = offload(&platform, move |p| {
        p.send_channel_message(&id, &actor, &kind, &text)
    })
    .await?;
    Ok(Json(serde_json::json!({ "seq": seq, "msg_id": msg_id })))
}

#[derive(Deserialize)]
struct RecallBody {
    msg_id: String,
}

async fn recall_channel_message(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<RecallBody>,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = authed.user_id.clone();
    let msg_id = body.msg_id;
    offload(&platform, move |p| {
        p.recall_channel_message(&id, &actor, &msg_id)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
struct FetchQuery {
    #[serde(default)]
    after_seq: u64,
    limit: Option<usize>,
}

async fn fetch_channel_messages(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    Query(q): Query<FetchQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let limit = q.limit.unwrap_or(50);
    let (rows, next) = offload(&platform, move |p| {
        p.fetch_channel_messages(&id, &actor, q.after_seq, limit)
    })
    .await?;
    Ok(Json(serde_json::json!({
        "messages": rows.into_iter().map(|m| serde_json::json!({
            "seq": m.seq, "sender": m.sender, "text": m.payload,
            "kind": m.type_id, "msg_id": m.msg_id, "withdrawn": m.withdrawn,
        })).collect::<Vec<_>>(),
        "next_after_seq": next,
    })))
}

#[derive(Deserialize, Default)]
struct CommunityThreadQuery {
    limit: Option<usize>,
}

async fn community_threads(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    Query(q): Query<CommunityThreadQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let limit = q.limit.unwrap_or(50);
    let rows = offload(&platform, move |p| p.community_threads(&id, &actor, limit)).await?;
    Ok(Json(serde_json::json!({ "threads": rows })))
}

#[derive(Deserialize)]
struct ThreadBody {
    root_seq: i64,
    #[serde(default)]
    title: String,
    /// 只在 `/api/community/{id}/threads` 上必填：社区是话题的容器，
    /// 但一条话题始终挂在一个具体频道下。
    #[serde(default)]
    channel_id: Option<String>,
}

async fn create_thread(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ThreadBody>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let actor = authed.user_id.clone();
    let title = body.title;
    let root_seq = body.root_seq;
    let thread = offload(&platform, move |p| {
        p.create_thread(&id, &actor, root_seq, &title)
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "thread_id": thread })),
    ))
}

async fn create_thread_in_community(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ThreadBody>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let channel = body
        .channel_id
        .filter(|c| !c.trim().is_empty())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "channel_id is required when opening a thread from a community",
            )
        })?;
    // 频道必须属于这个社区，否则就是拿社区 id 越权写到别的社区里
    let actor = authed.user_id.clone();
    let title = body.title;
    let root_seq = body.root_seq;
    let thread = offload_api(&platform, move |p| {
        if p.channel_community(&channel)?.as_deref() != Some(id.as_str()) {
            return Err(PlatformError::NotFound.into());
        }
        p.create_thread(&channel, &actor, root_seq, &title)
            .map_err(ApiError::from)
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "thread_id": thread })),
    ))
}

async fn list_threads(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let rows = offload(&platform, move |p| p.list_threads(&id, &actor)).await?;
    Ok(Json(serde_json::json!({ "threads": rows })))
}

#[derive(Deserialize)]
struct ChannelRoleBody {
    user_id: String,
    role: String,
}

async fn list_channel_roles(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let id_for_task = id.clone();
    let (rows, me) = offload(&platform, move |p| {
        let rows = p.channel_roles(&id_for_task, &actor)?;
        let me = p
            .effective_channel_role(&id_for_task, &actor)?
            .map(|r| r.as_str().to_string());
        Ok((rows, me))
    })
    .await?;
    Ok(Json(serde_json::json!({
        "channel_id": id, "my_role": me, "roles": rows
    })))
}

async fn set_channel_role(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ChannelRoleBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let user_id = body.user_id;
    let user_id_for_response = user_id.clone();
    let role_arg = body.role;
    let role = offload(&platform, move |p| {
        p.set_channel_role(&id, &actor, &user_id, &role_arg)
    })
    .await?;
    Ok(Json(
        serde_json::json!({ "user_id": user_id_for_response, "role": role.as_str() }),
    ))
}

async fn reset_channel_role(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path((id, user)): Path<(String, String)>,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = authed.user_id.clone();
    offload(&platform, move |p| p.reset_channel_role(&id, &actor, &user)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ThreadReplyBody {
    text: String,
}

async fn send_thread_reply(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ThreadReplyBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    // ADR-28 样板 ①「消息写」：`thread_messages` 的行写入（含两次鉴权读）整段
    // 进阻塞池；`thread.reply` 通知帧仍在 await 侧扇出——投递是异步的，不该
    // 跟着 SQL 一起占线程。
    let actor = authed.user_id.clone();
    let text = body.text.clone();
    let channel_arg = id.clone();
    let text_arg = text.clone();
    let (seq, channel, to) = offload(&platform, move |p| {
        p.post_thread_reply(&channel_arg, &actor, &text_arg)
    })
    .await?;
    if !to.is_empty() {
        let mut env = crate::system_envelope(&channel, &authed.user_id, to, "");
        env.body = wxlike_core::MsgBody::Custom {
            kind: "thread.reply".into(),
            payload: serde_json::json!({
                "thread_id": id,
                "seq": seq,
                "text": text,
                "sent_at_ms": wxlike_core::types::now_ms(),
            }),
        };
        platform.emit(env).await;
    }
    Ok(Json(serde_json::json!({ "seq": seq })))
}

#[derive(Deserialize, Default)]
struct ThreadFetchQuery {
    #[serde(default)]
    after_seq: i64,
    limit: Option<usize>,
}

async fn fetch_thread_messages(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    Query(q): Query<ThreadFetchQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let limit = q.limit.unwrap_or(100);
    let rows = offload(&platform, move |p| {
        p.fetch_thread_messages(&id, &actor, q.after_seq, limit)
    })
    .await?;
    Ok(Json(serde_json::json!({
        "messages": rows.into_iter().map(|(seq, sender, text, _)| serde_json::json!({
            "seq": seq, "sender": sender, "text": text
        })).collect::<Vec<_>>()
    })))
}

async fn subscribe_thread(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = authed.user_id.clone();
    offload(&platform, move |p| p.subscribe_thread(&id, &actor)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unsubscribe_thread(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = authed.user_id.clone();
    offload(&platform, move |p| p.unsubscribe_thread(&id, &actor)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn thread_subscribers(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let (rows, fanout) = offload(&platform, move |p| {
        let rows = p.thread_subscribers(&id, &actor)?;
        let fanout = p.thread_fanout(&id)?;
        Ok((rows, fanout))
    })
    .await?;
    Ok(Json(serde_json::json!({
        "subscribers": rows,
        "fanout": fanout,
    })))
}

#[derive(Deserialize)]
struct SubscriptionBody {
    #[serde(default)]
    muted: bool,
}

/// `PUT /api/threads/{id}/subscribe`：订阅并把静音状态一次写定（幂等）。
async fn set_thread_subscription(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<SubscriptionBody>,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = authed.user_id.clone();
    let muted = body.muted;
    offload(&platform, move |p| {
        p.set_thread_subscription(&id, &actor, muted)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
