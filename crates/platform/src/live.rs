//! P5 直播频道（合同 §3.7 / ADR-13）：**信令与弹幕独立于频道时间线**。
//!
//! 职责切分（ADR-21）：
//! * 本模块 = 直播**会话控制面**：开播授权、推流凭据、观众计数、弹幕路由；
//! * 媒体面 = 外部 RTMP ingest（Xiu）+ HLS 分发（nginx/静态），本仓库不实现
//!   转码与分发——那是有状态边缘节点的事，放进来只会变成一个坏掉的媒体服务器；
//! * 消息面 = 弹幕走 `high_priority` 通道（`live.danmaku` 已在类型注册表登记），
//!   **不落库、不占频道序列号**（不变式 3）。
//!
//! # 推流凭据
//!
//! RTMP 的 `on_publish` 回调必须能独立验证"这把流密钥现在还有效"，因此凭据
//! 是自包含的：`<exp_ms>.<hmac_sha256(secret, stream_key|exp)>`。验证不查库
//! （避免 ingest 节点依赖数据库），过期即失效；停播后再推流由 `verify_ingest`
//! 的会话状态检查拒绝。
//!
//! # 观众与弹幕
//!
//! 观众名单是弹幕的收件集来源：弹幕只发给**当前在房**的人（离线者拉历史时
//! 不该看到三小时前的弹幕）。因此 `join/leave` 维护 `live_viewers`，弹幕路由
//! 读它。峰值观众数落库，供 P6 运营看板。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use hmac::{Hmac, Mac as _};
use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::validate;
use crate::{Platform, PlatformState};

type HmacSha256 = Hmac<Sha256>;

