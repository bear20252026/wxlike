//! G14-01 管理身份：**独立 audience + 资源级权限矩阵 + 敏感操作步进验证**。
//!
//! 与既有 [`crate::rbac`] 的分工（不重复、不互相替代）：
//!
//! * `rbac` 管**老操作目录**（D1/D2/D3 的 `OPS`）：一条操作一个能力等级，够用。
//! * 本模块管**运营域**（G14 六个子域）：权限按 `(资源, 动作)` 二维授予。
//!   为什么需要第二维：审核岗要能发小程序包但不能签包，财务岗要能批退款但不能
//!   处置工单——这些差异用"能力等级"表达不了，只能落到资源上。
//!
//! 三条硬边界：
//!
//! 1. **audience 独立**。管理面只认两种凭据：运维静态令牌（只读）与后台会话。
//!    终端用户令牌（`wxu_*`）在这里一律拒绝，并且给出明确原因——不是"密码错"，
//!    而是"你拿的是用户令牌"。
//! 2. **默认拒绝**。矩阵里没有的 `(角色, 资源, 动作)` 一律 false。
//! 3. **敏感动作要 step-up**。`Destructive / ExportPii / SignPackage / ApproveMoney`
//!    四类动作除了角色够，还要求**当场**验过一次动态码，且只换一个短窗口
//!    （默认 15 分钟）。理由：8 小时的会话里，人不总是在屏幕前。

use serde::Serialize;

use crate::accounts::AccountStore;
use crate::error::{AdminError, Result};
use crate::rbac::Role;
use crate::read::now_ms;
use crate::store::Store;

/// 步进验证窗口默认长度。15 分钟：够做完一次处置，又不至于让"验过码"变成
/// 一整天的通行证。
pub const STEP_UP_TTL_MS: i64 = 15 * 60 * 1000;

/// 凭据的 audience。管理面只接受前两种。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Audience {
    /// 运维静态令牌：`WXLIKE_ADMIN_TOKEN`。只读，且不对应任何"人"。
    Operator,
    /// 后台会话（登录后拿到）。写操作的唯一合法来源。
    AdminSession,
    /// 终端用户令牌。管理面**永不**接受。
    EndUser,
    /// 认不出的形状。
    Unknown,
}

impl Audience {
    pub fn as_str(self) -> &'static str {
        match self {
            Audience::Operator => "operator",
            Audience::AdminSession => "admin_session",
            Audience::EndUser => "end_user",
            Audience::Unknown => "unknown",
        }
    }
}

/// 终端用户令牌的前缀约定由 G02 定；这里只做**兜底识别**，用于给出可读的拒绝理由。
/// 就算格式变了，它也只会落到 [`Audience::Unknown`]，仍然进不来。
pub const END_USER_TOKEN_PREFIXES: &[&str] = &["wxu_", "usr_"];

/// 判定一次请求的 audience。`known_admin_session` 由会话表查询结果决定。
pub fn audience_of(presented: &str, operator_token: &str, known_admin_session: bool) -> Audience {
    if !operator_token.is_empty() && constant_time_eq(presented, operator_token) {
        return Audience::Operator;
    }
    if known_admin_session {
        return Audience::AdminSession;
    }
    if END_USER_TOKEN_PREFIXES
        .iter()
        .any(|p| presented.starts_with(p))
    {
        return Audience::EndUser;
    }
    Audience::Unknown
}

/// 管理面准入：终端用户令牌不是"密码错"，是"走错了门"。
pub fn require_admin_audience(audience: Audience) -> Result<()> {
    match audience {
        Audience::Operator | Audience::AdminSession => Ok(()),
        Audience::EndUser => Err(AdminError::Forbidden(
            "这是终端用户令牌：管理 API 使用独立 audience，只接受后台会话（POST /api/login）"
                .into(),
        )),
        Audience::Unknown => Err(AdminError::Unauthorized),
    }
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// 权限矩阵的资源维度。粒度按"运维真的会分给不同的人"来定，
/// 不按数据库表来定——表结构会变，岗位不会天天变。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Resource {
    Audit,
    Identity,
    Device,
    Group,
    Case,
    Content,
    MiniApp,
    Service,
    Merchant,
    Order,
    Recon,
    Journal,
    Job,
    Privacy,
    Health,
    Config,
}

