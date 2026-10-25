//! 会话授权与资源 scope：**MCP/CLI 不是第二个授权中心**，而是同一条链的前哨。
//!
//! 这一层的职责只有四件，任何一件失效都会变成真实的越权：
//!
//! 1. **audience 必须是 identity-v1 的枚举值**（`user` / `admin`）：
//!    拿到"发给别的服务"的令牌直接透传，是最常见的 MCP 事故（token passthrough）；
//! 2. **主体与会话绑定**：`sub` 是 actor，`sid` 是登录会话；进程启动时绑定的
//!    会话必须与令牌一致，跨用户复用会话 id 一律拒；
//! 3. **scope 只收窄、不放大**：有效 scope = 令牌 scope ∩ 策略授予；
//!    代理链（agent 代表用户）只能衰减，不能加长；
//! 4. **资源再校验**：每个会话/内容/媒体资源都按 scope 逐个匹配，
//!    不是"有会话 scope 就能操作所有会话"。
//!
//! 服务端仍然会**再次**鉴权——这里做的是"别把注定被拒的请求发出去"，
//! 以及"别把不该进日志的东西写进日志"（[`Secret`] 的 Debug 只打指纹）。

use serde_json::Value;

use crate::catalog::{Capability, CommandSpec, ScopeClass};
use crate::policy::glob_match;

/// 令牌受众：与 `contracts/domains/identity/contract.json`（identity-v1）的
/// `audience` 枚举一字不差——`user` / `admin`。
///
/// 这里**不**自造 `wxlike-mcp` 这种受众值：受众是签发方的枚举，
/// 消费方另立一个值，等于把"谁签的"这条线索切断。MCP 防透传靠的是
/// "audience 合法 + 会话绑定 + scope 收窄"三件套，不是靠改名。
pub const AUDIENCE_USER: &str = "user";
/// 管理员受众：签发方给后台会话的值。普通 agent 拿到它也不代表能调管理动作，
/// 还要看 `roles` 是否含 `admin`（见 [`ActorContext::is_admin`]）。
pub const AUDIENCE_ADMIN: &str = "admin";

/// 会打印但**不泄露内容**的字符串（令牌、密钥、口令）。
///
/// 只实现 `Debug`/`Display` 的脱敏形态；取值必须显式 [`Secret::expose`]，
/// 这样"顺手 `{:?}` 一下"不会把 token 写进日志或错误消息。
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// 显式取明文。调用点应当只有"构造 HTTP 头"这一处。
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// 指纹：前 6 位 + 长度。足够在日志里对齐"是不是同一把令牌"，不可反推。
    pub fn fingerprint(&self) -> String {
        let head: String = self.0.chars().take(6).collect();
        format!("{head}…({} chars)", self.0.chars().count())
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret({})", self.fingerprint())
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.fingerprint())
    }
}

/// 一个 scope 条目：`类别:选择器`，例如 `conv:g-*`、`media:*`、`admin:*`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub class: ScopeClass,
    pub selector: String,
}

impl Scope {
    /// 解析一个条目。类别不认识就返回 `None`（**不猜**，猜错等于放权）。
    pub fn parse(raw: &str) -> Option<Self> {
        let (class, selector) = raw.split_once(':')?;
        let class = match class {
            "account" => ScopeClass::Account,
            "conv" => ScopeClass::Conversation,
            "content" => ScopeClass::Content,
            "media" => ScopeClass::Media,
            "payment" => ScopeClass::Payment,
            "admin" => ScopeClass::Admin,
            _ => return None,
        };
        let selector = selector.trim();
        if selector.is_empty() {
            return None;
        }
        Some(Self {
            class,
            selector: selector.to_string(),
        })
    }

    /// 规范写法：`conv:g-1`、`media:*`。错误消息与摘要都用这个形态，
    /// 与令牌里的 `scope` 字段逐字对齐，人才对得上。
    pub fn as_token(&self) -> String {
        format!("{}:{}", short_class(self.class), self.selector)
    }

    fn matches(&self, class: ScopeClass, resource: &str) -> bool {
        self.class == class && glob_match(&self.selector, resource)
    }
}

/// 类别在令牌里的短名（`Conversation` → `conv`）。
fn short_class(class: ScopeClass) -> &'static str {
    match class {
        ScopeClass::Account => "account",
        ScopeClass::Conversation => "conv",
        ScopeClass::Content => "content",
        ScopeClass::Media => "media",
        ScopeClass::Payment => "payment",
        ScopeClass::Admin => "admin",
    }
}

