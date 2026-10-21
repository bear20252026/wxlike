//! P4 账号找回与人机校验（合同 §3.1「找回+captcha」）。
//!
//! 两件事放一起，因为它们是同一条链路的两个环节：风控要求挑战 → 客户端取
//! 题作答 → 通过后才允许注册/登录/重置。
//!
//! # 恢复码
//!
//! 邮箱找回需要 SMTP 凭据与域名信誉，属部署方资产（合同 1.4 内部测试模式
//! 不接外部邮件）。所以这里实现的是**离线恢复码**：登录态下生成一次性
//! 恢复码（明文只显示一次，库里存 argon2id 摘要），凭"账号名 + 恢复码 +
//! 新密码"重置。重置会吊销该用户全部会话——否则找回等于给攻击者留了后门。
//!
//! # 人机校验
//!
//! 内置的是算术挑战，够挡住脚本化的批量注册，**不是**对抗专业打码平台的
//! 方案；生产接入（极验/Turnstile）只需替换 [`Platform::issue_captcha`]
//! 的题目生成与 [`Platform::solve_captcha`] 的校验，接口形状不变。
//!
//! 三条安全约束：
//! * 答案只存摘要（`sha256`），库被读走也无法直接刷题；
//! * 一次性（`used`）+ 5 分钟过期，重放无效；
//! * 恢复码用 argon2id（与密码同强度），且重置后立即失效。

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use rand::Rng as _;
use rusqlite::{params, OptionalExtension as _};
use sha2::{Digest as _, Sha256};

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::extract::ApiJson;
use crate::validate;
use crate::{Platform, PlatformState};

/// 题目有效期。
pub const CAPTCHA_TTL_MS: u64 = 5 * 60 * 1000;
/// 恢复码有效期（生成后 30 天内可用）。
pub const RECOVERY_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1000;

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/captcha", post(issue_captcha))
        .route("/api/recovery/code", post(issue_recovery))
        .route("/api/recovery/reset", post(reset))
}

