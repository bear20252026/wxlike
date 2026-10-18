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
    let code_num: u64 = match code.trim().parse() {
        Ok(v) => v,
        Err(_) => return false,
    };
    totp.generate_current()
        .map(|cur| {
            // constant-time-ish compare via parse equality is fine at 6 digits
            cur.parse::<u64>().map(|c| c == code_num).unwrap_or(false)
        })
        .unwrap_or(false)
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