/// 一组 scope。空集 = 什么都不许（与 `Policy` 的默认拒绝一致）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScopeSet {
    entries: Vec<Scope>,
    /// 解析失败的条目。保留下来是为了**启动时报错**而不是静默丢权限。
    invalid: Vec<String>,
}

impl ScopeSet {
    /// 用空格或逗号分隔解析（OAuth 的 `scope` 是空格分隔，配置文件里常用逗号）。
    pub fn parse(raw: &str) -> Self {
        let mut set = ScopeSet::default();
        // 单独的 `*` = 六类全开。允许这个写法是为了本地开发与"全权授予"的可读性；
        // 它不解析成 `*:*` 是因为错误消息里要能看出具体是哪一类被拒。
        let raw = if raw.trim() == "*" {
            "account:* conv:* content:* media:* payment:* admin:*"
        } else {
            raw
        };
        for token in raw
            .split([' ', ','])
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            match Scope::parse(token) {
                Some(scope) => set.entries.push(scope),
                None => set.invalid.push(token.to_string()),
            }
        }
        set
    }

    pub fn invalid(&self) -> &[String] {
        &self.invalid
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[Scope] {
        &self.entries
    }

    /// 某类别 + 某资源是否在授权范围内。
    pub fn allows(&self, class: ScopeClass, resource: &str) -> bool {
        self.entries.iter().any(|s| s.matches(class, resource))
    }

    /// 交集：**只收窄**。两个来源都允许的才算有效授权。
    pub fn intersect(&self, other: &ScopeSet) -> ScopeSet {
        let entries = self
            .entries
            .iter()
            .filter(|s| {
                other
                    .entries
                    .iter()
                    .any(|o| o.class == s.class && selectors_overlap(&o.selector, &s.selector))
            })
            .cloned()
            .collect();
        ScopeSet {
            entries,
            invalid: Vec::new(),
        }
    }
}

/// 两个选择器是否有交集。只有"相等"和"其中一个是 `*`"两种情况算重叠——
/// 保守是正确的方向：算不出交集就当没有。
fn selectors_overlap(a: &str, b: &str) -> bool {
    a == "*" || b == "*" || a == b
}

/// 令牌声明（由服务端签发；签名由宿主校验，本层只做语义校验）。
#[derive(Clone, Debug, serde::Deserialize)]
pub struct Claims {
    pub sub: String,
    pub sid: String,
    pub aud: String,
    #[serde(default)]
    pub scope: String,
    pub exp: i64,
    #[serde(default)]
    pub nbf: i64,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub deleg: Option<Delegation>,
}

/// 代理声明：`actor` 是谁在代表用户，`scope` 是**它自己**被授予的范围。
#[derive(Clone, Debug, serde::Deserialize)]
pub struct Delegation {
    pub actor: String,
    #[serde(default)]
    pub scope: String,
    pub exp: i64,
}

/// 校验通过后的身份上下文：后续每个请求都带着它做资源判定。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActorContext {
    pub actor: String,
    pub session_id: String,
    pub scopes: ScopeSet,
    pub roles: Vec<String>,
    pub expires_at: i64,
    /// 代表谁在操作（代理链上的人）；直接登录时为 `None`。
    pub on_behalf_of: Option<String>,
}

impl ActorContext {
    pub fn is_admin(&self) -> bool {
        self.roles.iter().any(|r| r == "admin")
    }

    /// 给人看的一行摘要（**不含 token**）。
    pub fn summary(&self) -> String {
        format!(
            "actor={} session={} scopes=[{}] roles=[{}] exp={}{}",
            self.actor,
            self.session_id,
            self.scopes
                .entries()
                .iter()
                .map(Scope::as_token)
                .collect::<Vec<_>>()
                .join(" "),
            self.roles.join(","),
            self.expires_at,
            match &self.on_behalf_of {
                Some(u) => format!(" on_behalf_of={u}"),
                None => String::new(),
            }
        )
    }
}