impl Resource {
    pub fn as_str(self) -> &'static str {
        match self {
            Resource::Audit => "audit",
            Resource::Identity => "identity",
            Resource::Device => "device",
            Resource::Group => "group",
            Resource::Case => "case",
            Resource::Content => "content",
            Resource::MiniApp => "mini_app",
            Resource::Service => "service",
            Resource::Merchant => "merchant",
            Resource::Order => "order",
            Resource::Recon => "recon",
            Resource::Journal => "journal",
            Resource::Job => "job",
            Resource::Privacy => "privacy",
            Resource::Health => "health",
            Resource::Config => "config",
        }
    }

    pub const ALL: &'static [Resource] = &[
        Resource::Audit,
        Resource::Identity,
        Resource::Device,
        Resource::Group,
        Resource::Case,
        Resource::Content,
        Resource::MiniApp,
        Resource::Service,
        Resource::Merchant,
        Resource::Order,
        Resource::Recon,
        Resource::Journal,
        Resource::Job,
        Resource::Privacy,
        Resource::Health,
        Resource::Config,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Read,
    Write,
    Destructive,
    /// 导出含个人信息的报表。独立动作，不与 `Read` 混在一起。
    ExportPii,
    /// 给小程序包签名。审核岗有 `Write` 也不能签。
    SignPackage,
    /// 放行资金（退款/提现/补账）。
    ApproveMoney,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Read => "read",
            Action::Write => "write",
            Action::Destructive => "destructive",
            Action::ExportPii => "export_pii",
            Action::SignPackage => "sign_package",
            Action::ApproveMoney => "approve_money",
        }
    }

    /// 这四类动作除了角色够，还要当场验一次动态码。
    pub fn needs_step_up(self) -> bool {
        matches!(
            self,
            Action::Destructive | Action::ExportPii | Action::SignPackage | Action::ApproveMoney
        )
    }
}

/// 只读集合：值班/审计岗能看的面。隐私请求单列在 [`Role::Moderator`] 之上，
/// 因为它逐条对应到一个真实用户的账号处置。
const READ_EVERYWHERE_EXCEPT_PRIVACY: &[Resource] = &[
    Resource::Audit,
    Resource::Identity,
    Resource::Device,
    Resource::Group,
    Resource::Case,
    Resource::Content,
    Resource::MiniApp,
    Resource::Service,
    Resource::Merchant,
    Resource::Order,
    Resource::Recon,
    Resource::Journal,
    Resource::Job,
    Resource::Health,
    Resource::Config,
];

/// **唯一的权限矩阵**。加一个资源/动作 = 在这里加一条分支 + 补一条用例。
pub fn permits(role: Role, resource: Resource, action: Action) -> bool {
    if role == Role::Superadmin {
        return true;
    }
    match (role, action) {
        (Role::Viewer, Action::Read) => READ_EVERYWHERE_EXCEPT_PRIVACY.contains(&resource),
        (Role::Moderator, Action::Read) => {
            READ_EVERYWHERE_EXCEPT_PRIVACY.contains(&resource) || resource == Resource::Privacy
        }
        (Role::Moderator, Action::Write) => matches!(
            resource,
            Resource::Privacy | Resource::Job | Resource::Case | Resource::Content
        ),
        (Role::Moderator, Action::Destructive) => matches!(
            resource,
            Resource::Identity
                | Resource::Device
                | Resource::Group
                | Resource::Content
                | Resource::Case
        ),
        (Role::Publisher, Action::Read) | (Role::Publisher, Action::Write) => matches!(
            resource,
            Resource::Content | Resource::MiniApp | Resource::Service | Resource::Config
        ),
        (Role::Publisher, Action::SignPackage) => resource == Resource::MiniApp,
        (Role::Finance, Action::Read) | (Role::Finance, Action::Write) => matches!(
            resource,
            Resource::Merchant | Resource::Order | Resource::Recon | Resource::Journal
        ),
        (Role::Finance, Action::ApproveMoney) => {
            matches!(
                resource,
                Resource::Order | Resource::Recon | Resource::Journal
            )
        }
        _ => false,
    }
}

