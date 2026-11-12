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
//!
//! #245 追加的第 4 条：**凭据先于请求体**。认证有两种落地形式——路由层的 [`gate`]
//! 与 handler 参数上的 [`AdminAuth`] 提取器——它们共用 [`from_parts`]，所以
//! "未认证只拿到 401/403、拿不到 422 里的字段名"这条性质不再依赖每个 handler
//! 是否记得先写一行准入。用例：`crates/admin/tests/g14_01_authn_order.rs`。

use std::fmt;
use std::sync::Arc;

use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::HeaderMap;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::accounts::{AccountStore, Session};
use crate::error::{AdminError, Result};
use crate::http::App;
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

pub(crate) fn constant_time_eq(a: &str, b: &str) -> bool {
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

// ---------------------------------------------------------------------------
// 认证提取器：**凭据先于请求体**（#245 的顺序契约）
// ---------------------------------------------------------------------------

/// 只认 `Authorization: Bearer <x>`，规则只有一份： [`wxlike_types::Bearer`]。
///
/// 这里曾经自己写了一遍 `strip_prefix("Bearer ")`——大小写敏感，于是
/// `Authorization: bearer abc`（RFC 6750 §2.1 下完全合法）在管理面被
/// 判成"没带凭据"，而同一个令牌在平台面是被认出来的。头写法差异不该
/// 改变鉴权结论，所以解析收缩成一处。
pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    let raw = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    wxlike_types::Bearer::parse(raw).map(wxlike_types::Bearer::token)
}

/// 静态令牌对应的"操作者名"。它不是任何一个人的账号名——
/// 写进审计里就是"这次改动来自运维令牌"，追责时别去后台找一个不存在的人。
pub const OPERATOR_ACTOR: &str = "operator-token";

/// 一次请求的**认证**结果："你是不是后台的人"。
///
/// 权限判定不在这里（那是 [`permits`] / [`require`] 与 [`crate::rbac::authorize`]，
/// 永远在服务端、永远在拿到 role 之后）。这个类型只做凭据 → 身份那一段，
/// 所以它能安全地放在 axum 的参数表第一位：
/// **提取器按参数顺序执行，把它写在任何 `ApiJson` 之前，
/// 未认证的请求就一定拿不到"请求体字段名"这类契约信息**（#245）。
///
/// #245 之前这类判定写在 handler 体内第一行，看起来是"先鉴权"，
/// 实际早于它运行的却是 body 提取器——形状上 impossible 的事，编译器不报错。
#[derive(Clone)]
pub struct AdminAuth {
    /// 呈现的凭据原串。只在进程内存里活着：不落审计、不进响应、不进日志。
    /// 留着它是因为登出要按原串删会话。
    credential: String,
    pub audience: Audience,
    /// 会话凭据才有；静态运营令牌没有"人"，所以是 `None`。
    pub session: Option<Session>,
}

/// `Debug` 里必须看不到凭据：调试打印、panic 报告、tracing 的 `{incoming:?}`
/// 都会走到这里，一个不小心就是令牌泄漏。
impl fmt::Debug for AdminAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdminAuth")
            .field("audience", &self.audience)
            .field("credential", &"[presented]")
            .field("session", &self.session)
            .finish()
    }
}

impl AdminAuth {
    /// 呈现的凭据原串。只允许用于"按 token 删除自己的会话"这类内部动作，
    /// 永不写进响应体或审计。
    pub fn credential(&self) -> &str {
        &self.credential
    }

    /// 留痕用的操作者名。静态令牌走 [`OPERATOR_ACTOR`]，不留空串——
    /// 空 actor 的审计行在检索时等于消失。
    pub fn actor(&self) -> &str {
        match &self.session {
            Some(session) => session.actor.as_str(),
            None => OPERATOR_ACTOR,
        }
    }

    /// 这次请求的身份级别。静态令牌按 `viewer` 的读面处理：
    /// 它能看运营面板，但看不到逐人的隐私数据。
    pub fn role(&self) -> Role {
        match &self.session {
            Some(session) => session.role,
            None => Role::Viewer,
        }
    }

