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
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::sessions`]。

use crate::error::{PlatformError, Result};
use crate::Platform;
use rusqlite::{params, OptionalExtension as _};
use serde::Serialize;
use wxlike_identity::refresh::{self, Rotated};
use wxlike_identity::{step_up, Audience, IdError};

/// device_id 的长度上限（客户端生成串，超长按畸形拒绝）。
const DEVICE_ID_MAX_LEN: usize = 64;

/// 下线设备要求的 step-up 用途（封闭集合里的一个）。
pub const STEP_UP_DEVICE_REVOKE: &str = "device.revoke";

/// 身份域错误 → 平台错误。规则在 features crate 里，这里只做类型搬运；
/// HTTP 状态由既有的 `From<PlatformError> for ApiError` 统一决定。
impl From<IdError> for PlatformError {
    fn from(err: IdError) -> Self {
        match err {
            IdError::BadInput(msg) => Self::BadInput(msg),
            IdError::Unauthorized(_) | IdError::Expired => Self::Unauthorized,
            IdError::Forbidden(_) => Self::Forbidden,
            IdError::NotFound(_) => Self::NotFound,
            IdError::Conflict(msg) => Self::Conflict(msg.to_string()),
            IdError::RateLimited { retry_after_ms } => Self::RateLimited {
                retry_after_ms: retry_after_ms.max(0) as u64,
            },
            // 库里的一行读不出合法状态：这是服务端的数据完整性问题，
            // 绝不能折成任何一种"看起来还行"的作业状态糊过去（#114）。
            IdError::Corrupt(what) => Self::Internal(format!("corrupt stored row: {what}")),
            IdError::Db(e) => Self::Sqlite(e),
            IdError::Json(e) => Self::Codec(e),
        }
    }
}

/// 刷新链里与设备相关的那部分（对外形状）。
#[derive(Serialize, Debug)]
pub struct RefreshReply {
    pub user_id: String,
    pub sid: String,
    pub device_id: String,
    pub token: String,
    pub refresh_token: String,
    pub expires_at: i64,
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

    /// 会话的 `sid`（刷新链按 sid 归组）。
    pub fn session_sid(&self, token: &str) -> Result<Option<String>> {
        let c = self.db().lock();
        let sid: Option<Option<String>> = c
            .query_row(
                "SELECT sid FROM sessions WHERE token = ?1",
                params![crate::auth::token_digest(token)],
                |r| r.get(0),
            )
            .optional()?;
        Ok(sid.flatten())
    }

    /// 登录成功后签发首枚刷新令牌（与访问会话同 sid，便于一起撤销）。
    pub fn issue_refresh(&self, user_id: &str, sid: &str, device_id: &str) -> Result<String> {
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        Ok(refresh::issue_default(
            &c,
            user_id,
            sid,
            device_id,
            Audience::User,
            now,
        )?)
    }

    /// 轮换刷新令牌：旧访问会话作废，发新访问会话 + 新刷新令牌。
    ///
    /// 重放检测在 [`refresh::rotate`] 里：旧令牌第二次出现即作废整条链。
    /// 这里的顺序是"先轮换、再换会话"——轮换失败就不该留下新会话。
    pub fn rotate_refresh(&self, raw: &str, ip: &str) -> Result<(Rotated, String)> {
        let now = wxlike_core::types::now_ms() as i64;
        let device_name = {
            let c = self.db().lock();
            match refresh::find(&c, raw)? {
                Some(chain) => c
                    .query_row(
                        "SELECT device_name FROM sessions WHERE sid = ?1",
                        params![chain.sid],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?
                    .unwrap_or_default(),
                None => String::new(),
            }
        };
        let rotated = {
            let c = self.db().lock();
            refresh::rotate(&c, raw, now)?
        };
        let token = self.create_session_for_device(
            &rotated.user_id,
            self.options().session_ttl_ms,
            &rotated.device_id,
            &device_name,
            ip,
        )?;
        // 新会话沿用链上的 sid：同一台设备只有一个 sid，撤销时一并失效。
        let digest = crate::auth::token_digest(&token);
        let c = self.db().lock();
        c.execute(
            "UPDATE sessions SET sid = ?2 WHERE token = ?1",
            params![digest, rotated.sid],
        )?;
        c.execute(
            "DELETE FROM sessions WHERE sid = ?1 AND token <> ?2",
            params![rotated.sid, digest],
        )?;
        Ok((rotated, token))
    }

    /// 授予一次 step-up（密码或 TOTP 已在上层校验过）。
    pub fn grant_step_up(&self, user_id: &str, sid: &str, purpose: &str) -> Result<i64> {
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        Ok(step_up::grant_default(&c, user_id, sid, purpose, now)?)
    }

    /// 消费一次 step-up（单次有效）。
    pub fn consume_step_up(&self, user_id: &str, sid: &str, purpose: &str) -> Result<()> {
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        step_up::consume(&c, user_id, sid, purpose, now)?;
        Ok(())
    }

    /// 下线一台设备：删会话 + 作废该设备全部刷新令牌。
    pub fn revoke_device_sessions(&self, user_id: &str, device_id: &str) -> Result<usize> {
        if device_id.is_empty() || device_id.len() > DEVICE_ID_MAX_LEN {
            return Err(PlatformError::BadInput("device_id malformed"));
        }
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        let n = c.execute(
            "DELETE FROM sessions WHERE user_id = ?1 AND device_id = ?2",
            params![user_id, device_id],
        )?;
        refresh::revoke_device(&c, user_id, device_id, now)?;
        Ok(n)
    }

    /// 撤销指定 sid 时同步作废它的刷新链（否则刷新能把会话续回来）。
    pub fn revoke_refresh_of_sid(&self, sid: &str) -> Result<usize> {
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        Ok(refresh::revoke_sid(&c, sid, now)?)
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

/// 让被撤销的设备立刻清本地缓存：`session.revoked` 不进消息类型注册表，
/// 因此只在在线连接上飘过，不落进任何会话时间线。
pub(crate) fn cache_clear_envelope(user_id: &str, device_id: &str) -> wxlike_core::Envelope {
    let mut env = crate::system_envelope(
        &format!("sys-{user_id}"),
        "system",
        vec![user_id.to_string()],
        "",
    );
    env.body = wxlike_core::MsgBody::Custom {
        kind: "session.revoked".into(),
        payload: serde_json::json!({
            "device_id": device_id,
            "at_ms": wxlike_core::types::now_ms(),
        }),
    };
    env
}
