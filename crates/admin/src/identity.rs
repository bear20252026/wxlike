//! G14-02 用户、设备、群与申诉管理。
//!
//! 这个模块只做**运营编排**，三条边界必须说清：
//!
//! 1. **效果由域侧执行**。封号、撤设备、群治理的真实动作在 G02 的域里（会话失效、
//!    设备密钥作废、群成员缓存刷新都发生在平台进程内）。adminx 通过
//!    [`PlatformPort`] 发一条命令过去，**不**直接改平台库——绕过平台进程改库会留下
//!    "后台显示封了、服务其实还通"的静默不一致。
//! 2. **不可绕过 E2EE**。后台能看的只有 [`METADATA_FIELDS`] 白名单里的字段。模块里
//!    根本没有"取消息正文"的入口；[`project`] 反而会把任何非白名单字段丢掉。
//! 3. **危险动作两阶段**。先 `prepare` 拿确认令牌（带指纹与 TTL），再带上令牌执行。
//!    令牌一次性、绑指纹：换个对象/换个理由复用不了，重放也不行。
//!
//! 申诉工单的状态机在 [`CaseState`]，库里只落结果。误封必须能恢复、且恢复本身
//! 也要留痕——所以每个动作（含被拒的）都走 [`AuditLog`]。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};

use crate::audit::{AuditLog, Draft, Outcome};
use crate::auth::{self, Action, Resource, StepUp};
use crate::error::{AdminError, Result};
use crate::rbac::Role;
use crate::read::{now_ms, page_limit, Paged};
use crate::store::Store;
use crate::upstream::{self, Upstream};

// ---------------------------------------------------------------------------
// 域侧端口
// ---------------------------------------------------------------------------

/// 一次域命令。`reason` 会随命令一起送到域侧，域侧也要留痕。
#[derive(Clone, Debug, Serialize)]
pub struct Command {
    pub target: String,
    pub reason: String,
    /// 封禁到期时间（毫秒）。`None` = 永久。
    pub until_ms: Option<i64>,
    pub actor: String,
    pub trace: String,
    /// 群治理的动词（`freeze` / `unfreeze` / `transfer` / `dissolve`）。
    pub verb: Option<String>,
}

/// 域命令的回执。`before`/`after` 是**状态摘要**（不是正文），进审计时还会再脱敏。
#[derive(Clone, Debug, Default, Serialize)]
pub struct Effect {
    pub reference: String,
    pub before: serde_json::Value,
    pub after: serde_json::Value,
    /// 这条命令连带踢掉了多少个会话。封禁的验收面就是它非零。
    pub sessions_revoked: u64,
    /// 域侧说"已经是这个状态了"（幂等命中）。
    pub already: bool,
}

/// 域侧端口。生产实现是 [`HttpPort`]（转发到平台 `/api/admin/identity/*`）。
///
/// 上游卡（G02-05）未合入时联调可用 [`FixturePort`]；**最终验收必须换成真实域**，
/// 这是任务单明写的约束，代码上靠装配点区分，不靠注释。
#[async_trait]
pub trait PlatformPort: Send + Sync {
    async fn suspend_user(&self, cmd: &Command) -> Result<Effect>;
    async fn unsuspend_user(&self, cmd: &Command) -> Result<Effect>;
    async fn revoke_device(&self, cmd: &Command) -> Result<Effect>;
    async fn govern_group(&self, cmd: &Command) -> Result<Effect>;
}

/// 转发到平台管理面。路径与平台端点一一对应，id 先过 [`upstream::check_target`]。
pub struct HttpPort {
    upstream: Upstream,
}

impl HttpPort {
    pub fn new(upstream: Upstream) -> Self {
        Self { upstream }
    }

    async fn post(
        &self,
        path_template: &str,
        cmd: &Command,
        extra: serde_json::Value,
    ) -> Result<Effect> {
        let path = upstream::render_path(path_template, Some(&cmd.target))?;
        let mut body = serde_json::json!({
            "reason": cmd.reason,
            "actor": cmd.actor,
            "trace": cmd.trace,
        });
        if let (Some(map), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
            for (k, v) in extra {
                map.insert(k.clone(), v.clone());
            }
        }
        if let Some(verb) = &cmd.verb {
            body["action"] = serde_json::json!(verb);
        }
        let reply = self
            .upstream
            .call("POST", &path, &cmd.actor, Some(body))
            .await?;
        Ok(effect_from_reply(reply.body))
    }
}

/// 上游回执 → [`Effect`]。字段缺省都不算错：老版本平台只回 `{"ok":true}`。
fn effect_from_reply(body: serde_json::Value) -> Effect {
    Effect {
        reference: body
            .get("reference")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        before: body
            .get("before")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        after: body
            .get("after")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        sessions_revoked: body
            .get("sessions_revoked")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        already: body
            .get("already")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    }
}

