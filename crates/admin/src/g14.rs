//! G14 的 HTTP 面：把六个子域装进 adminx 同一进程、同一套会话与准入。
//!
//! 单独一个模块而不是往 `http.rs` 里加二十个 handler：老文件承载 D1/D2/D3 的
//! 契约（读面 + 老操作目录），G14 是新的一层。混在一起改，评审看不出哪行属于谁。
//!
//! 统一约定：
//!
//! * 路由前缀 `/api/g14/`，与老接口不冲突。
//! * **读**允许静态运营令牌或后台会话；**写**必须后台会话（可追责），且过
//!   [`crate::auth`] 的资源矩阵 + 限流；敏感动作还要步进验证。
//! * 每个响应带 `trace`，与审计里那一行对得上号。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::accounts::Session;
use crate::audit::{AuditLog, Filter as AuditFilter};
use crate::auth::{self, Action, Audience, Resource, StepUp};
use crate::commerce::{
    CommerceService, HttpMoneyPort, JournalLine, MerchantScope, MoneyPort, OrderFilter,
};
use crate::error::{AdminError, Result};
use crate::http::App;
use crate::identity::{
    CaseFilter, CaseState, Confirmer, DangerRequest, HttpPort, IdentityService, NewCase,
    PlatformPort,
};
use crate::jobs::{
    health_summary, JobFilter, JobService, JobState, PrivacyKind, PrivacyService, RateLimiter,
};
use crate::publishing::{
    ContentPort, Decision, HttpContentPort, ItemState, PublishingService, QueueFilter,
};
use crate::rbac::Role;
use crate::store::Store;
use crate::upstream::{self, Upstream};
use wxlike_platform::extract::ApiJson;

/// G14 的全部运行时依赖。装配一次，路由之间共享。
#[derive(Clone)]
pub struct G14 {
    pub audit: AuditLog,
    pub stepup: StepUp,
    pub identity: IdentityService,
    pub publishing: PublishingService,
    pub commerce: CommerceService,
    pub jobs: JobService,
    pub privacy: PrivacyService,
    pub limiter: Arc<RateLimiter>,
}

impl G14 {
    /// 生产装配：三个端口全部走 HTTP 转发（域侧在平台进程里）。
    pub fn with_http(store: Store, upstream: Upstream) -> Self {
        Self {
            audit: AuditLog::new(store.clone()),
            stepup: StepUp::new(store.clone()),
            identity: IdentityService::new(
                store.clone(),
                AuditLog::new(store.clone()),
                Arc::new(HttpPort::new(upstream.clone())),
                StepUp::new(store.clone()),
            ),
            publishing: PublishingService::new(
                store.clone(),
                AuditLog::new(store.clone()),
                Arc::new(HttpContentPort::new(upstream.clone())),
                StepUp::new(store.clone()),
            ),
            commerce: CommerceService::new(
                store.clone(),
                AuditLog::new(store.clone()),
                Arc::new(HttpMoneyPort::new(upstream.clone())),
                StepUp::new(store.clone()),
            ),
            jobs: JobService::new(
                store.clone(),
                AuditLog::new(store.clone()),
                Arc::new(HttpPort::new(upstream)),
            ),
            privacy: PrivacyService::new(
                store.clone(),
                AuditLog::new(store.clone()),
                StepUp::new(store),
            ),
            limiter: Arc::new(RateLimiter::new(60, 2.0)),
        }
    }

    /// 测试/联调装配：端口由调用方给（[`crate::identity::FixturePort`] 等）。
    pub fn with_ports(
        store: Store,
        identity: Arc<dyn PlatformPort>,
        content: Arc<dyn ContentPort>,
        money: Arc<dyn MoneyPort>,
    ) -> Self {
        Self {
            audit: AuditLog::new(store.clone()),
            stepup: StepUp::new(store.clone()),
            identity: IdentityService::new(
                store.clone(),
                AuditLog::new(store.clone()),
                identity.clone(),
                StepUp::new(store.clone()),
            )
            .with_confirmer(Confirmer::new()),
            publishing: PublishingService::new(
                store.clone(),
                AuditLog::new(store.clone()),
                content,
                StepUp::new(store.clone()),
            ),
            commerce: CommerceService::new(
                store.clone(),
                AuditLog::new(store.clone()),
                money,
                StepUp::new(store.clone()),
            ),
            jobs: JobService::new(store.clone(), AuditLog::new(store.clone()), identity),
            privacy: PrivacyService::new(
                store.clone(),
                AuditLog::new(store.clone()),
                StepUp::new(store),
            ),
            limiter: Arc::new(RateLimiter::new(ADMIN_RATE_CAPACITY, ADMIN_RATE_PER_SECOND)),
        }
    }

