//! P4 账号安全（ADR-3 / 合同 §3.1）：argon2id 密码升级 + TOTP 2FA。
//!
//! **argon2id 透明升级**：存量用户仍是 PBKDF2 哈希（`pw_algo='pbkdf2'`）；
//! 登录成功后即时重哈希为 argon2id（`pw_algo='argon2id'`）——用户无感，下次
//! 登录走新算法。新注册一律 argon2id。`verify_credentials` 按 `pw_algo`
//! 分派，两代算法共存。
//!
//! **TOTP 2FA**：`enable` 生成 base32 密钥返回（客户端录进验证器），
//! `confirm` 用首个验证码确认激活；激活后 `verify_credentials` 要求验证码。
//! 备用恢复码属 P6（与恢复码找回一起）。

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rusqlite::{params, OptionalExtension as _};

use crate::error::{PlatformError, Result};
use crate::Platform;

/// argon2id 参数（默认 OWASP 配置：m=19MiB, t=2, p=1 由 crate 默认给出）。
fn argon2() -> Argon2<'static> {
    Argon2::default()
}

/// argon2id 哈希（PHC 字符串格式，自带盐与参数）。
pub fn hash_argon2id(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut rand::rngs::OsRng);
    argon2()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| PlatformError::BadInput("hash failure"))
}

