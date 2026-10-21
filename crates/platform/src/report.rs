//! P4 举报工单的持久化与 HTTP 面（状态机在 [`wxlike_report`]）。
//!
//! 分层理由：状态机是纯逻辑（可脱离数据库验收），本文件只做"读出来 →
//! 应用动作 → 写回去"，不重新实现任何流转规则。所有写路径都经过
//! `Ticket::triage/act/close/reject`，非法流转在这里就是 409，而不是
//! 靠 SQL 的 CHECK 兜底。
//!
//! 权限：举报人只能看自己的工单；处置动作需要管理令牌（P6 adminx 之前用
//! `x-admin-token`，见 [`crate::PlatformOptions::admin_token`]）。

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension as _};
use serde::Serialize;
use wxlike_report::{Action, Reason, State as TicketState, Ticket};

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::validate;
use crate::{Platform, PlatformState};

/// `reports` 一行的读取形状（列多，别名避免 clippy::type_complexity）。
type ReportRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
    i64,
    String,
);

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/reports", post(create))
        .route("/api/reports", get(mine))
        .route("/api/admin/reports", get(queue))
        .route("/api/admin/reports/{id}/triage", post(triage))
        .route("/api/admin/reports/{id}/act", post(act))
        .route("/api/admin/reports/{id}/close", post(close))
        .route("/api/admin/reports/{id}/reject", post(reject))
}

#[derive(serde::Deserialize)]
pub struct ReportBody {
    pub target_kind: String,
    pub target_id: String,
    pub reason: String,
    #[serde(default)]
    pub detail: String,
}

#[derive(serde::Deserialize)]
pub struct NoteBody {
    #[serde(default)]
    pub note: String,
}

/// 处置请求体。
///
/// `Action` 是**内部标签**枚举（`#[serde(tag = "action")]`），它的原生形状
/// 本来就是平铺的：
///
/// ```json
/// {"action": "mute", "duration_ms": 60000, "note": "禁言一小时"}
/// ```
///
/// 但历史上 `ActBody { action: Action }` 直接 derive 出来的形状是**嵌套**的
/// （`{"action": {"action": "warn"}}`）——同一个键出现两次，很难看，也让
/// 客户端和文档各写各的。`crates/admin` 早就遇到过同一个问题，做法是
/// "两种都收、在边界归一化"（见 `admin::accounts::parse_action`）。这里照做：
///
/// - **平铺是规范形状**，文档与 curl 示例都用它；
/// - **嵌套仍然接受**，因为已经发出去的调用方不该被一次格式调整打断；
/// - 缺 `action` 一律 422，不猜默认值——"默默按 warn 处理"是最坏的兜底。
#[derive(Debug)]
pub struct ActBody {
    pub action: Action,
    pub note: String,
}

impl<'de> serde::Deserialize<'de> for ActBody {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;

        let raw = serde_json::Value::deserialize(deserializer)?;
        let note = raw
            .get("note")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let action = parse_action_value(&raw).map_err(D::Error::custom)?;
        Ok(Self { action, note })
    }
}

/// 从请求体里取出 `Action`：先按**平铺**解析（规范形状），失败再试旧的嵌套形状。
///
/// 拆成独立函数是为了压平嵌套：写在 `Deserialize` 里会变成
/// `match` 套 `match` 再套闭包，深度 4 层——质量闸门会拦（实测拦过一次）。
fn parse_action_value(raw: &serde_json::Value) -> std::result::Result<Action, String> {
    const HELP: &str = "（可选 warn / mute{\"duration_ms\":N} / ban / remove_content / none）";

    let flat_err = match serde_json::from_value::<Action>(raw.clone()) {
        Ok(action) => return Ok(action),
        Err(err) => err,
    };
    // 平铺失败：只可能是"嵌套"或"根本没写 action"
    let Some(inner) = raw.get("action") else {
        return Err(format!("缺少 action：{flat_err}{HELP}"));
    };
    serde_json::from_value::<Action>(inner.clone())
        .map_err(|nested| format!("action 不合法：flat={flat_err}; nested={nested}{HELP}"))
}

#[derive(serde::Deserialize, Default)]
pub struct QueueQuery {
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Serialize)]
struct TicketView<'a> {
    ticket: &'a Ticket,
    overdue: bool,
}

impl Platform {
    /// 创建工单（风控由调用方在路由层过闸：举报轰炸在 `recovery` 模块的
    /// `risk_gate` 里拦）。
    pub fn create_report(
        &self,
        reporter: &str,
        target_kind: &str,
        target_id: &str,
        reason: Reason,
        detail: &str,
    ) -> Result<Ticket> {
        let id = crate::auth::random_hex(8);
        let ticket = Ticket::open(
            &id,
            reporter,
            target_kind,
            target_id,
            reason,
            detail,
            wxlike_core::types::now_ms(),
        )
        .map_err(map_report_error)?;
        self.save_report(&ticket)?;
        self.audit(reporter, "report.create", &id, target_kind);
        Ok(ticket)
    }