    pub fn store_audit(&self) -> &AuditLog {
        &self.audit
    }
}

const ADMIN_RATE_CAPACITY: u32 = 600;
const ADMIN_RATE_PER_SECOND: f64 = 600.0;

type Shared = Arc<App>;

/// 路由。挂到 [`App::router`]。
pub fn router() -> Router<Shared> {
    Router::new()
        .route("/api/g14/auth/matrix", get(matrix))
        .route(
            "/api/g14/session/stepup",
            post(stepup_verify).get(stepup_state),
        )
        .route("/api/g14/audit", get(audit_query))
        .route("/api/g14/audit/verify", get(audit_verify))
        .route("/api/g14/health/summary", get(health))
        // 身份
        .route("/api/g14/identity/cases", get(list_cases).post(create_case))
        .route("/api/g14/identity/cases/{id}", get(get_case))
        .route(
            "/api/g14/identity/cases/{id}/transition",
            post(transition_case),
        )
        .route("/api/g14/identity/users/{id}/suspend", post(suspend_user))
        .route(
            "/api/g14/identity/users/{id}/unsuspend",
            post(unsuspend_user),
        )
        .route("/api/g14/identity/devices/{id}/revoke", post(revoke_device))
        .route("/api/g14/identity/groups/{id}/govern", post(govern_group))
        .route("/api/g14/identity/batch", post(batch_suspend))
        // 发布
        .route("/api/g14/publishing/rules", get(publish_rules))
        .route("/api/g14/publishing/queue", get(publish_queue))
        .route("/api/g14/publishing/submissions", post(submit_review))
        .route("/api/g14/publishing/items/{id}/decide", post(decide_item))
        .route(
            "/api/g14/publishing/mini/{app}/releases",
            get(list_releases).post(create_release),
        )
        .route(
            "/api/g14/publishing/mini/releases/{id}/sign",
            post(sign_release),
        )
        .route(
            "/api/g14/publishing/mini/releases/{id}/publish",
            post(publish_release),
        )
        .route(
            "/api/g14/publishing/mini/releases/{id}/revoke",
            post(revoke_release),
        )
        .route("/api/g14/publishing/services", get(list_services))
        .route("/api/g14/publishing/services/{id}", put(upsert_service))
        // 商务
        .route("/api/g14/commerce/merchants", get(list_merchants))
        .route("/api/g14/commerce/orders", get(list_orders))
        .route("/api/g14/commerce/orders/export.csv", get(export_orders))
        .route(
            "/api/g14/commerce/orders/{id}/refunds",
            post(request_refund),
        )
        .route("/api/g14/commerce/refunds", get(list_refunds))
        .route(
            "/api/g14/commerce/refunds/{id}/approve",
            post(approve_refund),
        )
        .route("/api/g14/commerce/recon", get(list_recon))
        .route("/api/g14/commerce/recon/{id}/resolve", post(resolve_recon))
        .route(
            "/api/g14/commerce/journal",
            get(list_journal).post(post_journal),
        )
        // 作业与隐私
        .route("/api/g14/jobs", get(list_jobs))
        .route("/api/g14/jobs/{id}/retry", post(retry_job))
        .route("/api/g14/jobs/{id}/cancel", post(cancel_job))
        .route(
            "/api/g14/privacy/requests",
            get(list_privacy).post(request_privacy),
        )
        .route(
            "/api/g14/privacy/requests/{id}/fulfill",
            post(fulfill_privacy),
        )
        .route(
            "/api/g14/privacy/requests/{id}/download",
            get(download_privacy),
        )
}

// ---------------------------------------------------------------------------
// 准入
// ---------------------------------------------------------------------------