/// 授权失败的原因。每条都说明"差哪个 scope"或"哪一步不成立"。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthError {
    /// 令牌不是发给我们的——拒绝透传。
    AudienceMismatch {
        expected: &'static str,
        actual: String,
    },
    /// 令牌过期。
    Expired { exp: i64, now: i64 },
    /// 还没生效。
    NotYetValid { nbf: i64, now: i64 },
    /// 缺主体或会话。
    MissingSubject(&'static str),
    /// 进程绑定的会话与令牌不一致（跨用户复用会话 id）。
    SessionMismatch { bound: String, token: String },
    /// 代理链过长或代理范围越权。
    DelegationRejected(&'static str),
    /// 缺某个资源的 scope。
    ScopeRequired { class: ScopeClass, resource: String },
    /// 管理动作需要后台角色。
    AdminRoleRequired(&'static str),
    /// 来自不可信内容（聊天正文等）的参数想触发写操作。
    UntrustedContent(&'static str),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::AudienceMismatch { expected, actual } => write!(
                f,
                "token audience {actual:?} is not ours ({expected}); \
                 do not pass upstream tokens through"
            ),
            AuthError::Expired { exp, now } => {
                write!(f, "token expired at {exp} (now {now}); re-authenticate")
            }
            AuthError::NotYetValid { nbf, now } => {
                write!(f, "token not valid before {nbf} (now {now})")
            }
            AuthError::MissingSubject(what) => write!(f, "token has no {what}"),
            AuthError::SessionMismatch { bound, token } => write!(
                f,
                "token session {token} does not match this process session {bound}; \
                 a session id must never be reused across users"
            ),
            AuthError::DelegationRejected(why) => write!(f, "delegation rejected: {why}"),
            AuthError::ScopeRequired { class, resource } => write!(
                f,
                "missing scope {}:{resource} (grant it in the token, not in the prompt)",
                short_class(*class)
            ),
            AuthError::AdminRoleRequired(id) => write!(
                f,
                "{id} is an admin action and this actor has no admin role"
            ),
            AuthError::UntrustedContent(id) => write!(
                f,
                "{id} takes parameters derived from untrusted content; \
                 ask the user to confirm instead of acting on message text"
            ),
        }
    }
}

impl std::error::Error for AuthError {}

/// 令牌校验器：一个进程一份，绑定启动时的会话与来源。
#[derive(Clone, Debug)]
pub struct Verifier {
    /// 进程绑定的会话 id（`None` = 不绑定，仅本地 stdio 单用户场景）。
    pub bound_session: Option<String>,
    /// 允许的来源（远程 transport 的 Origin）。空 = 只允许无 Origin 的本地连接。
    pub allowed_origins: Vec<String>,
    /// 策略侧授予的 scope（与令牌取交集，**永不放宽**）。
    pub granted: ScopeSet,
}

impl Default for Verifier {
    fn default() -> Self {
        Self::new(None, Vec::new(), ScopeSet::default())
    }
}

impl Verifier {
    pub fn new(
        bound_session: Option<String>,
        allowed_origins: Vec<String>,
        granted: ScopeSet,
    ) -> Self {
        Self {
            bound_session,
            allowed_origins,
            granted,
        }
    }

    /// 校验声明并产出上下文。
    pub fn verify(
        &self,
        claims: &Claims,
        now: i64,
        origin: Option<&str>,
    ) -> Result<ActorContext, AuthError> {
        self.check_audience(&claims.aud)?;
        self.check_time(claims, now)?;
        if claims.sub.trim().is_empty() {
            return Err(AuthError::MissingSubject("sub"));
        }
        if claims.sid.trim().is_empty() {
            return Err(AuthError::MissingSubject("sid"));
        }
        self.check_session(&claims.sid)?;
        self.check_origin(origin)?;
        let scopes = self.effective_scopes(claims, now)?;
        let mut roles = claims.roles.clone();
        // `audience=admin` 是签发方的声明：并进角色，但仍然要过 admin 命令的
        // 第二道判定（`Authorize`），不给"一个受众值通行所有管理动作"的后门。
        if claims.aud == AUDIENCE_ADMIN && !roles.iter().any(|r| r == AUDIENCE_ADMIN) {
            roles.push(AUDIENCE_ADMIN.to_string());
        }
        Ok(ActorContext {
            actor: claims.sub.clone(),
            session_id: claims.sid.clone(),
            scopes,
            roles,
            expires_at: claims.exp,
            on_behalf_of: claims.deleg.as_ref().map(|d| d.actor.clone()),
        })
    }

    /// 受众必须是 identity-v1 里的枚举值；否则就是"拿别人的令牌办自己的事"。
    fn check_audience(&self, actual: &str) -> Result<(), AuthError> {
        if actual == AUDIENCE_USER || actual == AUDIENCE_ADMIN {
            return Ok(());
        }
        Err(AuthError::AudienceMismatch {
            expected: AUDIENCE_USER,
            actual: actual.to_string(),
        })
    }

