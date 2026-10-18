//! P3 支柱 C：社区 / 频道 / 话题（合同 §3.4，ADR-16 三支柱之三）。
//!
//! Schema 形状借用 **TangSengDaoDaoServer**（Apache-2.0，`refs/TangSengDaoDaoServer`
//! `modules/group/sql/`）的群/成员表设计：角色编号 0=成员 1=群主 2=管理员、
//! `member_limit` 上限、`tenant_id` 预留列；频道四类型（文字/公告/语音/直播）
//! 按 Mattermost 的 channel-purpose 模型（MIT，`refs` 未含该仓，仅对照其文档
//! 模型）；话题（Thread）按 Zulip 的 topic 语义——**回复聚合在话题内，不占
//! 频道主时间线**（不变式 3 的直接推论）。
//!
//! 频道时间线**不在此存储**：直接复用 P3-1 读扩散信箱
//! （`wxlike_message::MessageStore`，owner=Channel）——三支柱共用一套信箱
//! 基础设施（不变式 1），本模块只管成员/权限/形态。

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::validate;
use crate::{Platform, PlatformState};

/// TangSeng 借用的角色编号（0 成员 / 1 群主 / 2 管理员）。
pub const ROLE_MEMBER: i64 = 0;
pub const ROLE_OWNER: i64 = 1;
pub const ROLE_ADMIN: i64 = 2;

/// 频道四类型（合同 §3.4）。
pub const CHANNEL_KINDS: [&str; 4] = ["text", "announce", "voice", "live"];

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/communities", post(create_community))
        .route("/api/communities/mine", get(my_communities))
        .route("/api/communities/{id}/join", post(join_community))
        .route("/api/communities/{id}/members", get(community_members))
        .route("/api/communities/{id}/channels", post(create_channel))
        .route("/api/communities/{id}/channels", get(list_channels))
        .route("/api/channels/{id}/messages", post(send_channel_message))
        .route("/api/channels/{id}/messages", get(fetch_channel_messages))
        .route("/api/channels/{id}/threads", post(create_thread))
        .route("/api/channels/{id}/threads", get(list_threads))
        .route("/api/threads/{id}/messages", post(send_thread_reply))
        .route("/api/threads/{id}/messages", get(fetch_thread_messages))
        .route("/api/threads/{id}/subscribe", post(subscribe_thread))
        .route("/api/threads/{id}/subscribe", delete(unsubscribe_thread))
        .route("/api/threads/{id}/subscribers", get(thread_subscribers))
}