/// 一个请求的凭据解析结果。
struct Caller {
    session: Session,
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn new_trace() -> String {
    format!("g14_{}", wxlike_platform::random_hex(8))
}

/// 读：静态运营令牌或后台会话。返回 `None` 表示用的是静态令牌（没有"人"）。
fn read_caller(app: &Shared, headers: &HeaderMap, trace: &str) -> Result<Option<Caller>> {
    let presented = bearer(headers).ok_or(AdminError::Unauthorized)?;
    let session = app.accounts().session(presented)?;
    let audience = auth::audience_of(presented, &app.config().token, session.is_some());
    auth::require_admin_audience(audience)?;
    let _ = trace;
    Ok(session.map(|s| Caller { session: s }))
}

/// 写：必须是后台会话 + 角色矩阵 + 限流。
fn write_caller(
    app: &Shared,
    headers: &HeaderMap,
    resource: Resource,
    action: Action,
    trace: &str,
) -> Result<Caller> {
    let presented = bearer(headers).ok_or(AdminError::Unauthorized)?;
    let session = app.accounts().session(presented)?;
    let audience = auth::audience_of(presented, &app.config().token, session.is_some());
    auth::require_admin_audience(audience)?;
    // 静态运营令牌没有"人"，写不出可追责的留痕：给一句能照做的拒绝，而不是 401。
    if audience == Audience::Operator {
        return Err(AdminError::Forbidden(
            "静态令牌只能读：写操作必须用登录后的会话（POST /api/login）".into(),
        ));
    }
    let session = session.ok_or(AdminError::SessionExpired)?;
    let caller = Caller { session };
    app.g14().limiter.check(&caller.session.actor)?;
    match auth::require(caller.session.role, resource, action) {
        Ok(()) => Ok(caller),
        Err(err) => {
            // 越权尝试也要留痕：谁在试什么，运维得看得见
            let _ = app.g14().audit.append(
                crate::audit::Draft::new(
                    &caller.session.actor,
                    caller.session.role.as_str(),
                    "g14.denied",
                    resource.as_str(),
                    "",
                )
                .reason("权限不足")
                .trace(trace)
                .outcome(crate::audit::Outcome::Denied)
                .detail(serde_json::json!({"action": action.as_str(), "error": err.to_string()})),
            );
            Err(err)
        }
    }
}

/// 只校验"是后台会话"：步进验证管的是会话本身，不针对某个资源。
/// 用资源矩阵去卡它会让"publisher 想签包先要 audit 读权限"这种荒唐事发生。
fn session_caller(app: &Shared, headers: &HeaderMap) -> Result<Caller> {
    let presented = bearer(headers).ok_or(AdminError::Unauthorized)?;
    let session = app.accounts().session(presented)?;
    let audience = auth::audience_of(presented, &app.config().token, session.is_some());
    auth::require_admin_audience(audience)?;
    if audience == Audience::Operator {
        return Err(AdminError::Forbidden(
            "静态令牌不能开步进验证窗口：用登录后的会话".into(),
        ));
    }
    let session = session.ok_or(AdminError::SessionExpired)?;
    app.g14().limiter.check(&session.actor)?;
    Ok(Caller { session })
}

/// 读也要角色够（`viewer` 能看的面里没有 `privacy`）。
fn read_role(app: &Shared, headers: &HeaderMap, resource: Resource, trace: &str) -> Result<Role> {
    let caller = read_caller(app, headers, trace)?;
    match caller {
        // 静态令牌按 viewer 的读面处理：能看运营面，但看不到隐私请求这类逐人数据
        None => Ok(Role::Viewer),
        Some(c) => {
            auth::require(c.session.role, resource, Action::Read)?;
            Ok(c.session.role)
        }
    }
}

fn actor_of(app: &Shared, headers: &HeaderMap, trace: &str) -> Result<(String, Role)> {
    match read_caller(app, headers, trace)? {
        None => Ok(("operator-token".to_string(), Role::Viewer)),
        Some(c) => Ok((c.session.actor, c.session.role)),
    }
}

// ---------------------------------------------------------------------------
// 身份与审计
// ---------------------------------------------------------------------------

async fn matrix(State(app): State<Shared>, headers: HeaderMap) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    read_role(&app, &headers, Resource::Audit, &trace)?;
    Ok(Json(serde_json::json!({
        "trace": trace,
        "grants": auth::grants(),
        "roles": Role::NAMES,
        "step_up_ttl_ms": app.g14().stepup.ttl_ms(),
    })))
}

#[derive(Deserialize)]
struct StepUpBody {
    code: String,
}

async fn stepup_verify(
    State(app): State<Shared>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<StepUpBody>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let caller = session_caller(&app, &headers)?;
    let state = app
        .g14()
        .stepup
        .verify(app.accounts(), &caller.session.actor, &body.code)?;
    Ok(Json(serde_json::json!({"trace": trace, "stepup": state})))
}

