//! WP-01 account endpoints: register / login / me / profile / logout.
//!
//! Response shapes are exactly the contract in `docs/WORK_BREAKDOWN.md` WP-01.
//! `GET /api/profile/{user_id}` returns the intentionally narrow public view
//! (`user_id`, `name`, `avatar`); `signature` is self-only and only appears on
//! `/api/me`.
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::users`]。

use crate::auth::{self};
use crate::domain::repo::users::SqliteUsersRepo;
use crate::error::{PlatformError, Result};
use crate::validate;
use crate::Platform;
use wxlike_persistence::{NewUser, UsersRepo};

impl Credentials {
    pub(crate) fn captcha(&self) -> Option<(&str, &str)> {
        super::captcha_pair(self.captcha_id.as_deref(), self.captcha_answer.as_deref())
    }
}

/// 可选的连接对端地址。
///
/// `axum::extract::ConnectInfo<T>` 在缺失时直接 500，而测试里用
/// `Router::oneshot` 调用根本没有 connect info。这里自己包一层：有就取，
/// 没有就是 `None`（调用方退化为按账号名计数）。
pub(crate) struct PeerAddr(pub(crate) Option<std::net::SocketAddr>);
impl<S> axum::extract::FromRequestParts<S> for PeerAddr
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        Ok(PeerAddr(
            parts
                .extensions
                .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                .map(|c| c.0),
        ))
    }
}

/// 来源标识：**默认只认真实 peer 地址**（issue #19）。
///
/// 以前是无条件读 `x-forwarded-for` / `x-real-ip` / `x-client-id`。这三个头
/// 客户端都能自己填，于是"换一个头值 = 换一个限流桶"——注册与登录失败计数
/// 形同虚设（换号 + 换头可以无限撞库）。
///
/// 现在的规则：
/// - `trust_proxy == false`（默认）：只看连接的对端 IP，转发头**完全不读**；
/// - `trust_proxy == true`：说明部署在受信反向代理后面，才采信
///   `x-forwarded-for` 首段（`x-client-id` 已删除：它不是标准头，没有可信来源）。
///
/// 没有 peer 信息时返回 `None`——**不伪造一个 `"local"` 键**：那会把所有直连
/// 客户端塞进同一个桶。调用方对 `None` 的处理是退化为按账号名计数。
pub(crate) fn client_key(
    peer: Option<std::net::SocketAddr>,
    headers: &axum::http::HeaderMap,
    trust_proxy: bool,
) -> Option<String> {
    if trust_proxy {
        if let Some(v) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            let first = v.split(',').next().unwrap_or("").trim();
            if !first.is_empty() {
                return Some(first.to_string());
            }
        }
    }
    peer.map(|addr| addr.ip().to_string())
}

#[derive(serde::Serialize)]
pub struct AuthReply {
    pub user_id: String,
    pub token: String,
    /// 刷新令牌（G02-01）。注册/登录都会发；老客户端忽略该字段即可。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
}

impl ProfilePatch {
    /// 补丁里出现了哪些字段名（审计用：字段名不是个人数据，值才是）。
    pub fn changed_fields(&self) -> Vec<&'static str> {
        let mut fields = Vec::new();
        if self.name.is_some() {
            fields.push("name");
        }
        if self.avatar.is_some() {
            fields.push("avatar");
        }
        if self.signature.is_some() {
            fields.push("signature");
        }
        fields
    }
}

#[derive(serde::Serialize)]
pub struct Profile {
    pub user_id: String,
    pub name: String,
    pub avatar: String,
    pub signature: String,
}

#[derive(serde::Serialize)]
pub struct PublicProfile {
    pub user_id: String,
    pub name: String,
    pub avatar: String,
}

impl Platform {
    /// Resolves a display name to a user id (used by friend/group endpoints).
    ///
    /// 保留旧语义：**包含已注销账号**（历史调用点依赖"名字仍能解析到 id"）；
    /// 是否允许继续操作由各自的授权路径判断。
    pub fn user_id_by_name(&self, name: &str) -> Result<Option<String>> {
        let c = self.db().lock()?;
        SqliteUsersRepo::new(&c).find_id_by_name(name, true, None)
    }

    /// Creates the user row. All SQL for one logical operation runs under a
    /// single connection guard, so existence-check + insert cannot interleave
    /// with another registrant.
    ///
    /// AR-3/H-3 (audit round 4): hash with argon2id from day one, matching
    /// the documented design ("新注册一律 argon2id") — registration was the
    /// last password-creation path still on PBKDF2, which left every fresh
    /// account on the weaker algorithm until its first login upgraded it.
    pub fn register(&self, name: &str, password: &str) -> Result<String> {
        let name = validate::name(name)?;
        validate::password(password)?;
        let phc = super::security::hash_argon2id(password)?;
        let user_id = auth::random_hex(8);
        let now = wxlike_core::types::now_ms();
        {
            let c = self.db().lock()?;
            let repo = SqliteUsersRepo::new(&c);
            if repo.find_id_by_name(name, true, None)?.is_some() {
                return Err(PlatformError::Conflict("name".into()));
            }
            repo.insert(&NewUser {
                id: user_id.as_str(),
                name,
                pw_salt: "",
                pw_hash: phc.as_str(),
                pw_rounds: 0,
                pw_algo: "argon2id",
                avatar: "",
                signature: "",
                created_at: now as i64,
            })
            .map_err(|e| match e {
                PlatformError::Sqlite(err) if wxlike_core::sql::is_unique_violation(&err) => {
                    PlatformError::Conflict("name".into())
                }
                other => other,
            })?;
        }
        // 审计会重新锁同一个非重入 mutex，所以连接守卫已经在上面的块里释放。
        self.audit(&user_id, "user.register", &user_id, "");
        Ok(user_id)
    }

    /// `Ok(None)` = unknown user or wrong password (callers render 401 without
    /// distinguishing the two, so accounts cannot be enumerated by timing).
    /// P4：登录校验按 `pw_algo` 分派（pbkdf2 存量 / argon2id 新户），成功后
    /// 透明升级到 argon2id（用户无感）。
    pub fn verify_credentials(&self, name: &str, password: &str) -> Result<Option<String>> {
        let name = validate::name(name)?;
        let creds = {
            let c = self.db().lock()?;
            SqliteUsersRepo::new(&c).load_credentials(name)?
        };
        let Some(creds) = creds else {
            return Ok(None);
        };
        // 已注销的账号在端口里已经变成“不存在”：口令校验返回 None，登录回
        // 401 invalid credentials——既不泄露“这个号注销过”，也不给任何凭据
        // 换回会话的机会（#114）。
        let ok = match creds.pw_algo.as_str() {
            "argon2id" => super::security::verify_argon2id(password, &creds.pw_hash),
            _ => {
                let rounds = if creds.pw_rounds <= 0 {
                    auth::DEFAULT_ROUNDS
                } else {
                    creds.pw_rounds as u32
                };
                auth::verify_password(password, &creds.pw_salt, &creds.pw_hash, rounds)?
            }
        };
        if !ok {
            return Ok(None);
        }
        // 透明升级：老算法成功一次即重哈希（失败不阻断登录）
        if creds.pw_algo != "argon2id" {
            if let Err(err) = self.upgrade_password_to_argon2id(&creds.user_id, password) {
                tracing::warn!(user = %creds.user_id, "argon2 upgrade failed: {err}");
            }
        }
        Ok(Some(creds.user_id))
    }

    pub fn profile_of(&self, user_id: &str) -> Result<Profile> {
        let row = {
            let c = self.db().lock()?;
            SqliteUsersRepo::new(&c).load_profile(user_id)?
        };
        let row = row.ok_or(PlatformError::NotFound)?;
        Ok(Profile {
            user_id: user_id.to_string(),
            name: row.name,
            avatar: row.avatar,
            signature: row.signature,
        })
    }

    /// Applies only the fields present in the patch.
    ///
    /// G02-05（#114）的三条收紧：
    ///
    /// * **已注销账号一律拒写**（`deleted_at IS NULL`）：匿名化之后那行只是外键
    ///   锚点，把它改回去等于让一个"已经不存在的账号"重新有名字；
    /// * **先验全部字段、再在一个事务里写**：旧写法逐字段边验边写，`signature`
    ///   畸形时 `name` 已经落库——一次"参数错误"的失败请求留下半套资料；
    /// * 重名检查跳过已注销行，与 `verify_credentials` 的口径一致。
    pub fn update_profile(&self, user_id: &str, patch: &ProfilePatch) -> Result<Profile> {
        {
            let c = self.db().lock()?;
            let repo = SqliteUsersRepo::new(&c);
            if !repo.active_exists(user_id)? {
                return Err(PlatformError::NotFound);
            }
            let name = patch.name.as_ref().map(|v| validate::name(v)).transpose()?;
            let avatar = patch
                .avatar
                .as_ref()
                .map(|v| validate::avatar(v))
                .transpose()?;
            let signature = patch
                .signature
                .as_ref()
                .map(|v| validate::signature(v))
                .transpose()?;
            if let Some(name) = name {
                if repo.find_id_by_name(name, false, Some(user_id))?.is_some() {
                    return Err(PlatformError::Conflict("name".into()));
                }
            }
            // 先验完再写：仓储层用一个事务写三条 UPDATE，避免畸形 signature
            // 留下已经改掉的 name。
            repo.update_profile(user_id, name, avatar, signature)?;
        }
        self.profile_of(user_id)
    }
}

/// 登录/注册的统一回包：顺手挂一条刷新链（G02-01）。
///
/// 刷新链签发失败只记日志——登录本身已经成功，客户端可以退回到
/// "过期重新登录"，不该因为刷新链的问题让登录失败。
pub(crate) fn auth_reply(
    platform: &Platform,
    user_id: String,
    token: String,
    device_id: &str,
) -> AuthReply {
    let refresh_token = mint_refresh(platform, &user_id, &token, device_id);
    AuthReply {
        user_id,
        token,
        refresh_token,
    }
}

/// 为刚建立的访问会话挂一条刷新链（G02-01）。失败只记日志：登录本身已经
/// 成功，客户端可以退回到"过期重新登录"。
fn mint_refresh(
    platform: &Platform,
    user_id: &str,
    token: &str,
    device_id: &str,
) -> Option<String> {
    let sid = match platform.session_sid(token) {
        Ok(Some(sid)) => sid,
        _ => return None,
    };
    match platform.issue_refresh(user_id, &sid, device_id) {
        Ok(raw) => Some(raw),
        Err(err) => {
            eprintln!("[platform] refresh chain not issued: {err:#}");
            None
        }
    }
}

#[derive(serde::Deserialize)]
pub struct Credentials {
    pub name: String,
    pub password: String,
    /// P4 2FA：启用 TOTP 后必填。
    #[serde(default)]
    pub totp_code: Option<String>,
    /// P4 设备管理：设备标识与展示名（缺省时按"未知设备"记）。
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub device_name: String,
    /// P4 人机校验：风控要求挑战时必填（`POST /api/captcha` 取题）。
    #[serde(default)]
    pub captcha_id: Option<String>,
    #[serde(default)]
    pub captcha_answer: Option<String>,
}

#[derive(serde::Deserialize, Default)]
pub struct ProfilePatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub signature: Option<String>,
}
#[cfg(test)]
mod client_key_tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> axum::http::HeaderMap {
        let mut h = axum::http::HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, v.parse().unwrap());
        }
        h
    }

    /// issue #19 的核心：**默认不采信转发头**。
    ///
    /// 直连部署下 `x-forwarded-for` 是客户端自己填的，信它等于"换个头 = 换个
    /// 限流桶"。这条在函数层次断言，比走 HTTP 撞阈值直接得多。
    #[test]
    fn forwarded_headers_are_ignored_unless_trusted() {
        let h = headers(&[
            ("x-forwarded-for", "203.0.113.7, 10.0.0.1"),
            ("x-real-ip", "203.0.113.8"),
            ("x-client-id", "anything"),
        ]);
        let peer: std::net::SocketAddr = "10.0.0.1:5555".parse().unwrap();

        assert_eq!(
            client_key(Some(peer), &h, false).as_deref(),
            Some("10.0.0.1"),
            "默认必须只看真实 peer"
        );
        assert_eq!(
            client_key(Some(peer), &h, true).as_deref(),
            Some("203.0.113.7"),
            "受信代理部署下取 XFF 首段"
        );
    }

    /// 没有 peer 时返回 `None`（调用方退化为按账号名计数），
    /// **不**伪造一个 "local" 键把所有直连客户端塞进同一个桶。
    #[test]
    fn no_peer_means_no_source_key() {
        let h = headers(&[("x-real-ip", "203.0.113.8")]);
        assert_eq!(client_key(None, &h, false), None);
        // 即使带了别的转发头，没有 peer 也不认
        assert_eq!(client_key(None, &h, true), None);
    }
}