    fn check_time(&self, claims: &Claims, now: i64) -> Result<(), AuthError> {
        if claims.exp <= now {
            return Err(AuthError::Expired {
                exp: claims.exp,
                now,
            });
        }
        if claims.nbf > now {
            return Err(AuthError::NotYetValid {
                nbf: claims.nbf,
                now,
            });
        }
        Ok(())
    }

    fn check_session(&self, token_sid: &str) -> Result<(), AuthError> {
        match &self.bound_session {
            Some(bound) if bound != token_sid => Err(AuthError::SessionMismatch {
                bound: bound.clone(),
                token: token_sid.to_string(),
            }),
            _ => Ok(()),
        }
    }

    fn check_origin(&self, origin: Option<&str>) -> Result<(), AuthError> {
        let Some(origin) = origin else {
            return Ok(()); // 本地 stdio：没有 Origin
        };
        if self.allowed_origins.iter().any(|o| o == origin) {
            return Ok(());
        }
        Err(AuthError::DelegationRejected("origin not allowed"))
    }

    /// 有效 scope = 令牌 ∩ 策略授予；代理链只衰减。
    fn effective_scopes(&self, claims: &Claims, now: i64) -> Result<ScopeSet, AuthError> {
        let token = ScopeSet::parse(&claims.scope);
        if !token.invalid().is_empty() {
            return Err(AuthError::DelegationRejected("unparsable scope entry"));
        }
        let Some(deleg) = &claims.deleg else {
            return Ok(token.intersect(&self.granted));
        };
        self.check_delegation(deleg, &token, claims, now)?;
        let parent = ScopeSet::parse(&deleg.scope);
        Ok(token.intersect(&parent).intersect(&self.granted))
    }

    /// 代理只能衰减：范围 ⊆ 父范围、过期 ≤ 父过期、没有套娃。
    fn check_delegation(
        &self,
        deleg: &Delegation,
        token: &ScopeSet,
        claims: &Claims,
        now: i64,
    ) -> Result<(), AuthError> {
        if deleg.exp <= now {
            return Err(AuthError::DelegationRejected("delegation expired"));
        }
        if deleg.exp > claims.exp {
            return Err(AuthError::DelegationRejected(
                "delegation outlives the token",
            ));
        }
        if deleg.actor.trim().is_empty() {
            return Err(AuthError::DelegationRejected("delegation has no actor"));
        }
        let parent = ScopeSet::parse(&deleg.scope);
        let widened = token
            .entries()
            .iter()
            .any(|s| !parent.allows(s.class, &s.selector));
        // 代理链只有一层：`Claims` 的 schema 里 `deleg` 不嵌套，
        // 所以"套娃代理"在解析阶段就消失了，不需要额外的深度计数。
        if widened {
            return Err(AuthError::DelegationRejected(
                "delegated scope wider than parent",
            ));
        }
        Ok(())
    }

    /// 每条命令的**资源级**判定：类别对 + 资源逐个匹配 + 管理角色。
    pub fn authorize(
        &self,
        ctx: &ActorContext,
        spec: &CommandSpec,
        params: &Value,
    ) -> Result<(), AuthError> {
        if spec.scope == ScopeClass::Admin && !ctx.is_admin() {
            return Err(AuthError::AdminRoleRequired(spec.id));
        }
        for resource in resources_in(spec, params) {
            if !ctx.scopes.allows(spec.scope, &resource) {
                return Err(AuthError::ScopeRequired {
                    class: spec.scope,
                    resource,
                });
            }
        }
        Ok(())
    }
}

/// 一条命令涉及哪些资源 id。`None` = 资源未知，按"必须显式 `*`"处理。
///
/// 这里刻意与 `policy::convs_in` 分开：那个是"会话白名单"，这个是"scope 资源"，
/// 两者回答的问题不同（一个是"有没有被允许"，一个是"被允许到什么范围"）。
fn resources_in(spec: &CommandSpec, params: &Value) -> Vec<String> {
    let keys: &[&str] = match spec.scope {
        ScopeClass::Conversation => &["conv", "from_conv", "to_conv"],
        ScopeClass::Content => &["community"],
        ScopeClass::Media => &["video_id", "job_id"],
        ScopeClass::Account => &["user_id"],
        ScopeClass::Payment | ScopeClass::Admin => &[],
    };
    let mut out: Vec<String> = keys
        .iter()
        .filter_map(|k| params.get(*k).and_then(Value::as_str))
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect();
    if out.is_empty() {
        // 命令声明了资源类别却拿不到资源 id：要求 `类别:*`，不给"空即全部"
        out.push("*".to_string());
    }
    out
}

