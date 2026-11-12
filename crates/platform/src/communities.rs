//! **handler 层**（`communities`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::offload;
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
    let id = platform.create_community(&authed.user_id, &body.name, body.member_limit)?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "community_id": id })),
    ))
}

async fn my_communities(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let rows = platform.my_communities(&authed.user_id)?;
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
    let state = platform.join_community(&id, &authed.user_id)?;
    Ok(Json(serde_json::json!({ "state": state })))
}

async fn community_members(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let rows = platform.community_members(&id, &authed.user_id)?;
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
    let channel = platform.create_channel(&id, &authed.user_id, &body.name, &body.kind)?;
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
    let rows = platform.list_channels(&id, &authed.user_id)?;
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
    platform.recall_channel_message(&id, &authed.user_id, &body.msg_id)?;
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
    let (rows, next) = platform.fetch_channel_messages(
        &id,
        &authed.user_id,
        q.after_seq,
        q.limit.unwrap_or(50),
    )?;
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
    let rows = platform.community_threads(&id, &authed.user_id, q.limit.unwrap_or(50))?;
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
    let thread = platform.create_thread(&id, &authed.user_id, body.root_seq, &body.title)?;
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
    if platform.channel_community(&channel)?.as_deref() != Some(id.as_str()) {
        return Err(PlatformError::NotFound.into());
    }
    let thread = platform.create_thread(&channel, &authed.user_id, body.root_seq, &body.title)?;
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
    let rows = platform.list_threads(&id, &authed.user_id)?;
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
    let rows = platform.channel_roles(&id, &authed.user_id)?;
    let me = platform
        .effective_channel_role(&id, &authed.user_id)?
        .map(|r| r.as_str());
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
    let role = platform.set_channel_role(&id, &authed.user_id, &body.user_id, &body.role)?;
    Ok(Json(
        serde_json::json!({ "user_id": body.user_id, "role": role.as_str() }),
    ))
}

async fn reset_channel_role(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path((id, user)): Path<(String, String)>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.reset_channel_role(&id, &authed.user_id, &user)?;
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
    let rows = platform.fetch_thread_messages(
        &id,
        &authed.user_id,
        q.after_seq,
        q.limit.unwrap_or(100),
    )?;
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
    platform.subscribe_thread(&id, &authed.user_id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unsubscribe_thread(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.unsubscribe_thread(&id, &authed.user_id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn thread_subscribers(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let rows = platform.thread_subscribers(&id, &authed.user_id)?;
    let fanout = platform.thread_fanout(&id)?;
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
    platform.set_thread_subscription(&id, &authed.user_id, body.muted)?;
    Ok(StatusCode::NO_CONTENT)
}