fn rank(v: &wxlike_risk::Verdict) -> u8 {
    match v {
        wxlike_risk::Verdict::Allow => 0,
        wxlike_risk::Verdict::Throttle { .. } => 1,
        wxlike_risk::Verdict::Challenge { .. } => 2,
        wxlike_risk::Verdict::Deny { .. } => 3,
    }
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

impl Platform {
    /// 风控闸门：把 [`wxlike_risk::Verdict`] 翻译成平台错误/放行。
    ///
    /// `captcha` 为 `Some((id, answer))` 时，Challenge 分支会真正校验并消费
    /// 题目；缺失则返回 [`PlatformError::CaptchaRequired`]（HTTP 428），
    /// 客户端据此取题重试。
    pub(crate) fn risk_gate(
        &self,
        action: wxlike_risk::Action,
        key: &str,
        captcha: Option<(&str, &str)>,
    ) -> Result<()> {
        self.risk_gate_many(&[(action, key)], captcha)
    }

    /// 多键闸门：登录同时按账号与来源计数，取**最严**的裁决。
    ///
    /// 题目只消费一次——两个键都命中 Challenge 时不能各验一遍（第二次必然
    /// 因为"已使用"失败，正常用户会被自己的正确验证码挡住）。
    pub(crate) fn risk_gate_many(
        &self,
        checks: &[(wxlike_risk::Action, &str)],
        captcha: Option<(&str, &str)>,
    ) -> Result<()> {
        let now = wxlike_core::types::now_ms();
        let mut worst: Option<wxlike_risk::Verdict> = None;
        for (action, key) in checks {
            let v = self.risk().check(*action, key, now)?;
            worst = Some(match (worst, v) {
                (None, v) => v,
                (Some(a), b) => {
                    if rank(&b) > rank(&a) {
                        b
                    } else {
                        a
                    }
                }
            });
        }
        match worst.unwrap_or(wxlike_risk::Verdict::Allow) {
            wxlike_risk::Verdict::Allow => Ok(()),
            wxlike_risk::Verdict::Throttle { retry_after_ms } => {
                Err(PlatformError::RateLimited { retry_after_ms })
            }
            wxlike_risk::Verdict::Deny { .. } => Err(PlatformError::RateLimited {
                retry_after_ms: 60_000,
            }),
            wxlike_risk::Verdict::Challenge { .. } => match captcha {
                Some((id, answer)) => self.solve_captcha(id, answer).map_err(|e| match e {
                    PlatformError::BadInput(_) => PlatformError::CaptchaRequired,
                    other => other,
                }),
                None => Err(PlatformError::CaptchaRequired),
            },
        }
    }

    /// 记一次受控动作（风控计数）。
    pub(crate) fn risk_record(&self, action: wxlike_risk::Action, key: &str) -> Result<()> {
        self.risk()
            .record(action, key, wxlike_core::types::now_ms())?;
        Ok(())
    }

    pub(crate) fn risk_clear(&self, action: wxlike_risk::Action, key: &str) -> Result<()> {
        self.risk().clear(action, key)?;
        Ok(())
    }

    /// 出题：返回 `(challenge_id, question)`。答案只以摘要入库。
    pub fn issue_captcha(&self) -> Result<(String, String)> {
        let mut rng = rand::thread_rng();
        let a: u32 = rng.gen_range(2..=9);
        let b: u32 = rng.gen_range(2..=9);
        let question = format!("{a} + {b} = ?");
        let id = crate::auth::random_hex(8);
        let now = wxlike_core::types::now_ms();
        let c = self.db().lock();
        c.execute(
            "INSERT INTO captcha(id, answer_hash, created_at, expires_at, used)
             VALUES(?1, ?2, ?3, ?4, 0)",
            params![
                id,
                sha256_hex(&(a + b).to_string()),
                now as i64,
                now.saturating_add(CAPTCHA_TTL_MS) as i64
            ],
        )?;
        Ok((id, question))
    }

    /// 校验并消费题目。错误一律 `BadInput`（不区分"过期/已用/答错"，
    /// 避免把题目状态变成探测面）。
    pub fn solve_captcha(&self, id: &str, answer: &str) -> Result<()> {
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        let row: Option<(String, i64, i64)> = c
            .query_row(
                "SELECT answer_hash, expires_at, used FROM captcha WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((hash, expires_at, used)) = row else {
            return Err(PlatformError::BadInput("captcha not found"));
        };
        if used != 0 || expires_at <= now {
            return Err(PlatformError::BadInput("captcha expired"));
        }
        let normalized = answer.trim().to_string();
        if sha256_hex(&normalized) != hash {
            return Err(PlatformError::BadInput("captcha wrong"));
        }
        c.execute("UPDATE captcha SET used = 1 WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// 生成一次性恢复码（明文只返回这一次）。
    pub fn issue_recovery_code(&self, user_id: &str) -> Result<String> {
        let user_id = validate::id(user_id)?.to_string();
        let raw = crate::auth::random_hex(10); // 80 bit
        let code = format!(
            "{}-{}-{}-{}",
            &raw[0..5],
            &raw[5..10],
            &raw[10..15],
            &raw[15..20]
        );
        let hash = crate::security::hash_argon2id(&code)?;
        let c = self.db().lock();
        c.execute(
            "INSERT INTO recovery_codes(user_id, code_hash, created_at, used_at)
             VALUES(?1, ?2, ?3, NULL)
             ON CONFLICT(user_id) DO UPDATE SET
               code_hash = excluded.code_hash,
               created_at = excluded.created_at,
               used_at = NULL",
            params![user_id, hash, wxlike_core::types::now_ms() as i64],
        )?;
        drop(c);
        self.audit(&user_id, "account.recovery.issue", &user_id, "");
        Ok(code)
    }

    /// 凭恢复码重置密码：校验 → 换 argon2id 密码 → 吊销全部会话 → 作废恢复码。
    /// 返回被重置的用户 id（不返回 token：重置后必须重新登录）。
    pub fn reset_password_with_code(
        &self,
        name: &str,
        code: &str,
        new_password: &str,
    ) -> Result<String> {
        let name = validate::name(name)?.to_string();
        validate::password(new_password)?;
        let code = code.trim();
        let user_id = self
            .user_id_by_name(&name)?
            .ok_or(PlatformError::NotFound)?;
        let row: Option<(String, i64, Option<i64>)> = {
            let c = self.db().lock();
            c.query_row(
                "SELECT code_hash, created_at, used_at FROM recovery_codes WHERE user_id = ?1",
                params![user_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                    ))
                },
            )
            .optional()?
        };
        let Some((hash, created_at, used_at)) = row else {
            return Err(PlatformError::Forbidden);
        };
        if used_at.is_some() {
            return Err(PlatformError::Forbidden);
        }
        let now = wxlike_core::types::now_ms() as i64;
        if now.saturating_sub(created_at) > RECOVERY_TTL_MS as i64 {
            return Err(PlatformError::Forbidden);
        }
        if !crate::security::verify_argon2id(code, &hash) {
            self.audit(&user_id, "account.recovery.reset", &user_id, "bad code");
            return Err(PlatformError::Forbidden);
        }

        let phc = crate::security::hash_argon2id(new_password)?;
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        tx.execute(
            "UPDATE users SET pw_algo = 'argon2id', pw_salt = '', pw_hash = ?2, pw_rounds = 0
             WHERE id = ?1",
            params![user_id, phc],
        )?;
        tx.execute("DELETE FROM sessions WHERE user_id = ?1", params![user_id])?;
        tx.execute(
            "UPDATE recovery_codes SET used_at = ?2 WHERE user_id = ?1",
            params![user_id, now],
        )?;
        tx.commit()?;
        drop(c);
        self.audit(&user_id, "account.recovery.reset", &user_id, "ok");
        Ok(user_id)
    }

    /// 是否已设置恢复码（UI 提示"未设置"用）。
    pub fn has_recovery_code(&self, user_id: &str) -> Result<bool> {
        let c = self.db().lock();
        let used: Option<Option<i64>> = c
            .query_row(
                "SELECT used_at FROM recovery_codes WHERE user_id = ?1",
                params![user_id],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()?;
        Ok(matches!(used, Some(None)))
    }
}

async fn issue_captcha(
    State(platform): State<PlatformState>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let (id, question) = platform.issue_captcha()?;
    Ok(Json(serde_json::json!({
        "challenge_id": id,
        "question": question,
        "expires_in_ms": CAPTCHA_TTL_MS,
    })))
}

async fn issue_recovery(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    // 二次确认因子：2FA 已启用时必须带验证码，否则任何拿到 token 的人都能
    // 生成恢复码把账号永久接管。
    if platform.totp_enabled(&authed.user_id)? {
        return Err(ApiError::new(
            StatusCode::PRECONDITION_REQUIRED,
            "totp code required to issue a recovery code",
        ));
    }
    let code = platform.issue_recovery_code(&authed.user_id)?;
    Ok(Json(serde_json::json!({
        "recovery_code": code,
        "expires_in_ms": RECOVERY_TTL_MS,
    })))
}

#[derive(serde::Deserialize)]
pub struct ResetBody {
    pub name: String,
    pub recovery_code: String,
    pub new_password: String,
}

async fn reset(
    State(platform): State<PlatformState>,
    ApiJson(body): ApiJson<ResetBody>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.risk_gate(
        wxlike_risk::Action::Login,
        &format!("recovery:{}", body.name),
        None,
    )?;
    platform.reset_password_with_code(&body.name, &body.recovery_code, &body.new_password)?;
    platform.risk_record(
        wxlike_risk::Action::Login,
        &format!("recovery:{}", body.name),
    )?;
    Ok(StatusCode::NO_CONTENT)
}