async fn stepup_state(
    State(app): State<Shared>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let caller = session_caller(&app, &headers)?;
    let state = app.g14().stepup.state(&caller.session.actor)?;
    Ok(Json(serde_json::json!({"trace": trace, "stepup": state})))
}

async fn audit_query(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    read_role(&app, &headers, Resource::Audit, &trace)?;
    let filter = AuditFilter {
        actor: qstr(&q, "actor"),
        action: qstr(&q, "action"),
        object_kind: qstr(&q, "object_kind"),
        object_id: qstr(&q, "object_id"),
        outcome: qstr(&q, "outcome").and_then(|v| match v.as_str() {
            "ok" => Some(crate::audit::Outcome::Ok),
            "denied" => Some(crate::audit::Outcome::Denied),
            "failed" => Some(crate::audit::Outcome::Failed),
            _ => None,
        }),
        from: qint(&q, "from"),
        to: qint(&q, "to"),
        limit: qusize(&q, "limit"),
        offset: qint(&q, "offset"),
    };
    let page = app.g14().audit.query(&filter)?;
    Ok(Json(serde_json::json!({"trace": trace, "page": page})))
}

async fn audit_verify(
    State(app): State<Shared>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    read_role(&app, &headers, Resource::Audit, &trace)?;
    let report = app.g14().audit.verify()?;
    Ok(Json(serde_json::json!({"trace": trace, "chain": report})))
}

async fn health(State(app): State<Shared>, headers: HeaderMap) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    read_role(&app, &headers, Resource::Health, &trace)?;
    let summary = health_summary(app.store(), app.g14().store_audit(), app.uptime_ms())?;
    Ok(Json(serde_json::json!({"trace": trace, "health": summary})))
}

// ---------------------------------------------------------------------------
// 身份
// ---------------------------------------------------------------------------

async fn list_cases(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    read_role(&app, &headers, Resource::Case, &trace)?;
    let filter = CaseFilter {
        state: qstr(&q, "state").and_then(|v| CaseState::parse(&v)),
        kind: qstr(&q, "kind"),
        subject_id: qstr(&q, "subject_id"),
        limit: qusize(&q, "limit"),
        offset: qint(&q, "offset"),
    };
    let page = app.g14().identity.cases(&filter)?;
    Ok(Json(serde_json::json!({"trace": trace, "page": page})))
}

#[derive(Deserialize)]
struct NewCaseBody {
    kind: String,
    subject_kind: String,
    subject_id: String,
    #[serde(default)]
    reporter: String,
    reason: String,
    #[serde(default)]
    appeal_of: Option<String>,
}

async fn create_case(
    State(app): State<Shared>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<NewCaseBody>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let caller = write_caller(&app, &headers, Resource::Case, Action::Write, &trace)?;
    let case = app.g14().identity.create_case(
        &caller.session.actor,
        caller.session.role,
        &trace,
        NewCase {
            kind: body.kind,
            subject_kind: body.subject_kind,
            subject_id: body.subject_id,
            reporter: body.reporter,
            reason: body.reason,
            appeal_of: body.appeal_of,
        },
    )?;
    Ok(Json(serde_json::json!({"trace": trace, "case": case})))
}

async fn get_case(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    read_role(&app, &headers, Resource::Case, &trace)?;
    let case = app.g14().identity.case(&id)?.ok_or(AdminError::NotFound)?;
    Ok(Json(serde_json::json!({"trace": trace, "case": case})))
}

#[derive(Deserialize)]
struct TransitionBody {
    state: String,
    reason: String,
    #[serde(default)]
    assignee: Option<String>,
}

async fn transition_case(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<TransitionBody>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let caller = write_caller(&app, &headers, Resource::Case, Action::Write, &trace)?;
    let next = CaseState::parse(&body.state).ok_or_else(|| {
        AdminError::bad_input("state 只能是 open/triaged/actioned/rejected/closed")
    })?;
    let case = app.g14().identity.transition(
        &caller.session.actor,
        caller.session.role,
        &trace,
        &id,
        next,
        &body.reason,
        body.assignee.as_deref(),
    )?;
    Ok(Json(serde_json::json!({"trace": trace, "case": case})))
}

#[derive(Deserialize)]
struct DangerBody {
    reason: String,
    #[serde(default)]
    until_ms: Option<i64>,
    #[serde(default)]
    confirm: Option<String>,
    #[serde(default)]
    current: Option<serde_json::Value>,
    #[serde(default)]
    action: Option<String>,
}