    /// 写操作的准入：**必须是后台会话**。
    ///
    /// 静态令牌在这里给 403 而不是一句"没权限"：它对应的不是某个人，
    /// 写不出可追责的留痕，所以要用"登录后再试"这条路纠正。
    pub fn writer(&self) -> Result<&Session> {
        if self.audience == Audience::Operator {
            return Err(AdminError::Forbidden(
                "静态令牌只能读：写操作必须用登录后的会话（POST /api/login）".into(),
            ));
        }
        self.session.as_ref().ok_or(AdminError::SessionExpired)
    }
}

/// 凭据 → 身份。同步版本：给已经在 blocking 上下文里的调用点用
/// （`crate::g14` 的老准入助手就走在这一条上）。
pub fn authenticate(app: &App, credential: Option<&str>) -> Result<AdminAuth> {
    let Some(presented) = credential else {
        return Err(AdminError::Unauthorized);
    };
    let session = app.accounts().session(presented)?;
    let audience = audience_of(presented, &app.config().token, session.is_some());
    require_admin_audience(audience)?;
    Ok(AdminAuth {
        credential: presented.to_string(),
        audience,
        session,
    })
}

/// 认证一次请求的 `Parts`。**唯一的一份凭据→身份判定**：[`gate`]（路由层）与
/// [`AdminAuth`]（handler 参数）都走这里。
///
/// 会碰 SQLite 的那一段整段进 blocking 池（ADR-28），async 上下文里不持连接锁。
pub async fn from_parts(
    parts: &mut Parts,
    state: &Arc<App>,
) -> std::result::Result<AdminAuth, AdminError> {
    // 路由层的准入门（[`gate`]）已经跑过时直接复用它的结论：
    // 一次请求查两遍会话表不是"更保险"，只是多一次锁竞争。
    if let Some(incoming) = parts.extensions.get::<AdminAuth>() {
        return Ok(incoming.clone());
    }
    // 头解析是纯内存操作，留在 async 上下文里；会碰 SQLite 的那一段进池。
    let Some(credential) = bearer(&parts.headers).map(str::to_owned) else {
        return Err(AdminError::Unauthorized);
    };
    admission_in_blocking_pool(state.clone(), credential).await
}

impl FromRequestParts<Arc<App>> for AdminAuth {
    type Rejection = AdminError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<App>,
    ) -> std::result::Result<Self, Self::Rejection> {
        from_parts(parts, state).await
    }
}

/// [`authenticate`] 的 blocking 池版本。
///
/// `JoinError`（池里的任务 panic 或被取消）一律**拒绝**并留日志：
/// 认证结论不可信时没有任何一条路径应该继续往下走。
async fn admission_in_blocking_pool(app: Arc<App>, credential: String) -> Result<AdminAuth> {
    let ran =
        tokio::task::spawn_blocking(move || authenticate(app.as_ref(), Some(&credential))).await;
    match ran {
        Ok(result) => result,
        Err(err) => {
            tracing::error!(%err, "管理面准入任务异常结束（blocking 池 panic 或被取消）");
            Err(AdminError::Admission)
        }
    }
}

/// 管理面**不需要后台凭据**的精确路径。每一条都要写清"为什么可以不认证"：
///
/// * `/`：登录页本身（内联单页，数据一律另走 `/api/*`）。
/// * `/health`：存活探针。探针拿不到令牌是常态。
/// * `/api/login`：换凭据的地方——它要凭据就死循环了。
pub const PUBLIC_PATHS: &[&str] = &["/", "/health", "/api/login"];

/// 路径是否可以不认证就到达。
///
/// 除 [`PUBLIC_PATHS`] 外只有一条例外形状：
/// `GET /api/g14/privacy/requests/{id}/download`。申请者手里只有一次性
/// `download_token`（G14-05），`PrivacyService::claim` 自己验令牌、过期与哈希，
/// 带不上也不该带上后台会话。放它过去是因为"它不是管理动作"，
/// **不是**因为它可以裸奔——用例
/// `g14_01_authn_order::privacy_download_stays_token_bound` 钉住了
/// "没后台凭据也拿不到别人的包"。
///
/// 判定逐条精确，不用"前缀像不像"：`starts_with("/health")` 会连 `/healthz`
/// 一起放行，`starts_with("…/requests/")` 会连同前缀的 `…/{id}/fulfill`
/// （那条必须认证）一起放行。"顺手多放一条"正是这张表要避免的失误。
///
/// 其余路径**默认在门后**：新增管理端点不需要登记任何东西，
/// 就已经有 #245 的顺序保证；反过来，往这里加一行必须同时加一条用例。
pub fn is_public(path: &str) -> bool {
    PUBLIC_PATHS.contains(&path) || is_privacy_download(path)
}

