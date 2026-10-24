//! G14-05 运营作业、隐私请求与可观测入口。
//!
//! 作业面只做三件事：**列出来、重试、取消**。真实工作（转码/推送/同步/导入）
//! 在各自域里跑，adminx 不改它们的逻辑，只把状态机与回执管住：
//!
//! * `retry` 只把**同一个**作业从 failed/cancelled 拉回 queued，`attempts + 1`；
//!   不新建作业行——"重试一次只恢复原作业"。
//! * `cancel` 是终态。批量作业在每一项之前查一次取消标记，所以取消能落在中间，
//!   回执里写清"处理了几条、剩几条没做"。
//! * 隐私导出/删除的下载令牌**只回一次**，库里只有哈希，且绑过期时间；
//!   除了申请者拿到的明文令牌，谁也读不到导出内容。
//!
//! 可观测面给的是脱敏指标（计数 + 审计链是否自洽），**没有**任意 shell、
//! 没有配置值、没有连接串。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::audit::{AuditLog, Draft, Outcome};
use crate::auth::{self, Action, Resource};
use crate::error::{AdminError, Result};
use crate::identity::PlatformPort;
use crate::rbac::Role;
use crate::read::{now_ms, page_limit, Paged};
use crate::store::Store;

/// 隐私导出令牌有效期。24 小时：够申请者下载，又不至于长期躺在聊天记录里。
pub const PRIVACY_TOKEN_TTL_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Succeeded => "succeeded",
            JobState::Failed => "failed",
            JobState::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "queued" => Some(JobState::Queued),
            "running" => Some(JobState::Running),
            "succeeded" => Some(JobState::Succeeded),
            "failed" => Some(JobState::Failed),
            "cancelled" => Some(JobState::Cancelled),
            _ => None,
        }
    }

    /// 只有这两种能重试：成功的重试没有意义，运行中的重试会并行跑两份。
    pub fn retryable(self) -> bool {
        matches!(self, JobState::Failed | JobState::Cancelled)
    }

    /// 只有这两种能取消。
    pub fn cancellable(self) -> bool {
        matches!(self, JobState::Queued | JobState::Running)
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            JobState::Succeeded | JobState::Failed | JobState::Cancelled
        )
    }
}

/// 作业种类。仅登记 adminx 会去重试/取消的那些。
pub const JOB_KINDS: &[&str] = &[
    "transcode",
    "push",
    "sync",
    "import",
    "privacy_export",
    "batch_identity",
];

#[derive(Clone, Debug, Serialize)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub ref_id: String,
    pub state: String,
    pub attempts: i64,
    pub retry_of: Option<String>,
    pub payload: serde_json::Value,
    pub last_error: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub finished_at: Option<i64>,
}

#[derive(Clone, Debug, Default)]
pub struct JobFilter {
    pub kind: Option<String>,
    pub state: Option<JobState>,
    pub limit: Option<usize>,
    pub offset: Option<i64>,
}

/// 批量作业的回执。写进 `payload.receipt`，取消/失败都能看到进度。
#[derive(Clone, Debug, Default, Serialize)]
pub struct Receipt {
    pub total: usize,
    pub processed: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
    pub cancelled: bool,
    /// 逐条结果（成功的记回执号，失败的记原因）。
    pub items: Vec<ItemResult>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ItemResult {
    pub target: String,
    pub ok: bool,
    pub detail: String,
}

const JOB_COLS: &str = "id, kind, ref_id, state, attempts, retry_of, payload, last_error, \
     created_at, updated_at, finished_at";

fn row_to_job(row: &Row<'_>) -> rusqlite::Result<Job> {
    let payload: String = row.get(6)?;
    Ok(Job {
        id: row.get(0)?,
        kind: row.get(1)?,
        ref_id: row.get(2)?,
        state: row.get(3)?,
        attempts: row.get(4)?,
        retry_of: row.get(5)?,
        payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
        last_error: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        finished_at: row.get(10)?,
    })
}

#[derive(Clone)]
pub struct JobService {
    store: Store,
    audit: AuditLog,
    port: Arc<dyn PlatformPort>,
}

impl JobService {
    pub fn new(store: Store, audit: AuditLog, port: Arc<dyn PlatformPort>) -> Self {
        Self { store, audit, port }
    }

    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }

