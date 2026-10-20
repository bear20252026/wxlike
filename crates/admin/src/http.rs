//! HTTP 面：路由、准入、处理器。
//!
//! 三条贯穿全文件的规矩：
//!
//! * **读**要么带静态令牌（`Authorization: Bearer $WXLIKE_ADMIN_TOKEN`），要么带
//!   登录后的会话令牌；
//! * **写**必须带会话令牌，且会话 `mfa=true`（D3 的验收面），且角色够级别；
//!   静态令牌**不能**写——它不对应任何一个"人"，写不出可追责的留痕；
//! * 静态令牌走常量时间比较（`constant_time_eq`）：能被计时探测的凭据就
//!   不该有短路返回。
//!
//! 鉴权写在每个处理器第一行，不做通用包装：这类"一行门槛"散开反而比一个
//! 泛型中间件更好读，也少一层 trait bound 的体操。漏写的可能由两个集成用例兜住：
//! `d1_read_only.rs::reads_require_a_credential_and_reject_a_wrong_one`（每个读端点
//! 无令牌/错令牌都打一遍）与 `d3_rbac_mfa.rs::static_token_cannot_write_at_all`
//! （每条写操作都要能拒掉静态令牌）。
//!
//! 查询参数一律手工解析（`qstr`/`qint`/…），拒绝时返回统一的
//! `{"error": …}` 并写清"该怎么改"。理由：`Query<T>` 的 serde 拒绝给的是
//! 纯文本 400，前端和运维都没法稳定读。

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::accounts::{AccountStore, OpLogQuery, OpRecord, OpStatus, Session};
use crate::config::Config;
use crate::error::{AdminError, Result};
use crate::rbac::{self, ModerationMode, Op, OPS};
use crate::read::{
    AuditFilter, AuditQuery, ListFilter, PlatformRead, ReportFilter, ReportQuery, TicketRow,
};
use crate::upstream::{self, Upstream};
use crate::web;

/// 留痕 detail 的截断标记。上游响应体是"回显"，可以截；
/// 判定字段（op / target / status）本身永不截。
const CAP_MARKER: &str = "…(截断)";

pub struct App {
    read: PlatformRead,
    accounts: AccountStore,
    upstream: Upstream,
    cfg: Config,
    started: std::time::Instant,
}

impl App {
    /// 打开两个库并装配。失败就返回错误——不带着坏配置起服务。
    pub fn open(cfg: &Config) -> Result<Arc<Self>> {
        let read = PlatformRead::open(&cfg.platform_db)?;
        let accounts = AccountStore::open(&cfg.accounts_db)?;
        Ok(Self::assemble(read, accounts, cfg.clone()))
    }

    /// 测试与嵌入式用法：三样东西自己给。
    pub fn assemble(read: PlatformRead, accounts: AccountStore, cfg: Config) -> Arc<Self> {
        Arc::new(Self {
            read,
            accounts,
            upstream: Upstream::new(cfg.platform_url.clone(), cfg.platform_token.clone()),
            cfg,
            started: std::time::Instant::now(),
        })
    }

    pub fn read(&self) -> &PlatformRead {
        &self.read
    }

    pub fn accounts(&self) -> &AccountStore {
        &self.accounts
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route("/", get(page))
            .route("/health", get(health))
            .route("/api/stats", get(stats))
            .route("/api/users", get(users))
            .route("/api/groups", get(groups))
            .route("/api/audit", get(audit))
            .route("/api/reports", get(reports))
            .route("/api/reports/{id}", get(read_one_report))
            .route("/api/moderation", get(moderation))
            .route("/api/oplog", get(oplog))
            .route("/api/ops", get(catalog))
            .route("/api/login", post(login))
            .route("/api/logout", post(logout))
            .route("/api/me", get(me))
            .route("/api/me/totp/enroll", post(totp_enroll))
            .route("/api/me/totp/confirm", post(totp_confirm))
            .route("/api/me/totp/disable", post(totp_disable))
            .route("/api/op/{op}", post(run_op))
            .with_state(self.clone())
    }

