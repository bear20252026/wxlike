//! 扫码登录的 HTTP 传输层（G02-07，审计第 5 轮挂载）。
//!
//! 分层与 crate 约定一致：**规则在 `crates/features/identity/src/qr_login.rs`
//! 只实现一次**（状态机、PKCE、原子抢占），本模块只做参数解析、鉴权主体
//! 透传与 JSON 形状——identity 没有 axum 依赖，HTTP 面归平台。
//!
//! # 路由契约（客户端桥按这套写，改动即破坏）
//!
//! | 方法 | 路径 | 鉴权 | 语义 |
//! |------|------|------|------|
//! | POST | `/api/qr/tickets` | Bearer（创建者） | 建票，返回 `{ticket_id, secret, payload, expires_at_ms}`，`payload = wxlike-qr://{ticket_id}/{secret}` 是二维码内容 |
//! | GET  | `/api/qr/tickets/{id}` | Bearer 创建者 | 轮询 `{state, account_hint?}`，state ∈ CREATED \| SCANNED \| APPROVED \| EXPIRED \| REJECTED |
//! | POST | `/api/qr/tickets/{id}/scan` | Bearer 扫码者 | body `{secret}`：核验 secret、推进到 SCANNED、回设备线索 |
//! | POST | `/api/qr/tickets/{id}/approve` | Bearer 扫码者 | 批准 |
//! | POST | `/api/qr/tickets/{id}/reject` | Bearer 扫码者 | 拒绝 |
//! | POST | `/api/qr/tickets/{id}/exchange` | **匿名**（这就是要登录的那台设备）| body `{secret}`：APPROVED 后一次性兑换，返回创建者账号的真实会话 `{token, user_id, name}` |
//!
//! # 与 identity 状态机的两点契约性偏差（都记在这里，防止"改回去"）
//!
//! 1. **兑换发给创建者的账号**。`qr_login::exchange` 的语义是"新设备登录成
//!    扫码者的账号"（WeChat 流：桌面出码、手机扫码）。本契约相反：创建者
//!    已认证、想让**另一台设备**以自己身份登录，所以兑换发的是创建者会话。
//!    抢占式 `approved → consumed` CAS 与 `qr_login::exchange` 同一形状，
//!    只是会话签发对象不同——核心状态机没有被改写。
//! 2. **PKCE verifier 由服务端持有**（`qr_ticket_creator.verifier`）。
//!    契约的轮询/兑换不带 verifier，桌面端拿到的是 `secret`（进二维码）。
//!    verifier 留在服务端保证：轮询（GET）只对创建者开放；兑换仍以持有
//!    secret 为凭——这是契约接受的边界，别把它当漏洞修掉，改契约要先改桥。
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::qr_login`]。

use crate::error::ApiError;
use crate::Platform;
use axum::body::Bytes;
use axum::http::StatusCode;
use axum::Json;
use rusqlite::{params, OptionalExtension as _};
use wxlike_identity::qr_login::{PollState, ScanView};

use super::repo::qr_login::{check_qr_state, locate_ticket_by_secret, ticket_creator};

/// QR 票据 secret 的十六进制长度（与 identity::qr_login 生成侧一致）。
pub(crate) const QR_SECRET_HEX_LEN: usize = 32;

/// 建票 body 的三个可选字段。
///
/// 契约没规定建票的 body：空 body 与 `{}` 等价；每个字段里**只有空白**也算没填，
/// 各自回落到默认值（`device_id` 回落到随机 `qr-<hex>`，platform 回落 `web`，
/// label 回落 `QR login`）。这段规范化单独成函数，是为了让 handler 留在
/// 质量闸门的 60 行线以内——不是新增行为，行为与内联时逐字等价。
pub(crate) fn parse_create_body(body: &Bytes) -> std::result::Result<TicketPrefs, ApiError> {
    let parsed: CreateBody = if body.is_empty() {
        CreateBody::default()
    } else {
        serde_json::from_slice(body)
            .map_err(|err| ApiError::bad_request(format!("invalid ticket body: {err}")))?
    };
    let fallback = |raw: Option<String>, default: &str| {
        let raw = raw.unwrap_or_default();
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            default.to_string()
        } else {
            trimmed.to_string()
        }
    };
    Ok(TicketPrefs {
        device_id: match parsed.device_id {
            Some(ref raw) if !raw.trim().is_empty() => raw.trim().to_string(),
            _ => format!("qr-{}", crate::auth::random_hex(8)),
        },
        platform: fallback(parsed.platform, "web"),
        label: fallback(parsed.label, "QR login"),
    })
}

/// `parse_create_body` 的产物：三个都已规范化的字符串。
pub(crate) struct TicketPrefs {
    pub(crate) device_id: String,
    pub(crate) platform: String,
    pub(crate) label: String,
}

pub(crate) fn consume_qr_ticket(
    platform: &Platform,
    ticket_id: &str,
    secret: &str,
    now: i64,
) -> std::result::Result<(String, String, String), ApiError> {
    let conn = platform.db().lock();
    let id = locate_ticket_by_secret(&conn, secret)?;
    secret_must_match(ticket_id, &id)?;
    let creator = ticket_creator(&conn, &id)?;

    let (state, expires_at, device_id, label): (String, i64, String, String) = conn
        .query_row(
            "SELECT state, expires_at, device_id, label FROM identity_qr WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(db_err)?
        .ok_or_else(|| ApiError::not_found("no such ticket"))?;
    check_qr_state(&conn, &id, &state, expires_at, now)?;

    let consumed = conn
        .execute(
            "UPDATE identity_qr SET state = 'consumed', consumed_at = ?2 \
             WHERE id = ?1 AND state = 'approved'",
            params![id, now],
        )
        .map_err(db_err)?;
    if consumed != 1 {
        return Err(ApiError::conflict("ticket already used"));
    }
    Ok((creator, device_id, label))
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------
pub(crate) fn secret_must_match(
    path_id: &str,
    secret_id: &str,
) -> std::result::Result<(), ApiError> {
    if path_id == secret_id {
        Ok(())
    } else {
        // 路径与 secret 指向不同的票 = 调用方拼错了；宁可 400 也不猜
        Err(ApiError::bad_request("secret does not match this ticket"))
    }
}

pub(crate) fn scan_json(view: &ScanView) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ticket_id": view.challenge_id,
        "state": "SCANNED",
        "platform": view.platform,
        "label": view.label,
        "ip_hint": view.ip_hint,
    }))
}

/// 契约的 state 名 ←→ `qr_login::PollState`。
///
/// `WAITING_APPROVAL` 在 identity 的状态机里没有独立态（scanned 即等待批准），
/// 契约集合里保留这个名字，映射取更直接的 `SCANNED`。
pub(crate) fn state_str(state: PollState) -> &'static str {
    match state {
        PollState::Pending => "CREATED",
        PollState::Scanned => "SCANNED",
        PollState::Approved => "APPROVED",
        PollState::Rejected => "REJECTED",
        PollState::Expired => "EXPIRED",
    }
}

/// identity 错误 → HTTP。`Display` 不含凭据（identity 的约定），可以进消息；
/// 只有 Db 分支换 generic 并落日志。
pub(crate) fn id_err(err: wxlike_identity::IdError) -> ApiError {
    use wxlike_identity::IdError;
    match err {
        IdError::BadInput(what) => ApiError::unprocessable(format!("bad input: {what}")),
        IdError::Unauthorized(what) => ApiError::unauthorized(format!("unauthorized: {what}")),
        IdError::Forbidden(what) => ApiError::forbidden(format!("forbidden: {what}")),
        IdError::NotFound(what) => ApiError::not_found(format!("no such {what}")),
        IdError::Conflict(what) => ApiError::conflict(what),
        IdError::Expired => ApiError::new(StatusCode::GONE, "ticket expired"),
        IdError::RateLimited { retry_after_ms } => ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            format!("rate limited, retry after {retry_after_ms} ms"),
        ),
        IdError::Db(err) => db_err(err),
        // 新增于 #114：库里读不出合法状态。500，细节只进日志。
        IdError::Corrupt(what) => {
            tracing::error!(row = %what, "qr login: corrupt stored row");
            ApiError::internal("storage failure")
        }
        IdError::Json(err) => {
            tracing::error!(error = %err, "qr login: encoding failure");
            ApiError::internal("encoding failure")
        }
    }
}

pub(crate) fn db_err(err: rusqlite::Error) -> ApiError {
    tracing::error!(error = %err, "qr login: database failure");
    ApiError::internal("database failure")
}

impl Platform {
    /// 用户显示名（兑换回包 `{name}` 用；用户不存在时退回 id 本身）。
    pub fn display_name_of(&self, user_id: &str) -> crate::error::Result<String> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT name FROM users WHERE id = ?1",
            params![user_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .unwrap_or_else(|| user_id.to_string()))
    }
}

/// 建票时可带的设备线索（都可省，省了用默认值）。
#[derive(serde::Deserialize, Default)]
#[serde(default)]
pub(crate) struct CreateBody {
    pub(crate) device_id: Option<String>,
    pub(crate) platform: Option<String>,
    pub(crate) label: Option<String>,
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_state_names_match_the_contract() {
        assert_eq!(state_str(PollState::Pending), "CREATED");
        assert_eq!(state_str(PollState::Scanned), "SCANNED");
        assert_eq!(state_str(PollState::Approved), "APPROVED");
        assert_eq!(state_str(PollState::Rejected), "REJECTED");
        assert_eq!(state_str(PollState::Expired), "EXPIRED");
    }
}