// ---------------------------------------------------------------------------
// 数据形状
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CommunityRow {
    pub id: String,
    pub name: String,
    pub owner_uid: String,
    pub member_limit: i64,
    pub announcement: String,
    pub tenant_id: String,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ChannelRow {
    pub id: String,
    pub community_id: String,
    pub name: String,
    pub kind: String,
    pub tenant_id: String,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ThreadRow {
    pub id: String,
    pub channel_id: String,
    pub root_seq: i64,
    pub title: String,
    pub created_by: String,
    pub created_at: i64,
}

fn valid_kind(kind: &str) -> bool {
    CHANNEL_KINDS.contains(&kind)
}

impl Platform {
    // ---- 成员/角色（TangSeng 形状） ----

    fn community_role(&self, community: &str, user: &str) -> Result<Option<i64>> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT role FROM community_members WHERE community_id = ?1 AND user_id = ?2",
            params![community, user],
            |r| r.get::<_, i64>(0),
        )
        .optional()?)
    }

    fn require_member(&self, community: &str, user: &str) -> Result<i64> {
        self.community_role(community, user)?
            .ok_or(PlatformError::Forbidden)
    }

    fn require_manager(&self, community: &str, user: &str) -> Result<()> {
        match self.require_member(community, user)? {
            r if r == ROLE_OWNER || r == ROLE_ADMIN => Ok(()),
            _ => Err(PlatformError::Forbidden),
        }
    }

    fn channel_community(&self, channel: &str) -> Result<Option<String>> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT community_id FROM channels WHERE id = ?1",
            params![channel],
            |r| r.get(0),
        )
        .optional()?)
    }

    /// 频道发送裁决：announce 型仅管理员/群主（Mattermost 广播频道语义）；
    /// 其余三型成员可发。直播/语音的时间线在此为过渡承载，P5 媒体面接管。
    fn channel_send_allowed(&self, channel: &str, user: &str) -> Result<bool> {
        let Some(community) = self.channel_community(channel)? else {
            return Err(PlatformError::NotFound);
        };
        let role = self.require_member(&community, user)?;
        let kind: String = {
            let c = self.db().lock();
            c.query_row(
                "SELECT kind FROM channels WHERE id = ?1",
                params![channel],
                |r| r.get(0),
            )?
        };
        if kind == "announce" {
            Ok(role == ROLE_OWNER || role == ROLE_ADMIN)
        } else {
            Ok(true)
        }
    }

    // ---- 社区 ----

    pub fn create_community(&self, owner: &str, name: &str, member_limit: i64) -> Result<String> {
        let name = validate::name(name)?;
        let owner = validate::id(owner)?;
        let member_limit = member_limit.clamp(2, 100_000);
        let id = crate::auth::random_hex(8);
        self.db().lock().execute_batch(&format!(
            "INSERT INTO communities(id, name, owner_uid, member_limit, tenant_id, created_at)
             VALUES('{id}', '{}', '{owner}', {member_limit}, 'default', {});",
            name.replace('\'', "''"),
            wxlike_core::types::now_ms() as i64
        ))?;
        self.db().lock().execute(
            "INSERT INTO community_members(community_id, user_id, role, joined_at)
             VALUES(?1, ?2, ?3, ?4)",
            params![id, owner, ROLE_OWNER, wxlike_core::types::now_ms() as i64],
        )?;
        Ok(id)
    }

    pub fn join_community(&self, community: &str, user: &str) -> Result<&'static str> {
        let community = validate::id(community)?;
        let user = validate::id(user)?;
        if self.community_role(community, user)?.is_some() {
            return Ok("already_member");
        }
        let c = self.db().lock();
        let (limit, current): (i64, i64) = c.query_row(
            "SELECT member_limit,
                    (SELECT COUNT(*) FROM community_members WHERE community_id = ?1)
             FROM communities WHERE id = ?1",
            params![community],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if current >= limit {
            return Err(PlatformError::BadInput("community member limit reached"));
        }
        c.execute(
            "INSERT INTO community_members(community_id, user_id, role, joined_at)
             VALUES(?1, ?2, ?3, ?4)",
            params![
                community,
                user,
                ROLE_MEMBER,
                wxlike_core::types::now_ms() as i64
            ],
        )?;
        Ok("joined")
    }

    pub fn my_communities(&self, user: &str) -> Result<Vec<(CommunityRow, i64)>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT c.id, c.name, c.owner_uid, c.member_limit, c.announcement, c.tenant_id,
                    c.created_at, m.role
             FROM communities c JOIN community_members m
               ON m.community_id = c.id AND m.user_id = ?1
             ORDER BY c.created_at",
        )?;
        let rows = stmt.query_map(params![user], |r| {
            Ok((
                CommunityRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    owner_uid: r.get(2)?,
                    member_limit: r.get(3)?,
                    announcement: r.get(4)?,
                    tenant_id: r.get(5)?,
                    created_at: r.get(6)?,
                },
                r.get(7)?,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn community_members(&self, community: &str, user: &str) -> Result<Vec<(String, i64)>> {
        self.require_member(community, user)?;
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT user_id, role FROM community_members WHERE community_id = ?1
             ORDER BY role DESC, joined_at",
        )?;
        let rows = stmt.query_map(params![community], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // ---- 频道 ----

    pub fn create_channel(
        &self,
        community: &str,
        actor: &str,
        name: &str,
        kind: &str,
    ) -> Result<String> {
        let community = validate::id(community)?;
        self.require_manager(community, actor)?;
        let name = validate::name(name)?;
        if !valid_kind(kind) {
            return Err(PlatformError::BadInput(
                "kind must be text|announce|voice|live",
            ));
        }
        let id = crate::auth::random_hex(8);
        self.db().lock().execute(
            "INSERT INTO channels(id, community_id, name, kind, tenant_id, created_at)
             VALUES(?1, ?2, ?3, ?4, 'default', ?5)",
            params![
                id,
                community,
                name,
                kind,
                wxlike_core::types::now_ms() as i64
            ],
        )?;
        Ok(id)
    }

    pub fn list_channels(&self, community: &str, user: &str) -> Result<Vec<ChannelRow>> {
        self.require_member(community, user)?;
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT id, community_id, name, kind, tenant_id, created_at
             FROM channels WHERE community_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt
            .query_map(params![community], row_channel)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 频道发消息：走读扩散信箱（不变式 1/3），announce 型做管理员闸门。
    /// 返回信箱 seq。
    pub async fn send_channel_message(
        &self,
        channel: &str,
        actor: &str,
        text: &str,
    ) -> Result<u64> {
        let channel = validate::id(channel)?;
        let actor = validate::id(actor)?;
        if !self.channel_send_allowed(channel, actor)? {
            return Err(PlatformError::Forbidden);
        }
        if text.is_empty() || text.chars().count() > 5_000 {
            return Err(PlatformError::BadInput("text required"));
        }
        let store = self.message_store.as_ref().ok_or_else(|| {
            PlatformError::Sqlite(rusqlite::Error::InvalidColumnType(
                0,
                "mailbox".into(),
                rusqlite::types::Type::Null,
            ))
        })?;
        let msg_id = crate::auth::random_hex(16);
        match store.append(
            (wxlike_message::OwnerType::Channel, channel),
            actor,
            "text",
            &msg_id,
            text,
        ) {
            Ok(Some(seq)) => Ok(seq),
            Ok(None) => Err(PlatformError::Sqlite(rusqlite::Error::InvalidColumnType(
                0,
                "mailbox".into(),
                rusqlite::types::Type::Null,
            ))), // 随机 msg_id 冲突近乎不可能
            Err(wxlike_message::MailError::Storage(e)) => Err(e.into()),
            Err(_) => Err(PlatformError::Sqlite(rusqlite::Error::InvalidColumnType(
                0,
                "mailbox".into(),
                rusqlite::types::Type::Null,
            ))),
        }
    }

    pub fn fetch_channel_messages(
        &self,
        channel: &str,
        user: &str,
        after_seq: u64,
        limit: usize,
    ) -> Result<(Vec<wxlike_message::MailMessage>, Option<u64>)> {
        let channel = validate::id(channel)?;
        let Some(community) = self.channel_community(channel)? else {
            return Err(PlatformError::NotFound);
        };
        self.require_member(&community, user)?;
        let store = self.message_store.as_ref().ok_or_else(|| {
            PlatformError::Sqlite(rusqlite::Error::InvalidColumnType(
                0,
                "mailbox".into(),
                rusqlite::types::Type::Null,
            ))
        })?;
        store
            .fetch(
                (wxlike_message::OwnerType::Channel, channel),
                after_seq,
                limit,
            )
            .map_err(|e| e.into())
    }

    // ---- 话题（Zulip 语义：聚合在话题内，不占频道主时间线） ----

    pub fn create_thread(
        &self,
        channel: &str,
        actor: &str,
        root_seq: i64,
        title: &str,
    ) -> Result<String> {
        let channel = validate::id(channel)?;
        let actor = validate::id(actor)?;
        let Some(community) = self.channel_community(channel)? else {
            return Err(PlatformError::NotFound);
        };
        self.require_member(&community, actor)?;
        if root_seq <= 0 {
            return Err(PlatformError::BadInput(
                "root_seq must reference a channel message",
            ));
        }
        // 根消息必须在频道时间线内存在
        let store = self
            .message_store
            .as_ref()
            .ok_or_else(|| PlatformError::Mail("message store unavailable".into()))?;
        let head = store
            .head_seq((wxlike_message::OwnerType::Channel, channel))
            .map_err(PlatformError::from)?;
        if root_seq as u64 > head {
            return Err(PlatformError::BadInput("root_seq beyond channel timeline"));
        }
        let id = crate::auth::random_hex(8);
        self.db().lock().execute(
            "INSERT INTO threads(id, channel_id, root_seq, title, created_by, tenant_id, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, 'default', ?6)",
            params![
                id,
                channel,
                root_seq,
                title,
                actor,
                wxlike_core::types::now_ms() as i64
            ],
        )?;
        // 发起者自动订阅
        self.db().lock().execute(
            "INSERT OR IGNORE INTO thread_subs(thread_id, user_id) VALUES(?1, ?2)",
            params![id, actor],
        )?;
        Ok(id)
    }

    pub fn list_threads(&self, channel: &str, user: &str) -> Result<Vec<ThreadRow>> {
        let channel = validate::id(channel)?;
        let Some(community) = self.channel_community(channel)? else {
            return Err(PlatformError::NotFound);
        };
        self.require_member(&community, user)?;
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT id, channel_id, root_seq, title, created_by, created_at
             FROM threads WHERE channel_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![channel], |r| {
            Ok(ThreadRow {
                id: r.get(0)?,
                channel_id: r.get(1)?,
                root_seq: r.get(2)?,
                title: r.get(3)?,
                created_by: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    fn thread_community(&self, thread: &str) -> Result<String> {
        let c = self.db().lock();
        c.query_row(
            "SELECT c.community_id FROM channels c JOIN threads t ON t.channel_id = c.id
             WHERE t.id = ?1",
            params![thread],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(PlatformError::NotFound)
    }

    pub fn send_thread_reply(&self, thread: &str, actor: &str, text: &str) -> Result<i64> {
        let thread = validate::id(thread)?;
        let actor = validate::id(actor)?;
        let community = self.thread_community(thread)?;
        self.require_member(&community, actor)?;
        if text.is_empty() || text.chars().count() > 5_000 {
            return Err(PlatformError::BadInput("text required"));
        }
        let c = self.db().lock();
        // 话题回复自持序列，绝不写频道信箱（不变式 3）
        c.execute(
            "INSERT INTO thread_messages(thread_id, sender, text, created_at)
             VALUES(?1, ?2, ?3, ?4)",
            params![thread, actor, text, wxlike_core::types::now_ms() as i64],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn fetch_thread_messages(
        &self,
        thread: &str,
        user: &str,
        after_seq: i64,
        limit: usize,
    ) -> Result<Vec<(i64, String, String, i64)>> {
        let thread = validate::id(thread)?;
        let community = self.thread_community(thread)?;
        self.require_member(&community, user)?;
        let limit = limit.clamp(1, 200) as i64;
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT seq, sender, text, created_at FROM thread_messages
             WHERE thread_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![thread, after_seq, limit], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    pub fn subscribe_thread(&self, thread: &str, user: &str) -> Result<()> {
        let thread = validate::id(thread)?;
        let community = self.thread_community(thread)?;
        self.require_member(&community, user)?;
        self.db().lock().execute(
            "INSERT OR IGNORE INTO thread_subs(thread_id, user_id) VALUES(?1, ?2)",
            params![thread, user],
        )?;
        Ok(())
    }

    pub fn unsubscribe_thread(&self, thread: &str, user: &str) -> Result<()> {
        let thread = validate::id(thread)?;
        let community = self.thread_community(thread)?;
        self.require_member(&community, user)?;
        self.db().lock().execute(
            "DELETE FROM thread_subs WHERE thread_id = ?1 AND user_id = ?2",
            params![thread, user],
        )?;
        Ok(())
    }

    pub fn thread_subscribers(&self, thread: &str, user: &str) -> Result<Vec<String>> {
        let community = self.thread_community(thread)?;
        self.require_member(&community, user)?;
        let c = self.db().lock();
        let mut stmt =
            c.prepare("SELECT user_id FROM thread_subs WHERE thread_id = ?1 ORDER BY user_id")?;
        let rows = stmt.query_map(params![thread], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

fn row_channel(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChannelRow> {
    Ok(ChannelRow {
        id: r.get(0)?,
        community_id: r.get(1)?,
        name: r.get(2)?,
        kind: r.get(3)?,
        tenant_id: r.get(4)?,
        created_at: r.get(5)?,
    })
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CommunityBody {
    name: String,
    #[serde(default)]
    member_limit: i64,
}

async fn create_community(
    State(platform): State<PlatformState>,
    authed: Authed,
    Json(body): Json<CommunityBody>,
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
    Json(body): Json<ChannelBody>,
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
}

async fn send_channel_message(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    Json(body): Json<ChannelMessageBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let seq = platform
        .send_channel_message(&id, &authed.user_id, &body.text)
        .await?;
    Ok(Json(serde_json::json!({ "seq": seq })))
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
            "withdrawn": m.withdrawn,
        })).collect::<Vec<_>>(),
        "next_after_seq": next,
    })))
}

#[derive(Deserialize)]
struct ThreadBody {
    root_seq: i64,
    #[serde(default)]
    title: String,
}

async fn create_thread(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    Json(body): Json<ThreadBody>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let thread = platform.create_thread(&id, &authed.user_id, body.root_seq, &body.title)?;
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
struct ThreadReplyBody {
    text: String,
}

async fn send_thread_reply(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    Json(body): Json<ThreadReplyBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let seq = platform.send_thread_reply(&id, &authed.user_id, &body.text)?;
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
    Ok(Json(serde_json::json!({ "subscribers": rows })))
}