#[async_trait]
impl PlatformPort for HttpPort {
    async fn suspend_user(&self, cmd: &Command) -> Result<Effect> {
        let extra = serde_json::json!({ "until_ms": cmd.until_ms });
        self.post("/api/admin/identity/users/{id}/suspend", cmd, extra)
            .await
    }

    async fn unsuspend_user(&self, cmd: &Command) -> Result<Effect> {
        self.post(
            "/api/admin/identity/users/{id}/unsuspend",
            cmd,
            serde_json::json!({}),
        )
        .await
    }

    async fn revoke_device(&self, cmd: &Command) -> Result<Effect> {
        self.post(
            "/api/admin/identity/devices/{id}/revoke",
            cmd,
            serde_json::json!({}),
        )
        .await
    }

    async fn govern_group(&self, cmd: &Command) -> Result<Effect> {
        self.post(
            "/api/admin/identity/groups/{id}/govern",
            cmd,
            serde_json::json!({}),
        )
        .await
    }
}

/// **只给上游未合入时的联调/测试用**。生产装配（`App::open`）只装 [`HttpPort`]。
///
/// 它做两件真实的事：记录调用序列（断言"到底发了几条命令"）与可编程的故障
/// （供应商故障注入、慢返回）。
#[derive(Default)]
pub struct FixturePort {
    calls: Mutex<Vec<(String, Command)>>,
    fail_next: Mutex<Option<AdminError>>,
    delay_ms: Mutex<u64>,
    sessions_revoked: Mutex<u64>,
}

impl FixturePort {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fail_next(&self, err: AdminError) {
        *self
            .fail_next
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(err);
    }

    pub fn set_delay_ms(&self, ms: u64) {
        *self.delay_ms.lock().unwrap_or_else(PoisonError::into_inner) = ms;
    }

    pub fn set_sessions_revoked(&self, n: u64) {
        *self
            .sessions_revoked
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = n;
    }

    pub fn calls(&self) -> Vec<(String, Command)> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn record(&self, op: &str, cmd: &Command) -> Result<Effect> {
        let err = self
            .fail_next
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(err) = err {
            return Err(err);
        }
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((op.to_string(), cmd.clone()));
        let n = *self
            .sessions_revoked
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Ok(Effect {
            reference: format!("fixture:{op}:{}", cmd.target),
            before: serde_json::json!({"state": "active"}),
            after: serde_json::json!({
                "state": match op {
                    "suspend" => "suspended",
                    "unsuspend" => "active",
                    "revoke_device" => "revoked",
                    _ => "governed",
                }
            }),
            sessions_revoked: n,
            already: false,
        })
    }
}

#[async_trait]
impl PlatformPort for FixturePort {
    async fn suspend_user(&self, cmd: &Command) -> Result<Effect> {
        let ms = *self.delay_ms.lock().unwrap_or_else(PoisonError::into_inner);
        if ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        }
        self.record("suspend", cmd)
    }

    async fn unsuspend_user(&self, cmd: &Command) -> Result<Effect> {
        self.record("unsuspend", cmd)
    }

    async fn revoke_device(&self, cmd: &Command) -> Result<Effect> {
        self.record("revoke_device", cmd)
    }

    async fn govern_group(&self, cmd: &Command) -> Result<Effect> {
        self.record("govern_group", cmd)
    }
}

// ---------------------------------------------------------------------------
// 两阶段确认
// ---------------------------------------------------------------------------

/// 确认令牌有效期。5 分钟：够看完预览再点确认，又不至于让人把令牌贴进工单复用。
pub const CONFIRM_TTL_MS: i64 = 5 * 60 * 1000;

#[derive(Clone, Debug, Serialize)]
pub struct Confirmation {
    pub token: String,
    /// 令牌只能用于这一件事（动作 + 对象 + 理由 + 时长 的摘要）。
    pub fingerprint: String,
    pub expires_at_ms: i64,
}

/// 一次危险动作的结局：要么"先给你看预览"，要么"已经执行"。
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum Dangerous {
    /// 只预览，等确认令牌。
    Preview(Preview),
    /// 已执行，带回执。
    Done(Effect),
}

#[derive(Clone, Debug, Serialize)]
pub struct Preview {
    pub action: String,
    pub target: String,
    pub reason: String,
    pub until_ms: Option<i64>,
    /// 域侧当前状态的**元数据投影**，让人确认前能看清对象。
    pub current: serde_json::Value,
    pub confirmation: Confirmation,
    pub next: String,
}

/// 确认令牌池。放内存里而不是库里：令牌活 5 分钟，进程重启后本来就该重来，
/// 落盘只会多一份"忘了清理"的垃圾。
#[derive(Clone)]
pub struct Confirmer {
    inner: Arc<Mutex<HashMap<String, Confirmation>>>,
    ttl_ms: i64,
}

impl Confirmer {
    pub fn new() -> Self {
        Self::with_ttl(CONFIRM_TTL_MS)
    }

