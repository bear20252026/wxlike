//! P4 设备管理（合同 §3.1「设备列表、远程下线」）。
//!
//! 与 `devices` 模块的区别：那边是**推送令牌**（哪个 token 该收通知），
//! 这边是**登录会话**（哪台设备还持有有效凭证）。产品上两者合成一页"设备
//! 管理"，数据面必须分开——换手机时推送令牌立刻失效，但旧会话要用户主动踢。
//!
//! 会话对外只暴露 `sid`（12 hex），**永不返回 token**：页面能下线设备，
//! 但拿不到能冒用的凭证。`current` 标记当前设备，前端据此禁止"踢自己"
//! 的误操作（虽然服务端也允许，等于登出）。
//!
//! 远程下线的语义是**立即失效**：删行即失效（token 查询直接落空），不依赖
//! 过期时间——这是 ADR-10 选 DB session 而非无状态 JWT 的核心理由。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::params;
use serde::Serialize;

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::{Platform, PlatformState};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/sessions", get(list))
        .route("/api/sessions/{sid}/revoke", post(revoke))
        .route("/api/sessions/revoke-others", post(revoke_others))
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct SessionRow {
    pub sid: String,
    pub device_id: String,
    pub device_name: String,
    pub ip: String,
    pub created_at: i64,
    pub last_seen: i64,
    pub current: bool,
}

impl Platform {
    /// 该用户全部有效会话，最近活跃在前。过期行顺手清掉。
    pub fn sessions_of(&self, user_id: &str, current_token: &str) -> Result<Vec<SessionRow>> {
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        c.execute(
            "DELETE FROM sessions WHERE user_id = ?1 AND expires_at <= ?2",
            params![user_id, now],
        )?;
        let mut stmt = c.prepare(
            "SELECT sid, device_id, device_name, ip, created_at, last_seen, token
             FROM sessions WHERE user_id = ?1 ORDER BY last_seen DESC, created_at DESC",
        )?;
        let rows = stmt
            .query_map(params![user_id], |r| {
                let token: String = r.get(6)?;
                Ok(SessionRow {
                    sid: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    device_id: r.get(1)?,
                    device_name: r.get(2)?,
                    ip: r.get(3)?,
                    created_at: r.get(4)?,
                    last_seen: r.get(5)?,
                    // 列里是摘要（AR-3/H-2），比对也要用摘要
                    current: token == crate::auth::token_digest(current_token),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 按 sid 下线一台设备。返回是否命中（不存在/已过期 → false，不报错，
    /// 避免把 sid 变成存在性探测面）。
    pub fn revoke_session_by_sid(&self, user_id: &str, sid: &str) -> Result<bool> {
        if sid.is_empty() || sid.len() > 32 {
            return Err(PlatformError::BadInput("sid malformed"));
        }
        let c = self.db().lock();
        let n = c.execute(
            "DELETE FROM sessions WHERE user_id = ?1 AND sid = ?2",
            params![user_id, sid],
        )?;
        Ok(n > 0)
    }

    /// 下线除当前设备外的全部会话（"一键退出其他设备"）。
    pub fn revoke_other_sessions(&self, user_id: &str, keep_token: &str) -> Result<usize> {
        // keep 参数同样要以摘要比对，否则会把当前会话一并删掉（AR-3/H-2）
        let keep = crate::auth::token_digest(keep_token);
        let c = self.db().lock();
        let n = c.execute(
            "DELETE FROM sessions WHERE user_id = ?1 AND token <> ?2",
            params![user_id, keep],
        )?;
        Ok(n)
    }

    /// 记录活跃时间（登录、设备页访问、关键操作时调用）。
    pub fn touch_session(&self, token: &str) -> Result<()> {
        let token = crate::auth::token_digest(token);
        let c = self.db().lock();
        c.execute(
            "UPDATE sessions SET last_seen = ?2 WHERE token = ?1",
            params![token, wxlike_core::types::now_ms() as i64],
        )?;
        Ok(())
    }
}

async fn list(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    platform.touch_session(&authed.token)?;
    let sessions = platform.sessions_of(&authed.user_id, &authed.token)?;
    // 推送令牌一并返回：设备管理页要显示"这台设备能否收到推送"
    let push = platform.devices_of(&authed.user_id)?;
    Ok(Json(serde_json::json!({
        "sessions": sessions,
        "push_devices": push,
    })))
}

async fn revoke(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(sid): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let hit = platform.revoke_session_by_sid(&authed.user_id, &sid)?;
    if !hit {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "no such session"));
    }
    platform.audit(&authed.user_id, "session.revoke", &sid, "");
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_others(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let n = platform.revoke_other_sessions(&authed.user_id, &authed.token)?;
    platform.audit(&authed.user_id, "session.revoke_others", "", &n.to_string());
    Ok(Json(serde_json::json!({ "revoked": n })))
}
