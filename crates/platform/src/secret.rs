//! P4 秘密聊天完整版（合同 §1.3 / §3.2 / ADR-1）。
//!
//! 加密本身在 [`wxlike_core::e2ee`]（vodozemac Olm/Megolm + 乱序窗口 +
//! 重放拒绝）。这一层补的是**产品面**：会话开关、消失消息 TTL、安全码核对。
//!
//! # 三条产品约束
//!
//! 1. **仅 1:1**：合同把 E2EE 范围限定在"1:1 秘密聊天（可选模式）"，群聊
//!    走 Megolm 是另一条路径（`Crypto::encrypt_group`），不在此开关内；
//! 2. **双向一致**：任一方开/关都落到同一个 `conv_id` 行，避免"我以为关了"；
//! 3. **不可转发**：秘密会话的转发/合并转发在发送端就被拒绝——转发会破坏
//!    "两端各持一份密钥"的前提，且消失消息在第三方副本上失效。
//!
//! # 消失消息
//!
//! TTL 存在服务端（会话级），但**删除是两端各自执行的**：服务端不持有明文，
//! 只下发 `ttl_ms`，客户端按消息落库时间 + TTL 到期清除。服务端只负责在
//! 拉历史时过滤掉已过期区间，避免"换设备重装后历史复活"。
//!
//! # 安全码
//!
//! 两端身份公钥排序后取 `sha256`，转成 60 位十进制（Signal safety number
//! 的形态）。用户口头/扫码比对后调用 `verify` 落一条记录；`peer_identity`
//! 一并存下——对方换设备（身份公钥变）时核对状态自动失效。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension as _};
use sha2::{Digest as _, Sha256};

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::extract::ApiJson;
use crate::validate;
use crate::{Platform, PlatformState};

/// 允许的消失消息时长：关 / 30 秒 / 5 分钟 / 1 小时 / 1 天 / 7 天。
pub const TTL_CHOICES_MS: [u64; 6] = [
    0,
    30 * 1000,
    5 * 60 * 1000,
    60 * 60 * 1000,
    24 * 60 * 60 * 1000,
    7 * 24 * 60 * 60 * 1000,
];

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/secret/{conv}", put(configure))
        .route("/api/secret/{conv}", get(state))
        .route("/api/secret/{peer}/safety-number", get(safety_number))
        .route("/api/secret/{peer}/verify", put(verify))
}

#[derive(serde::Deserialize)]
pub struct SecretConfig {
    pub enabled: bool,
    #[serde(default)]
    pub ttl_ms: u64,
}

#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct SecretState {
    pub conv_id: String,
    pub enabled: bool,
    pub ttl_ms: u64,
}

impl Platform {
    /// 开/关秘密聊天并设置消失消息 TTL。
    pub fn set_secret_chat(
        &self,
        user_id: &str,
        conv_id: &str,
        enabled: bool,
        ttl_ms: u64,
    ) -> Result<SecretState> {
        let conv_id = validate::conv(conv_id)?.to_string();
        if !TTL_CHOICES_MS.contains(&ttl_ms) {
            return Err(PlatformError::BadInput("ttl_ms not an allowed choice"));
        }
        // 只有会话参与者能改（直聊 conv_id 形如 d-<a>~<b>）
        self.require_conv_member(user_id, &conv_id)?;
        let c = self.db().lock();
        c.execute(
            "INSERT INTO secret_settings(conv_id, enabled, ttl_ms, updated_at)
             VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(conv_id) DO UPDATE SET
               enabled = excluded.enabled,
               ttl_ms = excluded.ttl_ms,
               updated_at = excluded.updated_at",
            params![
                conv_id,
                i64::from(enabled),
                ttl_ms as i64,
                wxlike_core::types::now_ms() as i64
            ],
        )?;
        Ok(SecretState {
            conv_id,
            enabled,
            ttl_ms,
        })
    }