/// 参数来源：命令行/MCP 显式给出的算可信；从聊天正文、文件名、网页里
/// 抠出来的算不可信。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provenance {
    Trusted,
    /// 不可信，附带一句"从哪来的"（进错误消息，便于人定位）。
    Untrusted(&'static str),
}

/// **提示注入的最后一道闸**：不可信来源的参数不得直接触发写/破坏性动作。
///
/// 为什么不是"过滤可疑词"：正文里出现 "ignore previous instructions" 与否
/// 从来不是判据；判据只有"这个参数是谁给的"。
pub fn guard_provenance(spec: &CommandSpec, provenance: Provenance) -> Result<(), AuthError> {
    match (spec.capability, provenance) {
        (Capability::Read, _) => Ok(()),
        (_, Provenance::Trusted) => Ok(()),
        (_, Provenance::Untrusted(_)) => Err(AuthError::UntrustedContent(spec.id)),
    }
}

/// 一个进程的授权闸门：身份上下文 + 校验器，CLI 与 MCP 共用。
///
/// 为什么不是每处各写一遍：两侧的判定必须**逐字一致**，否则会出现
/// "CLI 能发、MCP 不能发"这种谁也说不清的行为差异。
#[derive(Clone, Debug)]
pub struct AuthGate {
    verifier: Verifier,
    ctx: ActorContext,
}

impl AuthGate {
    /// 由已校验的上下文构造（启动时校验一次，请求期只做资源判定）。
    pub fn new(verifier: Verifier, ctx: ActorContext) -> Self {
        Self { verifier, ctx }
    }

    /// 从"进程自述"构造：actor/session/scope 来自启动参数或环境变量。
    ///
    /// 这不是信任边界——服务端仍会再验一次；这里的作用是**绑定会话**并
    /// 提前拒掉注定失败的请求。签名与声明真实性由宿主（`Verifier`）负责。
    pub fn from_parts(
        actor: &str,
        session_id: &str,
        scope: &str,
        bound_session: Option<&str>,
        allowed_origins: Vec<String>,
    ) -> Result<Self, AuthError> {
        let granted = ScopeSet::parse(scope);
        let verifier = Verifier::new(bound_session.map(str::to_string), allowed_origins, granted);
        let claims = Claims {
            sub: actor.to_string(),
            sid: session_id.to_string(),
            aud: AUDIENCE_USER.to_string(),
            scope: scope.to_string(),
            exp: i64::MAX,
            nbf: 0,
            roles: Vec::new(),
            deleg: None,
        };
        let ctx = verifier.verify(&claims, 0, None)?;
        Ok(Self::new(verifier, ctx))
    }

    pub fn context(&self) -> &ActorContext {
        &self.ctx
    }

    /// 请求期检查：来源可信度 → 管理角色 → 资源 scope。
    pub fn check(
        &self,
        spec: &CommandSpec,
        params: &Value,
        provenance: Provenance,
    ) -> Result<(), AuthError> {
        guard_provenance(spec, provenance)?;
        self.verifier.authorize(&self.ctx, spec, params)
    }

    /// 给人看的自查表：每条命令在当前 scope 下是否可调（不含网络）。
    pub fn render(&self, policy: &crate::policy::Policy) -> String {
        let mut out = format!(
            "身份上下文：{}

",
            self.ctx.summary()
        );
        out.push_str(&format!(
            "{:<20} {:<12} 判定
",
            "命令", "scope"
        ));
        for spec in crate::CATALOG {
            let probe = probe_params(spec);
            let verdict = match self.check(spec, &probe, Provenance::Trusted) {
                Ok(()) if policy.authorize(spec.id, &probe).is_ok() => "允许".to_string(),
                Ok(()) => "策略拒绝".to_string(),
                Err(e) => format!("scope 拒绝 — {e}"),
            };
            out.push_str(&format!(
                "{:<20} {:<12} {}
",
                spec.id,
                spec.scope.as_str(),
                verdict
            ));
        }
        out
    }
}