fn danger_request(
    app: &Shared,
    headers: &HeaderMap,
    id: &str,
    body: DangerBody,
    trace: &str,
) -> Result<DangerRequest> {
    let caller = write_caller(app, headers, Resource::Identity, Action::Destructive, trace)?;
    Ok(DangerRequest {
        actor: caller.session.actor,
        role: caller.session.role,
        trace: trace.to_string(),
        target: id.to_string(),
        reason: body.reason,
        until_ms: body.until_ms,
        confirm: body.confirm,
        current: body.current.unwrap_or(serde_json::Value::Null),
    })
}

async fn suspend_user(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<DangerBody>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let req = danger_request(&app, &headers, &id, body, &trace)?;
    let out = app.g14().identity.suspend(req).await?;
    Ok(Json(serde_json::json!({"trace": trace, "result": out})))
}

async fn unsuspend_user(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<DangerBody>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let req = danger_request(&app, &headers, &id, body, &trace)?;
    let out = app.g14().identity.unsuspend(req).await?;
    Ok(Json(serde_json::json!({"trace": trace, "result": out})))
}

async fn revoke_device(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<DangerBody>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let req = danger_request(&app, &headers, &id, body, &trace)?;
    let out = app.g14().identity.revoke_device(req).await?;
    Ok(Json(serde_json::json!({"trace": trace, "result": out})))
}

async fn govern_group(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<DangerBody>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let verb = body.action.clone().unwrap_or_default();
    if !["freeze", "unfreeze", "transfer", "dissolve"].contains(&verb.as_str()) {
        return Err(AdminError::bad_input(
            "action 只能是 freeze/unfreeze/transfer/dissolve",
        ));
    }
    let req = danger_request(&app, &headers, &id, body, &trace)?;
    let out = app.g14().identity.govern_group(req, &verb).await?;
    Ok(Json(serde_json::json!({"trace": trace, "result": out})))
}

#[derive(Deserialize)]
struct BatchBody {
    reason: String,
    targets: Vec<String>,
}

async fn batch_suspend(
    State(app): State<Shared>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<BatchBody>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let caller = write_caller(
        &app,
        &headers,
        Resource::Identity,
        Action::Destructive,
        &trace,
    )?;
    let job = app
        .g14()
        .jobs
        .run_batch(
            &caller.session.actor,
            caller.session.role,
            &trace,
            &body.reason,
            &body.targets,
        )
        .await?;
    Ok(Json(serde_json::json!({"trace": trace, "job": job})))
}

// ---------------------------------------------------------------------------
// 发布
// ---------------------------------------------------------------------------

async fn publish_rules(
    State(app): State<Shared>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    read_role(&app, &headers, Resource::Content, &trace)?;
    Ok(Json(serde_json::json!({
        "trace": trace,
        "rules": crate::publishing::rules(),
    })))
}

async fn publish_queue(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    read_role(&app, &headers, Resource::Content, &trace)?;
    let filter = QueueFilter {
        kind: qstr(&q, "kind"),
        state: qstr(&q, "state").and_then(|v| ItemState::parse(&v)),
        limit: qusize(&q, "limit"),
        offset: qint(&q, "offset"),
    };
    let page = app.g14().publishing.queue(&filter)?;
    Ok(Json(serde_json::json!({"trace": trace, "page": page})))
}

#[derive(Deserialize)]
struct SubmissionBody {
    kind: String,
    subject_id: String,
    version: String,
    #[serde(default)]
    submitted_by: String,
    idem: String,
}

async fn submit_review(
    State(app): State<Shared>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<SubmissionBody>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let caller = write_caller(&app, &headers, Resource::Content, Action::Write, &trace)?;
    let item = app.g14().publishing.submit(
        &caller.session.actor,
        caller.session.role,
        &trace,
        crate::publishing::Submission {
            kind: body.kind,
            subject_id: body.subject_id,
            version: body.version,
            submitted_by: body.submitted_by,
            idem: body.idem,
        },
    )?;
    Ok(Json(serde_json::json!({"trace": trace, "item": item})))
}

#[derive(Deserialize)]
struct DecideBody {
    decision: String,
    reason: String,
}