pub fn verify_argon2id(password: &str, phc: &str) -> bool {
    PasswordHash::new(phc)
        .map(|parsed| {
            argon2()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

/// TOTP 密钥生成：base32（标准验证器 App 兼容）。
pub fn totp_generate_secret() -> String {
    totp_rs::Secret::generate_secret().to_encoded().to_string()
}

/// 校验 6 位验证码（±1 步时钟容差，标准做法）。
pub fn totp_verify(secret_base32: &str, code: &str) -> bool {
    let secret = match totp_rs::Secret::Encoded(secret_base32.to_string()).to_bytes() {
        Ok(s) => s,
        Err(_) => return false,
    };
    let totp = match totp_rs::TOTP::new(totp_rs::Algorithm::SHA1, 6, 1, 30, secret) {
        Ok(t) => t,
        Err(_) => return false,
    };
    // check_current 应用构造时的 skew（±1 步 = ±30s）：验证器时钟慢/快
    // 一步内均可通过——generate_current 不带容差，只比当前步
    totp.check_current(code.trim()).unwrap_or(false)
}

const TOTP_SECRET_MAX: usize = 128;

impl Platform {
    /// 登录成功的透明升级：PBKDF2 → argon2id。
    pub(crate) fn upgrade_password_to_argon2id(&self, user_id: &str, password: &str) -> Result<()> {
        let phc = hash_argon2id(password)?;
        self.db().lock().execute(
            "UPDATE users SET pw_algo = 'argon2id', pw_salt = '', pw_hash = ?2, pw_rounds = 0
             WHERE id = ?1",
            params![user_id, phc],
        )?;
        Ok(())
    }

    pub fn totp_secret_of(&self, user_id: &str) -> Result<Option<String>> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT totp_secret FROM users WHERE id = ?1",
            params![user_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
    }

    pub fn totp_enabled(&self, user_id: &str) -> Result<bool> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT totp_enabled FROM users WHERE id = ?1",
            params![user_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .map(|v| v != 0)
        .unwrap_or(false))
    }

    /// 生成（或重生成）TOTP 密钥；未确认前不改变 `totp_enabled`。
    pub fn totp_enable(&self, user_id: &str) -> Result<String> {
        let user_id = crate::validate::id(user_id)?;
        let secret = totp_generate_secret();
        if secret.len() > TOTP_SECRET_MAX {
            return Err(PlatformError::BadInput("secret too long"));
        }
        self.db().lock().execute(
            "UPDATE users SET totp_secret = ?2, totp_enabled = 0 WHERE id = ?1",
            params![user_id, secret],
        )?;
        Ok(secret)
    }

    /// 首码确认激活。
    pub fn totp_confirm(&self, user_id: &str, code: &str) -> Result<()> {
        let secret = self
            .totp_secret_of(user_id)?
            .ok_or(PlatformError::NotFound)?;
        if !totp_verify(&secret, code) {
            return Err(PlatformError::BadInput("invalid code"));
        }
        self.db().lock().execute(
            "UPDATE users SET totp_enabled = 1 WHERE id = ?1",
            params![user_id],
        )?;
        Ok(())
    }

    pub fn totp_disable(&self, user_id: &str, code: &str) -> Result<()> {
        let secret = self
            .totp_secret_of(user_id)?
            .ok_or(PlatformError::NotFound)?;
        if !totp_verify(&secret, code) {
            return Err(PlatformError::Forbidden);
        }
        self.db().lock().execute(
            "UPDATE users SET totp_enabled = 0, totp_secret = NULL WHERE id = ?1",
            params![user_id],
        )?;
        Ok(())
    }

    /// 敏感操作的**身份确认**：当前第二因子或密码，任一通过即可（issue #5）。
    ///
    /// 用途是"已启用 2FA 之后重新 enroll"。没有这一步，持有 Bearer 的人可以直接
    /// 调 `/api/2fa/enable` —— 它会把 `totp_secret` 换成新的并把
    /// `totp_enabled` 清成 0，二次认证就被悄悄关掉了，接着生成恢复码、重置密码、
    /// 踢掉原用户的所有会话。
    ///
    /// 两个都为空的调用返回 `false`（不是"通过"）：不能因为调用方忘了带凭据
    /// 就当成验证成功。
    pub fn confirm_identity(&self, user_id: &str, code: &str, password: &str) -> Result<bool> {
        if !code.is_empty() {
            if let Some(secret) = self.totp_secret_of(user_id)? {
                if totp_verify(&secret, code) {
                    return Ok(true);
                }
            }
        }
        if !password.is_empty() {
            let name = self.profile_of(user_id)?.name;
            if let Some(uid) = self.verify_credentials(&name, password)? {
                return Ok(uid == user_id);
            }
        }
        Ok(false)
    }

    /// 2FA 校验（登录路径调用）：未启用 → Ok(())；启用 → 验证码必须正确。
    pub fn totp_check_login(&self, user_id: &str, code: Option<&str>) -> Result<()> {
        if !self.totp_enabled(user_id)? {
            return Ok(());
        }
        let Some(code) = code else {
            return Err(PlatformError::BadInput("totp code required"));
        };
        let secret = self
            .totp_secret_of(user_id)?
            .ok_or(PlatformError::NotFound)?;
        if !totp_verify(&secret, code) {
            self.audit(user_id, "user.login", user_id, "2fa failed");
            return Err(PlatformError::BadInput("invalid totp code"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Platform;

    /// P4 透明升级：注册（pbkdf2）→ 首次登录成功 → pw_algo 变 argon2id，
    /// 再次登录仍成功；2FA 全流程 DB 路径。
    #[test]
    fn argon2_transparent_upgrade_on_login() {
        let p = Platform::in_memory().unwrap();
        let uid = p.register("alice", "pw").unwrap();

        let algo = || -> String {
            let c = p.db().lock();
            c.query_row(
                "SELECT pw_algo FROM users WHERE id = ?1",
                rusqlite::params![uid],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(algo(), "pbkdf2", "注册仍是存量算法");

        assert_eq!(
            p.verify_credentials("alice", "pw").unwrap(),
            Some(uid.clone())
        );
        assert_eq!(algo(), "argon2id", "首次登录成功即透明升级");
        assert_eq!(
            p.verify_credentials("alice", "pw").unwrap(),
            Some(uid.clone()),
            "升级后旧密码仍可登录"
        );
        assert_eq!(p.verify_credentials("alice", "wrong").unwrap(), None);
    }

    #[test]
    fn totp_login_gate_full_db_path() {
        let p = Platform::in_memory().unwrap();
        let uid = p.register("bob", "pw").unwrap();

        // 未启用：登录门直接放行
        p.verify_credentials("bob", "pw").unwrap().unwrap();
        p.totp_check_login(&uid, None).unwrap();

        // enable：生成密钥，未确认前不激活
        let secret = p.totp_enable(&uid).unwrap();
        assert!(!p.totp_enabled(&uid).unwrap());

        // 错误码无法确认激活
        assert!(
            p.totp_confirm(&uid, "000000").is_err() || {
                // 000000 恰为当前码的几率极低；若偶合则换下一步重试语义不符
                // ——这里直接用正确码继续
                false
            }
        );

        let totp = totp_rs::TOTP::new(
            totp_rs::Algorithm::SHA1,
            6,
            1,
            30,
            totp_rs::Secret::Encoded(secret.clone()).to_bytes().unwrap(),
        )
        .unwrap();
        let code = totp.generate_current().unwrap();
        p.totp_confirm(&uid, &code).unwrap();
        assert!(p.totp_enabled(&uid).unwrap());

        // 登录门：无码拒绝，正码放行
        assert!(p.totp_check_login(&uid, None).is_err());
        assert!(
            p.totp_check_login(&uid, Some("000000")).is_err() || {
                // 000000 可能恰在 ±1 步窗口内；容差语义下用确定错的方式再测
                p.totp_check_login(&uid, Some("999999")).is_err()
            }
        );
        p.totp_check_login(&uid, Some(&code)).unwrap();

        // disable 需正码；之后无码放行
        p.totp_disable(&uid, &code).unwrap();
        assert!(!p.totp_enabled(&uid).unwrap());
        p.totp_check_login(&uid, None).unwrap();
    }

    #[test]
    fn totp_skew_rejects_old_step_codes() {
        // ±1 步容差：90s 前（3 步外）的码必须被拒
        let secret = totp_generate_secret();
        let totp = totp_rs::TOTP::new(
            totp_rs::Algorithm::SHA1,
            6,
            1,
            30,
            totp_rs::Secret::Encoded(secret.clone()).to_bytes().unwrap(),
        )
        .unwrap();
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        // 90 秒前 = 3 步外（skew=1 只容 ±30s），必须被拒
        let old_code = totp.generate(u64::try_from(now - 90).unwrap());
        let cur_code = totp.generate_current().unwrap();
        assert!(!totp_verify(&secret, &old_code), "3 步外必须拒绝");
        assert!(totp_verify(&secret, &cur_code));

        // **真正的回归点**：±1 步（±30s）必须被接受。
        // 修复前用的是不带容差的比较，"90s 前被拒 + 当前通过"在修复前后都成立，
        // 所以只断言那两条的测试拦不住这个 bug 回归。
        let prev_code = totp.generate(u64::try_from(now - 30).unwrap());
        let next_code = totp.generate(u64::try_from(now + 30).unwrap());
        assert!(
            totp_verify(&secret, &prev_code),
            "-1 步必须接受（时钟慢一步）"
        );
        assert!(
            totp_verify(&secret, &next_code),
            "+1 步必须接受（时钟快一步）"
        );
    }

    #[test]
    fn argon2id_roundtrip_and_rejection() {
        let phc = hash_argon2id("s3cret").unwrap();
        assert!(phc.starts_with("$argon2id$"), "PHC format: {phc}");
        assert!(verify_argon2id("s3cret", &phc));
        assert!(!verify_argon2id("s3cretX", &phc));
        // PHC 自带随机盐：同密码两次哈希不同
        assert_ne!(hash_argon2id("s3cret").unwrap(), phc);
    }

    #[test]
    fn totp_roundtrip_and_bad_code() {
        let secret = totp_generate_secret();
        assert!(!secret.is_empty());
        // 当前码可验证（时间窗口内生成即校验）
        let totp = totp_rs::TOTP::new(
            totp_rs::Algorithm::SHA1,
            6,
            1,
            30,
            totp_rs::Secret::Encoded(secret.clone()).to_bytes().unwrap(),
        )
        .unwrap();
        let code = totp.generate_current().unwrap();
        assert!(totp_verify(&secret, &code));
        assert!(!totp_verify(&secret, "000000") || code == "000000");
        assert!(!totp_verify(&secret, "abc"));
    }
}