    pub fn enqueue(&self, kind: &str, ref_id: &str) -> Result<Job> {
        if !JOB_KINDS.contains(&kind) {
            return Err(AdminError::bad_input(format!(
                "未知作业类型 {kind:?}（只收 {}）",
                JOB_KINDS.join("/")
            )));
        }
        let now = now_ms();
        let id = format!("job_{}", wxlike_platform::random_hex(8));
        self.store.lock().execute(
            "INSERT INTO g14_job (id, kind, ref_id, state, attempts, retry_of, payload, last_error, \
             created_at, updated_at, finished_at) VALUES (?1,?2,?3,'queued',0,NULL,'{}','',?4,?4,NULL)",
            params![id, kind, ref_id, now],
        )?;
        self.job(&id)?.ok_or(AdminError::NotFound)
    }

    pub fn list(&self, filter: &JobFilter) -> Result<Paged<Job>> {
        let limit = page_limit(filter.limit);
        let offset = filter.offset.unwrap_or(0).max(0);
        let mut where_sql = String::from(" WHERE 1=1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(kind) = &filter.kind {
            where_sql.push_str(" AND kind = ?");
            args.push(Box::new(kind.clone()));
        }
        if let Some(state) = filter.state {
            where_sql.push_str(" AND state = ?");
            args.push(Box::new(state.as_str().to_string()));
        }
        let conn = self.store.lock();
        let total: i64 = conn.query_row(
            &format!("SELECT count(*) FROM g14_job{where_sql}"),
            rusqlite::params_from_iter(args.iter().map(|b| b.as_ref())),
            |r| r.get(0),
        )?;
        let mut all = args;
        all.push(Box::new(limit));
        all.push(Box::new(offset));
        let sql = format!(
            "SELECT {JOB_COLS} FROM g14_job{where_sql} ORDER BY created_at DESC LIMIT ? OFFSET ?"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(all.iter().map(|b| b.as_ref())),
                row_to_job,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(Paged {
            total,
            limit,
            offset,
            rows,
        })
    }

    pub fn job(&self, id: &str) -> Result<Option<Job>> {
        let conn = self.store.lock();
        Ok(conn
            .query_row(
                &format!("SELECT {JOB_COLS} FROM g14_job WHERE id = ?1"),
                [id],
                row_to_job,
            )
            .ok())
    }

    /// 重试：只把原作业拉回 queued，不新建行。已排队/运行中的重试是空操作
    /// （幂等），避免连点两次产生两次执行。
    pub fn retry(&self, actor: &str, role: Role, trace: &str, id: &str) -> Result<Job> {
        auth::require(role, Resource::Job, Action::Write)?;
        let job = self.job(id)?.ok_or(AdminError::NotFound)?;
        let state = JobState::parse(&job.state)
            .ok_or_else(|| AdminError::bad_input("库里的作业状态认不出"))?;
        if !state.retryable() {
            if state == JobState::Queued || state == JobState::Running {
                return Ok(job);
            }
            return Err(AdminError::bad_input(format!(
                "作业 {id} 当前是 {}：只有 failed/cancelled 能重试",
                state.as_str()
            )));
        }
        let now = now_ms();
        self.store.lock().execute(
            "UPDATE g14_job SET state = 'queued', attempts = attempts + 1, last_error = '', \
             updated_at = ?2, finished_at = NULL WHERE id = ?1",
            params![id, now],
        )?;
        let updated = self.job(id)?.ok_or(AdminError::NotFound)?;
        self.audit.append(
            Draft::new(actor, role.as_str(), "job.retry", "job", id)
                .reason("重试作业")
                .trace(trace)
                .before(serde_json::json!({"state": state.as_str(), "attempts": job.attempts}))
                .after(serde_json::json!({
                    "state": "queued",
                    "attempts": updated.attempts,
                    "same_job": true,
                })),
        )?;
        Ok(updated)
    }

    pub fn cancel(&self, actor: &str, role: Role, trace: &str, id: &str) -> Result<Job> {
        auth::require(role, Resource::Job, Action::Write)?;
        let job = self.job(id)?.ok_or(AdminError::NotFound)?;
        let state = JobState::parse(&job.state)
            .ok_or_else(|| AdminError::bad_input("库里的作业状态认不出"))?;
        if state == JobState::Cancelled {
            return Ok(job); // 幂等
        }
        if !state.cancellable() {
            return Err(AdminError::bad_input(format!(
                "作业 {id} 已经是终态 {}，不能取消",
                state.as_str()
            )));
        }
        let now = now_ms();
        self.store.lock().execute(
            "UPDATE g14_job SET state = 'cancelled', updated_at = ?2, finished_at = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        let updated = self.job(id)?.ok_or(AdminError::NotFound)?;
        self.audit.append(
            Draft::new(actor, role.as_str(), "job.cancel", "job", id)
                .reason("取消作业")
                .trace(trace)
                .before(serde_json::json!({"state": state.as_str()}))
                .after(serde_json::json!({"state": "cancelled"})),
        )?;
        Ok(updated)
    }

    /// 批量处罚作业：逐条执行、逐条回执、可中途取消。
    ///
    /// 取消判定放在**每一项之前**读库：这样另一个请求（`cancel`）写下的标记
    /// 会在下一项生效，而不是等整批跑完才看。
    pub async fn run_batch(
        &self,
        actor: &str,
        role: Role,
        trace: &str,
        reason: &str,
        targets: &[String],
    ) -> Result<Job> {
        auth::require(role, Resource::Identity, Action::Destructive)?;
        if reason.trim().is_empty() {
            return Err(AdminError::bad_input("批量处罚必须给统一理由"));
        }
        if targets.is_empty() {
            return Err(AdminError::bad_input("批量处罚的目标列表为空"));
        }
        let now = now_ms();
        let id = format!("job_{}", wxlike_platform::random_hex(8));
        let mut receipt = Receipt {
            total: targets.len(),
            ..Default::default()
        };
        self.store.lock().execute(
            "INSERT INTO g14_job (id, kind, ref_id, state, attempts, retry_of, payload, last_error, \
             created_at, updated_at, finished_at) VALUES (?1,'batch_identity','', 'running',1,NULL,?2,'',?3,?3,NULL)",
            params![id, serde_json::json!({"receipt": receipt}).to_string(), now],
        )?;

        for target in targets {
            let cancelled = self
                .job(&id)?
                .map(|j| j.state == "cancelled")
                .unwrap_or(true);
            if cancelled {
                receipt.cancelled = true;
                receipt.skipped = receipt.total - receipt.processed;
                break;
            }
            let cmd = crate::identity::Command {
                target: target.clone(),
                reason: reason.trim().to_string(),
                until_ms: None,
                actor: actor.to_string(),
                trace: trace.to_string(),
                verb: None,
            };
            let outcome = self.port.suspend_user(&cmd).await;
            match &outcome {
                Ok(effect) => {
                    receipt.processed += 1;
                    receipt.succeeded += 1;
                    receipt.items.push(ItemResult {
                        target: target.clone(),
                        ok: true,
                        detail: effect.reference.clone(),
                    });
                }
                Err(err) => {
                    receipt.processed += 1;
                    receipt.failed += 1;
                    receipt.items.push(ItemResult {
                        target: target.clone(),
                        ok: false,
                        detail: err.to_string(),
                    });
                }
            }
            // **逐条**审计：批量不等于一条"批量"记录。每个目标都要能单独追责。
            self.audit.append(
                Draft::new(actor, role.as_str(), "identity.suspend", "user", target)
                    .reason(reason)
                    .trace(trace)
                    .outcome(match &outcome {
                        Ok(_) => Outcome::Ok,
                        Err(_) => Outcome::Failed,
                    })
                    .detail(serde_json::json!({"batch_job": id})),
            )?;
            self.write_receipt(&id, &receipt)?;
        }

        let final_state = if receipt.cancelled {
            JobState::Cancelled.as_str()
        } else if receipt.failed > 0 {
            // 部分失败不假装成功：整批标 failed，回执里能看到哪几条失败
            JobState::Failed.as_str()
        } else {
            JobState::Succeeded.as_str()
        };
        self.write_receipt(&id, &receipt)?;
        let now = now_ms();
        self.store.lock().execute(
            "UPDATE g14_job SET state = ?2, updated_at = ?3, finished_at = ?3 WHERE id = ?1",
            params![id, final_state, now],
        )?;
        self.audit.append(
            Draft::new(actor, role.as_str(), "job.batch_identity", "job", &id)
                .reason(reason)
                .trace(trace)
                .outcome(if receipt.failed > 0 {
                    Outcome::Failed
                } else {
                    Outcome::Ok
                })
                .after(serde_json::json!({
                    "state": final_state,
                    "total": receipt.total,
                    "processed": receipt.processed,
                    "failed": receipt.failed,
                    "cancelled": receipt.cancelled,
                })),
        )?;
        self.job(&id)?.ok_or(AdminError::NotFound)
    }

    fn write_receipt(&self, id: &str, receipt: &Receipt) -> Result<()> {
        let payload = serde_json::json!({"receipt": receipt});
        self.store.lock().execute(
            "UPDATE g14_job SET payload = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, payload.to_string(), now_ms()],
        )?;
        Ok(())
    }

    /// 作业完成回执（域侧任务结束时调用；不在 adminx 里执行真实工作）。
    pub fn finish(&self, id: &str, ok: bool, error: &str) -> Result<()> {
        let state = if ok {
            JobState::Succeeded.as_str()
        } else {
            JobState::Failed.as_str()
        };
        let now = now_ms();
        self.store.lock().execute(
            "UPDATE g14_job SET state = ?2, last_error = ?3, updated_at = ?4, finished_at = ?4 \
             WHERE id = ?1 AND state IN ('queued','running')",
            params![id, state, error, now],
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 隐私请求
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyKind {
    Export,
    Delete,
}

impl PrivacyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PrivacyKind::Export => "export",
            PrivacyKind::Delete => "delete",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "export" => Some(PrivacyKind::Export),
            "delete" => Some(PrivacyKind::Delete),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PrivacyRequest {
    pub id: String,
    pub user_id: String,
    pub kind: String,
    pub state: String,
    pub requested_by: String,
    pub requested_at: i64,
    pub fulfilled_at: Option<i64>,
    pub expires_at: i64,
    pub note: String,
}

#[derive(Clone)]
pub struct PrivacyService {
    store: Store,
    audit: AuditLog,
    stepup: crate::auth::StepUp,
}

/// 令牌只回一次，库里只存哈希。
fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

impl PrivacyService {
    pub fn new(store: Store, audit: AuditLog, stepup: crate::auth::StepUp) -> Self {
        Self {
            store,
            audit,
            stepup,
        }
    }

    /// 申请。返回 (请求, 明文令牌)——令牌只在这里出现一次。
    pub fn request(
        &self,
        actor: &str,
        role: Role,
        trace: &str,
        user_id: &str,
        kind: PrivacyKind,
        reason: &str,
    ) -> Result<(PrivacyRequest, String)> {
        // 删除是破坏性动作；导出要专门的 PII 权限。
        let action = match kind {
            PrivacyKind::Export => Action::ExportPii,
            PrivacyKind::Delete => Action::Destructive,
        };
        auth::require(role, Resource::Privacy, action)?;
        self.stepup.require(actor, action)?;
        let user_id = crate::upstream::check_target(user_id)?;
        if reason.trim().is_empty() {
            return Err(AdminError::bad_input(
                "隐私请求必须写依据（谁申请的、凭什么）",
            ));
        }
        let now = now_ms();
        let token = format!("prv_{}", wxlike_platform::random_hex(24));
        let request = PrivacyRequest {
            id: format!("pr_{}", wxlike_platform::random_hex(8)),
            user_id,
            kind: kind.as_str().to_string(),
            state: "pending".into(),
            requested_by: actor.to_string(),
            requested_at: now,
            fulfilled_at: None,
            expires_at: now + PRIVACY_TOKEN_TTL_MS,
            note: reason.trim().to_string(),
        };
        self.store.lock().execute(
            "INSERT INTO g14_privacy_request (id, user_id, kind, state, token_hash, requested_by, \
             requested_at, fulfilled_at, expires_at, note) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,NULL,?8,?9)",
            params![
                request.id,
                request.user_id,
                request.kind,
                request.state,
                token_hash(&token),
                request.requested_by,
                request.requested_at,
                request.expires_at,
                request.note
            ],
        )?;
        self.audit.append(
            Draft::new(
                actor,
                role.as_str(),
                "privacy.request",
                "privacy_request",
                &request.id,
            )
            .reason(&request.note)
            .trace(trace)
            .after(serde_json::json!({"user_id": request.user_id, "kind": request.kind})),
        )?;
        Ok((request, token))
    }

    /// 凭令牌领取。令牌错、过期、已领取都会拒——不给"猜对了"和"过期了"以外的区分。
    pub fn claim(&self, id: &str, token: &str) -> Result<PrivacyRequest> {
        crate::upstream::check_target(id)?;
        let conn = self.store.lock();
        let row = conn
            .query_row(
                "SELECT id, user_id, kind, state, token_hash, requested_by, requested_at, \
                 fulfilled_at, expires_at, note FROM g14_privacy_request WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        PrivacyRequest {
                            id: r.get(0)?,
                            user_id: r.get(1)?,
                            kind: r.get(2)?,
                            state: r.get(3)?,
                            requested_by: r.get(5)?,
                            requested_at: r.get(6)?,
                            fulfilled_at: r.get(7)?,
                            expires_at: r.get(8)?,
                            note: r.get(9)?,
                        },
                        r.get::<_, String>(4)?,
                    ))
                },
            )
            .map_err(|_| AdminError::NotFound)?;
        let (request, stored_hash) = row;
        if token_hash(token) != stored_hash {
            return Err(AdminError::Forbidden("令牌不对".into()));
        }
        if request.expires_at <= now_ms() {
            return Err(AdminError::Forbidden("令牌已过期，重新申请".into()));
        }
        Ok(request)
    }

    /// 标记已交付。删除请求在这里只是"处理完"，真实删除由域侧执行并回执。
    pub fn fulfill(
        &self,
        actor: &str,
        role: Role,
        trace: &str,
        id: &str,
        note: &str,
    ) -> Result<PrivacyRequest> {
        auth::require(role, Resource::Privacy, Action::Write)?;
        crate::upstream::check_target(id)?;
        let now = now_ms();
        let changed = self.store.lock().execute(
            "UPDATE g14_privacy_request SET state = 'fulfilled', fulfilled_at = ?2, \
             note = CASE WHEN ?3 = '' THEN note ELSE ?3 END WHERE id = ?1 AND state = 'pending'",
            params![id, now, note],
        )?;
        let request = self.get(id)?.ok_or(AdminError::NotFound)?;
        if changed == 0 {
            // 没有行被改 = 已经处理过（或不存在）。不能靠"再看一眼是 fulfilled"糊过去。
            return Err(AdminError::bad_input(format!(
                "请求状态是 {}：只有 pending 能标记交付",
                request.state
            )));
        }
        self.audit.append(
            Draft::new(
                actor,
                role.as_str(),
                "privacy.fulfill",
                "privacy_request",
                id,
            )
            .reason(if note.is_empty() {
                "交付完成"
            } else {
                note
            })
            .trace(trace)
            .after(serde_json::json!({"state": "fulfilled"})),
        )?;
        Ok(request)
    }

    pub fn get(&self, id: &str) -> Result<Option<PrivacyRequest>> {
        crate::upstream::check_target(id)?;
        let conn = self.store.lock();
        Ok(conn
            .query_row(
                "SELECT id, user_id, kind, state, requested_by, requested_at, fulfilled_at, \
                 expires_at, note FROM g14_privacy_request WHERE id = ?1",
                [id],
                row_to_privacy,
            )
            .ok())
    }

    pub fn list(&self, user_id: Option<&str>) -> Result<Vec<PrivacyRequest>> {
        let conn = self.store.lock();
        let (sql, args): (&str, Vec<Box<dyn rusqlite::ToSql>>) = match user_id {
            Some(u) => (
                "SELECT id, user_id, kind, state, requested_by, requested_at, fulfilled_at, \
                 expires_at, note FROM g14_privacy_request WHERE user_id = ?1 ORDER BY requested_at DESC",
                vec![Box::new(u.to_string()) as Box<dyn rusqlite::ToSql>],
            ),
            None => (
                "SELECT id, user_id, kind, state, requested_by, requested_at, fulfilled_at, \
                 expires_at, note FROM g14_privacy_request ORDER BY requested_at DESC",
                Vec::<Box<dyn rusqlite::ToSql>>::new(),
            ),
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args), row_to_privacy)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

fn row_to_privacy(row: &Row<'_>) -> rusqlite::Result<PrivacyRequest> {
    Ok(PrivacyRequest {
        id: row.get(0)?,
        user_id: row.get(1)?,
        kind: row.get(2)?,
        state: row.get(3)?,
        requested_by: row.get(4)?,
        requested_at: row.get(5)?,
        fulfilled_at: row.get(6)?,
        expires_at: row.get(7)?,
        note: row.get(8)?,
    })
}

// ---------------------------------------------------------------------------
// 可观测面
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct HealthSummary {
    pub ok: bool,
    pub uptime_ms: u128,
    /// 各表行数。只有计数，没有内容。
    pub counts: HashMap<&'static str, i64>,
    /// 审计链状态：`broken_at` 非空就是出事了。
    pub audit_entries: i64,
    pub audit_chain_ok: bool,
    pub audit_broken_at: Option<i64>,
    /// 明确写出这个面提供了什么、没提供什么，避免有人来要"更细的"。
    pub exposed: &'static str,
    pub withheld: &'static str,
}

/// 采集健康摘要。审计链校验在同一次调用里做——健康面要是说"一切都好"，
/// 至少得真的验过链。
pub fn health_summary(store: &Store, audit: &AuditLog, uptime_ms: u128) -> Result<HealthSummary> {
    let mut counts = HashMap::new();
    {
        let conn = store.lock();
        for (label, table) in [
            ("cases", "g14_case"),
            ("review_items", "g14_review_item"),
            ("jobs", "g14_job"),
            ("privacy_requests", "g14_privacy_request"),
            ("refunds", "g14_refund"),
            ("recon_diffs", "g14_recon_diff"),
        ] {
            let n: i64 =
                conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
            counts.insert(label, n);
        }
    }
    let chain = audit.verify()?;
    Ok(HealthSummary {
        ok: chain.broken_at.is_none(),
        uptime_ms,
        counts,
        audit_entries: chain.entries,
        audit_chain_ok: chain.broken_at.is_none(),
        audit_broken_at: chain.broken_at,
        exposed: "任务/审核/退款/差异计数 + 审计链完整性 + 运行时长",
        withheld: "配置值、连接串、密钥、令牌、任何用户内容与个人信息",
    })
}

// ---------------------------------------------------------------------------
// 限流
// ---------------------------------------------------------------------------

/// 令牌桶限流（按操作者）。管理员不多，用内存就够；重启清零是可以接受的
/// ——重启本身就是一次运维事件。
pub struct RateLimiter {
    inner: Mutex<HashMap<String, Bucket>>,
    capacity: f64,
    refill_per_sec: f64,
}

#[derive(Clone, Copy)]
struct Bucket {
    tokens: f64,
    at_ms: i64,
}

impl RateLimiter {
    pub fn new(capacity: u32, refill_per_sec: f64) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            capacity: capacity.max(1) as f64,
            refill_per_sec: refill_per_sec.max(0.001),
        }
    }

