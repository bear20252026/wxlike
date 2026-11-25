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
//! 方案；生产接入（极验/Turnstile）只需替换 `Platform::issue_captcha`
//! 的题目生成与 `Platform::solve_captcha` 的校验，接口形状不变。
//!
//! 三条安全约束：
//! * 答案只存摘要（`sha256`），库被读走也无法直接刷题；
//! * 一次性（`used`）+ 5 分钟过期，重放无效；
//! * 恢复码用 argon2id（与密码同强度），且重置后立即失效。
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::recovery`]。

use crate::error::{PlatformError, Result};
use crate::validate;
use crate::Platform;
use rand::Rng as _;
use sha2::{Digest as _, Sha256};
use wxlike_core::tx::ConnectionTx as _;

use super::repo::recovery::{
    captcha_row, consume_captcha, delete_sessions, insert_captcha, mark_recovery_used,
    recovery_row, recovery_used_at, update_password, upsert_recovery_code,
};

/// 题目有效期。
pub const CAPTCHA_TTL_MS: u64 = 5 * 60 * 1000;

/// 恢复码有效期（生成后 30 天内可用）。
pub const RECOVERY_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1000;
fn rank(v: &wxlike_risk::Verdict) -> u8 {
    match v {
        wxlike_risk::Verdict::Allow => 0,
        wxlike_risk::Verdict::Throttle { .. } => 1,
        wxlike_risk::Verdict::Challenge { .. } => 2,
        wxlike_risk::Verdict::Deny { .. } => 3,
    }
}

/// 两个裁决取更严的那个。
fn stricter(a: wxlike_risk::Verdict, b: wxlike_risk::Verdict) -> wxlike_risk::Verdict {
    if rank(&b) > rank(&a) {
        b
    } else {
        a
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
        let worst = self.worst_verdict(checks)?;
        self.verdict_result(worst.unwrap_or(wxlike_risk::Verdict::Allow), captcha)
    }

    /// 逐个键取裁决，保留排名最高的那个。
    fn worst_verdict(
        &self,
        checks: &[(wxlike_risk::Action, &str)],
    ) -> Result<Option<wxlike_risk::Verdict>> {
        let now = wxlike_core::types::now_ms();
        let mut worst: Option<wxlike_risk::Verdict> = None;
        for (action, key) in checks {
            let v = self.risk().check(*action, key, now)?;
            worst = Some(match worst {
                None => v,
                Some(a) => stricter(a, v),
            });
        }
        Ok(worst)
    }

    /// 把裁决翻译成平台错误；Challenge 在这里消费题目。
    fn verdict_result(
        &self,
        verdict: wxlike_risk::Verdict,
        captcha: Option<(&str, &str)>,
    ) -> Result<()> {
        match verdict {
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
    pub(crate) fn issue_captcha(&self) -> Result<(String, String)> {
        let mut rng = rand::thread_rng();
        let a: u32 = rng.gen_range(2..=9);
        let b: u32 = rng.gen_range(2..=9);
        let question = format!("{a} + {b} = ?");
        let id = crate::auth::random_hex(8);
        let now = wxlike_core::types::now_ms();
        let c = self.db().acquire();
        insert_captcha(
            &c,
            &id,
            &sha256_hex(&(a + b).to_string()),
            now as i64,
            now.saturating_add(CAPTCHA_TTL_MS) as i64,
        )?;
        Ok((id, question))
    }

    /// 校验并消费题目。错误一律 `BadInput`（不区分"过期/已用/答错"，
    /// 避免把题目状态变成探测面）。
    pub(crate) fn solve_captcha(&self, id: &str, answer: &str) -> Result<()> {
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().acquire();
        let row = captcha_row(&c, id)?;
        let Some(row) = row else {
            return Err(PlatformError::BadInput("captcha not found"));
        };
        if row.used != 0 || row.expires_at <= now {
            return Err(PlatformError::BadInput("captcha expired"));
        }
        let normalized = answer.trim().to_string();
        if sha256_hex(&normalized) != row.answer_hash {
            return Err(PlatformError::BadInput("captcha wrong"));
        }
        consume_captcha(&c, id)?;
        Ok(())
    }

    /// 生成一次性恢复码（明文只返回这一次）。
    pub(crate) fn issue_recovery_code(&self, user_id: &str) -> Result<String> {
        let user_id = validate::id(user_id)?.to_string();
        let raw = crate::auth::random_hex(10); // 80 bit
        let code = format!(
            "{}-{}-{}-{}",
            &raw[0..5],
            &raw[5..10],
            &raw[10..15],
            &raw[15..20]
        );
        let hash = super::security::hash_argon2id(&code)?;
        let c = self.db().acquire();
        upsert_recovery_code(&c, &user_id, &hash, wxlike_core::types::now_ms() as i64)?;
        drop(c);
        self.audit(&user_id, "account.recovery.issue", &user_id, "");
        Ok(code)
    }

    /// 凭恢复码重置密码：校验 → 换 argon2id 密码 → 吊销全部会话 → 作废恢复码。
    /// 返回被重置的用户 id（不返回 token：重置后必须重新登录）。
    pub(crate) fn reset_password_with_code(
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
        let row = {
            let c = self.db().acquire();
            recovery_row(&c, &user_id)?
        };
        let Some(row) = row else {
            return Err(PlatformError::Forbidden);
        };
        if row.used_at.is_some() {
            return Err(PlatformError::Forbidden);
        }
        let now = wxlike_core::types::now_ms() as i64;
        if now.saturating_sub(row.created_at) > RECOVERY_TTL_MS as i64 {
            return Err(PlatformError::Forbidden);
        }
        if !super::security::verify_argon2id(code, &row.code_hash) {
            self.audit(&user_id, "account.recovery.reset", &user_id, "bad code");
            return Err(PlatformError::Forbidden);
        }

        let phc = super::security::hash_argon2id(new_password)?;
        let mut c = self.db().lock()?;
        c.with_tx(|tx| -> Result<()> {
            let conn = crate::storage::StorageConnection::sqlite_tx(tx);
            update_password(&conn, &user_id, &phc)?;
            delete_sessions(&conn, &user_id)?;
            mark_recovery_used(&conn, &user_id, now)?;
            Ok(())
        })?;
        drop(c);
        self.audit(&user_id, "account.recovery.reset", &user_id, "ok");
        Ok(user_id)
    }

    /// 是否已设置恢复码（UI 提示"未设置"用）。
    pub(crate) fn has_recovery_code(&self, user_id: &str) -> Result<bool> {
        let c = self.db().acquire();
        let used = recovery_used_at(&c, user_id)?;
        Ok(matches!(used, Some(None)))
    }
}

impl ResetBody {
    pub(crate) fn captcha(&self) -> Option<(&str, &str)> {
        super::captcha_pair(self.captcha_id.as_deref(), self.captcha_answer.as_deref())
    }
}

#[derive(serde::Deserialize)]
pub struct ResetBody {
    pub name: String,
    pub recovery_code: String,
    pub new_password: String,
    /// P4 人机校验：风控要求挑战时必填（`POST /api/captcha` 取题）。
    ///
    /// 以前这个结构体没有这两个字段，于是**一旦风控触发就永远 428**——
    /// 用户拿着正确的恢复码也过不去，只能等窗口过期。加进来才对得起
    /// "失败计数"这条设计：它应该是摩擦，不是锁死。
    #[serde(default)]
    pub captcha_id: Option<String>,
    #[serde(default)]
    pub captcha_answer: Option<String>,
}
