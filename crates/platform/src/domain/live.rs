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
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::live`]。

use crate::error::{PlatformError, Result};
use crate::validate;
use crate::Platform;
use hmac::{Hmac, Mac as _};
use serde::Serialize;
use sha2::Sha256;
use wxlike_core::tx::ConnectionTx as _;

use super::repo::communities::{
    channel_community as repo_channel_community, community_role as repo_community_role,
};
use super::repo::live::{
    active_live_snapshot, active_session_exists, active_session_id, active_session_ref,
    danmaku_recipients, delete_viewer, end_live_session, insert_live_session, insert_viewer,
    live_state_for_stream, refresh_viewer_counts,
};
use super::repo::trust as trust_repo;

type HmacSha256 = Hmac<Sha256>;

/// 推流凭据默认有效期（开播时下发；过期由 RTMP 回调重新取）。
pub const INGEST_TTL_MS: u64 = 6 * 60 * 60 * 1000;

/// 弹幕长度上限（客户端渲染与带宽双重考虑）。
pub const DANMAKU_MAX_CHARS: usize = 80;

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
        let c = self.db().acquire();
        match live_state_for_stream(&c, stream_key)?.as_deref() {
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
        let (id, stream_key) = c.with_tx(|tx| -> Result<(String, String)> {
            let port = crate::storage::StorageConnection::sqlite_tx(tx);
            if active_session_exists(&port, &channel)? {
                return Err(PlatformError::Conflict("channel is already live".into()));
            }
            let id = crate::auth::random_hex(8);
            let stream_key = crate::auth::random_hex(12);
            insert_live_session(&port, &id, &channel, actor, &stream_key, recording, now)?;
            Ok((id, stream_key))
        })?;
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
        let id = c.with_tx(|tx| -> Result<String> {
            let port = crate::storage::StorageConnection::sqlite_tx(tx);
            let session = active_session_ref(&port, &channel)?.ok_or(PlatformError::NotFound)?;
            let (id, host) = (session.id, session.host);
            // 主播本人，或社群管理员（主持人失联时的兜底）
            if host != actor && !is_manager {
                return Err(PlatformError::Forbidden);
            }
            end_live_session(&port, &id, now)?;
            Ok(id)
        })?;
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
        let c = self.db().acquire();
        let row = active_live_snapshot(&c, &channel)?;
        Ok(match row {
            Some(snapshot) => LiveState {
                channel_id: channel,
                live: true,
                host: Some(snapshot.host),
                viewers: snapshot.viewers,
                recording: snapshot.recording != 0,
                hls_url: format!("/live/{}/index.m3u8", snapshot.stream_key),
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
        active_session_id(&self.db().acquire(), channel)?.ok_or(PlatformError::NotFound)
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
        c.with_tx(|tx| -> Result<i64> {
            let port = crate::storage::StorageConnection::sqlite_tx(tx);
            insert_viewer(&port, &session, user, now)?;
            refresh_viewer_counts(&port, &session)
        })
    }

    pub fn leave_live(&self, channel: &str, user: &str) -> Result<i64> {
        let channel = validate::id(channel)?.to_string();
        let session = self.live_session_id(&channel)?;
        let mut c = self.db().lock();
        c.with_tx(|tx| -> Result<i64> {
            let port = crate::storage::StorageConnection::sqlite_tx(tx);
            delete_viewer(&port, &session, user)?;
            refresh_viewer_counts(&port, &session)
        })
    }

    /// 弹幕：只发给当前在房观众，走 `high_priority`（不落库、不占序列号）。
    /// 返回实际收件人数（不含发送者自己）。
    pub async fn send_danmaku(&self, channel: &str, user: &str, text: &str) -> Result<usize> {
        let channel = validate::id(channel)?.to_string();
        let user = user.to_string();
        self.ensure_danmaku_member(&channel, &user).await?;
        if text.trim().is_empty() || text.chars().count() > DANMAKU_MAX_CHARS {
            return Err(PlatformError::BadInput("danmaku text length"));
        }
        let decision = self.moderate(text);
        match decision.outcome {
            wxlike_moderation::Outcome::Publish | wxlike_moderation::Outcome::PublishFlagged => {}
            wxlike_moderation::Outcome::Hold => {
                self.queue_held_danmaku(&channel, &user, text, decision)
                    .await?;
                return Err(PlatformError::Forbidden);
            }
            wxlike_moderation::Outcome::Reject => return Err(PlatformError::Forbidden),
        }
        let (session, recipients) = self.danmaku_delivery(&channel, &user).await?;
        if recipients.is_empty() {
            return Ok(0);
        }
        let mut env = crate::system_envelope(&channel, &user, recipients.clone(), "");
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

    /// 频道成员才能发弹幕；权限判定在阻塞池里完成。
    async fn ensure_danmaku_member(&self, channel: &str, user: &str) -> Result<()> {
        let channel_for_task = channel.to_string();
        let user_for_task = user.to_string();
        self.db()
            .blocking(move |c| {
                let port = crate::storage::StorageConnection::sqlite_borrow(c);
                let community = repo_channel_community(&port, &channel_for_task)?
                    .ok_or(PlatformError::NotFound)?;
                repo_community_role(&port, &community, &user_for_task)?
                    .ok_or(PlatformError::Forbidden)?;
                Ok(())
            })
            .await
    }

    /// 软违规挂起：只把判定结果与原文写进审核队列，正文不落消息时间线。
    async fn queue_held_danmaku(
        &self,
        channel: &str,
        user: &str,
        text: &str,
        decision: wxlike_moderation::Decision,
    ) -> Result<()> {
        let labels = serde_json::to_string(&decision.labels)?;
        let decision_json = serde_json::to_string(&decision)?;
        let conv = channel.to_string();
        let sender = user.to_string();
        let body = text.to_string();
        let now = wxlike_core::types::now_ms() as i64;
        self.db()
            .blocking(move |c| {
                let id = crate::auth::random_hex(8);
                trust_repo::enqueue_conn(
                    c,
                    &id,
                    &conv,
                    &sender,
                    &body,
                    &labels,
                    &decision_json,
                    now,
                )
            })
            .await
    }

    /// 当前活跃场次 + 收件人；空列表由调用方直接返回 0。
    async fn danmaku_delivery(&self, channel: &str, user: &str) -> Result<(String, Vec<String>)> {
        let channel_for_task = channel.to_string();
        let user_for_task = user.to_string();
        self.db()
            .blocking(move |c| {
                let port = crate::storage::StorageConnection::sqlite_borrow(c);
                let session =
                    active_session_id(&port, &channel_for_task)?.ok_or(PlatformError::NotFound)?;
                let recipients = danmaku_recipients(&port, &session, &user_for_task)?;
                Ok((session, recipients))
            })
            .await
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
pub(crate) struct StartReply {
    pub(crate) session: LiveSession,
    pub(crate) ingest_token: String,
    pub(crate) ingest_url: String,
}