    pub fn secret_state(&self, conv_id: &str) -> Result<SecretState> {
        let conv_id = validate::conv(conv_id)?.to_string();
        let c = self.db().lock();
        let row: Option<(i64, i64)> = c
            .query_row(
                "SELECT enabled, ttl_ms FROM secret_settings WHERE conv_id = ?1",
                params![conv_id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()?;
        Ok(match row {
            Some((enabled, ttl)) => SecretState {
                conv_id,
                enabled: enabled != 0,
                ttl_ms: ttl.max(0) as u64,
            },
            None => SecretState {
                conv_id,
                enabled: false,
                ttl_ms: 0,
            },
        })
    }

    /// 会话是否处于秘密聊天模式（发送端据此选择密文路径 + 禁止转发）。
    pub fn is_secret_conv(&self, conv_id: &str) -> Result<bool> {
        Ok(self.secret_state(conv_id)?.enabled)
    }

    /// 两端身份公钥 → 60 位安全码。顺序无关（内部排序），换设备即变化。
    pub fn safety_number(&self, a: &str, b: &str) -> Result<String> {
        let (ka, kb) = (self.identity_of(a)?, self.identity_of(b)?);
        let (first, second) = if ka <= kb { (ka, kb) } else { (kb, ka) };
        let mut hasher = Sha256::new();
        hasher.update(first.as_bytes());
        hasher.update(b"|");
        hasher.update(second.as_bytes());
        let digest = hasher.finalize();
        // 30 字节 → 每字节取两个十进制数字，共 60 位
        let mut out = String::with_capacity(60);
        for byte in digest.iter().take(30) {
            out.push(char::from(b'0' + (byte / 10) % 10));
            out.push(char::from(b'0' + (byte % 10)));
        }
        Ok(out)
    }

    fn identity_of(&self, user_id: &str) -> Result<String> {
        let c = self.db().lock();
        c.query_row(
            "SELECT identity FROM prekey_bundles WHERE user_id = ?1",
            params![user_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .ok_or(PlatformError::NotFound)
    }

    /// 记录"我已核对过对方安全码"。`peer_identity` 与当前密钥不一致时拒绝，
    /// 防止拿旧密钥的核对结果冒充新设备。
    pub fn verify_peer(&self, user_id: &str, peer_id: &str, peer_identity: &str) -> Result<()> {
        let current = self.identity_of(peer_id)?;
        if current != peer_identity {
            return Err(PlatformError::BadInput(
                "identity changed since verification",
            ));
        }
        let conv = crate::direct_conv_id(user_id, peer_id);
        let c = self.db().lock();
        c.execute(
            "INSERT INTO secret_verifications(conv_id, user_id, peer_identity, verified_at)
             VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(conv_id, user_id) DO UPDATE SET
               peer_identity = excluded.peer_identity,
               verified_at = excluded.verified_at",
            params![
                conv,
                user_id,
                peer_identity,
                wxlike_core::types::now_ms() as i64
            ],
        )?;
        Ok(())
    }

    /// 核对状态：`Some(at_ms)` 且密钥未变才算已核对。
    pub fn verification_of(&self, user_id: &str, peer_id: &str) -> Result<Option<i64>> {
        let conv = crate::direct_conv_id(user_id, peer_id);
        let current = self.identity_of(peer_id)?;
        let c = self.db().lock();
        let row: Option<(String, i64)> = c
            .query_row(
                "SELECT peer_identity, verified_at FROM secret_verifications
                 WHERE conv_id = ?1 AND user_id = ?2",
                params![conv, user_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()?;
        Ok(match row {
            Some((identity, at)) if identity == current => Some(at),
            _ => None,
        })
    }

    /// 直聊会话必须包含调用者（群会话不允许开秘密模式）。
    pub(crate) fn require_conv_member(&self, user_id: &str, conv_id: &str) -> Result<()> {
        let rest = conv_id
            .strip_prefix("d-")
            .ok_or(PlatformError::BadInput("secret chat is 1:1 only"))?;
        let (a, b) = rest
            .split_once('~')
            .ok_or(PlatformError::BadInput("malformed direct conversation"))?;
        if a == user_id || b == user_id {
            Ok(())
        } else {
            Err(PlatformError::Forbidden)
        }
    }
}

async fn configure(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    ApiJson(body): ApiJson<SecretConfig>,
) -> std::result::Result<Json<SecretState>, ApiError> {
    let state = platform.set_secret_chat(&authed.user_id, &conv, body.enabled, body.ttl_ms)?;
    platform.audit(
        &authed.user_id,
        "secret.configure",
        &conv,
        if body.enabled { "on" } else { "off" },
    );
    Ok(Json(state))
}

async fn state(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
) -> std::result::Result<Json<SecretState>, ApiError> {
    // 读也要校验成员：会话状态本身（是否秘密、TTL 多久）属隐私
    let state = platform.secret_state(&conv)?;
    platform
        .require_conv_member(&authed.user_id, &state.conv_id)
        .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "not a participant"))?;
    Ok(Json(state))
}

#[derive(serde::Serialize)]
struct SafetyReply {
    safety_number: String,
    peer_identity: String,
    verified_at: Option<i64>,
}

async fn safety_number(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(peer): Path<String>,
) -> std::result::Result<Json<SafetyReply>, ApiError> {
    let peer = validate::id(&peer)?.to_string();
    let number = platform.safety_number(&authed.user_id, &peer)?;
    let identity = platform
        .public_identity(&peer)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "peer has no published keys"))?;
    Ok(Json(SafetyReply {
        safety_number: number,
        peer_identity: identity,
        verified_at: platform.verification_of(&authed.user_id, &peer)?,
    }))
}

#[derive(serde::Deserialize)]
struct VerifyBody {
    peer_identity: String,
}

async fn verify(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(peer): Path<String>,
    ApiJson(body): ApiJson<VerifyBody>,
) -> std::result::Result<StatusCode, ApiError> {
    let peer = validate::id(&peer)?.to_string();
    platform.verify_peer(&authed.user_id, &peer, &body.peer_identity)?;
    platform.audit(&authed.user_id, "secret.verify", &peer, "");
    Ok(StatusCode::NO_CONTENT)
}