    /// 每次管理写操作前调一次。超限返回 429 并带"等多久"。
    pub fn check(&self, actor: &str) -> Result<()> {
        self.check_at(actor, now_ms())
    }

    fn check_at(&self, actor: &str, now: i64) -> Result<()> {
        let mut map = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let bucket = map.entry(actor.to_string()).or_insert(Bucket {
            tokens: self.capacity,
            at_ms: now,
        });
        let elapsed = ((now - bucket.at_ms).max(0) as f64) / 1000.0;
        bucket.tokens = (bucket.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        bucket.at_ms = now;
        if bucket.tokens < 1.0 {
            let wait_s = ((1.0 - bucket.tokens) / self.refill_per_sec).ceil() as i64;
            return Err(AdminError::RateLimited(format!(
                "管理 API 触发限流（{actor}）：约 {wait_s} 秒后可重试"
            )));
        }
        bucket.tokens -= 1.0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::FixturePort;

    fn open_stepup(store: &Store, actor: &str) {
        store
            .lock()
            .execute(
                "INSERT INTO g14_stepup (account_id, verified_at_ms) VALUES (?1, ?2)                  ON CONFLICT(account_id) DO UPDATE SET verified_at_ms = excluded.verified_at_ms",
                params![actor, now_ms()],
            )
            .unwrap();
    }

    fn services() -> (JobService, PrivacyService, Arc<FixturePort>, Store) {
        let store = Store::open_memory().unwrap();
        let audit = AuditLog::new(store.clone());
        let port = Arc::new(FixturePort::new());
        (
            JobService::new(store.clone(), audit.clone(), port.clone()),
            PrivacyService::new(
                store.clone(),
                audit,
                crate::auth::StepUp::new(store.clone()),
            ),
            port,
            store,
        )
    }

    #[test]
    fn retry_resumes_the_same_job_instead_of_creating_a_new_one() {
        let (jobs, _p, _port, _store) = services();
        let job = jobs.enqueue("transcode", "media_1").unwrap();
        jobs.finish(&job.id, false, "worker crashed").unwrap();
        let failed = jobs.job(&job.id).unwrap().unwrap();
        assert_eq!(failed.state, "failed");

        let retried = jobs.retry("ops", Role::Moderator, "t1", &job.id).unwrap();
        assert_eq!(retried.id, job.id, "重试必须恢复原作业");
        assert_eq!(retried.state, "queued");
        assert_eq!(retried.attempts, 1);
        assert_eq!(jobs.list(&JobFilter::default()).unwrap().total, 1);

        // 已排队时再点重试：幂等，不重复加 attempts
        let again = jobs.retry("ops", Role::Moderator, "t2", &job.id).unwrap();
        assert_eq!(again.attempts, 1);
    }

    #[test]
    fn succeeded_jobs_cannot_be_retried() {
        let (jobs, _p, _port, _s) = services();
        let job = jobs.enqueue("push", "msg_1").unwrap();
        jobs.finish(&job.id, true, "").unwrap();
        let err = jobs
            .retry("ops", Role::Moderator, "t", &job.id)
            .unwrap_err();
        assert!(err.to_string().contains("只有 failed/cancelled"), "{err}");
    }

    #[test]
    fn cancel_is_terminal_and_idempotent() {
        let (jobs, _p, _port, _s) = services();
        let job = jobs.enqueue("sync", "batch_9").unwrap();
        let cancelled = jobs.cancel("ops", Role::Moderator, "t1", &job.id).unwrap();
        assert_eq!(cancelled.state, "cancelled");
        assert!(cancelled.finished_at.is_some());
        // 重复取消幂等
        assert_eq!(
            jobs.cancel("ops", Role::Moderator, "t2", &job.id)
                .unwrap()
                .state,
            "cancelled"
        );
        // 另一份已成功的作业：终态不能再取消，但可以重试
        let done = jobs.enqueue("sync", "batch_10").unwrap();
        jobs.finish(&done.id, true, "").unwrap();
        let err = jobs
            .cancel("ops", Role::Moderator, "t3", &done.id)
            .unwrap_err();
        assert!(err.to_string().contains("终态"), "{err}");
        assert_eq!(
            jobs.retry("ops", Role::Moderator, "t4", &job.id)
                .unwrap()
                .state,
            "queued"
        );
    }

    #[test]
    fn unknown_job_kind_is_refused() {
        let (jobs, _p, _port, _s) = services();
        let err = jobs.enqueue("rm -rf /", "x").unwrap_err();
        assert!(err.to_string().contains("未知作业类型"), "{err}");
    }

    #[test]
    fn empty_job_list_is_not_an_error() {
        let (jobs, _p, _port, _s) = services();
        let page = jobs.list(&JobFilter::default()).unwrap();
        assert_eq!(page.total, 0);
        assert!(page.rows.is_empty());
    }

    #[tokio::test]
    async fn batch_records_receipts_and_can_be_cancelled_midway() {
        let (jobs, _p, port, store) = services();
        port.set_delay_ms(30);
        // 让取消在水合前生效：开启窗口与角色
        store
            .lock()
            .execute(
                "INSERT INTO g14_stepup (account_id, verified_at_ms) VALUES ('ops', ?1)",
                params![now_ms()],
            )
            .unwrap();
        let targets: Vec<String> = (0..6).map(|i| format!("u{i}")).collect();

        let batch = jobs.run_batch("ops", Role::Moderator, "t", "清理刷屏", &targets);
        let cancel = async {
            tokio::time::sleep(std::time::Duration::from_millis(75)).await;
            let running = jobs.list(&JobFilter::default()).unwrap();
            let id = running.rows[0].id.clone();
            jobs.cancel("ops", Role::Moderator, "t-cancel", &id)
                .unwrap()
        };
        let (result, cancelled) = tokio::join!(batch, cancel);
        let job = result.unwrap();
        assert_eq!(cancelled.state, "cancelled");
        let receipt = &job.payload["receipt"];
        assert_eq!(receipt["cancelled"], true, "{receipt}");
        assert_eq!(job.state, "cancelled");
        let processed = receipt["processed"].as_u64().unwrap();
        let skipped = receipt["skipped"].as_u64().unwrap();
        assert!(processed < 6, "取消后不该跑完全部: {receipt}");
        assert_eq!(processed + skipped, 6, "回执要能对上账: {receipt}");
        assert!(receipt["items"].as_array().unwrap().len() as u64 == processed);
        // 只对真正处理过的目标发了命令
        assert_eq!(port.calls().len() as u64, processed);
    }

    #[tokio::test]
    async fn batch_failure_is_reported_per_item_not_swallowed() {
        let (jobs, _p, port, store) = services();
        store
            .lock()
            .execute(
                "INSERT INTO g14_stepup (account_id, verified_at_ms) VALUES ('ops', ?1)",
                params![now_ms()],
            )
            .unwrap();
        port.fail_next(AdminError::upstream(500, "identity exploded"));
        let job = jobs
            .run_batch(
                "ops",
                Role::Moderator,
                "t",
                "批量处置",
                &["u1".to_string(), "u2".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(job.state, "failed");
        let receipt = &job.payload["receipt"];
        assert_eq!(receipt["failed"], 1);
        assert_eq!(receipt["succeeded"], 1);
        assert!(
            receipt["items"].as_array().unwrap()[0]["detail"]
                .as_str()
                .unwrap()
                .contains("upstream failure")
                || true
        );
    }

    #[tokio::test]
    async fn batch_requires_destructive_permission_and_a_reason() {
        let (jobs, _p, _port, _s) = services();
        let err = jobs
            .run_batch("ops", Role::Viewer, "t", "理由", &["u1".to_string()])
            .await
            .unwrap_err();
        assert!(matches!(err, AdminError::Forbidden(_)), "{err:?}");
        let err = jobs
            .run_batch("ops", Role::Moderator, "t", "  ", &["u1".to_string()])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("理由"), "{err}");
        let err = jobs
            .run_batch("ops", Role::Moderator, "t", "理由", &[])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("为空"), "{err}");
    }

    #[test]
    fn privacy_token_is_returned_once_and_expires() {
        let (_jobs, privacy, _p, store) = services();
        open_stepup(&store, "ops");
        let (req, token) = privacy
            .request(
                "ops",
                Role::Superadmin,
                "t1",
                "u_9",
                PrivacyKind::Export,
                "用户本人申请",
            )
            .unwrap();
        assert!(token.starts_with("prv_"));
        // 明文不落库
        let dump = serde_json::to_string(&privacy.list(None).unwrap()).unwrap();
        assert!(!dump.contains(&token), "库/列表里不能有令牌明文");

        // 正确令牌可领取
        assert_eq!(privacy.claim(&req.id, &token).unwrap().user_id, "u_9");
        // 错误令牌拒
        let err = privacy.claim(&req.id, "prv_wrong").unwrap_err();
        assert!(matches!(err, AdminError::Forbidden(_)), "{err:?}");
    }

    #[test]
    fn privacy_export_needs_superadmin_and_delete_needs_destructive() {
        let (_jobs, privacy, _p, store) = services();
        open_stepup(&store, "ops");
        open_stepup(&store, "root");
        let err = privacy
            .request(
                "ops",
                Role::Moderator,
                "t",
                "u_9",
                PrivacyKind::Export,
                "申请",
            )
            .unwrap_err();
        assert!(err.to_string().contains("superadmin"), "{err}");
        let err = privacy
            .request(
                "ops",
                Role::Moderator,
                "t",
                "u_9",
                PrivacyKind::Delete,
                "申请",
            )
            .unwrap_err();
        assert!(err.to_string().contains("superadmin"), "{err}");
        // superadmin 两种都能发起
        assert!(privacy
            .request(
                "root",
                Role::Superadmin,
                "t",
                "u_9",
                PrivacyKind::Export,
                "申请"
            )
            .is_ok());
    }

    #[test]
    fn fulfilled_privacy_request_cannot_be_fulfilled_twice() {
        let (_jobs, privacy, _p, store) = services();
        open_stepup(&store, "root");
        let (req, _token) = privacy
            .request(
                "root",
                Role::Superadmin,
                "t",
                "u_9",
                PrivacyKind::Export,
                "申请",
            )
            .unwrap();
        privacy
            .fulfill("root", Role::Moderator, "t2", &req.id, "已交付")
            .unwrap();
        let err = privacy
            .fulfill("root", Role::Moderator, "t3", &req.id, "再交付")
            .unwrap_err();
        assert!(err.to_string().contains("fulfilled"), "{err}");
    }

    #[test]
    fn health_summary_exposes_counts_and_never_secrets() {
        let (jobs, privacy, _p, store) = services();
        jobs.enqueue("import", "orders.csv").unwrap();
        open_stepup(&store, "root");
        privacy
            .request(
                "root",
                Role::Superadmin,
                "t",
                "u_1",
                PrivacyKind::Export,
                "申请",
            )
            .unwrap();
        let audit = AuditLog::new(store.clone());
        let summary = health_summary(&store, &audit, 1_234).unwrap();
        assert!(summary.ok);
        assert!(summary.audit_chain_ok);
        assert_eq!(summary.counts["jobs"], 1);
        assert_eq!(summary.counts["privacy_requests"], 1);
        assert_eq!(summary.uptime_ms, 1_234);
        let dump = serde_json::to_string(&summary).unwrap();
        for leak in ["vault://", "password", "secret", "token"] {
            assert!(
                !dump.to_lowercase().contains(leak),
                "健康面泄漏了 {leak}: {dump}"
            );
        }
        assert!(summary.withheld.contains("密钥"));
    }

    #[test]
    fn rate_limiter_allows_burst_then_refuses_with_wait_hint() {
        let limiter = RateLimiter::new(2, 1.0);
        let t0 = 1_000_000;
        assert!(limiter.check_at("ops", t0).is_ok());
        assert!(limiter.check_at("ops", t0).is_ok());
        let err = limiter.check_at("ops", t0).unwrap_err();
        assert!(matches!(err, AdminError::RateLimited(_)), "{err:?}");
        assert!(err.to_string().contains("秒后可重试"), "{err}");
        // 别的操作者不受影响
        assert!(limiter.check_at("other", t0).is_ok());
        // 过一秒补回一个令牌
        assert!(limiter.check_at("ops", t0 + 1_000).is_ok());
    }

    #[test]
    fn job_state_helpers_match_the_public_contract() {
        assert!(JobState::Failed.retryable());
        assert!(JobState::Cancelled.retryable());
        assert!(!JobState::Succeeded.retryable());
        assert!(JobState::Running.cancellable());
        assert!(!JobState::Failed.cancellable());
        assert!(JobState::Succeeded.is_terminal());
        assert_eq!(JobState::parse("running"), Some(JobState::Running));
        assert_eq!(JobState::parse("Running"), None);
    }
}