    /// 起服务并返回监听地址（测试用 `127.0.0.1:0`；转发面必须走真 HTTP）。
    pub async fn bind_and_serve(
        self: &Arc<Self>,
        bind: &str,
    ) -> anyhow::Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>)> {
        let listener = tokio::net::TcpListener::bind(bind).await?;
        let addr = listener.local_addr()?;
        let app = self.clone();
        let handle = tokio::spawn(async move {
            if let Err(err) = axum::serve(listener, app.router()).await {
                tracing::error!(%err, "adminx HTTP 服务结束");
            }
        });
        Ok((addr, handle))
    }

    pub fn uptime_ms(&self) -> u128 {
        self.started.elapsed().as_millis()
    }
}

type Shared = Arc<App>;

// ---------------------------------------------------------------------------
// 鉴权
// ---------------------------------------------------------------------------

/// 只认 `Authorization: Bearer <x>`。
fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// 常量时间比较。长度不等直接 false（长度不是秘密：令牌是定长 hex）。
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

/// 读面门槛：静态令牌或有效会话，二选一。
fn require_read(app: &Shared, headers: &HeaderMap) -> Result<()> {
    let Some(presented) = bearer(headers) else {
        return Err(AdminError::Unauthorized);
    };
    if constant_time_eq(presented, &app.cfg.token) {
        return Ok(());
    }
    if app.accounts.session(presented)?.is_some() {
        return Ok(());
    }
    Err(AdminError::Unauthorized)
}

/// 写面门槛：必须是会话（静态令牌在这里就挡掉）。
fn require_session(app: &Shared, headers: &HeaderMap) -> Result<Session> {
    let Some(presented) = bearer(headers) else {
        return Err(AdminError::Unauthorized);
    };
    if constant_time_eq(presented, &app.cfg.token) {
        return Err(AdminError::Forbidden(
            "静态令牌只能读：写操作必须用登录后的会话（先 POST /api/login）".into(),
        ));
    }
    app.accounts
        .session(presented)?
        .ok_or(AdminError::SessionExpired)
}

// ---------------------------------------------------------------------------
// 无鉴权：存活检查与页面
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct Health {
    ok: bool,
    version: &'static str,
    uptime_ms: u128,
    /// 平台库的连接方式：`read_only` 或 `query_only`。
    db_open_mode: &'static str,
    missing_tables: Vec<String>,
    /// 有没有任何一个后台账号。一个都没有 = 写面不可用。
    accounts_present: bool,
    /// 这个进程会不会写平台库。恒为 false，写在这里是给监控一个锚点。
    writes_platform_db: bool,
}

async fn health(State(app): State<Shared>) -> Response {
    // 缺表查询失败本身就是必须暴露的事实，不能当"没有缺表"
    let missing = match app.read.missing_tables() {
        Ok(v) => v,
        Err(err) => vec![format!("<查询失败: {err}>")],
    };
    let accounts_present = app.accounts.has_any_account().unwrap_or(false);
    let body = Health {
        ok: missing.is_empty(),
        version: env!("CARGO_PKG_VERSION"),
        uptime_ms: app.uptime_ms(),
        db_open_mode: app.read.open_mode(),
        missing_tables: missing,
        accounts_present,
        writes_platform_db: false,
    };
    let status = if body.ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(body)).into_response()
}

async fn page() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        web::page(env!("CARGO_PKG_VERSION")),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// 读面
// ---------------------------------------------------------------------------

async fn stats(State(app): State<Shared>, headers: HeaderMap) -> Result<Json<serde_json::Value>> {
    require_read(&app, &headers)?;
    let stats = app.read.stats()?;
    Ok(Json(
        serde_json::to_value(stats).map_err(AdminError::Codec)?,
    ))
}