async fn decide_item(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<DecideBody>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let caller = write_caller(&app, &headers, Resource::Content, Action::Write, &trace)?;
    let decision = Decision::parse(&body.decision)
        .ok_or_else(|| AdminError::bad_input("decision 只能是 approve/reject/take_down/restore"))?;
    let item = app
        .g14()
        .publishing
        .decide(
            &caller.session.actor,
            caller.session.role,
            &trace,
            &id,
            decision,
            &body.reason,
        )
        .await?;
    Ok(Json(serde_json::json!({"trace": trace, "item": item})))
}

async fn list_releases(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    read_role(&app, &headers, Resource::MiniApp, &trace)?;
    let releases = app.g14().publishing.releases(Some(&app_id))?;
    Ok(Json(
        serde_json::json!({"trace": trace, "releases": releases}),
    ))
}

#[derive(Deserialize)]
struct ReleaseBody {
    version: String,
    idem: String,
}

async fn create_release(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
    ApiJson(body): ApiJson<ReleaseBody>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let caller = write_caller(&app, &headers, Resource::MiniApp, Action::Write, &trace)?;
    let release = app.g14().publishing.create_release(
        &caller.session.actor,
        caller.session.role,
        &trace,
        &app_id,
        &body.version,
        &body.idem,
    )?;
    Ok(Json(
        serde_json::json!({"trace": trace, "release": release}),
    ))
}

async fn sign_release(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let caller = write_caller(
        &app,
        &headers,
        Resource::MiniApp,
        Action::SignPackage,
        &trace,
    )?;
    let release = app
        .g14()
        .publishing
        .sign(&caller.session.actor, caller.session.role, &trace, &id)
        .await?;
    Ok(Json(
        serde_json::json!({"trace": trace, "release": release}),
    ))
}

async fn publish_release(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let caller = write_caller(&app, &headers, Resource::MiniApp, Action::Write, &trace)?;
    let release = app
        .g14()
        .publishing
        .publish(&caller.session.actor, caller.session.role, &trace, &id)
        .await?;
    Ok(Json(
        serde_json::json!({"trace": trace, "release": release}),
    ))
}

#[derive(Deserialize)]
struct ReasonBody {
    #[serde(default)]
    reason: String,
}

async fn revoke_release(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ReasonBody>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let caller = write_caller(
        &app,
        &headers,
        Resource::MiniApp,
        Action::Destructive,
        &trace,
    )?;
    let release = app
        .g14()
        .publishing
        .revoke(
            &caller.session.actor,
            caller.session.role,
            &trace,
            &id,
            &body.reason,
        )
        .await?;
    Ok(Json(
        serde_json::json!({"trace": trace, "release": release}),
    ))
}

async fn list_services(
    State(app): State<Shared>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    read_role(&app, &headers, Resource::Service, &trace)?;
    Ok(Json(serde_json::json!({
        "trace": trace,
        "services": app.g14().publishing.services()?,
    })))
}

async fn upsert_service(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<crate::publishing::ServicePatch>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let caller = write_caller(&app, &headers, Resource::Service, Action::Write, &trace)?;
    let cfg = app.g14().publishing.upsert_service(
        &caller.session.actor,
        caller.session.role,
        &trace,
        &id,
        body,
    )?;
    Ok(Json(serde_json::json!({"trace": trace, "service": cfg})))
}

// ---------------------------------------------------------------------------
// 商务
// ---------------------------------------------------------------------------

/// 商户视野。默认全量；`?as_merchant=<id>` 只有超管能用，用来复现"商户只看自己"。
fn scope_of(q: &std::collections::HashMap<String, String>, role: Role) -> Result<MerchantScope> {
    let Some(id) = qstr(q, "as_merchant") else {
        return Ok(MerchantScope::All);
    };
    if role != Role::Superadmin {
        return Err(AdminError::Forbidden(
            "as_merchant 只有 superadmin 能用（商户自助账号走独立的 scope 绑定）".into(),
        ));
    }
    upstream::check_target(&id)?;
    Ok(MerchantScope::Only(id))
}

async fn list_merchants(
    State(app): State<Shared>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    read_role(&app, &headers, Resource::Merchant, &trace)?;
    Ok(Json(serde_json::json!({
        "trace": trace,
        "merchants": app.g14().commerce.merchants()?,
    })))
}

async fn list_orders(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let role = read_role(&app, &headers, Resource::Order, &trace)?;
    let scope = scope_of(&q, role)?;
    let filter = OrderFilter {
        merchant_id: qstr(&q, "merchant_id"),
        state: qstr(&q, "state"),
        limit: qusize(&q, "limit"),
        offset: qint(&q, "offset"),
    };
    let page = app.g14().commerce.orders(&scope, &filter)?;
    Ok(Json(serde_json::json!({"trace": trace, "page": page})))
}