    pub fn with_ttl(ttl_ms: i64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            ttl_ms: ttl_ms.max(1),
        }
    }

    /// 发一张新令牌。顺手清掉过期项：池子很小，但别让它无界增长。
    pub fn prepare(
        &self,
        action: &str,
        target: &str,
        reason: &str,
        until_ms: Option<i64>,
    ) -> Confirmation {
        let now = now_ms();
        let confirmation = Confirmation {
            token: format!("cfm_{}", wxlike_platform::random_hex(16)),
            fingerprint: fingerprint(action, target, reason, until_ms),
            expires_at_ms: now + self.ttl_ms,
        };
        let mut pool = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        pool.retain(|_, c| c.expires_at_ms > now);
        pool.insert(confirmation.token.clone(), confirmation.clone());
        confirmation
    }

    /// 消费令牌。一次性 + 指纹绑定：重放、换对象、过期都会被拒，
    /// 且拒绝消息说清是哪一种。
    pub fn consume(&self, token: &str, fingerprint_want: &str) -> Result<()> {
        let mut pool = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(c) = pool.remove(token) else {
            return Err(AdminError::bad_input(
                "确认令牌无效或已使用：危险操作要重新预览（POST 同一路径带 dry_run=1）",
            ));
        };
        if c.expires_at_ms <= now_ms() {
            return Err(AdminError::bad_input(
                "确认令牌已过期：重新预览（POST 同一路径带 dry_run=1）",
            ));
        }
        if c.fingerprint != fingerprint_want {
            return Err(AdminError::bad_input(
                "确认令牌与本次操作不匹配（对象/理由/时长变了）：重新预览后再确认",
            ));
        }
        Ok(())
    }
}

impl Default for Confirmer {
    fn default() -> Self {
        Self::new()
    }
}

/// 指纹 = 这次操作的可判定要素。理由也进来：改理由就是改处置依据，必须重新确认。
pub fn fingerprint(action: &str, target: &str, reason: &str, until_ms: Option<i64>) -> String {
    crate::audit::digest(&serde_json::json!({
        "action": action,
        "target": target,
        "reason": reason,
        "until_ms": until_ms,
    }))
}

// ---------------------------------------------------------------------------
// 只读投影：E2EE 边界
// ---------------------------------------------------------------------------

/// 后台**允许**看到的用户元数据字段。白名单，不是黑名单：
/// 黑名单会在域侧新增字段时默认放开，那正好是最危险的方向。
pub const METADATA_FIELDS: &[&str] = &[
    "user_id",
    "display_name",
    "state",
    "created_at",
    "last_seen_ms",
    "device_count",
    "group_count",
    "report_count",
    "suspended_until_ms",
    "labels",
];

/// 把域侧返回的对象投影成白名单元数据。非白名单字段一律丢弃，
/// 并在结果里标出丢了几个字段——运维要知道自己没看全。
pub fn project(raw: &serde_json::Value) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    let mut dropped = 0usize;
    if let Some(map) = raw.as_object() {
        for (k, v) in map {
            if METADATA_FIELDS.contains(&k.as_str()) {
                out.insert(k.clone(), v.clone());
            } else {
                dropped += 1;
            }
        }
    }
    out.insert("dropped_fields".into(), serde_json::json!(dropped));
    out.insert(
        "content_visible".into(),
        serde_json::json!("none: 后台不持有消息明文，也不请求它"),
    );
    serde_json::Value::Object(out)
}

// ---------------------------------------------------------------------------
// 工单状态机
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseState {
    Open,
    Triaged,
    Actioned,
    Rejected,
    Closed,
}