    pub fn report(&self, id: &str) -> Result<Option<Ticket>> {
        let c = self.db().lock();
        let row: Option<ReportRow> = c
            .query_row(
                "SELECT reporter, target_kind, target_id, reason, detail, state, action_json,
                        handler, created_at, updated_at, history_json
                 FROM reports WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                        r.get(9)?,
                        r.get(10)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            reporter,
            target_kind,
            target_id,
            reason,
            detail,
            state,
            action_json,
            handler,
            created_at,
            updated_at,
            history_json,
        )) = row
        else {
            return Ok(None);
        };
        Ok(Some(Ticket {
            id: id.to_string(),
            reporter,
            target_kind,
            target_id,
            reason: Reason::parse(&reason).ok_or(PlatformError::BadInput("bad reason"))?,
            detail,
            state: TicketState::parse(&state).ok_or(PlatformError::BadInput("bad state"))?,
            created_at: created_at.max(0) as u64,
            updated_at: updated_at.max(0) as u64,
            action: action_json.and_then(|s| serde_json::from_str(&s).ok()),
            handler,
            history: serde_json::from_str(&history_json).unwrap_or_default(),
        }))
    }

    /// 举报人自己的工单，最新在前。
    pub fn reports_by(&self, reporter: &str, limit: usize) -> Result<Vec<Ticket>> {
        let ids: Vec<String> = {
            let c = self.db().lock();
            let mut stmt = c.prepare(
                "SELECT id FROM reports WHERE reporter = ?1 ORDER BY created_at DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![reporter, limit.clamp(1, 200) as i64], |r| {
                r.get::<_, String>(0)
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(t) = self.report(&id)? {
                out.push(t);
            }
        }
        Ok(out)
    }

    /// 处置队列：按状态过滤（`open` / `triaged` / 空 = 全部未终结）。
    pub fn report_queue(&self, state: Option<&str>, limit: usize) -> Result<Vec<Ticket>> {
        let ids: Vec<String> = {
            let c = self.db().lock();
            match state {
                Some(s) => {
                    let mut stmt = c.prepare(
                        "SELECT id FROM reports WHERE state = ?1 ORDER BY created_at ASC LIMIT ?2",
                    )?;
                    let rows = stmt.query_map(params![s, limit.clamp(1, 200) as i64], |r| {
                        r.get::<_, String>(0)
                    })?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()?
                }
                None => {
                    let mut stmt = c.prepare(
                        "SELECT id FROM reports WHERE state IN ('open','triaged','actioned')
                         ORDER BY created_at ASC LIMIT ?1",
                    )?;
                    let rows = stmt.query_map(params![limit.clamp(1, 200) as i64], |r| {
                        r.get::<_, String>(0)
                    })?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()?
                }
            }
        };
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(t) = self.report(&id)? {
                out.push(t);
            }
        }
        Ok(out)
    }

    pub fn triage_report(&self, id: &str, actor: &str, note: &str) -> Result<Ticket> {
        let mut t = self.report(id)?.ok_or(PlatformError::NotFound)?;
        t.triage(actor, note, wxlike_core::types::now_ms())
            .map_err(map_report_error)?;
        self.save_report(&t)?;
        Ok(t)
    }

    pub fn act_on_report(
        &self,
        id: &str,
        actor: &str,
        action: Action,
        note: &str,
    ) -> Result<Ticket> {
        let mut t = self.report(id)?.ok_or(PlatformError::NotFound)?;
        t.act(actor, action, note, wxlike_core::types::now_ms())
            .map_err(map_report_error)?;
        self.save_report(&t)?;
        self.audit(actor, "report.action", id, &format!("{:?}", t.action));
        Ok(t)
    }

    pub fn close_report(&self, id: &str, actor: &str, note: &str) -> Result<Ticket> {
        let mut t = self.report(id)?.ok_or(PlatformError::NotFound)?;
        t.close(actor, note, wxlike_core::types::now_ms())
            .map_err(map_report_error)?;
        self.save_report(&t)?;
        Ok(t)
    }

    pub fn reject_report(&self, id: &str, actor: &str, note: &str) -> Result<Ticket> {
        let mut t = self.report(id)?.ok_or(PlatformError::NotFound)?;
        t.reject(actor, note, wxlike_core::types::now_ms())
            .map_err(map_report_error)?;
        self.save_report(&t)?;
        Ok(t)
    }

    fn save_report(&self, t: &Ticket) -> Result<()> {
        let action = match &t.action {
            Some(a) => Some(serde_json::to_string(a)?),
            None => None,
        };
        let history = serde_json::to_string(&t.history)?;
        let c = self.db().lock();
        c.execute(
            "INSERT INTO reports(id, reporter, target_kind, target_id, reason, detail, state,
                                 action_json, handler, created_at, updated_at, history_json)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET
               state = excluded.state,
               action_json = excluded.action_json,
               handler = excluded.handler,
               updated_at = excluded.updated_at,
               history_json = excluded.history_json",
            params![
                t.id,
                t.reporter,
                t.target_kind,
                t.target_id,
                t.reason.as_str(),
                t.detail,
                t.state.as_str(),
                action,
                t.handler,
                t.created_at,
                t.updated_at,
                history
            ],
        )?;
        Ok(())
    }
}