async fn export_orders(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Response> {
    let trace = new_trace();
    let (actor, role) = actor_of(&app, &headers, &trace)?;
    let scope = scope_of(&q, role)?;
    let filter = OrderFilter {
        merchant_id: qstr(&q, "merchant_id"),
        state: qstr(&q, "state"),
        limit: qusize(&q, "limit"),
        offset: qint(&q, "offset"),
    };
    let csv = app
        .g14()
        .commerce
        .export_orders(&actor, role, &trace, &scope, &filter)?;
    Ok((
        [(axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8")],
        csv,
    )
        .into_response())
}

#[derive(Deserialize)]
struct RefundBody {
    amount_cents: i64,
    reason: String,
    idem: String,
}

async fn request_refund(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<RefundBody>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let caller = write_caller(&app, &headers, Resource::Order, Action::Write, &trace)?;
    let refund = app.g14().commerce.request_refund(
        &caller.session.actor,
        caller.session.role,
        &trace,
        &MerchantScope::All,
        &id,
        body.amount_cents,
        &body.reason,
        &body.idem,
    )?;
    Ok(Json(serde_json::json!({"trace": trace, "refund": refund})))
}

async fn list_refunds(
    State(app): State<Shared>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    read_role(&app, &headers, Resource::Order, &trace)?;
    Ok(Json(serde_json::json!({
        "trace": trace,
        "refunds": app.g14().commerce.refunds()?,
    })))
}

#[derive(Deserialize)]
struct ApproveBody {
    note: String,
}

async fn approve_refund(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ApproveBody>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let caller = write_caller(
        &app,
        &headers,
        Resource::Order,
        Action::ApproveMoney,
        &trace,
    )?;
    let refund = app
        .g14()
        .commerce
        .approve_refund(
            &caller.session.actor,
            caller.session.role,
            &trace,
            &id,
            &body.note,
        )
        .await?;
    Ok(Json(serde_json::json!({"trace": trace, "refund": refund})))
}

async fn list_recon(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    read_role(&app, &headers, Resource::Recon, &trace)?;
    let batch = qstr(&q, "batch");
    Ok(Json(serde_json::json!({
        "trace": trace,
        "diffs": app.g14().commerce.diffs(batch.as_deref())?,
    })))
}

#[derive(Deserialize)]
struct ResolveBody {
    decision: String,
    note: String,
}

async fn resolve_recon(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ResolveBody>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let caller = write_caller(
        &app,
        &headers,
        Resource::Recon,
        Action::ApproveMoney,
        &trace,
    )?;
    let diff = app.g14().commerce.resolve_diff(
        &caller.session.actor,
        caller.session.role,
        &trace,
        &id,
        &body.decision,
        &body.note,
    )?;
    Ok(Json(serde_json::json!({"trace": trace, "diff": diff})))
}

async fn list_journal(
    State(app): State<Shared>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    read_role(&app, &headers, Resource::Journal, &trace)?;
    Ok(Json(serde_json::json!({
        "trace": trace,
        "entries": app.g14().commerce.journal()?,
    })))
}

#[derive(Deserialize)]
struct JournalBody {
    entry_no: String,
    memo: String,
    source: String,
    lines: Vec<JournalLine>,
}

async fn post_journal(
    State(app): State<Shared>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<JournalBody>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let caller = write_caller(
        &app,
        &headers,
        Resource::Journal,
        Action::ApproveMoney,
        &trace,
    )?;
    let entries = app.g14().commerce.post_journal(
        &caller.session.actor,
        caller.session.role,
        &trace,
        &body.entry_no,
        &body.memo,
        &body.source,
        &body.lines,
    )?;
    Ok(Json(
        serde_json::json!({"trace": trace, "entries": entries}),
    ))
}

// ---------------------------------------------------------------------------
// 作业与隐私
// ---------------------------------------------------------------------------

async fn list_jobs(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    read_role(&app, &headers, Resource::Job, &trace)?;
    let filter = JobFilter {
        kind: qstr(&q, "kind"),
        state: qstr(&q, "state").and_then(|v| JobState::parse(&v)),
        limit: qusize(&q, "limit"),
        offset: qint(&q, "offset"),
    };
    let page = app.g14().jobs.list(&filter)?;
    Ok(Json(serde_json::json!({"trace": trace, "page": page})))
}

async fn retry_job(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let caller = write_caller(&app, &headers, Resource::Job, Action::Write, &trace)?;
    let job = app
        .g14()
        .jobs
        .retry(&caller.session.actor, caller.session.role, &trace, &id)?;
    Ok(Json(serde_json::json!({"trace": trace, "job": job})))
}

async fn cancel_job(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let caller = write_caller(&app, &headers, Resource::Job, Action::Write, &trace)?;
    let job = app
        .g14()
        .jobs
        .cancel(&caller.session.actor, caller.session.role, &trace, &id)?;
    Ok(Json(serde_json::json!({"trace": trace, "job": job})))
}

async fn list_privacy(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    read_role(&app, &headers, Resource::Privacy, &trace)?;
    let user = qstr(&q, "user_id");
    Ok(Json(serde_json::json!({
        "trace": trace,
        "requests": app.g14().privacy.list(user.as_deref())?,
    })))
}

#[derive(Deserialize)]
struct PrivacyBody {
    user_id: String,
    kind: String,
    reason: String,
}

async fn request_privacy(
    State(app): State<Shared>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<PrivacyBody>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let caller = write_caller(&app, &headers, Resource::Privacy, Action::Write, &trace)?;
    let kind = PrivacyKind::parse(&body.kind)
        .ok_or_else(|| AdminError::bad_input("kind 只能是 export 或 delete"))?;
    let action = match kind {
        PrivacyKind::Export => Action::ExportPii,
        PrivacyKind::Delete => Action::Destructive,
    };
    auth::require(caller.session.role, Resource::Privacy, action)?;
    let (request, token) = app.g14().privacy.request(
        &caller.session.actor,
        caller.session.role,
        &trace,
        &body.user_id,
        kind,
        &body.reason,
    )?;
    // 令牌只在这一条响应里出现一次
    Ok(Json(serde_json::json!({
        "trace": trace,
        "request": request,
        "download_token": token,
        "next": "把 download_token 交给申请者；GET /api/g14/privacy/requests/{id}/download?token=…",
    })))
}

async fn fulfill_privacy(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ReasonBody>,
) -> Result<Json<serde_json::Value>> {
    let trace = new_trace();
    let caller = write_caller(&app, &headers, Resource::Privacy, Action::Write, &trace)?;
    let request = app.g14().privacy.fulfill(
        &caller.session.actor,
        caller.session.role,
        &trace,
        &id,
        &body.reason,
    )?;
    Ok(Json(
        serde_json::json!({"trace": trace, "request": request}),
    ))
}

async fn download_privacy(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>> {
    // 这条路由**不要**会话令牌：申请者手里只有 download_token。
    let token = qstr(&q, "token")
        .ok_or_else(|| AdminError::bad_input("缺少 token：用申请时返回的 download_token"))?;
    let request = app.g14().privacy.claim(&id, &token)?;
    Ok(Json(serde_json::json!({
        "request": request,
        "note": "导出内容由域侧作业产出；此端点只放行申请者本人拿令牌取件",
    })))
}

// ---------------------------------------------------------------------------
// 查询参数（与 http.rs 同一套宽容度）
// ---------------------------------------------------------------------------

fn qstr(q: &std::collections::HashMap<String, String>, key: &str) -> Option<String> {
    q.get(key)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn qint(q: &std::collections::HashMap<String, String>, key: &str) -> Option<i64> {
    qstr(q, key).and_then(|v| v.parse().ok())
}

fn qusize(q: &std::collections::HashMap<String, String>, key: &str) -> Option<usize> {
    qstr(q, key).and_then(|v| v.parse().ok())
}

/// 响应里带 trace 的统一形状（供文档与前端约定）。
#[derive(Serialize)]
pub struct Traced<T: Serialize> {
    pub trace: String,
    pub data: T,
}

/// 供测试与文档引用的一句话说明：这套路由的准入模型是什么。
pub const ACCESS_MODEL: &str = concat!(
    "读：运营静态令牌或后台会话；写：后台会话 + 资源矩阵 + 限流；",
    "敏感动作：额外步进验证（TOTP，窗口 15 分钟）",
);

/// 仅用于编译期检查 `Audience` 与准入说明一致（防止有人删掉 audience 判定却留着文档）。
pub fn audiences() -> [Audience; 4] {
    [
        Audience::Operator,
        Audience::AdminSession,
        Audience::EndUser,
        Audience::Unknown,
    ]
}