/// 矩阵条目的可读形式，给 `GET /api/g14/auth/matrix` 与前台按钮显隐用。
#[derive(Clone, Debug, Serialize)]
pub struct Grant {
    pub role: &'static str,
    pub resource: &'static str,
    pub action: &'static str,
    pub step_up: bool,
}

pub fn grants() -> Vec<Grant> {
    let roles = [
        Role::Viewer,
        Role::Moderator,
        Role::Publisher,
        Role::Finance,
        Role::Superadmin,
    ];
    let actions = [
        Action::Read,
        Action::Write,
        Action::Destructive,
        Action::ExportPii,
        Action::SignPackage,
        Action::ApproveMoney,
    ];
    let mut out = Vec::new();
    for role in roles {
        for resource in Resource::ALL {
            for action in actions {
                if permits(role, *resource, action) {
                    out.push(Grant {
                        role: role.as_str(),
                        resource: resource.as_str(),
                        action: action.as_str(),
                        step_up: action.needs_step_up(),
                    });
                }
            }
        }
    }
    out
}

/// 准入判定。拒绝消息必须带上"该找谁"：只说"没权限"等于没说。
pub fn require(role: Role, resource: Resource, action: Action) -> Result<()> {
    if permits(role, resource, action) {
        return Ok(());
    }
    let fix = match resource {
        Resource::MiniApp if action == Action::SignPackage => "publisher",
        Resource::Identity | Resource::Device | Resource::Group | Resource::Case
            if action != Action::Read =>
        {
            "moderator"
        }
        Resource::Order | Resource::Recon | Resource::Journal => "finance",
        Resource::Content => "moderator",
        Resource::MiniApp | Resource::Service | Resource::Config => "publisher",
        Resource::Privacy => "moderator",
        _ => "superadmin",
    };
    Err(AdminError::Forbidden(format!(
        "角色 {} 不能对 {} 执行 {}（让 superadmin 建一个 --role {fix} 的账号，\
         或用具备该权限的账号操作）",
        role.as_str(),
        resource.as_str(),
        action.as_str()
    )))
}

/// 步进验证窗口。
#[derive(Clone)]
pub struct StepUp {
    store: Store,
    ttl_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct StepUpState {
    pub actor: String,
    pub verified_at_ms: i64,
    pub valid_until_ms: i64,
    pub ttl_ms: i64,
    pub method: String,
}

impl StepUp {
    pub fn new(store: Store) -> Self {
        Self::with_ttl(store, STEP_UP_TTL_MS)
    }

    pub fn with_ttl(store: Store, ttl_ms: i64) -> Self {
        Self {
            store,
            ttl_ms: ttl_ms.max(1),
        }
    }

    pub fn ttl_ms(&self) -> i64 {
        self.ttl_ms
    }

    /// 验码并打开窗口。验不过不写库——失败也不留"半开"的窗口。
    pub fn verify(&self, accounts: &AccountStore, actor: &str, code: &str) -> Result<StepUpState> {
        if code.trim().is_empty() {
            return Err(AdminError::MfaRequired(
                "敏感操作需要动态码：请求体带 {\"code\":\"6位数字\"}".into(),
            ));
        }
        accounts.verify_second_factor(actor, code.trim())?;
        let now = now_ms();
        self.store.lock().execute(
            "INSERT INTO g14_stepup (account_id, verified_at_ms, method) VALUES (?1, ?2, 'totp') \
             ON CONFLICT(account_id) DO UPDATE SET verified_at_ms = excluded.verified_at_ms, \
             method = excluded.method",
            rusqlite::params![actor, now],
        )?;
        Ok(StepUpState {
            actor: actor.to_string(),
            verified_at_ms: now,
            valid_until_ms: now + self.ttl_ms,
            ttl_ms: self.ttl_ms,
            method: "totp".into(),
        })
    }