fn map_report_error(e: wxlike_report::ReportError) -> PlatformError {
    match e {
        wxlike_report::ReportError::IllegalTransition { .. } => {
            PlatformError::Conflict("illegal ticket transition".into())
        }
        wxlike_report::ReportError::EmptyActor => PlatformError::BadInput("actor required"),
        wxlike_report::ReportError::NoteTooLong => PlatformError::BadInput("note too long"),
        wxlike_report::ReportError::BadTargetKind => {
            PlatformError::BadInput("target_kind must be user|message|group|channel")
        }
        wxlike_report::ReportError::Terminal => PlatformError::Conflict("ticket closed".into()),
    }
}

fn require_admin(platform: &Platform, headers: &HeaderMap) -> std::result::Result<(), ApiError> {
    let presented = headers.get("x-admin-token").and_then(|v| v.to_str().ok());
    if platform.admin_ok(presented) {
        Ok(())
    } else {
        Err(PlatformError::AdminRequired.into())
    }
}

fn admin_actor(headers: &HeaderMap) -> String {
    headers
        .get("x-admin-actor")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("admin")
        .to_string()
}

async fn create(
    State(platform): State<PlatformState>,
    authed: Authed,
    Json(body): Json<ReportBody>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    // 举报轰炸过风控闸（Throttle/Deny 都落到 429）
    platform.risk_gate(wxlike_risk::Action::Report, &authed.user_id, None)?;
    let reason = Reason::parse(&body.reason)
        .ok_or_else(|| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "unknown reason"))?;
    let target_id = validate::id(&body.target_id)?;
    let ticket = platform.create_report(
        &authed.user_id,
        &body.target_kind,
        target_id,
        reason,
        &body.detail,
    )?;
    platform.risk_record(wxlike_risk::Action::Report, &authed.user_id)?;
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({ "ticket_id": ticket.id, "state": ticket.state })),
    ))
}

async fn mine(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let rows = platform.reports_by(&authed.user_id, 50)?;
    Ok(Json(serde_json::json!({ "tickets": rows })))
}

async fn queue(
    State(platform): State<PlatformState>,
    headers: HeaderMap,
    Query(q): Query<QueueQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    require_admin(&platform, &headers)?;
    let sla = platform.options().report_sla_ms;
    let now = wxlike_core::types::now_ms();
    let tickets = platform.report_queue(q.state.as_deref(), q.limit.unwrap_or(50))?;
    let views: Vec<TicketView> = tickets
        .iter()
        .map(|t| TicketView {
            ticket: t,
            overdue: t.is_overdue(now, sla),
        })
        .collect();
    Ok(Json(serde_json::json!({ "queue": views })))
}

async fn triage(
    State(platform): State<PlatformState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<NoteBody>,
) -> std::result::Result<Json<Ticket>, ApiError> {
    require_admin(&platform, &headers)?;
    Ok(Json(platform.triage_report(
        &id,
        &admin_actor(&headers),
        &body.note,
    )?))
}

async fn act(
    State(platform): State<PlatformState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ActBody>,
) -> std::result::Result<Json<Ticket>, ApiError> {
    require_admin(&platform, &headers)?;
    Ok(Json(platform.act_on_report(
        &id,
        &admin_actor(&headers),
        body.action,
        &body.note,
    )?))
}

async fn close(
    State(platform): State<PlatformState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<NoteBody>,
) -> std::result::Result<Json<Ticket>, ApiError> {
    require_admin(&platform, &headers)?;
    Ok(Json(platform.close_report(
        &id,
        &admin_actor(&headers),
        &body.note,
    )?))
}

async fn reject(
    State(platform): State<PlatformState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<NoteBody>,
) -> std::result::Result<Json<Ticket>, ApiError> {
    require_admin(&platform, &headers)?;
    Ok(Json(platform.reject_report(
        &id,
        &admin_actor(&headers),
        &body.note,
    )?))
}