/// 推流凭据默认有效期（开播时下发；过期由 RTMP 回调重新取）。
pub const INGEST_TTL_MS: u64 = 6 * 60 * 60 * 1000;
/// 弹幕长度上限（客户端渲染与带宽双重考虑）。
pub const DANMAKU_MAX_CHARS: usize = 80;

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/live/{channel}/start", post(start))
        .route("/api/live/{channel}/stop", post(stop))
        .route("/api/live/{channel}", get(state))
        .route("/api/live/{channel}/join", post(join))
        .route("/api/live/{channel}/leave", post(leave))
        .route("/api/live/{channel}/danmaku", post(danmaku))
        .route("/api/live/ingest/authorize", post(authorize_ingest))
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct LiveSession {
    pub id: String,
    pub channel_id: String,
    pub host: String,
    pub stream_key: String,
    pub state: String,
    pub recording: bool,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub viewers: i64,
    pub peak_viewers: i64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct LiveState {
    pub channel_id: String,
    pub live: bool,
    pub host: Option<String>,
    pub viewers: i64,
    pub recording: bool,
    /// HLS 播放地址（相对路径；由部署方反代到媒体节点）。
    pub hls_url: String,
}

#[derive(Deserialize)]
struct StartBody {
    #[serde(default)]
    recording: bool,
}

#[derive(Deserialize)]
struct DanmakuBody {
    text: String,
}

#[derive(Deserialize)]
struct IngestAuthBody {
    stream_key: String,
    token: String,
}

impl Platform {
    fn live_secret(&self) -> &str {
        self.options().live_ingest_secret.as_str()
    }

    fn sign_ingest(&self, stream_key: &str, exp_ms: u64) -> String {
        let mut mac = HmacSha256::new_from_slice(self.live_secret().as_bytes())
            .unwrap_or_else(|_| HmacSha256::new_from_slice(b"wxlike-live").expect("static key"));
        mac.update(stream_key.as_bytes());
        mac.update(b"|");
        mac.update(exp_ms.to_string().as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// 自包含推流凭据：`<exp_ms>.<sig>`。ingest 节点只需共享 secret。
    pub fn ingest_token(&self, stream_key: &str, ttl_ms: u64) -> String {
        let exp = wxlike_core::types::now_ms().saturating_add(ttl_ms);
        format!("{exp}.{}", self.sign_ingest(stream_key, exp))
    }

    /// 校验推流凭据：签名 + 有效期 + 会话仍在直播。
    pub fn verify_ingest(&self, stream_key: &str, token: &str, now_ms: u64) -> Result<()> {
        let (exp_s, sig) = token.split_once('.').ok_or(PlatformError::Forbidden)?;
        let exp: u64 = exp_s.parse().map_err(|_| PlatformError::Forbidden)?;
        if now_ms > exp {
            return Err(PlatformError::Forbidden);
        }
        let expected = self.sign_ingest(stream_key, exp);
        if !constant_time_eq(sig.as_bytes(), expected.as_bytes()) {
            return Err(PlatformError::Forbidden);
        }
        // 会话状态：停播后的凭据即使未过期也不能推流
        let c = self.db().lock();
        let live: Option<String> = c
            .query_row(
                "SELECT state FROM live_sessions WHERE stream_key = ?1 ORDER BY started_at DESC LIMIT 1",
                params![stream_key],
                |r| r.get(0),
            )
            .optional()?;
        match live.as_deref() {
            Some("live") => Ok(()),
            _ => Err(PlatformError::Forbidden),
        }
    }

    /// 开播：频道必须是 `live` 类型，且调用者是社群管理员/群主（合同 §3.4）。
    pub fn start_live(&self, channel: &str, actor: &str, recording: bool) -> Result<LiveSession> {
        let channel = validate::id(channel)?.to_string();
        let community = self
            .channel_community(&channel)?
            .ok_or(PlatformError::NotFound)?;
        self.require_manager(&community, actor)?;
        if self.channel_kind(&channel)?.as_deref() != Some("live") {
            return Err(PlatformError::BadInput("channel is not a live channel"));
        }
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM live_sessions WHERE channel_id = ?1 AND state = 'live'",
                params![channel],
                |r| r.get(0),
            )
            .optional()?;
        if existing.is_some() {
            return Err(PlatformError::Conflict("channel is already live".into()));
        }
        let id = crate::auth::random_hex(8);
        let stream_key = crate::auth::random_hex(12);
        tx.execute(
            "INSERT INTO live_sessions(id, channel_id, host, stream_key, state, recording,
                                       started_at, ended_at, viewers, peak_viewers)
             VALUES(?1, ?2, ?3, ?4, 'live', ?5, ?6, NULL, 0, 0)",
            params![id, channel, actor, stream_key, i64::from(recording), now],
        )?;
        tx.commit()?;
        drop(c);
        self.audit(actor, "live.start", &channel, &id);
        Ok(LiveSession {
            id,
            channel_id: channel,
            host: actor.to_string(),
            stream_key,
            state: "live".into(),
            recording,
            started_at: now,
            ended_at: None,
            viewers: 0,
            peak_viewers: 0,
        })
    }

    /// 停播：主播或社群管理员。幂等（已停播返回 `NotFound`）。
    pub fn stop_live(&self, channel: &str, actor: &str) -> Result<()> {
        let channel = validate::id(channel)?.to_string();
        let community = self
            .channel_community(&channel)?
            .ok_or(PlatformError::NotFound)?;
        self.require_member(&community, actor)?;
        // 先把权限结论算完再拿 DB 锁：require_manager 内部也要锁同一把
        // 非重入互斥锁，持锁调用会死锁（本模块第一次运行就被这条坑住）。
        let is_manager = self.require_manager(&community, actor).is_ok();
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        let session: Option<(String, String)> = tx
            .query_row(
                "SELECT id, host FROM live_sessions WHERE channel_id = ?1 AND state = 'live'",
                params![channel],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((id, host)) = session else {
            return Err(PlatformError::NotFound);
        };
        // 主播本人，或社群管理员（主持人失联时的兜底）
        if host != actor && !is_manager {
            return Err(PlatformError::Forbidden);
        }
        tx.execute(
            "UPDATE live_sessions SET state = 'ended', ended_at = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        tx.execute(
            "DELETE FROM live_viewers WHERE session_id = ?1",
            params![id],
        )?;
        tx.commit()?;
        drop(c);
        self.audit(actor, "live.stop", &channel, &id);
        Ok(())
    }

    /// 直播状态（成员可见）：是否在播、主播、观众数、播放地址。
    pub fn live_state(&self, channel: &str, user: &str) -> Result<LiveState> {
        let channel = validate::id(channel)?.to_string();
        let community = self
            .channel_community(&channel)?
            .ok_or(PlatformError::NotFound)?;
        self.require_member(&community, user)?;
        let c = self.db().lock();
        let row: Option<(String, String, i64, i64, i64)> = c
            .query_row(
                "SELECT host, stream_key, recording, viewers, peak_viewers
                 FROM live_sessions WHERE channel_id = ?1 AND state = 'live'",
                params![channel],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        Ok(match row {
            Some((host, key, recording, viewers, _peak)) => LiveState {
                channel_id: channel,
                live: true,
                host: Some(host),
                viewers,
                recording: recording != 0,
                hls_url: format!("/live/{key}/index.m3u8"),
            },
            None => LiveState {
                channel_id: channel,
                live: false,
                host: None,
                viewers: 0,
                recording: false,
                hls_url: String::new(),
            },
        })
    }

    fn live_session_id(&self, channel: &str) -> Result<String> {
        let c = self.db().lock();
        c.query_row(
            "SELECT id FROM live_sessions WHERE channel_id = ?1 AND state = 'live'",
            params![channel],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .ok_or(PlatformError::NotFound)
    }

    /// 进房：观众计数 +1，峰值同步抬升。
    pub fn join_live(&self, channel: &str, user: &str) -> Result<i64> {
        let channel = validate::id(channel)?.to_string();
        let community = self
            .channel_community(&channel)?
            .ok_or(PlatformError::NotFound)?;
        self.require_member(&community, user)?;
        let session = self.live_session_id(&channel)?;
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        tx.execute(
            "INSERT INTO live_viewers(session_id, user_id, joined_at) VALUES(?1, ?2, ?3)
             ON CONFLICT(session_id, user_id) DO NOTHING",
            params![session, user, now],
        )?;
        tx.execute(
            "UPDATE live_sessions SET viewers = (SELECT COUNT(*) FROM live_viewers WHERE session_id = ?1),
                    peak_viewers = MAX(peak_viewers, (SELECT COUNT(*) FROM live_viewers WHERE session_id = ?1))
             WHERE id = ?1",
            params![session],
        )?;
        let viewers: i64 = tx.query_row(
            "SELECT viewers FROM live_sessions WHERE id = ?1",
            params![session],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(viewers)
    }

    pub fn leave_live(&self, channel: &str, user: &str) -> Result<i64> {
        let channel = validate::id(channel)?.to_string();
        let session = self.live_session_id(&channel)?;
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        tx.execute(
            "DELETE FROM live_viewers WHERE session_id = ?1 AND user_id = ?2",
            params![session, user],
        )?;
        tx.execute(
            "UPDATE live_sessions SET viewers = (SELECT COUNT(*) FROM live_viewers WHERE session_id = ?1)
             WHERE id = ?1",
            params![session],
        )?;
        let viewers: i64 = tx.query_row(
            "SELECT viewers FROM live_sessions WHERE id = ?1",
            params![session],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(viewers)
    }

    /// 弹幕：只发给当前在房观众，走 `high_priority`（不落库、不占序列号）。
    /// 返回实际收件人数（不含发送者自己）。
    pub async fn send_danmaku(&self, channel: &str, user: &str, text: &str) -> Result<usize> {
        let channel = validate::id(channel)?.to_string();
        let community = self
            .channel_community(&channel)?
            .ok_or(PlatformError::NotFound)?;
        self.require_member(&community, user)?;
        if text.trim().is_empty() || text.chars().count() > DANMAKU_MAX_CHARS {
            return Err(PlatformError::BadInput("danmaku text length"));
        }
        // 审核：弹幕同样过内容管线（软违规按模式挂起/打标，硬违规拒绝）
        let verdict = self.moderate_for_send(user, &channel, text)?;
        match verdict.decision.outcome {
            wxlike_moderation::Outcome::Publish | wxlike_moderation::Outcome::PublishFlagged => {}
            wxlike_moderation::Outcome::Hold => return Err(PlatformError::Forbidden),
            wxlike_moderation::Outcome::Reject => return Err(PlatformError::Forbidden),
        }
        let session = self.live_session_id(&channel)?;
        let recipients: Vec<String> = {
            let c = self.db().lock();
            let mut stmt = c.prepare(
                "SELECT user_id FROM live_viewers WHERE session_id = ?1 AND user_id <> ?2",
            )?;
            let rows = stmt.query_map(params![session, user], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        if recipients.is_empty() {
            return Ok(0);
        }
        let mut env = crate::system_envelope(&channel, user, recipients.clone(), "");
        env.body = wxlike_core::MsgBody::Custom {
            kind: "live.danmaku".into(),
            payload: serde_json::json!({
                "session_id": session,
                "text": text,
                "sent_at_ms": wxlike_core::types::now_ms(),
            }),
        };
        self.emit(env).await;
        Ok(recipients.len())
    }
}

/// 常数时间比较（长度不同也走完全程，避免按字节提前返回）。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = (a.len() ^ b.len()) as u8;
    let n = a.len().max(b.len());
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    diff == 0
}

#[derive(Serialize)]
struct StartReply {
    session: LiveSession,
    ingest_token: String,
    ingest_url: String,
}

async fn start(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(channel): Path<String>,
    Json(body): Json<StartBody>,
) -> std::result::Result<Json<StartReply>, ApiError> {
    let session = platform.start_live(&channel, &authed.user_id, body.recording)?;
    let token = platform.ingest_token(&session.stream_key, INGEST_TTL_MS);
    let base = std::env::var("WXLIKE_RTMP_BASE")
        .unwrap_or_else(|_| "rtmp://127.0.0.1:1935/live".to_string());
    let ingest_url = format!("{base}/{}", session.stream_key);
    Ok(Json(StartReply {
        session,
        ingest_token: token,
        ingest_url,
    }))
}

async fn stop(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(channel): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.stop_live(&channel, &authed.user_id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn state(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(channel): Path<String>,
) -> std::result::Result<Json<LiveState>, ApiError> {
    Ok(Json(platform.live_state(&channel, &authed.user_id)?))
}

async fn join(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(channel): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let viewers = platform.join_live(&channel, &authed.user_id)?;
    Ok(Json(serde_json::json!({ "viewers": viewers })))
}

async fn leave(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(channel): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let viewers = platform.leave_live(&channel, &authed.user_id)?;
    Ok(Json(serde_json::json!({ "viewers": viewers })))
}

async fn danmaku(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(channel): Path<String>,
    Json(body): Json<DanmakuBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let delivered = platform
        .send_danmaku(&channel, &authed.user_id, &body.text)
        .await?;
    Ok(Json(serde_json::json!({ "delivered": delivered })))
}

/// RTMP `on_publish` 回调：ingest 节点持共享 secret 调用，无用户 token。
async fn authorize_ingest(
    State(platform): State<PlatformState>,
    Json(body): Json<IngestAuthBody>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.verify_ingest(&body.stream_key, &body.token, wxlike_core::types::now_ms())?;
    Ok(StatusCode::NO_CONTENT)
}