/// 自查用的探针参数：取 scope 里的第一个具体值，没有就给一个必然不匹配的。
fn probe_params(spec: &CommandSpec) -> Value {
    let mut map = serde_json::Map::new();
    for key in [
        "conv",
        "from_conv",
        "to_conv",
        "community",
        "video_id",
        "job_id",
        "user_id",
    ] {
        if key == "user_id" && spec.scope != ScopeClass::Account {
            continue;
        }
        map.insert(key.to_string(), Value::String("probe".into()));
    }
    for p in spec.args {
        if p.required && !map.contains_key(p.name) {
            map.insert(p.name.to_string(), probe_value(p.ty));
        }
    }
    Value::Object(map)
}

/// 探针值：只看类型，不看语义——自查表要的是"能不能过闸"，不是"跑得对不对"。
fn probe_value(ty: crate::catalog::ParamType) -> Value {
    use crate::catalog::ParamType;
    match ty {
        ParamType::Str => Value::String("probe".into()),
        ParamType::Int | ParamType::NonNegativeInt => Value::from(1),
        ParamType::Bool => Value::Bool(true),
        ParamType::StrList => Value::Array(Vec::new()),
    }
}

/// 由 catalog 反查"这条命令需要哪个 scope"，用于启动时自检与错误提示。
pub fn required_scope(spec: &CommandSpec) -> String {
    format!("{}:*", spec.scope)
}