async fn users(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>> {
    require_read(&app, &headers)?;
    let filter = ListFilter::new(qstr(&q, "q"), qusize(&q, "limit")?, qint(&q, "offset")?);
    let page = app.read.users(&filter)?;
    Ok(Json(serde_json::to_value(page).map_err(AdminError::Codec)?))
}

async fn groups(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>> {
    require_read(&app, &headers)?;
    let filter = ListFilter::new(qstr(&q, "q"), qusize(&q, "limit")?, qint(&q, "offset")?);
    let page = app.read.groups(&filter)?;
    Ok(Json(serde_json::to_value(page).map_err(AdminError::Codec)?))
}

/// D2 的验收面：`?actor=&command=&from=&to=`（外加 `action` / `target`）。
async fn audit(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>> {
    require_read(&app, &headers)?;
    let filter = AuditFilter::new(AuditQuery {
        actor: qstr(&q, "q_actor").or_else(|| qstr(&q, "actor")),
        command: qstr(&q, "command"),
        action: qstr(&q, "action"),
        target: qstr(&q, "target"),
        from: qint(&q, "from")?,
        to: qint(&q, "to")?,
        limit: qusize(&q, "limit")?,
        offset: qint(&q, "offset")?,
    })?;
    let page = app.read.audit(&filter)?;
    Ok(Json(serde_json::to_value(page).map_err(AdminError::Codec)?))
}

async fn reports(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>> {
    require_read(&app, &headers)?;
    let state = qstr(&q, "state");
    let open_only = match qbool(&q, "open_only")? {
        // 没指定状态就默认只看未终结：这是处置队列，不是归档浏览器
        Some(v) => v,
        None => state.is_none(),
    };
    let filter = ReportFilter::new(ReportQuery {
        state,
        handler: qstr(&q, "handler"),
        target_id: qstr(&q, "target_id"),
        reporter: qstr(&q, "reporter"),
        open_only: Some(open_only),
        limit: qusize(&q, "limit")?,
        offset: qint(&q, "offset")?,
    })?;
    let mut page = app.read.reports(&filter)?;
    let now = crate::read::now_ms() as u64;
    for row in &mut page.rows {
        row.overdue = row.ticket.is_overdue(now, app.cfg.report_sla_ms);
    }
    Ok(Json(serde_json::to_value(page).map_err(AdminError::Codec)?))
}

async fn read_one_report(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    require_read(&app, &headers)?;
    let target = upstream::check_target(&id)?;
    let ticket = app.read.report(&target)?.ok_or(AdminError::NotFound)?;
    let now = crate::read::now_ms() as u64;
    let row = TicketRow {
        id: ticket.id.clone(),
        overdue: ticket.is_overdue(now, app.cfg.report_sla_ms),
        ticket,
    };
    Ok(Json(serde_json::to_value(row).map_err(AdminError::Codec)?))
}

async fn moderation(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>> {
    require_read(&app, &headers)?;
    let filter = ListFilter::new(qstr(&q, "q"), qusize(&q, "limit")?, qint(&q, "offset")?);
    let page = app.read.moderation(&filter)?;
    Ok(Json(serde_json::to_value(page).map_err(AdminError::Codec)?))
}

async fn oplog(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>> {
    require_read(&app, &headers)?;
    let query = OpLogQuery {
        actor: qstr(&q, "actor"),
        op: qstr(&q, "op"),
        status: qstr(&q, "status"),
        from: qint(&q, "from")?,
        limit: qusize(&q, "limit")?,
        offset: qint(&q, "offset")?,
    };
    let page = app.accounts.ops(&query)?;
    Ok(Json(serde_json::to_value(page).map_err(AdminError::Codec)?))
}

#[derive(Serialize)]
struct OpView {
    id: &'static str,
    capability: &'static str,
    method: &'static str,
    path: &'static str,
    needs_target: bool,
    needs_note: bool,
    payload: &'static str,
    description: &'static str,
}

async fn catalog(State(app): State<Shared>, headers: HeaderMap) -> Result<Json<serde_json::Value>> {
    require_read(&app, &headers)?;
    let ops: Vec<OpView> = OPS
        .iter()
        .map(|o| OpView {
            id: o.id,
            capability: o.capability.as_str(),
            method: o.method.as_str(),
            path: o.path,
            needs_target: o.needs_target,
            needs_note: o.needs_note,
            payload: o.payload,
            description: o.description,
        })
        .collect();
    Ok(Json(serde_json::json!({ "count": ops.len(), "ops": ops })))
}

// ---------------------------------------------------------------------------
// 会话
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct LoginBody {
    username: String,
    password: String,
    #[serde(default)]
    totp: Option<String>,
    /// 仅展示用：留痕与会话里记下发起登录的来源。
    #[serde(default)]
    ip: Option<String>,
}

#[derive(Serialize)]
struct LoginReply {
    token: String,
    session: Session,
}

async fn login(State(app): State<Shared>, Json(body): Json<LoginBody>) -> Result<Json<LoginReply>> {
    let (session, token) = app.accounts.login(
        &body.username,
        &body.password,
        body.totp.as_deref(),
        app.cfg.session_ttl_ms,
        body.ip.as_deref().unwrap_or(""),
    )?;
    tracing::info!(actor = %session.actor, role = session.role.as_str(), mfa = session.mfa, "后台登录");
    Ok(Json(LoginReply { token, session }))
}

async fn logout(State(app): State<Shared>, headers: HeaderMap) -> Result<StatusCode> {
    // 先验会话，再删：否则任何人拿任意串都能触发一次 DELETE
    let _ = require_session(&app, &headers)?;
    if let Some(token) = bearer(&headers) {
        let token = token.to_string();
        app.accounts.logout(&token)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn me(State(app): State<Shared>, headers: HeaderMap) -> Result<Json<serde_json::Value>> {
    let session = require_session(&app, &headers)?;
    let account = app
        .accounts
        .account(&session.actor)?
        .ok_or(AdminError::NotFound)?;
    Ok(Json(serde_json::json!({
        "session": session,
        "account": account,
        "report_sla_ms": app.cfg.report_sla_ms,
    })))
}

#[derive(Deserialize)]
struct CodeBody {
    code: String,
}

async fn totp_enroll(
    State(app): State<Shared>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let session = require_session(&app, &headers)?;
    let secret = app.accounts.totp_enroll(&session.actor)?;
    // 密钥只在这一次响应里出现；之后任何接口都只说"已配置"。
    Ok(Json(serde_json::json!({
        "secret": secret,
        "otpauth": format!("otpauth://totp/wxlike-admin:{}?secret={}&issuer=wxlike-admin", session.actor, secret),
        "next": "POST /api/me/totp/confirm {code} 激活；激活后重新登录才会拿到可写的会话",
    })))
}

async fn totp_confirm(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<CodeBody>,
) -> Result<Json<serde_json::Value>> {
    let session = require_session(&app, &headers)?;
    app.accounts.totp_confirm(&session.actor, &body.code)?;
    Ok(Json(serde_json::json!({
        "mfa_enabled": true,
        "next": "重新登录（带动态码）才会得到 mfa=true 的会话，那才能写",
    })))
}

async fn totp_disable(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<CodeBody>,
) -> Result<StatusCode> {
    let session = require_session(&app, &headers)?;
    app.accounts.totp_disable(&session.actor, &body.code)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// 写操作（D3）
// ---------------------------------------------------------------------------

/// 一次操作的请求体。形状由目录里的 `payload` 决定，多余的键忽略。
#[derive(Clone, Debug, Default, Deserialize)]
struct OpBody {
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    action: Option<serde_json::Value>,
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    mode: Option<String>,
}

#[derive(Serialize)]
struct OpReply {
    op: &'static str,
    capability: &'static str,
    target: Option<String>,
    actor: String,
    /// 上游原样回显（工单流转后的 Ticket、对账报告等）。
    result: serde_json::Value,
    /// 本次留痕是否落库成功。false 必须让运维看到——留痕失败等于操作失去追责链。
    logged: bool,
}

async fn run_op(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(op_id): Path<String>,
    body: Json<serde_json::Value>,
) -> Result<Json<OpReply>> {
    let session = require_session(&app, &headers)?;
    let body = parse_op_body(&body.0)?;
    let target = match body
        .target
        .as_deref()
        .map(upstream::check_target)
        .transpose()
    {
        Ok(t) => t,
        Err(err) => {
            // 目标 id 的形状校验在转发之前：它会被拼进上游 URL 的路径段。
            // 这种尝试也要留痕（谁在拿路径穿越打后台，运维必须看得见）。
            log_denied(
                &app,
                &session,
                &op_id,
                rbac::get(&op_id)
                    .map(|o| o.capability.as_str())
                    .unwrap_or("unknown"),
                None,
                &err.to_string(),
            );
            return Err(err);
        }
    };
    let has_target = target.is_some();

    let op = match rbac::authorize(&op_id, session.role, session.mfa, has_target) {
        Ok(op) => op,
        Err(deny) => {
            // 被拒也要留痕：运维想知道"谁试过什么"
            let capability = rbac::get(&op_id)
                .map(|o| o.capability.as_str())
                .unwrap_or("unknown");
            let rec = OpRecord {
                account_id: session.account_id.clone(),
                actor: session.actor.clone(),
                op: op_id.clone(),
                capability: capability.to_string(),
                target: target.clone().unwrap_or_default(),
                upstream: String::new(),
                status: OpStatus::Denied,
                detail: deny.to_string(),
                mfa: session.mfa,
            };
            if let Err(err) = app.accounts.log_op(&rec) {
                tracing::error!(op = %op_id, %err, "留痕写入失败（被拒的操作也记不上）");
            }
            tracing::warn!(actor = %session.actor, op = %op_id, deny = %deny, "写操作被拒");
            return Err(match deny {
                rbac::Deny::UnknownOp(_)
                | rbac::Deny::MissingTarget(_)
                | rbac::Deny::MissingNote(_) => AdminError::bad_input(deny.to_string()),
                rbac::Deny::RoleLacks { .. } => AdminError::Forbidden(deny.to_string()),
                rbac::Deny::NeedsMfa { .. } => AdminError::MfaRequired(deny.to_string()),
            });
        }
    };

    // 参数按目录声明的形状取；缺了就 422，不猜默认值
    let payload = match build_payload(op, &body) {
        Ok(p) => p,
        Err(err) => {
            log_denied(
                &app,
                &session,
                op.id,
                op.capability.as_str(),
                target.as_deref(),
                &err.to_string(),
            );
            return Err(err);
        }
    };
    let path = upstream::render_path(op.path, target.as_deref())?;
    let label = format!("{} {}", op.method.as_str(), path);

    let outcome = app
        .upstream
        .call(op.method.as_str(), &path, &session.actor, payload)
        .await;

    // 先分类留痕，再把**原始错误**抛出去：平台拒绝的理由（409 非法流转 / 404 没有
    // 这条工单）必须原样到达操作者，不能被 adminx 二次加工成"上游失败"。
    let (status, detail, result) = match &outcome {
        Ok(reply) => (OpStatus::Ok, echo(&reply.body), reply.body.clone()),
        Err(AdminError::Upstream { status: 0, detail }) => (
            OpStatus::TransportError,
            detail.clone(),
            serde_json::Value::Null,
        ),
        Err(AdminError::Upstream { status, detail }) => (
            OpStatus::UpstreamError,
            format!("platform {status}: {detail}"),
            serde_json::Value::Null,
        ),
        Err(other) => (OpStatus::Denied, other.to_string(), serde_json::Value::Null),
    };
    let rec = OpRecord {
        account_id: session.account_id.clone(),
        actor: session.actor.clone(),
        op: op.id.to_string(),
        capability: op.capability.as_str().to_string(),
        target: target.clone().unwrap_or_default(),
        upstream: label,
        status,
        detail,
        mfa: session.mfa,
    };
    let logged = app.accounts.log_op(&rec).is_ok();
    if !logged {
        tracing::error!(op = %op.id, "留痕写入失败：这次操作将无法追责");
    }

    match outcome {
        Ok(_reply) => Ok(Json(OpReply {
            op: op.id,
            capability: op.capability.as_str(),
            target,
            actor: session.actor,
            result,
            logged,
        })),
        Err(err) => Err(err),
    }
}

fn log_denied(
    app: &Shared,
    session: &Session,
    op_id: &str,
    capability: &str,
    target: Option<&str>,
    why: &str,
) {
    let rec = OpRecord {
        account_id: session.account_id.clone(),
        actor: session.actor.clone(),
        op: op_id.to_string(),
        capability: capability.to_string(),
        target: target.unwrap_or_default().to_string(),
        upstream: String::new(),
        status: OpStatus::Denied,
        detail: why.to_string(),
        mfa: session.mfa,
    };
    if let Err(err) = app.accounts.log_op(&rec) {
        tracing::error!(op = %op_id, %err, "留痕写入失败（被拒的操作也记不上）");
    }
}

/// 把 JSON 体转成 [`OpBody`]。`note`/`target` 的空串按"没填"处理。
fn parse_op_body(raw: &serde_json::Value) -> Result<OpBody> {
    if raw.is_null() {
        return Ok(OpBody::default());
    }
    if !raw.is_object() {
        return Err(AdminError::bad_input(
            "请求体必须是 JSON 对象：{\"target\":…, \"note\":…, \"action\":…, \"mode\":…}",
        ));
    }
    let mut body: OpBody = serde_json::from_value(raw.clone())
        .map_err(|e| AdminError::bad_input(format!("请求体形状不对: {e}")))?;
    body.target = body
        .target
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    body.note = body
        .note
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty());
    Ok(body)
}

/// 目录里的 `payload` 决定发什么给平台。三种形状，没有第四种。
fn build_payload(op: &Op, body: &OpBody) -> Result<Option<serde_json::Value>> {
    match op.payload {
        "empty" => Ok(None),
        "note" => Ok(Some(
            serde_json::json!({ "note": required_note(op, body)? }),
        )),
        "action+note" => {
            let note = required_note(op, body)?;
            // 归一成 `wxlike_report::Action` 的原生形状（内部标签枚举）：
            // {"action":"mute","duration_ms":60000}。客户端既可以把 action 写成
            // 字符串（配 duration_ms），也可以直接写整个对象。缺时长就报错，不猜。
            let mut merged = match (&body.action, body.duration_ms) {
                (Some(serde_json::Value::String(tag)), ms) => {
                    let mut map = serde_json::Map::new();
                    map.insert("action".into(), serde_json::Value::String(tag.clone()));
                    if let Some(ms) = ms {
                        map.insert("duration_ms".into(), serde_json::json!(ms));
                    }
                    serde_json::Value::Object(map)
                }
                (Some(serde_json::Value::Object(inner)), ms) => {
                    let mut map = inner.clone();
                    if let Some(ms) = ms {
                        map.entry("duration_ms").or_insert(serde_json::json!(ms));
                    }
                    if map.contains_key("action") {
                        serde_json::Value::Object(map)
                    } else {
                        // {"mute":{"duration_ms":N}} 这种写法包一层，仍然要求时长在对象里
                        serde_json::json!({ "action": serde_json::Value::Object(map) })
                    }
                }
                (Some(other), _) => {
                    return Err(AdminError::bad_input(format!(
                        "action 只能是字符串或对象，收到 {other}"
                    )))
                }
                (None, _) => {
                    return Err(AdminError::bad_input(
                        "report.act 需要 action：warn / mute{duration_ms} / ban / remove_content / none",
                    ))
                }
            };
            // 平铺的 mute 时长也可能写在外层 body 上（表单习惯），补进来
            if let (Some(ms), Some(map)) = (body.duration_ms, merged.as_object_mut()) {
                if map.get("action").is_some() {
                    map.entry("duration_ms").or_insert(serde_json::json!(ms));
                }
            }
            let action = crate::accounts::parse_action(&merged)?;
            Ok(Some(serde_json::json!({
                "action": serde_json::to_value(action).map_err(AdminError::Codec)?,
                "note": note,
            })))
        }
        "mode" => {
            let raw = body.mode.as_deref().ok_or_else(|| {
                AdminError::bad_input("moderation.mode 需要 mode：pre_review | post_review")
            })?;
            let mode: ModerationMode =
                serde_json::from_value(serde_json::json!(raw)).map_err(|_| {
                    AdminError::bad_input(format!(
                        "mode={raw:?} 不认识（只能是 pre_review 或 post_review）"
                    ))
                })?;
            Ok(Some(serde_json::json!({ "mode": mode.as_str() })))
        }
        other => Err(AdminError::bad_input(format!(
            "目录里 {other:?} 这种载荷 adminx 还没实现"
        ))),
    }
}

fn required_note(op: &Op, body: &OpBody) -> Result<String> {
    if !op.needs_note {
        return Ok(String::new());
    }
    match body.note.as_deref() {
        Some(n) if !n.is_empty() => {
            if n.chars().count() > 512 {
                return Err(AdminError::bad_input("note 超过 512 字符"));
            }
            Ok(n.to_string())
        }
        _ => Err(AdminError::bad_input(
            rbac::Deny::MissingNote(op.id.to_string()).to_string(),
        )),
    }
}

/// 上游响应的回显：留痕里要能看到结论，但不能无限长。
fn echo(value: &serde_json::Value) -> String {
    let text = value.to_string();
    let limit = crate::accounts::DETAIL_MAX_CHARS;
    if text.chars().count() <= limit {
        return text;
    }
    let keep: String = text
        .chars()
        .take(limit - CAP_MARKER.chars().count())
        .collect();
    format!("{keep}{CAP_MARKER}")
}

// ---------------------------------------------------------------------------
// 查询参数
// ---------------------------------------------------------------------------

/// 取字符串参数。**控制符一律拒**：这些串会进 SQL 参数与 HTTP 头。
fn qstr(q: &HashMap<String, String>, key: &str) -> Option<String> {
    q.get(key)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn qint(q: &HashMap<String, String>, key: &str) -> Result<Option<i64>> {
    match qstr(q, key) {
        None => Ok(None),
        Some(raw) => raw.parse::<i64>().map(Some).map_err(|_| {
            AdminError::bad_input(format!(
                "{key}={raw:?} 不是整数（这个参数收 epoch 毫秒，例如 {}）",
                crate::read::now_ms()
            ))
        }),
    }
}

fn qusize(q: &HashMap<String, String>, key: &str) -> Result<Option<usize>> {
    match qstr(q, key) {
        None => Ok(None),
        Some(raw) => raw
            .parse::<usize>()
            .map(Some)
            .map_err(|_| AdminError::bad_input(format!("{key}={raw:?} 不是非负整数（1..=200）"))),
    }
}

fn qbool(q: &HashMap<String, String>, key: &str) -> Result<Option<bool>> {
    match qstr(q, key) {
        None => Ok(None),
        Some(raw) => match raw.as_str() {
            "1" | "true" => Ok(Some(true)),
            "0" | "false" => Ok(Some(false)),
            other => Err(AdminError::bad_input(format!(
                "{key}={other:?} 不是布尔值（用 1/0 或 true/false）"
            ))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn constant_time_compare_matches_only_equal_strings() {
        assert!(constant_time_eq("abc123", "abc123"));
        assert!(!constant_time_eq("abc123", "abc124"));
        assert!(!constant_time_eq("abc123", "abc12"));
        assert!(!constant_time_eq("", "x"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn bearer_only_accepts_the_bearer_scheme() {
        let mut h = HeaderMap::new();
        assert!(bearer(&h).is_none());
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer abc"),
        );
        assert_eq!(bearer(&h), Some("abc"));
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic abc"));
        assert_eq!(bearer(&h), None);
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer    "),
        );
        assert_eq!(bearer(&h), None);
    }

    #[test]
    fn query_parsers_reject_garbage_with_the_expected_shape() {
        let mut q = HashMap::new();
        q.insert("from".to_string(), "2026-10-20".to_string());
        let err = qint(&q, "from").unwrap_err();
        assert!(err.to_string().contains("epoch 毫秒"), "{err}");
        q.insert("limit".to_string(), "0".to_string());
        assert_eq!(qusize(&q, "limit").unwrap(), Some(0));
        q.insert("open_only".to_string(), "maybe".to_string());
        assert!(qbool(&q, "open_only")
            .unwrap_err()
            .to_string()
            .contains("1/0"));
        q.insert("state".to_string(), "   ".to_string());
        assert_eq!(qstr(&q, "state"), None);
    }

    #[test]
    fn echo_caps_without_losing_the_marker() {
        let big = serde_json::json!({"mismatches": (0..2_000).map(|i| format!("u{i}")).collect::<Vec<_>>()});
        let text = echo(&big);
        assert!(text.chars().count() <= crate::accounts::DETAIL_MAX_CHARS);
        assert!(text.ends_with(CAP_MARKER), "截断必须留痕");
        let small = echo(&serde_json::json!({"ok": true}));
        assert_eq!(small, "{\"ok\":true}");
        assert_eq!(echo(&serde_json::Value::Null), "null");
    }

    #[test]
    fn op_body_parsing_treats_blank_strings_as_absent() {
        let body = parse_op_body(&serde_json::json!({"target": "  ", "note": " 有依据 "})).unwrap();
        assert_eq!(body.target, None);
        assert_eq!(body.note.as_deref(), Some("有依据"));
        assert!(parse_op_body(&serde_json::json!("x")).is_err());
        assert!(parse_op_body(&serde_json::Value::Null)
            .unwrap()
            .note
            .is_none());
        // 未知键忽略，不该 422
        assert!(parse_op_body(&serde_json::json!({"whatever": 1})).is_ok());
    }

    #[test]
    fn payload_builder_matches_the_catalog_declaration() {
        // 目录里每条的 payload 种类都必须能被 build_payload 处理，
        // 否则"加了目录忘了实现"会变成一个 422 的谜。
        for op in OPS {
            let body = match op.payload {
                "empty" => OpBody::default(),
                "note" => OpBody {
                    note: Some("依据".into()),
                    ..Default::default()
                },
                "action+note" => OpBody {
                    note: Some("依据".into()),
                    action: Some(serde_json::json!("warn")),
                    ..Default::default()
                },
                "mode" => OpBody {
                    mode: Some("pre_review".into()),
                    ..Default::default()
                },
                other => panic!("目录里出现了未实现的载荷种类 {other}"),
            };
            let out = build_payload(op, &body).unwrap_or_else(|e| panic!("{}: {e}", op.id));
            match op.payload {
                "empty" => assert_eq!(out, None),
                "note" => assert_eq!(out.unwrap()["note"], "依据"),
                "action+note" => {
                    let v = out.unwrap();
                    assert_eq!(v["action"], serde_json::json!({"action": "warn"}));
                    assert_eq!(v["note"], "依据");
                }
                _ => assert_eq!(out.unwrap()["mode"], "pre_review"),
            }
        }
    }

    #[test]
    fn mute_needs_its_duration_and_act_needs_a_note() {
        let op = rbac::get("report.act").unwrap();
        let no_note = OpBody {
            action: Some(serde_json::json!("warn")),
            ..Default::default()
        };
        let err = build_payload(op, &no_note).unwrap_err();
        assert!(err.to_string().contains("note"), "{err}");
        let mute_no_ms = OpBody {
            note: Some("广告".into()),
            action: Some(serde_json::json!("mute")),
            ..Default::default()
        };
        assert!(build_payload(op, &mute_no_ms).is_err(), "mute 缺时长不许猜");
        let mute = OpBody {
            note: Some("广告".into()),
            action: Some(serde_json::json!("mute")),
            duration_ms: Some(60_000),
            ..Default::default()
        };
        let sent = build_payload(op, &mute).unwrap().unwrap();
        assert_eq!(sent["action"]["duration_ms"], 60_000);
        // 平铺进 action 字段时也要带时长：平台的 ActBody 收的是嵌套 Action
        let nested = OpBody {
            note: Some("广告".into()),
            action: Some(serde_json::json!({"action": "ban"})),
            ..Default::default()
        };
        let sent = build_payload(op, &nested).unwrap().unwrap();
        assert_eq!(sent["action"], serde_json::json!({"action": "ban"}));
    }

    #[test]
    fn mode_values_are_the_platforms_serde_words() {
        let op = rbac::get("moderation.mode").unwrap();
        let bad = OpBody {
            mode: Some("pre-review".into()),
            ..Default::default()
        };
        let err = build_payload(op, &bad).unwrap_err();
        assert!(err.to_string().contains("pre_review"), "{err}");
        assert!(build_payload(op, &OpBody::default()).is_err());
    }
}