/// `GET /api/g14/privacy/requests/{id}/download`：只放行这一条形状。
fn is_privacy_download(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/api/g14/privacy/requests/") else {
        return false;
    };
    let Some(id) = rest.strip_suffix("/download") else {
        return false;
    };
    !id.is_empty() && !id.contains('/')
}

/// 路由层准入门：**先验凭据，再让任何 body 解析发生**（#245）。
///
/// 为什么还要这一层， handler 里不是已经有一行准入了吗：
/// axum 的参数提取发生在 handler 体之前，所以"写在第一行"的准入
/// 实际上跑在 `ApiJson` 之后——未认证的人于是能拿到 422 里的字段名
/// （#245 就是这么被发现的）。中间件包在路由外面，body 一次都不会被解析。
///
/// 它只做**认证**这一件事（你是谁），不做授权（你能不能）：
/// 授权仍然在服务端的矩阵与 step-up 里，两层各管一段，谁都别想靠对方兜底。
pub async fn gate(
    State(app): State<Arc<App>>,
    request: axum::extract::Request,
    next: middleware::Next,
) -> Response {
    if is_public(request.uri().path()) {
        return next.run(request).await;
    }
    // body 还挂在 `Request` 上，一次都没被读过：这里的判定先于任何解析。
    let (mut parts, body) = request.into_parts();
    let incoming = match from_parts(&mut parts, &app).await {
        Ok(incoming) => incoming,
        Err(err) => return err.into_response(),
    };
    parts.extensions.insert(incoming);
    next.run(axum::extract::Request::from_parts(parts, body))
        .await
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

    // --- #245 顺序契约的三块地基：header 解析、公开面表、认证结论的读法 ---

    /// 头解析与 [`wxlike_types::Bearer`] 逐字一致。
    ///
    /// 钉住的是**合法写法的全集**：scheme 大小写不敏感（RFC 6750 §2.1）、
    /// 分隔符是任意空白、头值两端 OWS 不算凭据。以前这里断言
    /// `"bearer abc"` 为 `None`，那是把当时的实现当成了规范——小写
    /// scheme 是合法的，拒绝它只是"这个客户端换个写法就 401"。
    #[test]
    fn bearer_only_accepts_a_non_empty_bearer_scheme() {
        let mut h = HeaderMap::new();
        assert_eq!(bearer(&h), None);
        for (raw, want) in [
            ("Bearer abc", Some("abc")),
            ("bearer abc", Some("abc")),
            ("BEARER abc", Some("abc")),
            ("Bearer\tabc", Some("abc")),
            ("  Bearer abc  ", Some("abc")),
            ("Bearer    ", None),
            ("Basic abc", None),
            ("Bearer", None),
        ] {
            h.insert(
                axum::http::header::AUTHORIZATION,
                axum::http::HeaderValue::from_static(raw),
            );
            assert_eq!(bearer(&h), want, "{raw:?}");
        }
    }

    /// 公开面只能靠"精确等于"，不能靠前缀像。这条用例的意义在于：
    /// 有人把判定改成 `starts_with` 时，这里必须立刻红。
    #[test]
    fn public_surface_is_exact_and_privacy_download_only_by_shape() {
        for p in [
            "/",
            "/health",
            "/api/login",
            "/api/g14/privacy/requests/req1/download",
        ] {
            assert!(is_public(p), "{p} 应当在门外");
        }
        for p in [
            "/healthz",
            "/api/login/other",
            "/api/g14/privacy/requests",
            "/api/g14/privacy/requests/req1",
            "/api/g14/privacy/requests/req1/fulfill",
            "/api/g14/privacy/requests/req1/download/extra",
            "/api/g14/privacy/requests//download",
            "/api/g14/audit",
            "/api/g14/session/stepup",
            "/api/op/report.act",
            "/api/me/totp/confirm",
        ] {
            assert!(!is_public(p), "{p} 必须在门后");
        }
    }

    // 提取器与 `authenticate` 用的是同一条判定，所以真实装配下的行为
    // （静态令牌 / 会话 / 终端用户令牌三种凭据）放在集成用例里跑：
    // HTTP 形状才是 #245 关心的东西 → `crates/admin/tests/g14_01_authn_order.rs`。
}