/// 便捷函数：从 `"conv:g-1 media:*"` 这样的字符串构造上下文（测试与本地启动用）。
pub fn context_from_parts(actor: &str, session: &str, scope: &str) -> ActorContext {
    ActorContext {
        actor: actor.to_string(),
        session_id: session.to_string(),
        scopes: ScopeSet::parse(scope),
        roles: Vec::new(),
        expires_at: i64::MAX,
        on_behalf_of: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog;
    use serde_json::json;

    const NOW: i64 = 1_700_000_000;

    fn claims(scope: &str) -> Claims {
        Claims {
            sub: "u-alice".into(),
            sid: "s-1".into(),
            aud: AUDIENCE_USER.into(),
            scope: scope.into(),
            exp: NOW + 3600,
            nbf: NOW - 10,
            roles: Vec::new(),
            deleg: None,
        }
    }

    fn verifier(granted: &str, bound: Option<&str>) -> Verifier {
        Verifier::new(
            bound.map(str::to_string),
            Vec::new(),
            ScopeSet::parse(granted),
        )
    }

    fn mk_ctx(scope: &str) -> ActorContext {
        context_from_parts("u-alice", "s-1", scope)
    }

    #[test]
    fn audience_must_be_from_identity_contract() {
        let v = verifier("conv:*", None);
        let mut c = claims("conv:*");
        c.aud = "https://calendar.example/mcp".into();
        let err = v.verify(&c, NOW, None).unwrap_err();
        assert!(matches!(err, AuthError::AudienceMismatch { .. }), "{err:?}");
        assert!(err.to_string().contains("upstream tokens"), "{err}");
    }

    /// identity-v1 的 `admin` 受众会在上下文里补上 admin 角色；
    /// `user` 受众不会——否则每个普通用户都能调管理动作。
    #[test]
    fn admin_audience_grants_the_admin_role_but_user_audience_does_not() {
        let v = verifier("*", None);
        let mut c = claims("admin:*");
        c.aud = AUDIENCE_ADMIN.into();
        let ctx = v.verify(&c, NOW, None).expect("verify");
        assert!(ctx.is_admin(), "admin 受众要能调管理动作");

        let mut c = claims("conv:*");
        c.aud = AUDIENCE_USER.into();
        let ctx = v.verify(&c, NOW, None).expect("verify");
        assert!(!ctx.is_admin(), "普通受众不能变成管理员");
    }

    #[test]
    fn expired_and_not_yet_valid_tokens_are_refused() {
        let v = verifier("*", None);
        let mut c = claims("conv:*");
        c.exp = NOW;
        assert!(matches!(
            v.verify(&c, NOW, None).unwrap_err(),
            AuthError::Expired { .. }
        ));

        let mut c = claims("conv:*");
        c.nbf = NOW + 60;
        assert!(matches!(
            v.verify(&c, NOW, None).unwrap_err(),
            AuthError::NotYetValid { .. }
        ));
    }

    /// 跨用户复用会话 id：令牌说 s-2，进程绑定 s-1 —— 一律拒。
    #[test]
    fn session_ids_cannot_be_reused_across_users() {
        let v = verifier("conv:*", Some("s-1"));
        let mut c = claims("conv:*");
        c.sid = "s-2".into();
        let err = v.verify(&c, NOW, None).unwrap_err();
        assert!(matches!(err, AuthError::SessionMismatch { .. }), "{err:?}");
        assert!(err.to_string().contains("s-2"), "{err}");
    }

    /// 有效 scope = 令牌 ∩ 策略授予：策略没给的，令牌给了也不算。
    #[test]
    fn granted_scope_only_narrows_the_token() {
        let v = verifier("conv:g-1", None);
        let c = claims("conv:g-1 conv:g-2 media:*");
        let ctx = v.verify(&c, NOW, None).expect("verify");
        assert!(ctx.scopes.allows(ScopeClass::Conversation, "g-1"));
        assert!(
            !ctx.scopes.allows(ScopeClass::Conversation, "g-2"),
            "策略没给 g-2"
        );
        assert!(
            !ctx.scopes.allows(ScopeClass::Media, "v-1"),
            "策略没给 media"
        );
    }

    #[test]
    fn delegation_cannot_widen_or_outlive_the_parent() {
        let v = verifier("*", None);

        // 代理范围比父范围大
        let mut c = claims("conv:g-1 conv:g-2");
        c.deleg = Some(Delegation {
            actor: "agent-7".into(),
            scope: "conv:g-1".into(),
            exp: NOW + 60,
        });
        let err = v.verify(&c, NOW, None).unwrap_err();
        assert!(
            matches!(err, AuthError::DelegationRejected(w) if w.contains("wider")),
            "{err:?}"
        );

        // 代理活得比令牌久
        let mut c = claims("conv:g-1");
        c.deleg = Some(Delegation {
            actor: "agent-7".into(),
            scope: "conv:g-1".into(),
            exp: NOW + 7200,
        });
        let err = v.verify(&c, NOW, None).unwrap_err();
        assert!(
            matches!(err, AuthError::DelegationRejected(w) if w.contains("outlives")),
            "{err:?}"
        );

        // 合法代理：只衰减
        let mut c = claims("conv:g-1");
        c.deleg = Some(Delegation {
            actor: "agent-7".into(),
            scope: "conv:g-1".into(),
            exp: NOW + 60,
        });
        let ctx = v.verify(&c, NOW, None).expect("verify");
        assert_eq!(ctx.on_behalf_of.as_deref(), Some("agent-7"));
    }

    #[test]
    fn unknown_scope_class_is_an_error_not_silently_ignored() {
        let v = verifier("*", None);
        let c = claims("conv:* money:all");
        let err = v.verify(&c, NOW, None).unwrap_err();
        assert!(
            matches!(err, AuthError::DelegationRejected(w) if w.contains("scope")),
            "{err:?}"
        );
    }

    /// 资源级：有会话 scope 不等于能操作所有会话。
    #[test]
    fn resource_scope_is_checked_per_conversation() {
        let v = verifier("*", None);
        let ctx = v.verify(&claims("conv:g-1"), NOW, None).expect("verify");
        let spec = catalog::get("conv.unread").expect("spec");

        assert!(v.authorize(&ctx, spec, &json!({"conv": "g-1"})).is_ok());
        let err = v
            .authorize(&ctx, spec, &json!({"conv": "g-2"}))
            .unwrap_err();
        assert!(matches!(err, AuthError::ScopeRequired { .. }), "{err:?}");
        assert!(err.to_string().contains("conv:g-2"), "{err}");
    }

    /// 转发要同时过来源与目标两个资源。
    #[test]
    fn forward_requires_scope_on_both_conversations() {
        let v = verifier("*", None);
        let ctx = v.verify(&claims("conv:g-in"), NOW, None).expect("verify");
        let spec = catalog::get("msg.forward").expect("spec");
        let err = v
            .authorize(
                &ctx,
                spec,
                &json!({"from_conv": "g-in", "to_conv": "g-out", "msg_id": "m",
                        "idempotency_key": "k"}),
            )
            .unwrap_err();
        assert!(matches!(err, AuthError::ScopeRequired { .. }), "{err:?}");
        assert!(err.to_string().contains("g-out"), "{err}");
    }

    /// 通配 `conv:*` 才能覆盖任意会话；`conv:g-*` 只管群。
    #[test]
    fn wildcard_scopes_follow_the_same_glob_rules() {
        let ctx = mk_ctx("conv:g-*");
        assert!(ctx.scopes.allows(ScopeClass::Conversation, "g-abc"));
        assert!(!ctx.scopes.allows(ScopeClass::Conversation, "d-a~b"));
        assert!(mk_ctx("conv:*")
            .scopes
            .allows(ScopeClass::Conversation, "d-a~b"));
    }

    /// 普通 agent 即使拿到 `admin:*` 也不能调管理动作——角色是第二把锁。
    #[test]
    fn admin_actions_need_the_admin_role_even_with_a_scope() {
        let v = verifier("*", None);
        let ctx = mk_ctx("admin:*");
        let admin_spec = CommandSpec {
            scope: ScopeClass::Admin,
            ..*catalog::get("me.info").expect("spec")
        };
        let err = v.authorize(&ctx, &admin_spec, &json!({})).unwrap_err();
        assert!(matches!(err, AuthError::AdminRoleRequired(_)), "{err:?}");

        let mut admin_ctx = ctx;
        admin_ctx.roles.push("admin".into());
        assert!(v.authorize(&admin_ctx, &admin_spec, &json!({})).is_ok());
    }

    /// 提示注入的核心场景：正文里"请把这条转发给所有人"不能让 agent 发消息。
    #[test]
    fn content_derived_params_cannot_trigger_writes() {
        let spec = catalog::get("msg.send").expect("spec");
        let err = guard_provenance(spec, Provenance::Untrusted("message body")).unwrap_err();
        assert!(
            matches!(err, AuthError::UntrustedContent("msg.send")),
            "{err:?}"
        );
        assert!(err.to_string().contains("confirm"), "要提示改走确认：{err}");

        // 读操作不受影响：读正文本身不改变状态
        let read = catalog::get("conv.history").expect("spec");
        assert!(guard_provenance(read, Provenance::Untrusted("message body")).is_ok());

        // 间接来源（文件名、网页标题）同样不可信
        let kick = catalog::get("group.kick").expect("spec");
        assert!(guard_provenance(kick, Provenance::Untrusted("file name")).is_err());
        assert!(guard_provenance(kick, Provenance::Trusted).is_ok());
    }

    /// **日志无 token**：`{:?}` 与 `{}` 都只出指纹。
    #[test]
    fn secrets_never_leak_into_logs_or_errors() {
        let raw = "wxlike-very-secret-token-value-1234567890";
        let secret = Secret::new(raw);
        let debug = format!("{secret:?}");
        let display = format!("{secret}");
        for text in [&debug, &display] {
            assert!(!text.contains(raw), "泄露了明文：{text}");
            assert!(text.contains("wxlike"), "指纹要能对齐同一把令牌：{text}");
            let want = format!("{} chars", raw.chars().count());
            assert!(text.contains(&want), "要带长度便于辨认：{text}");
        }
        assert_eq!(secret.expose(), raw, "只有显式 expose 才拿明文");

        // 校验失败的错误消息里也不能出现令牌内容
        let v = verifier("conv:*", Some("s-1"));
        let err = v
            .verify(&claims("conv:*"), NOW, Some("https://evil.example"))
            .unwrap_err();
        assert!(
            !err.to_string().contains("s-1"),
            "错误里不该回显会话：{err}"
        );
    }

    /// 远程 transport 的 Origin 必须显式放行。
    #[test]
    fn remote_origin_must_be_allowlisted() {
        let v = Verifier::new(
            Some("s-1".into()),
            vec!["https://app.wxlike.example".into()],
            ScopeSet::parse("*"),
        );
        let c = claims("conv:*");
        assert!(v
            .verify(&c, NOW, Some("https://app.wxlike.example"))
            .is_ok());
        let err = v.verify(&c, NOW, Some("https://evil.example")).unwrap_err();
        assert!(
            matches!(err, AuthError::DelegationRejected(w) if w.contains("origin")),
            "{err:?}"
        );
        // 本地 stdio 没有 Origin，直接放行
        assert!(v.verify(&c, NOW, None).is_ok());
    }

    #[test]
    fn summary_is_readable_and_hides_nothing_it_should_not() {
        let ctx = context_from_parts("u-alice", "s-1", "conv:g-1 media:*");
        let text = ctx.summary();
        assert!(text.contains("actor=u-alice"), "{text}");
        assert!(text.contains("conv:g-1"), "{text}");
        assert!(!text.contains("token"), "{text}");
    }
}