    /// 当前窗口（可能已过期，过期由 [`StepUp::require`] 判定）。
    pub fn state(&self, actor: &str) -> Result<Option<StepUpState>> {
        let conn = self.store.lock();
        let row = conn
            .query_row(
                "SELECT verified_at_ms, method FROM g14_stepup WHERE account_id = ?1",
                [actor],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .ok();
        Ok(row.map(|(verified_at_ms, method)| StepUpState {
            actor: actor.to_string(),
            verified_at_ms,
            valid_until_ms: verified_at_ms + self.ttl_ms,
            ttl_ms: self.ttl_ms,
            method,
        }))
    }

    /// 敏感动作的闸门。过期消息要说清"还剩多久 / 怎么续"。
    pub fn require(&self, actor: &str, action: Action) -> Result<()> {
        if !action.needs_step_up() {
            return Ok(());
        }
        let Some(state) = self.state(actor)? else {
            return Err(AdminError::MfaRequired(format!(
                "{} 是敏感动作，需要先做步进验证：POST /api/g14/session/stepup {{\"code\":\"…\"}}\
                 （窗口 {} 分钟）",
                action.as_str(),
                self.ttl_ms / 60_000
            )));
        };
        let now = now_ms();
        if state.valid_until_ms <= now {
            return Err(AdminError::MfaRequired(format!(
                "步进验证已过期（{} 秒前验的码）：重新 POST /api/g14/session/stepup",
                (now - state.verified_at_ms) / 1000
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rbac::Role;

    #[test]
    fn end_user_tokens_are_refused_with_a_different_reason_than_bad_tokens() {
        assert_eq!(
            audience_of("wxu_abc", "ops-secret", false),
            Audience::EndUser
        );
        assert_eq!(
            audience_of("usr_abc", "ops-secret", false),
            Audience::EndUser
        );
        assert_eq!(
            audience_of("ops-secret", "ops-secret", false),
            Audience::Operator
        );
        assert_eq!(
            audience_of("sess", "ops-secret", true),
            Audience::AdminSession
        );
        assert_eq!(audience_of("sess", "ops-secret", false), Audience::Unknown);

        let err = require_admin_audience(Audience::EndUser).unwrap_err();
        assert!(err.to_string().contains("独立 audience"), "{err}");
        let err = require_admin_audience(Audience::Unknown).unwrap_err();
        assert!(matches!(err, AdminError::Unauthorized), "{err:?}");
        assert!(require_admin_audience(Audience::Operator).is_ok());
        assert!(require_admin_audience(Audience::AdminSession).is_ok());
    }

    #[test]
    fn read_only_role_cannot_write_anything() {
        for resource in Resource::ALL {
            assert!(
                permits(Role::Viewer, *resource, Action::Read) || *resource == Resource::Privacy
            );
            assert!(
                !permits(Role::Viewer, *resource, Action::Write),
                "{resource:?}"
            );
            assert!(!permits(Role::Viewer, *resource, Action::Destructive));
            assert!(!permits(Role::Viewer, *resource, Action::ExportPii));
        }
        let err = require(Role::Viewer, Resource::Identity, Action::Write).unwrap_err();
        assert!(err.to_string().contains("moderator"), "{err}");
    }

    #[test]
    fn duty_separation_between_review_and_signing() {
        // 发布岗能进审核队列、能配服务卡片
        assert!(permits(Role::Publisher, Resource::Content, Action::Write));
        assert!(permits(Role::Publisher, Resource::Service, Action::Write));
        // 但签包是独立动作
        assert!(permits(
            Role::Publisher,
            Resource::MiniApp,
            Action::SignPackage
        ));
        assert!(!permits(
            Role::Moderator,
            Resource::MiniApp,
            Action::SignPackage
        ));
        // 财务碰不到内容与身份
        assert!(!permits(Role::Finance, Resource::Identity, Action::Write));
        assert!(!permits(Role::Finance, Resource::Content, Action::Write));
        // 审核岗碰不到资金放行
        assert!(!permits(
            Role::Moderator,
            Resource::Order,
            Action::ApproveMoney
        ));
        assert!(permits(
            Role::Finance,
            Resource::Order,
            Action::ApproveMoney
        ));
        // 导出个人信息只有 superadmin
        for role in [
            Role::Viewer,
            Role::Moderator,
            Role::Publisher,
            Role::Finance,
        ] {
            assert!(
                !permits(role, Resource::Privacy, Action::ExportPii),
                "{role:?}"
            );
        }
        assert!(permits(
            Role::Superadmin,
            Resource::Privacy,
            Action::ExportPii
        ));
    }

    #[test]
    fn new_roles_get_no_legacy_write_capability() {
        use crate::rbac::Capability;
        for role in [Role::Publisher, Role::Finance] {
            assert!(role.allows(Capability::Read));
            assert!(
                !role.allows(Capability::Write),
                "{role:?} 不该能跑 D1/D2/D3 的写操作"
            );
            assert!(!role.allows(Capability::Destructive));
        }
        assert_eq!(Role::parse("finance"), Some(Role::Finance));
        assert_eq!(Role::parse("publisher"), Some(Role::Publisher));
        assert_eq!(Role::parse("Publisher"), Some(Role::Publisher));
    }

    #[test]
    fn every_step_up_action_is_also_a_matrix_entry() {
        // 敏感动作必须至少有一个角色能做，否则这条动作是死代码
        for action in [
            Action::Destructive,
            Action::ExportPii,
            Action::SignPackage,
            Action::ApproveMoney,
        ] {
            assert!(action.needs_step_up());
            assert!(
                grants()
                    .iter()
                    .any(|g| g.action == action.as_str() && g.step_up),
                "{} 没有任何角色能做",
                action.as_str()
            );
        }
        assert!(!Action::Read.needs_step_up());
    }

    #[test]
    fn step_up_window_expires_and_must_be_renewed() {
        let store = Store::open_memory().unwrap();
        let step = StepUp::with_ttl(store.clone(), 1_000);
        let action = Action::SignPackage;

        // 没验过：拒绝，并给出续期办法
        let err = step.require("ops", action).unwrap_err();
        assert!(err.to_string().contains("/api/g14/session/stepup"), "{err}");
        // 非敏感动作不需要窗口
        assert!(step.require("ops", Action::Read).is_ok());

        // 手工写入一个已过期的窗口（不依赖真实 TOTP）
        store
            .lock()
            .execute(
                "INSERT INTO g14_stepup (account_id, verified_at_ms) VALUES ('ops', ?1)",
                rusqlite::params![now_ms() - 2_000],
            )
            .unwrap();
        let err = step.require("ops", action).unwrap_err();
        assert!(err.to_string().contains("过期"), "{err}");
        assert!(step.state("ops").unwrap().is_some());

        // 窗口内的判定
        store
            .lock()
            .execute(
                "UPDATE g14_stepup SET verified_at_ms = ?1 WHERE account_id = 'ops'",
                rusqlite::params![now_ms()],
            )
            .unwrap();
        assert!(step.require("ops", action).is_ok());
    }

    #[test]
    fn empty_code_never_opens_a_window() {
        let store = Store::open_memory().unwrap();
        let tmp = crate::tmp::TmpDir::create("auth-stepup");
        let accounts = AccountStore::open(&tmp.join("adminx.db")).unwrap();
        let step = StepUp::new(store);
        let err = step.verify(&accounts, "ops", "  ").unwrap_err();
        assert!(err.to_string().contains("动态码"), "{err}");
        assert!(step.state("ops").unwrap().is_none());
    }
}