impl CaseState {
    pub fn as_str(self) -> &'static str {
        match self {
            CaseState::Open => "open",
            CaseState::Triaged => "triaged",
            CaseState::Actioned => "actioned",
            CaseState::Rejected => "rejected",
            CaseState::Closed => "closed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(CaseState::Open),
            "triaged" => Some(CaseState::Triaged),
            "actioned" => Some(CaseState::Actioned),
            "rejected" => Some(CaseState::Rejected),
            "closed" => Some(CaseState::Closed),
            _ => None,
        }
    }

    /// 允许的流转。刻意不允许 `Open → Actioned`：处置前必须有人认领并写理由，
    /// 否则"谁判的、凭什么"就追不到。
    pub fn can_transition_to(self, next: CaseState) -> bool {
        matches!(
            (self, next),
            (CaseState::Open, CaseState::Triaged)
                | (CaseState::Open, CaseState::Rejected)
                | (CaseState::Triaged, CaseState::Actioned)
                | (CaseState::Triaged, CaseState::Rejected)
                | (CaseState::Actioned, CaseState::Closed)
                | (CaseState::Rejected, CaseState::Closed)
        )
    }

    pub fn is_terminal(self) -> bool {
        self == CaseState::Closed
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Case {
    pub id: String,
    pub kind: String,
    pub subject_kind: String,
    pub subject_id: String,
    pub state: String,
    pub reporter: String,
    pub assignee: String,
    pub reason: String,
    pub decision_reason: String,
    pub appeal_of: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

const CASE_COLS: &str = "id, kind, subject_kind, subject_id, state, reporter, assignee, reason, \
     decision_reason, appeal_of, created_at, updated_at";

fn row_to_case(row: &Row<'_>) -> rusqlite::Result<Case> {
    Ok(Case {
        id: row.get(0)?,
        kind: row.get(1)?,
        subject_kind: row.get(2)?,
        subject_id: row.get(3)?,
        state: row.get(4)?,
        reporter: row.get(5)?,
        assignee: row.get(6)?,
        reason: row.get(7)?,
        decision_reason: row.get(8)?,
        appeal_of: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

#[derive(Clone, Debug, Deserialize)]
pub struct NewCase {
    pub kind: String,
    pub subject_kind: String,
    pub subject_id: String,
    #[serde(default)]
    pub reporter: String,
    pub reason: String,
    #[serde(default)]
    pub appeal_of: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct CaseFilter {
    pub state: Option<CaseState>,
    pub kind: Option<String>,
    pub subject_id: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<i64>,
}

// ---------------------------------------------------------------------------
// 服务
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct IdentityService {
    store: Store,
    audit: AuditLog,
    port: Arc<dyn PlatformPort>,
    stepup: StepUp,
    confirmer: Confirmer,
}

/// 一次危险动作的请求。
#[derive(Clone, Debug)]
pub struct DangerRequest {
    pub actor: String,
    pub role: Role,
    pub trace: String,
    pub target: String,
    pub reason: String,
    pub until_ms: Option<i64>,
    /// `None` = 只预览（dry run），`Some` = 真的执行。
    pub confirm: Option<String>,
    /// 域侧当前状态摘要，用于预览与审计的 `before`。
    pub current: serde_json::Value,
}

impl IdentityService {
    pub fn new(store: Store, audit: AuditLog, port: Arc<dyn PlatformPort>, stepup: StepUp) -> Self {
        Self {
            store,
            audit,
            port,
            stepup,
            confirmer: Confirmer::new(),
        }
    }

    pub fn with_confirmer(mut self, confirmer: Confirmer) -> Self {
        self.confirmer = confirmer;
        self
    }

    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }

    // -- 工单 ---------------------------------------------------------------

    pub fn create_case(&self, actor: &str, role: Role, trace: &str, req: NewCase) -> Result<Case> {
        auth::require(role, Resource::Case, Action::Write)?;
        let reason = required_reason(&req.reason)?;
        if req.kind.trim().is_empty() || req.subject_id.trim().is_empty() {
            return Err(AdminError::bad_input("工单必须有 kind 与 subject_id"));
        }
        let subject_id = upstream::check_target(&req.subject_id)?;
        if let Some(appeal_of) = &req.appeal_of {
            upstream::check_target(appeal_of)?;
        }
        let now = now_ms();
        let case = Case {
            id: format!("case_{}", wxlike_platform::random_hex(8)),
            kind: req.kind.trim().to_string(),
            subject_kind: req.subject_kind.trim().to_string(),
            subject_id: subject_id.clone(),
            state: CaseState::Open.as_str().to_string(),
            reporter: req.reporter.trim().to_string(),
            assignee: String::new(),
            reason,
            decision_reason: String::new(),
            appeal_of: req.appeal_of.clone(),
            created_at: now,
            updated_at: now,
        };
        self.store.lock().execute(
            "INSERT INTO g14_case (id, kind, subject_kind, subject_id, state, reporter, assignee, \
             reason, decision_reason, appeal_of, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', ?7, '', ?8, ?9, ?9)",
            params![
                case.id,
                case.kind,
                case.subject_kind,
                case.subject_id,
                case.state,
                case.reporter,
                case.reason,
                case.appeal_of,
                now
            ],
        )?;
        self.audit.append(
            Draft::new(actor, role.as_str(), "case.create", "case", &case.id)
                .reason(&case.reason)
                .trace(trace)
                .after(serde_json::json!({"kind": case.kind, "state": case.state}))
                .detail(serde_json::json!({"subject_id": case.subject_id})),
        )?;
        Ok(case)
    }

    pub fn cases(&self, filter: &CaseFilter) -> Result<Paged<Case>> {
        let limit = page_limit(filter.limit);
        let offset = filter.offset.unwrap_or(0).max(0);
        let mut where_sql = String::from(" WHERE 1=1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(state) = filter.state {
            where_sql.push_str(" AND state = ?");
            args.push(Box::new(state.as_str().to_string()));
        }
        if let Some(kind) = &filter.kind {
            where_sql.push_str(" AND kind = ?");
            args.push(Box::new(kind.clone()));
        }
        if let Some(subject) = &filter.subject_id {
            where_sql.push_str(" AND subject_id = ?");
            args.push(Box::new(subject.clone()));
        }
        let conn = self.store.lock();
        let total: i64 = conn.query_row(
            &format!("SELECT count(*) FROM g14_case{where_sql}"),
            rusqlite::params_from_iter(args.iter().map(|b| b.as_ref())),
            |r| r.get(0),
        )?;
        let mut all = args;
        all.push(Box::new(limit));
        all.push(Box::new(offset));
        let sql = format!(
            "SELECT {CASE_COLS} FROM g14_case{where_sql} ORDER BY updated_at DESC LIMIT ? OFFSET ?"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(all.iter().map(|b| b.as_ref())),
                row_to_case,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(Paged {
            total,
            limit,
            offset,
            rows,
        })
    }

    pub fn case(&self, id: &str) -> Result<Option<Case>> {
        upstream::check_target(id)?;
        let conn = self.store.lock();
        let row = conn
            .query_row(
                &format!("SELECT {CASE_COLS} FROM g14_case WHERE id = ?1"),
                [id],
                row_to_case,
            )
            .ok();
        Ok(row)
    }

    /// 流转。`reason` 必填：处置与驳回都要能复核。
    pub fn transition(
        &self,
        actor: &str,
        role: Role,
        trace: &str,
        id: &str,
        next: CaseState,
        reason: &str,
        assignee: Option<&str>,
    ) -> Result<Case> {
        let action = match next {
            CaseState::Actioned | CaseState::Rejected => Action::Destructive,
            _ => Action::Write,
        };
        auth::require(role, Resource::Case, action)?;
        let reason = required_reason(reason)?;
        let current = self.case(id)?.ok_or(AdminError::NotFound)?;
        let from = CaseState::parse(&current.state).ok_or_else(|| {
            AdminError::bad_input(format!("库里的工单状态 {:?} 认不出", current.state))
        })?;
        if !from.can_transition_to(next) {
            return Err(AdminError::bad_input(format!(
                "工单 {} 不能从 {} 直接到 {}（合法路径见 identity::CaseState::can_transition_to）",
                id,
                from.as_str(),
                next.as_str()
            )));
        }
        // 状态机先判、步进验证后要：非法流转的结论比"再验一次码"更该先说。
        if action.needs_step_up() {
            self.stepup.require(actor, action)?;
        }
        let now = now_ms();
        let assignee = assignee.map(str::trim).filter(|s| !s.is_empty());
        self.store.lock().execute(
            "UPDATE g14_case SET state = ?2, decision_reason = ?3, updated_at = ?4, \
             assignee = COALESCE(?5, assignee) WHERE id = ?1",
            params![id, next.as_str(), reason, now, assignee],
        )?;
        let updated = self.case(id)?.ok_or(AdminError::NotFound)?;
        self.audit.append(
            Draft::new(actor, role.as_str(), "case.transition", "case", id)
                .reason(&reason)
                .trace(trace)
                .before(serde_json::json!({"state": from.as_str()}))
                .after(serde_json::json!({"state": next.as_str()})),
        )?;
        Ok(updated)
    }

    // -- 危险动作 -----------------------------------------------------------

    pub async fn suspend(&self, req: DangerRequest) -> Result<Dangerous> {
        let port = self.port.clone();
        self.danger(req, "suspend", "user", move |cmd| async move {
            port.suspend_user(&cmd).await
        })
        .await
    }

    /// 解封也走 Destructive：误封恢复要留痕，但"能解封"不该比"能封"更宽松。
    pub async fn unsuspend(&self, req: DangerRequest) -> Result<Dangerous> {
        let port = self.port.clone();
        self.danger(req, "unsuspend", "user", move |cmd| async move {
            port.unsuspend_user(&cmd).await
        })
        .await
    }

    pub async fn revoke_device(&self, req: DangerRequest) -> Result<Dangerous> {
        let port = self.port.clone();
        self.danger(req, "revoke_device", "device", move |cmd| async move {
            port.revoke_device(&cmd).await
        })
        .await
    }

    pub async fn govern_group(&self, req: DangerRequest, verb: &str) -> Result<Dangerous> {
        let reason = format!("{}（{}）", req.reason.trim(), verb);
        let port = self.port.clone();
        let verb_owned = verb.to_string();
        self.danger(
            DangerRequest { reason, ..req },
            "govern_group",
            "group",
            move |cmd| {
                let verb = verb_owned.clone();
                async move {
                    let cmd = Command {
                        verb: Some(verb),
                        ..cmd
                    };
                    port.govern_group(&cmd).await
                }
            },
        )
        .await
    }

    async fn danger<F, Fut>(
        &self,
        req: DangerRequest,
        op: &str,
        object_kind: &str,
        run: F,
    ) -> Result<Dangerous>
    where
        F: FnOnce(Command) -> Fut,
        Fut: std::future::Future<Output = Result<Effect>>,
    {
        let reason = required_reason(&req.reason)?;
        let target = upstream::check_target(&req.target)?;

        // 1) 准入：角色 + 步进验证。被拒也留痕——"谁试过封号"是运维要看的。
        let gate = auth::require(req.role, resource_for(object_kind), Action::Destructive)
            .and_then(|()| self.stepup.require(&req.actor, Action::Destructive));
        if let Err(err) = gate {
            self.audit.append(
                Draft::new(&req.actor, req.role.as_str(), op, object_kind, &target)
                    .reason(&reason)
                    .trace(&req.trace)
                    .outcome(Outcome::Denied)
                    .detail(serde_json::json!({"denied": err.to_string()})),
            )?;
            return Err(err);
        }

        // 2) 两阶段：没带确认令牌就只发预览。
        let want = fingerprint(op, &target, &reason, req.until_ms);
        let Some(token) = req.confirm.as_deref() else {
            let confirmation = self.confirmer.prepare(op, &target, &reason, req.until_ms);
            return Ok(Dangerous::Preview(Preview {
                action: op.to_string(),
                target,
                reason,
                until_ms: req.until_ms,
                current: project(&req.current),
                confirmation,
                next: "确认无误后带 {\"confirm\":\"<token>\"} 重发同一请求".into(),
            }));
        };
        if let Err(err) = self.confirmer.consume(token, &want) {
            self.audit.append(
                Draft::new(&req.actor, req.role.as_str(), op, object_kind, &target)
                    .reason(&reason)
                    .trace(&req.trace)
                    .outcome(Outcome::Denied)
                    .detail(serde_json::json!({"denied": err.to_string()})),
            )?;
            return Err(err);
        }

        // 3) 执行并留痕。失败也留痕（含上游 5xx / 传输失败）。
        let cmd = Command {
            target: target.clone(),
            reason: reason.clone(),
            until_ms: req.until_ms,
            actor: req.actor.clone(),
            trace: req.trace.clone(),
            verb: None,
        };
        match run(cmd).await {
            Ok(effect) => {
                self.audit.append(
                    Draft::new(&req.actor, req.role.as_str(), op, object_kind, &target)
                        .reason(&reason)
                        .trace(&req.trace)
                        .before(effect.before.clone())
                        .after(effect.after.clone())
                        .detail(serde_json::json!({
                            "reference": effect.reference,
                            "sessions_revoked": effect.sessions_revoked,
                            "already": effect.already,
                        })),
                )?;
                Ok(Dangerous::Done(effect))
            }
            Err(err) => {
                self.audit.append(
                    Draft::new(&req.actor, req.role.as_str(), op, object_kind, &target)
                        .reason(&reason)
                        .trace(&req.trace)
                        .outcome(Outcome::Failed)
                        .detail(serde_json::json!({"error": err.to_string()})),
                )?;
                Err(err)
            }
        }
    }
}

/// 对象种类 → 权限资源。写在一处，避免每个入口各判一遍走偏。
pub fn resource_for(object_kind: &str) -> Resource {
    match object_kind {
        "device" => Resource::Device,
        "group" => Resource::Group,
        "case" => Resource::Case,
        _ => Resource::Identity,
    }
}

fn required_reason(reason: &str) -> Result<String> {
    let trimmed = reason.trim();
    if trimmed.is_empty() {
        return Err(AdminError::bad_input(
            "处置理由不能为空：封禁/驳回/解封都要能复核依据",
        ));
    }
    if trimmed.chars().count() > crate::audit::REASON_MAX_CHARS {
        return Err(AdminError::bad_input("处置理由过长（上限 512 字符）"));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> (IdentityService, Arc<FixturePort>, Store) {
        let store = Store::open_memory().unwrap();
        let audit = AuditLog::new(store.clone());
        let port = Arc::new(FixturePort::new());
        let svc = IdentityService::new(
            store.clone(),
            audit,
            port.clone(),
            StepUp::new(store.clone()),
        );
        (svc, port, store)
    }

    /// 直接写库打开步进窗口：单元测试不该依赖真实 TOTP（那条路径由集成用例覆盖）。
    fn open_step_up(store: &Store, actor: &str) {
        store
            .lock()
            .execute(
                "INSERT INTO g14_stepup (account_id, verified_at_ms) VALUES (?1, ?2) \
                 ON CONFLICT(account_id) DO UPDATE SET verified_at_ms = excluded.verified_at_ms",
                params![actor, now_ms()],
            )
            .unwrap();
    }

    fn req(actor: &str, role: Role) -> DangerRequest {
        DangerRequest {
            actor: actor.into(),
            role,
            trace: "trace-x".into(),
            target: "u_1001".into(),
            reason: "举报成立：冒充客服".into(),
            until_ms: None,
            confirm: None,
            current: serde_json::json!({"user_id": "u_1001", "state": "active", "message": "正文"}),
        }
    }

    fn preview_of(d: Dangerous) -> Preview {
        match d {
            Dangerous::Preview(p) => p,
            Dangerous::Done(e) => panic!("期望预览，实际执行了: {e:?}"),
        }
    }

    fn effect_of(d: Dangerous) -> Effect {
        match d {
            Dangerous::Done(e) => e,
            Dangerous::Preview(p) => panic!("期望执行，实际只给了预览: {p:?}"),
        }
    }

    #[test]
    fn case_machine_forbids_skipping_triage() {
        assert!(CaseState::Open.can_transition_to(CaseState::Triaged));
        assert!(!CaseState::Open.can_transition_to(CaseState::Actioned));
        assert!(!CaseState::Open.can_transition_to(CaseState::Closed));
        assert!(CaseState::Triaged.can_transition_to(CaseState::Actioned));
        assert!(CaseState::Actioned.can_transition_to(CaseState::Closed));
        assert!(!CaseState::Closed.can_transition_to(CaseState::Triaged));
        assert!(CaseState::Closed.is_terminal());
        assert_eq!(CaseState::parse("actioned"), Some(CaseState::Actioned));
        assert_eq!(CaseState::parse("Actioned"), None);
    }

    #[test]
    fn projection_drops_every_content_field() {
        let raw = serde_json::json!({
            "user_id": "u1",
            "state": "active",
            "message": "私聊正文",
            "ciphertext": "AAAA",
            "session_key": "0011",
            "last_message": "你好",
        });
        let view = project(&raw);
        let text = serde_json::to_string(&view).unwrap();
        for leak in ["私聊正文", "AAAA", "0011", "你好"] {
            assert!(!text.contains(leak), "投影里出现了 {leak}: {text}");
        }
        assert_eq!(view["user_id"], "u1");
        assert_eq!(view["dropped_fields"], 4);
    }

    #[tokio::test]
    async fn danger_action_needs_role_then_step_up_then_confirmation() {
        let (svc, port, store) = service();

        // 1) 角色不够：直接拒，且不发命令
        let err = svc.suspend(req("ops", Role::Viewer)).await.unwrap_err();
        assert!(
            err.to_string().contains("superadmin") || err.to_string().contains("moderator"),
            "{err}"
        );
        assert!(port.calls().is_empty());

        // 2) 角色够但没步进验证：拒
        let err = svc.suspend(req("ops", Role::Moderator)).await.unwrap_err();
        assert!(matches!(err, AdminError::MfaRequired(_)), "{err:?}");
        assert!(port.calls().is_empty());

        // 3) 步进验证后：第一次只发预览
        open_step_up(&store, "ops");
        let preview = preview_of(svc.suspend(req("ops", Role::Moderator)).await.unwrap());
        assert_eq!(preview.action, "suspend");
        assert!(port.calls().is_empty(), "预览不能动域侧");
        assert!(
            !serde_json::to_string(&preview.current)
                .unwrap()
                .contains("正文"),
            "预览里的对象摘要也不能带正文"
        );

        // 4) 带令牌执行
        let mut confirmed = req("ops", Role::Moderator);
        confirmed.confirm = Some(preview.confirmation.token.clone());
        let effect = effect_of(svc.suspend(confirmed).await.unwrap());
        assert_eq!(effect.reference, "fixture:suspend:u_1001");
        assert_eq!(port.calls().len(), 1);

        // 5) 令牌重放：拒
        let mut replay = req("ops", Role::Moderator);
        replay.confirm = Some(preview.confirmation.token);
        let err = svc.suspend(replay).await.unwrap_err();
        assert!(err.to_string().contains("无效或已使用"), "{err}");
        assert_eq!(port.calls().len(), 1);
    }

    #[test]
    fn confirmation_is_bound_to_the_exact_request() {
        let confirmer = Confirmer::new();
        let c = confirmer.prepare("suspend", "u1", "理由A", None);
        let same = fingerprint("suspend", "u1", "理由A", None);
        assert!(confirmer.consume(&c.token, &same).is_ok());
        assert!(confirmer.consume(&c.token, &same).is_err(), "用过即废");

        let c = confirmer.prepare("suspend", "u1", "理由A", None);
        let other_target = fingerprint("suspend", "u2", "理由A", None);
        let err = confirmer.consume(&c.token, &other_target).unwrap_err();
        assert!(err.to_string().contains("不匹配"), "{err}");

        let c = confirmer.prepare("suspend", "u1", "理由A", None);
        let other_reason = fingerprint("suspend", "u1", "理由B", None);
        assert!(confirmer.consume(&c.token, &other_reason).is_err());
    }

    #[test]
    fn expired_confirmation_is_refused() {
        let confirmer = Confirmer::with_ttl(1);
        let c = confirmer.prepare("suspend", "u1", "理由", None);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let err = confirmer
            .consume(&c.token, &fingerprint("suspend", "u1", "理由", None))
            .unwrap_err();
        assert!(err.to_string().contains("过期"), "{err}");
    }

    #[tokio::test]
    async fn upstream_failure_is_audited_as_failed() {
        let (svc, port, store) = service();
        open_step_up(&store, "ops");
        port.fail_next(AdminError::upstream(503, "identity down"));
        let preview = preview_of(svc.suspend(req("ops", Role::Moderator)).await.unwrap());
        let mut confirmed = req("ops", Role::Moderator);
        confirmed.confirm = Some(preview.confirmation.token);
        let err = svc.suspend(confirmed).await.unwrap_err();
        assert!(
            matches!(err, AdminError::Upstream { status: 503, .. }),
            "{err:?}"
        );
        let log = svc.audit().query(&crate::audit::Filter::default()).unwrap();
        assert!(
            log.rows
                .iter()
                .any(|e| e.outcome == "failed" && e.action == "suspend"),
            "上游失败必须留痕: {log:?}"
        );
    }

    #[tokio::test]
    async fn ban_revokes_sessions_and_unban_is_reversible_with_trail() {
        let (svc, port, store) = service();
        open_step_up(&store, "ops");
        port.set_sessions_revoked(3);

        let preview = preview_of(svc.suspend(req("ops", Role::Moderator)).await.unwrap());
        let mut confirmed = req("ops", Role::Moderator);
        confirmed.confirm = Some(preview.confirmation.token);
        let effect = effect_of(svc.suspend(confirmed).await.unwrap());
        assert_eq!(effect.sessions_revoked, 3, "封禁必须连带踢会话");

        let mut undo = req("ops", Role::Moderator);
        undo.reason = "误封：证据不足".into();
        let preview = preview_of(svc.unsuspend(undo).await.unwrap());
        let mut confirmed = req("ops", Role::Moderator);
        confirmed.reason = "误封：证据不足".into();
        confirmed.confirm = Some(preview.confirmation.token);
        assert!(svc.unsuspend(confirmed).await.is_ok());

        let log = svc.audit().query(&crate::audit::Filter::default()).unwrap();
        let actions: Vec<_> = log.rows.iter().map(|e| e.action.as_str()).collect();
        assert!(
            actions.contains(&"suspend") && actions.contains(&"unsuspend"),
            "{actions:?}"
        );
        assert!(log.rows.iter().all(|e| !e.reason.is_empty()));
    }

    #[tokio::test]
    async fn empty_reason_and_bad_target_are_rejected_before_the_port() {
        let (svc, port, store) = service();
        open_step_up(&store, "ops");
        let mut blank = req("ops", Role::Moderator);
        blank.reason = "   ".into();
        assert!(svc
            .suspend(blank)
            .await
            .unwrap_err()
            .to_string()
            .contains("理由"));

        let mut traversal = req("ops", Role::Moderator);
        traversal.target = "../../etc/passwd".into();
        assert!(svc.suspend(traversal).await.is_err());
        assert!(port.calls().is_empty());
    }

    #[test]
    fn case_lifecycle_requires_reason_at_every_step() {
        let (svc, _port, _store) = service();
        let case = svc
            .create_case(
                "ops",
                Role::Moderator,
                "t1",
                NewCase {
                    kind: "report".into(),
                    subject_kind: "user".into(),
                    subject_id: "u_1".into(),
                    reporter: "u_2".into(),
                    reason: "骚扰".into(),
                    appeal_of: None,
                },
            )
            .unwrap();
        assert_eq!(case.state, "open");

        assert!(svc
            .transition(
                "ops",
                Role::Moderator,
                "t2",
                &case.id,
                CaseState::Rejected,
                "  ",
                None
            )
            .is_err());
        let err = svc
            .transition(
                "ops",
                Role::Moderator,
                "t3",
                &case.id,
                CaseState::Actioned,
                "成立",
                None,
            )
            .unwrap_err();
        assert!(err.to_string().contains("不能从 open"), "{err}");
        let triaged = svc
            .transition(
                "ops",
                Role::Moderator,
                "t4",
                &case.id,
                CaseState::Triaged,
                "已认领",
                Some("ops"),
            )
            .unwrap();
        assert_eq!(triaged.assignee, "ops");
        let err = svc
            .transition(
                "ops",
                Role::Moderator,
                "t5",
                &case.id,
                CaseState::Actioned,
                "成立",
                None,
            )
            .unwrap_err();
        assert!(matches!(err, AdminError::MfaRequired(_)), "{err:?}");
    }

    #[test]
    fn empty_case_list_is_not_an_error() {
        let (svc, _p, _s) = service();
        let page = svc.cases(&CaseFilter::default()).unwrap();
        assert_eq!(page.total, 0);
        assert!(page.rows.is_empty());
    }
}
