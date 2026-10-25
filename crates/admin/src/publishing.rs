//! G14-03 内容、小程序与服务发布审核。
//!
//! 三条与"审核台"直觉不同的规矩：
//!
//! 1. **审核不等于签包**。`publish.decide` 与 `publish.sign` 是两个动作：
//!    审核岗（moderator）能判内容能不能上，发布岗（publisher）才能给包签名。
//!    权限分离在 [`crate::auth`] 的矩阵里，不靠前端藏按钮。
//! 2. **配置只存引用**。供应商密钥永远是 `vault://...` 引用；把明文贴进后台
//!    会在入口被拒（[`check_key_ref`]）。回执里也永远不回密钥，只回"有没有配"。
//! 3. **失败可重试但不重复建版本**。`idem` 是唯一键：同一次提交/发布重放，
//!    拿回的是同一条记录；域侧失败时状态停在原处，重试用同一个 `idem`。
//!
//! 下架/恢复/发布这些**真的会改线上状态**的动作通过 [`ContentPort`] 发给域侧
//! （G05 内容、G07 小程序、G08 服务目录），adminx 只留队列与审计。

use std::sync::Arc;

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

/// 发布域命令。四个动作各对应一个上游端点。
#[async_trait]
pub trait ContentPort: Send + Sync {
    /// 下架 / 恢复 / 放行。`state` 取 `approved` / `taken_down`。
    async fn set_visibility(
        &self,
        subject_kind: &str,
        subject_id: &str,
        state: &str,
        note: &str,
    ) -> Result<String>;
    /// 给小程序包签名，返回签名串（真实实现由 G07 的签名服务产出）。
    async fn sign_package(&self, app_id: &str, version: &str) -> Result<String>;
    /// 发布一个已签名的版本。
    async fn publish_release(&self, app_id: &str, version: &str, signature: &str)
        -> Result<String>;
    /// 撤回已发布的版本。
    async fn revoke_release(&self, app_id: &str, version: &str, reason: &str) -> Result<String>;
}

pub struct HttpContentPort {
    upstream: Upstream,
}

impl HttpContentPort {
    pub fn new(upstream: Upstream) -> Self {
        Self { upstream }
    }

    async fn post(&self, path: &str, body: serde_json::Value) -> Result<String> {
        let reply = self
            .upstream
            .call("POST", path, "admin", Some(body))
            .await?;
        Ok(reply.reference())
    }
}

#[async_trait]
impl ContentPort for HttpContentPort {
    async fn set_visibility(
        &self,
        subject_kind: &str,
        subject_id: &str,
        state: &str,
        note: &str,
    ) -> Result<String> {
        upstream::check_target(subject_id)?;
        let path = match subject_kind {
            "mini" => format!("/api/admin/mini/{subject_id}/visibility"),
            "service" => format!("/api/admin/services/{subject_id}/visibility"),
            _ => format!("/api/admin/content/{subject_id}/visibility"),
        };
        self.post(&path, serde_json::json!({"state": state, "note": note}))
            .await
    }

    async fn sign_package(&self, app_id: &str, version: &str) -> Result<String> {
        upstream::check_target(app_id)?;
        upstream::check_target(version)?;
        self.post(
            &format!("/api/admin/mini/{app_id}/versions/{version}/sign"),
            serde_json::json!({}),
        )
        .await
    }

    async fn publish_release(
        &self,
        app_id: &str,
        version: &str,
        signature: &str,
    ) -> Result<String> {
        upstream::check_target(app_id)?;
        self.post(
            &format!("/api/admin/mini/{app_id}/versions/{version}/publish"),
            serde_json::json!({"signature": signature}),
        )
        .await
    }

    async fn revoke_release(&self, app_id: &str, version: &str, reason: &str) -> Result<String> {
        upstream::check_target(app_id)?;
        self.post(
            &format!("/api/admin/mini/{app_id}/versions/{version}/revoke"),
            serde_json::json!({"reason": reason}),
        )
        .await
    }
}

/// 联调用途的端口（同 [`crate::identity::FixturePort`] 的约束：最终验收必须换真实域）。
#[derive(Default)]
pub struct FixtureContentPort {
    pub calls: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
    pub fail_next: std::sync::Mutex<Option<AdminError>>,
}

impl FixtureContentPort {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fail_next(&self, err: AdminError) {
        *self.fail_next.lock().unwrap() = Some(err);
    }

    pub fn calls(&self) -> Vec<(String, serde_json::Value)> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, op: &str, detail: serde_json::Value) -> Result<String> {
        if let Some(err) = self.fail_next.lock().unwrap().take() {
            return Err(err);
        }
        self.calls.lock().unwrap().push((op.to_string(), detail));
        Ok(format!("fixture:{op}"))
    }
}

#[async_trait]
impl ContentPort for FixtureContentPort {
    async fn set_visibility(
        &self,
        subject_kind: &str,
        subject_id: &str,
        state: &str,
        note: &str,
    ) -> Result<String> {
        self.record(
            "set_visibility",
            serde_json::json!({"kind": subject_kind, "id": subject_id, "state": state, "note": note}),
        )
    }

    async fn sign_package(&self, app_id: &str, version: &str) -> Result<String> {
        self.record(
            "sign_package",
            serde_json::json!({"app": app_id, "version": version}),
        )?;
        Ok(format!("sig:fixture:{app_id}:{version}"))
    }

    async fn publish_release(
        &self,
        app_id: &str,
        version: &str,
        signature: &str,
    ) -> Result<String> {
        self.record(
            "publish_release",
            serde_json::json!({"app": app_id, "version": version, "signature": signature}),
        )
    }

    async fn revoke_release(&self, app_id: &str, version: &str, reason: &str) -> Result<String> {
        self.record(
            "revoke_release",
            serde_json::json!({"app": app_id, "version": version, "reason": reason}),
        )
    }
}

// ---------------------------------------------------------------------------
// 审核规则版本
// ---------------------------------------------------------------------------

/// 审核规则版本表。规则本身归 G05（内容策略），这里只登记"这一版是按哪份规则判的"，
/// 让事后复核能对上口径。
pub const RULES: &[(&str, &str)] = &[
    ("content", "content-policy-2026.10"),
    ("mini", "mini-review-2026.09"),
    ("service", "service-catalog-2026.10"),
];

pub fn rule_version_for(kind: &str) -> Option<&'static str> {
    RULES.iter().find(|(k, _)| *k == kind).map(|(_, v)| *v)
}

#[derive(Clone, Debug, Serialize)]
pub struct RuleInfo {
    pub kind: &'static str,
    pub version: &'static str,
}

pub fn rules() -> Vec<RuleInfo> {
    RULES
        .iter()
        .map(|(kind, version)| RuleInfo { kind, version })
        .collect()
}

// ---------------------------------------------------------------------------
// 审核队列
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemState {
    Pending,
    Approved,
    Rejected,
    TakenDown,
}

impl ItemState {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemState::Pending => "pending",
            ItemState::Approved => "approved",
            ItemState::Rejected => "rejected",
            ItemState::TakenDown => "taken_down",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(ItemState::Pending),
            "approved" => Some(ItemState::Approved),
            "rejected" => Some(ItemState::Rejected),
            "taken_down" => Some(ItemState::TakenDown),
            _ => None,
        }
    }

    /// 状态机：审批只能从 pending 出发；下架只能针对已通过的；恢复回 approved。
    pub fn allows(self, decision: Decision) -> bool {
        matches!(
            (self, decision),
            (ItemState::Pending, Decision::Approve) | (ItemState::Pending, Decision::Reject)
        ) || matches!(
            (self, decision),
            (ItemState::Approved, Decision::TakeDown) | (ItemState::TakenDown, Decision::Restore)
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approve,
    Reject,
    TakeDown,
    Restore,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Approve => "approve",
            Decision::Reject => "reject",
            Decision::TakeDown => "take_down",
            Decision::Restore => "restore",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "approve" => Some(Decision::Approve),
            "reject" => Some(Decision::Reject),
            "take_down" => Some(Decision::TakeDown),
            "restore" => Some(Decision::Restore),
            _ => None,
        }
    }

    /// 会改线上状态的两类动作要 step-up（`reject` 只是留在队列里，不动线上）。
    pub fn needs_step_up(self) -> bool {
        matches!(self, Decision::TakeDown | Decision::Restore)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Item {
    pub id: String,
    pub kind: String,
    pub subject_id: String,
    pub version: String,
    pub rule_version: String,
    pub state: String,
    pub submitted_by: String,
    pub decided_by: Option<String>,
    pub reason: String,
    pub created_at: i64,
    pub decided_at: Option<i64>,
}

const ITEM_COLS: &str = "id, kind, subject_id, version, rule_version, state, submitted_by, \
     decided_by, reason, created_at, decided_at";

fn row_to_item(row: &Row<'_>) -> rusqlite::Result<Item> {
    Ok(Item {
        id: row.get(0)?,
        kind: row.get(1)?,
        subject_id: row.get(2)?,
        version: row.get(3)?,
        rule_version: row.get(4)?,
        state: row.get(5)?,
        submitted_by: row.get(6)?,
        decided_by: row.get(7)?,
        reason: row.get(8)?,
        created_at: row.get(9)?,
        decided_at: row.get(10)?,
    })
}

#[derive(Clone, Debug, Deserialize)]
pub struct Submission {
    pub kind: String,
    pub subject_id: String,
    pub version: String,
    #[serde(default)]
    pub submitted_by: String,
    /// 幂等键。同一个 `idem` 重放不会产生第二条待审项。
    pub idem: String,
}

#[derive(Clone, Debug, Default)]
pub struct QueueFilter {
    pub kind: Option<String>,
    pub state: Option<ItemState>,
    pub limit: Option<usize>,
    pub offset: Option<i64>,
}

// ---------------------------------------------------------------------------
// 小程序发行
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseState {
    Draft,
    Signed,
    Published,
    Revoked,
}

impl ReleaseState {
    pub fn as_str(self) -> &'static str {
        match self {
            ReleaseState::Draft => "draft",
            ReleaseState::Signed => "signed",
            ReleaseState::Published => "published",
            ReleaseState::Revoked => "revoked",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "draft" => Some(ReleaseState::Draft),
            "signed" => Some(ReleaseState::Signed),
            "published" => Some(ReleaseState::Published),
            "revoked" => Some(ReleaseState::Revoked),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Release {
    pub id: String,
    pub app_id: String,
    pub version: String,
    pub state: String,
    pub signed_by: Option<String>,
    pub published_by: Option<String>,
    /// 只回签名摘要，不把整串签名放进列表。
    pub signature_digest: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ServiceConfig {
    pub service_id: String,
    pub title: String,
    pub category: String,
    pub regions: Vec<String>,
    pub enabled: bool,
    /// vault 引用。回执里永远只有它，没有密钥本体。
    pub key_ref: String,
    /// 有没有配密钥——只有这一个布尔，足够前台显示"已配置"。
    pub secret_configured: bool,
    pub version: i64,
    pub updated_at: i64,
    pub updated_by: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ServicePatch {
    pub title: String,
    pub category: String,
    #[serde(default)]
    pub regions: Vec<String>,
    #[serde(default)]
    pub enabled: bool,
    pub key_ref: String,
}

/// 密钥引用必须是 `vault://` 形式。**拒绝明文**是这一层唯一的防线：
/// 一旦允许贴 `sk_live_...`，它就会进库、进备份、进审计导出。
pub fn check_key_ref(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AdminError::bad_input(
            "key_ref 不能为空：没有密钥就填 vault://… 的占位引用",
        ));
    }
    if !trimmed.starts_with("vault://") {
        return Err(AdminError::bad_input(
            "key_ref 只能是 vault:// 引用：后台不接收密钥明文（明文会进库、进备份、进导出）",
        ));
    }
    if trimmed.chars().count() > 200 {
        return Err(AdminError::bad_input("key_ref 过长（上限 200 字符）"));
    }
    Ok(trimmed.to_string())
}

// ---------------------------------------------------------------------------
// 服务
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct PublishingService {
    store: Store,
    audit: AuditLog,
    port: Arc<dyn ContentPort>,
    stepup: StepUp,
}

impl PublishingService {
    pub fn new(store: Store, audit: AuditLog, port: Arc<dyn ContentPort>, stepup: StepUp) -> Self {
        Self {
            store,
            audit,
            port,
            stepup,
        }
    }

    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }

    // -- 队列 ---------------------------------------------------------------

    pub fn submit(&self, actor: &str, role: Role, trace: &str, sub: Submission) -> Result<Item> {
        auth::require(role, Resource::Content, Action::Write)?;
        let rule = rule_version_for(&sub.kind).ok_or_else(|| {
            AdminError::bad_input(format!(
                "未知审核类型 {:?}（只收 {}）",
                sub.kind,
                RULES.iter().map(|(k, _)| *k).collect::<Vec<_>>().join("/")
            ))
        })?;
        if sub.idem.trim().is_empty() {
            return Err(AdminError::bad_input("提交必须有幂等键 idem"));
        }
        let subject_id = upstream::check_target(&sub.subject_id)?;
        let now = now_ms();
        let conn = self.store.lock();
        // 幂等：同一个 idem 重放，返回已存在的那条，不新建版本
        if let Ok(existing) = conn.query_row(
            &format!("SELECT {ITEM_COLS} FROM g14_review_item WHERE idem = ?1"),
            [&sub.idem],
            row_to_item,
        ) {
            return Ok(existing);
        }
        let item = Item {
            id: format!("rv_{}", wxlike_platform::random_hex(8)),
            kind: sub.kind.clone(),
            subject_id,
            version: sub.version.trim().to_string(),
            rule_version: rule.to_string(),
            state: ItemState::Pending.as_str().to_string(),
            submitted_by: if sub.submitted_by.trim().is_empty() {
                actor.to_string()
            } else {
                sub.submitted_by.trim().to_string()
            },
            decided_by: None,
            reason: String::new(),
            created_at: now,
            decided_at: None,
        };
        conn.execute(
            "INSERT INTO g14_review_item (id, kind, subject_id, version, rule_version, state, \
             submitted_by, decided_by, reason, created_at, decided_at, idem) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,NULL,'',?8,NULL,?9)",
            params![
                item.id,
                item.kind,
                item.subject_id,
                item.version,
                item.rule_version,
                item.state,
                item.submitted_by,
                now,
                sub.idem
            ],
        )?;
        drop(conn);
        self.audit.append(
            Draft::new(
                actor,
                role.as_str(),
                "publish.submit",
                "review_item",
                &item.id,
            )
            .reason("提交审核")
            .trace(trace)
            .after(serde_json::json!({
                "kind": item.kind,
                "subject_id": item.subject_id,
                "version": item.version,
                "rule_version": item.rule_version,
            })),
        )?;
        Ok(item)
    }

    pub fn queue(&self, filter: &QueueFilter) -> Result<Paged<Item>> {
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
        Paged::query(&conn, &format!("SELECT count(*) FROM g14_review_item{where_sql}"),
            &format!("SELECT {ITEM_COLS} FROM g14_review_item{where_sql} ORDER BY created_at ASC LIMIT ? OFFSET ?"),
            args, limit, offset, row_to_item)
    }

    pub fn item(&self, id: &str) -> Result<Option<Item>> {
        upstream::check_target(id)?;
        let conn = self.store.lock();
        Ok(conn
            .query_row(
                &format!("SELECT {ITEM_COLS} FROM g14_review_item WHERE id = ?1"),
                [id],
                row_to_item,
            )
            .ok())
    }

    /// 判定。`reason` 必填（复核要看依据）；下架/恢复还要 step-up。
    pub async fn decide(
        &self,
        actor: &str,
        role: Role,
        trace: &str,
        id: &str,
        decision: Decision,
        reason: &str,
    ) -> Result<Item> {
        let action = if decision.needs_step_up() {
            Action::Destructive
        } else {
            Action::Write
        };
        auth::require(role, Resource::Content, action)?;
        let reason = required_reason(reason)?;
        let item = self.item(id)?.ok_or(AdminError::NotFound)?;
        let state = ItemState::parse(&item.state).ok_or_else(|| {
            AdminError::bad_input(format!("库里的审核状态 {:?} 认不出", item.state))
        })?;
        if !state.allows(decision) {
            return Err(AdminError::bad_input(format!(
                "审核项 {} 当前是 {}，不能执行 {}（合法组合见 ItemState::allows）",
                id,
                state.as_str(),
                decision.as_str()
            )));
        }
        // 状态机过了才要步进验证：先告诉用户"这一步本来就不允许"，
        // 比先要一次动态码再拒绝有用得多。
        if action.needs_step_up() {
            self.stepup.require(actor, action)?;
        }

        let next = match decision {
            Decision::Approve => ItemState::Approved,
            Decision::Reject => ItemState::Rejected,
            Decision::TakeDown => ItemState::TakenDown,
            Decision::Restore => ItemState::Approved,
        };
        self.apply_visibility(actor, role, trace, &item, decision, &reason)
            .await?;

        let now = now_ms();
        self.store.lock().execute(
            "UPDATE g14_review_item SET state = ?2, decided_by = ?3, reason = ?4, decided_at = ?5 \
             WHERE id = ?1",
            params![id, next.as_str(), actor, reason, now],
        )?;
        let updated = self.item(id)?.ok_or(AdminError::NotFound)?;
        self.audit.append(
            Draft::new(actor, role.as_str(), "publish.decide", "review_item", id)
                .reason(&reason)
                .trace(trace)
                .before(serde_json::json!({"state": state.as_str()}))
                .after(serde_json::json!({"state": next.as_str(), "decision": decision.as_str()})),
        )?;
        Ok(updated)
    }

    async fn apply_visibility(
        &self,
        actor: &str,
        role: Role,
        trace: &str,
        item: &Item,
        decision: Decision,
        reason: &str,
    ) -> Result<()> {
        // 线上效果先做：域侧失败就**不改库**，否则"库里下架了、线上还在"。
        if decision.needs_step_up() {
            let state_word = if decision == Decision::TakeDown {
                "taken_down"
            } else {
                "approved"
            };
            match self
                .port
                .set_visibility(&item.kind, &item.subject_id, state_word, reason)
                .await
            {
                Ok(_reference) => {}
                Err(err) => {
                    self.audit.append(
                        Draft::new(
                            actor,
                            role.as_str(),
                            "publish.decide",
                            "review_item",
                            &item.id,
                        )
                        .reason(reason)
                        .trace(trace)
                        .outcome(Outcome::Failed)
                        .detail(serde_json::json!({
                            "decision": decision.as_str(),
                            "error": err.to_string(),
                        })),
                    )?;
                    return Err(err);
                }
            }
        }

        Ok(())
    }

    // -- 小程序 -------------------------------------------------------------

    /// 建草稿（幂等）。
    pub fn create_release(
        &self,
        actor: &str,
        role: Role,
        trace: &str,
        app_id: &str,
        version: &str,
        idem: &str,
    ) -> Result<Release> {
        auth::require(role, Resource::MiniApp, Action::Write)?;
        let app_id = upstream::check_target(app_id)?;
        let version = upstream::check_target(version)?;
        if idem.trim().is_empty() {
            return Err(AdminError::bad_input("建版本必须有幂等键 idem"));
        }
        let conn = self.store.lock();
        if let Ok(existing) = self.release_by_idem(&conn, idem) {
            return Ok(existing);
        }
        if let Ok(existing) = self.release_by_version(&conn, &app_id, &version) {
            // 同版本已存在：返回它，让调用方看到"已存在"而不是报一个 500
            return Ok(existing);
        }
        let now = now_ms();
        let release = Release {
            id: format!("rel_{}", wxlike_platform::random_hex(8)),
            app_id,
            version,
            state: ReleaseState::Draft.as_str().to_string(),
            signed_by: None,
            published_by: None,
            signature_digest: String::new(),
            created_at: now,
            updated_at: now,
        };
        conn.execute(
            "INSERT INTO g14_mini_release (id, app_id, version, state, signed_by, published_by, \
             signature, idem, created_at, updated_at) VALUES (?1,?2,?3,?4,NULL,NULL,NULL,?5,?6,?6)",
            params![
                release.id,
                release.app_id,
                release.version,
                release.state,
                idem,
                now
            ],
        )?;
        drop(conn);
        self.audit.append(
            Draft::new(
                actor,
                role.as_str(),
                "mini.create_release",
                "mini_release",
                &release.id,
            )
            .reason("建小程序版本")
            .trace(trace)
            .after(serde_json::json!({"app_id": release.app_id, "version": release.version})),
        )?;
        Ok(release)
    }

    /// 签名。审核岗没有这个权限——这是任务单点名的"审核角色不能签包"。
    pub async fn sign(
        &self,
        actor: &str,
        role: Role,
        trace: &str,
        release_id: &str,
    ) -> Result<Release> {
        auth::require(role, Resource::MiniApp, Action::SignPackage)?;
        self.stepup.require(actor, Action::SignPackage)?;
        let release = self.release(release_id)?.ok_or(AdminError::NotFound)?;
        let state = ReleaseState::parse(&release.state)
            .ok_or_else(|| AdminError::bad_input("库里的发行状态认不出"))?;
        if state != ReleaseState::Draft {
            return Err(AdminError::bad_input(format!(
                "只有 draft 能签名，当前是 {}",
                state.as_str()
            )));
        }
        let signature = self
            .port
            .sign_package(&release.app_id, &release.version)
            .await?;
        let now = now_ms();
        self.store.lock().execute(
            "UPDATE g14_mini_release SET state = ?2, signed_by = ?3, signature = ?4, updated_at = ?5 \
             WHERE id = ?1",
            params![
                release_id,
                ReleaseState::Signed.as_str(),
                actor,
                signature,
                now
            ],
        )?;
        let updated = self.release(release_id)?.ok_or(AdminError::NotFound)?;
        self.audit.append(
            Draft::new(
                actor,
                role.as_str(),
                "mini.sign",
                "mini_release",
                release_id,
            )
            .reason("签名小程序包")
            .trace(trace)
            .before(serde_json::json!({"state": state.as_str()}))
            .after(serde_json::json!({
                "state": ReleaseState::Signed.as_str(),
                "signature_digest": updated.signature_digest,
            })),
        )?;
        Ok(updated)
    }

    /// 发布。失败不改状态，重试用同一个 release 即可——不会多出一个版本。
    pub async fn publish(
        &self,
        actor: &str,
        role: Role,
        trace: &str,
        release_id: &str,
    ) -> Result<Release> {
        auth::require(role, Resource::MiniApp, Action::Write)?;
        let release = self.release(release_id)?.ok_or(AdminError::NotFound)?;
        let state = ReleaseState::parse(&release.state)
            .ok_or_else(|| AdminError::bad_input("库里的发行状态认不出"))?;
        if state == ReleaseState::Published {
            // 幂等：重复发布返回当前状态，不再打一次上游
            return Ok(release);
        }
        if state != ReleaseState::Signed {
            return Err(AdminError::bad_input(format!(
                "只有 signed 能发布（当前 {}）：先由 publisher 角色签名",
                state.as_str()
            )));
        }
        // 先把签名读出来再 await：锁不能跨 await 活着（MutexGuard 不是 Send）
        let signature = self
            .signature_of(release_id)?
            .ok_or_else(|| AdminError::bad_input("这条发行没有签名记录，先重新签名"))?;
        let reference = self
            .port
            .publish_release(&release.app_id, &release.version, &signature)
            .await?;
        let now = now_ms();
        self.store.lock().execute(
            "UPDATE g14_mini_release SET state = ?2, published_by = ?3, updated_at = ?4 WHERE id = ?1",
            params![release_id, ReleaseState::Published.as_str(), actor, now],
        )?;
        let updated = self.release(release_id)?.ok_or(AdminError::NotFound)?;
        self.audit.append(
            Draft::new(
                actor,
                role.as_str(),
                "mini.publish",
                "mini_release",
                release_id,
            )
            .reason("发布小程序版本")
            .trace(trace)
            .before(serde_json::json!({"state": state.as_str()}))
            .after(serde_json::json!({
                "state": ReleaseState::Published.as_str(),
                "upstream": reference,
            })),
        )?;
        Ok(updated)
    }

    pub async fn revoke(
        &self,
        actor: &str,
        role: Role,
        trace: &str,
        release_id: &str,
        reason: &str,
    ) -> Result<Release> {
        auth::require(role, Resource::MiniApp, Action::Destructive)?;
        self.stepup.require(actor, Action::Destructive)?;
        let reason = required_reason(reason)?;
        let release = self.release(release_id)?.ok_or(AdminError::NotFound)?;
        let state = ReleaseState::parse(&release.state)
            .ok_or_else(|| AdminError::bad_input("库里的发行状态认不出"))?;
        if state != ReleaseState::Published {
            return Err(AdminError::bad_input(format!(
                "只有已发布版本能撤回（当前 {}）",
                state.as_str()
            )));
        }
        let reference = self
            .port
            .revoke_release(&release.app_id, &release.version, &reason)
            .await?;
        let now = now_ms();
        self.store.lock().execute(
            "UPDATE g14_mini_release SET state = ?2, updated_at = ?3 WHERE id = ?1",
            params![release_id, ReleaseState::Revoked.as_str(), now],
        )?;
        let updated = self.release(release_id)?.ok_or(AdminError::NotFound)?;
        self.audit.append(
            Draft::new(
                actor,
                role.as_str(),
                "mini.revoke",
                "mini_release",
                release_id,
            )
            .reason(&reason)
            .trace(trace)
            .before(serde_json::json!({"state": state.as_str()}))
            .after(serde_json::json!({
                "state": ReleaseState::Revoked.as_str(),
                "upstream": reference,
            })),
        )?;
        Ok(updated)
    }

    /// 读签名串。单独一个方法而不是内联在发布流程里：锁不该跨 `await`。
    fn signature_of(&self, id: &str) -> Result<Option<String>> {
        let conn = self.store.lock();
        Ok(conn
            .query_row(
                "SELECT signature FROM g14_mini_release WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap_or(None))
    }

    pub fn release(&self, id: &str) -> Result<Option<Release>> {
        upstream::check_target(id)?;
        let conn = self.store.lock();
        let row = conn
            .query_row(
                "SELECT id, app_id, version, state, signed_by, published_by, signature, \
                 created_at, updated_at FROM g14_mini_release WHERE id = ?1",
                [id],
                row_to_release,
            )
            .ok();
        Ok(row)
    }

    pub fn releases(&self, app_id: Option<&str>) -> Result<Vec<Release>> {
        let conn = self.store.lock();
        let sql = "SELECT id, app_id, version, state, signed_by, published_by, signature, \
                   created_at, updated_at FROM g14_mini_release";
        let (sql, args): (&str, Vec<Box<dyn rusqlite::ToSql>>) = match app_id {
            Some(id) => (
                "SELECT id, app_id, version, state, signed_by, published_by, signature, \
                 created_at, updated_at FROM g14_mini_release WHERE app_id = ?1 ORDER BY created_at DESC",
                vec![Box::new(id.to_string()) as Box<dyn rusqlite::ToSql>],
            ),
            None => (sql, Vec::new()),
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args), row_to_release)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn release_by_idem(
        &self,
        conn: &rusqlite::Connection,
        idem: &str,
    ) -> std::result::Result<Release, rusqlite::Error> {
        conn.query_row(
            "SELECT id, app_id, version, state, signed_by, published_by, signature, created_at, \
             updated_at FROM g14_mini_release WHERE idem = ?1",
            [idem],
            row_to_release,
        )
    }

    fn release_by_version(
        &self,
        conn: &rusqlite::Connection,
        app_id: &str,
        version: &str,
    ) -> std::result::Result<Release, rusqlite::Error> {
        conn.query_row(
            "SELECT id, app_id, version, state, signed_by, published_by, signature, created_at, \
             updated_at FROM g14_mini_release WHERE app_id = ?1 AND version = ?2",
            params![app_id, version],
            row_to_release,
        )
    }

    // -- 服务卡片 -----------------------------------------------------------

    pub fn services(&self) -> Result<Vec<ServiceConfig>> {
        let conn = self.store.lock();
        let mut stmt = conn.prepare(
            "SELECT service_id, title, category, regions, enabled, key_ref, version, updated_at, \
             updated_by FROM g14_service_config ORDER BY category, title",
        )?;
        let rows = stmt
            .query_map([], row_to_service)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn service(&self, id: &str) -> Result<Option<ServiceConfig>> {
        upstream::check_target(id)?;
        let conn = self.store.lock();
        Ok(conn
            .query_row(
                "SELECT service_id, title, category, regions, enabled, key_ref, version, \
                 updated_at, updated_by FROM g14_service_config WHERE service_id = ?1",
                [id],
                row_to_service,
            )
            .ok())
    }

    /// 配置变更版本化：每次成功写入 `version + 1`，旧版本号进审计。
    pub fn upsert_service(
        &self,
        actor: &str,
        role: Role,
        trace: &str,
        id: &str,
        patch: ServicePatch,
    ) -> Result<ServiceConfig> {
        auth::require(role, Resource::Service, Action::Write)?;
        let id = upstream::check_target(id)?;
        if patch.title.trim().is_empty() || patch.category.trim().is_empty() {
            return Err(AdminError::bad_input("服务卡片必须有 title 与 category"));
        }
        let key_ref = check_key_ref(&patch.key_ref)?;
        let before = self.service(&id)?;
        let version = before.as_ref().map(|s| s.version + 1).unwrap_or(1);
        let now = now_ms();
        let regions = serde_json::to_string(&patch.regions)?;
        self.store.lock().execute(
            "INSERT INTO g14_service_config (service_id, title, category, regions, enabled, key_ref, \
             version, updated_at, updated_by) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9) \
             ON CONFLICT(service_id) DO UPDATE SET title = excluded.title, \
             category = excluded.category, regions = excluded.regions, enabled = excluded.enabled, \
             key_ref = excluded.key_ref, version = excluded.version, \
             updated_at = excluded.updated_at, updated_by = excluded.updated_by",
            params![
                id,
                patch.title.trim(),
                patch.category.trim(),
                regions,
                i64::from(patch.enabled),
                key_ref,
                version,
                now,
                actor
            ],
        )?;
        let after = self.service(&id)?.ok_or(AdminError::NotFound)?;
        self.audit.append(
            Draft::new(actor, role.as_str(), "service.upsert", "service", &id)
                .reason("更新服务卡片配置")
                .trace(trace)
                .before(serde_json::json!({
                    "version": before.as_ref().map(|s| s.version),
                    "enabled": before.as_ref().map(|s| s.enabled),
                    "key_ref": before.as_ref().map(|s| s.key_ref.clone()),
                }))
                .after(serde_json::json!({
                    "version": after.version,
                    "enabled": after.enabled,
                    "key_ref": after.key_ref,
                })),
        )?;
        Ok(after)
    }
}

fn row_to_release(row: &Row<'_>) -> rusqlite::Result<Release> {
    let signature: Option<String> = row.get(6)?;
    Ok(Release {
        id: row.get(0)?,
        app_id: row.get(1)?,
        version: row.get(2)?,
        state: row.get(3)?,
        signed_by: row.get(4)?,
        published_by: row.get(5)?,
        signature_digest: signature
            .map(|s| crate::audit::digest(&serde_json::json!(s)))
            .unwrap_or_default(),
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn row_to_service(row: &Row<'_>) -> rusqlite::Result<ServiceConfig> {
    let regions: String = row.get(3)?;
    let key_ref: String = row.get(5)?;
    Ok(ServiceConfig {
        service_id: row.get(0)?,
        title: row.get(1)?,
        category: row.get(2)?,
        regions: serde_json::from_str(&regions).unwrap_or_default(),
        enabled: row.get::<_, i64>(4)? != 0,
        secret_configured: !key_ref.is_empty(),
        key_ref,
        version: row.get(6)?,
        updated_at: row.get(7)?,
        updated_by: row.get(8)?,
    })
}

fn required_reason(reason: &str) -> Result<String> {
    let trimmed = reason.trim();
    if trimmed.is_empty() {
        return Err(AdminError::bad_input("审核结论必须写理由（复核要看依据）"));
    }
    if trimmed.chars().count() > crate::audit::REASON_MAX_CHARS {
        return Err(AdminError::bad_input("理由过长（上限 512 字符）"));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> (PublishingService, Arc<FixtureContentPort>, Store) {
        let store = Store::open_memory().unwrap();
        let audit = AuditLog::new(store.clone());
        let port = Arc::new(FixtureContentPort::new());
        let svc = PublishingService::new(
            store.clone(),
            audit,
            port.clone(),
            StepUp::new(store.clone()),
        );
        (svc, port, store)
    }

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

    fn sub(idem: &str) -> Submission {
        Submission {
            kind: "content".into(),
            subject_id: "post_1".into(),
            version: "v1".into(),
            submitted_by: "u_1".into(),
            idem: idem.into(),
        }
    }

    #[test]
    fn duplicate_submission_does_not_create_a_second_version() {
        let (svc, _p, _s) = service();
        let a = svc
            .submit("ops", Role::Moderator, "t1", sub("idem-1"))
            .unwrap();
        let b = svc
            .submit("ops", Role::Moderator, "t2", sub("idem-1"))
            .unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(svc.queue(&QueueFilter::default()).unwrap().total, 1);
        // 不同 idem 才是新条目
        let c = svc
            .submit("ops", Role::Moderator, "t3", sub("idem-2"))
            .unwrap();
        assert_ne!(a.id, c.id);
    }

    #[test]
    fn unknown_kind_is_rejected_with_the_allowed_list() {
        let (svc, _p, _s) = service();
        let mut bad = sub("x");
        bad.kind = "podcast".into();
        let err = svc.submit("ops", Role::Moderator, "t", bad).unwrap_err();
        assert!(err.to_string().contains("content/mini/service"), "{err}");
    }

    #[tokio::test]
    async fn reviewer_cannot_sign_and_signer_can() {
        let (svc, port, store) = service();
        open_step_up(&store, "ops");
        let rel = svc
            .create_release("ops", Role::Publisher, "t1", "app_1", "1.0.0", "idem-a")
            .unwrap();
        // 审核岗（moderator）签不了
        let err = svc
            .sign("ops", Role::Moderator, "t2", &rel.id)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("publisher"), "{err}");
        // 发布岗可以，但要 step-up
        store.lock().execute("DELETE FROM g14_stepup", []).unwrap();
        let err = svc
            .sign("ops", Role::Publisher, "t3", &rel.id)
            .await
            .unwrap_err();
        assert!(matches!(err, AdminError::MfaRequired(_)), "{err:?}");
        open_step_up(&store, "ops");
        let signed = svc
            .sign("ops", Role::Publisher, "t4", &rel.id)
            .await
            .unwrap();
        assert_eq!(signed.state, "signed");
        assert!(port.calls().iter().any(|(op, _)| op == "sign_package"));
    }

    #[tokio::test]
    async fn publish_failure_can_be_retried_without_creating_a_new_version() {
        let (svc, port, store) = service();
        open_step_up(&store, "ops");
        let rel = svc
            .create_release("ops", Role::Publisher, "t1", "app_1", "1.0.0", "idem-a")
            .unwrap();
        let rel = svc
            .sign("ops", Role::Publisher, "t2", &rel.id)
            .await
            .unwrap();

        port.fail_next(AdminError::upstream(503, "publish down"));
        let err = svc
            .publish("ops", Role::Publisher, "t3", &rel.id)
            .await
            .unwrap_err();
        assert!(
            matches!(err, AdminError::Upstream { status: 503, .. }),
            "{err:?}"
        );
        // 状态停在 signed，可重试
        assert_eq!(svc.release(&rel.id).unwrap().unwrap().state, "signed");

        let published = svc
            .publish("ops", Role::Publisher, "t4", &rel.id)
            .await
            .unwrap();
        assert_eq!(published.state, "published");
        // 重复发布幂等：不再打上游，也不会多一个版本
        let calls_before = port.calls().len();
        let again = svc
            .publish("ops", Role::Publisher, "t5", &rel.id)
            .await
            .unwrap();
        assert_eq!(again.state, "published");
        assert_eq!(port.calls().len(), calls_before);
        assert_eq!(svc.releases(Some("app_1")).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn takedown_changes_visibility_and_restore_reverts_it() {
        let (svc, port, store) = service();
        open_step_up(&store, "ops");
        let item = svc
            .submit("ops", Role::Moderator, "t1", sub("idem-t"))
            .unwrap();
        let approved = svc
            .decide(
                "ops",
                Role::Moderator,
                "t2",
                &item.id,
                Decision::Approve,
                "无问题",
            )
            .await
            .unwrap();
        assert_eq!(approved.state, "approved");

        // 下架要 step-up
        store.lock().execute("DELETE FROM g14_stepup", []).unwrap();
        let err = svc
            .decide(
                "ops",
                Role::Moderator,
                "t3",
                &item.id,
                Decision::TakeDown,
                "违规",
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AdminError::MfaRequired(_)), "{err:?}");
        open_step_up(&store, "ops");

        let down = svc
            .decide(
                "ops",
                Role::Moderator,
                "t4",
                &item.id,
                Decision::TakeDown,
                "违规",
            )
            .await
            .unwrap();
        assert_eq!(down.state, "taken_down");
        assert!(port
            .calls()
            .iter()
            .any(|(op, d)| op == "set_visibility" && d["state"] == "taken_down"));

        let up = svc
            .decide(
                "ops",
                Role::Moderator,
                "t5",
                &item.id,
                Decision::Restore,
                "申诉通过",
            )
            .await
            .unwrap();
        assert_eq!(up.state, "approved");
        assert!(port
            .calls()
            .iter()
            .any(|(op, d)| op == "set_visibility" && d["state"] == "approved"));
    }

    #[tokio::test]
    async fn illegal_transitions_are_refused() {
        let (svc, _p, _s) = service();
        let item = svc
            .submit("ops", Role::Moderator, "t", sub("idem-i"))
            .unwrap();
        // pending 不能下架
        let err = svc
            .decide(
                "ops",
                Role::Moderator,
                "t",
                &item.id,
                Decision::TakeDown,
                "违规",
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("pending"), "{err}");
        // 驳回后不能再批准
        svc.decide(
            "ops",
            Role::Moderator,
            "t",
            &item.id,
            Decision::Reject,
            "违规",
        )
        .await
        .unwrap();
        let err = svc
            .decide(
                "ops",
                Role::Moderator,
                "t",
                &item.id,
                Decision::Approve,
                "改判",
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("rejected"), "{err}");
    }

    #[test]
    fn service_config_never_accepts_or_returns_plaintext_secrets() {
        let (svc, _p, _s) = service();
        let mut patch = ServicePatch {
            title: "翻译".into(),
            category: "ai".into(),
            regions: vec!["CN".into()],
            enabled: true,
            key_ref: "vault://wxlike/translation#api_key".into(),
        };
        let cfg = svc
            .upsert_service("ops", Role::Publisher, "t1", "svc_translate", patch.clone())
            .unwrap();
        assert!(cfg.secret_configured);
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(!json.contains("sk_live"), "{json}");
        assert!(json.contains("vault://"), "回执里要有引用");

        // 明文密钥直接拒
        patch.key_ref = "sk_live_abcdef123456".into();
        let err = svc
            .upsert_service("ops", Role::Publisher, "t2", "svc_translate", patch)
            .unwrap_err();
        assert!(err.to_string().contains("vault://"), "{err}");
        // 拒绝的写入不改版本
        assert_eq!(svc.service("svc_translate").unwrap().unwrap().version, 1);
    }

    #[test]
    fn service_config_updates_are_versioned() {
        let (svc, _p, _s) = service();
        let base = ServicePatch {
            title: "地图".into(),
            category: "tools".into(),
            regions: vec!["CN".into(), "SG".into()],
            enabled: false,
            key_ref: "vault://wxlike/map".into(),
        };
        let v1 = svc
            .upsert_service("ops", Role::Publisher, "t1", "svc_map", base.clone())
            .unwrap();
        let v2 = svc
            .upsert_service(
                "ops",
                Role::Publisher,
                "t2",
                "svc_map",
                ServicePatch {
                    enabled: true,
                    ..base
                },
            )
            .unwrap();
        assert_eq!((v1.version, v2.version), (1, 2));
        assert_eq!(v2.regions, vec!["CN".to_string(), "SG".to_string()]);
        let log = svc.audit().query(&crate::audit::Filter::default()).unwrap();
        assert!(log
            .rows
            .iter()
            .any(|e| e.action == "service.upsert" && e.after_digest != e.before_digest));
    }

    #[test]
    fn rule_versions_are_pinned_per_kind() {
        assert_eq!(rule_version_for("content"), Some("content-policy-2026.10"));
        assert_eq!(rule_version_for("podcast"), None);
        let (svc, _p, _s) = service();
        let item = svc
            .submit("ops", Role::Moderator, "t", sub("idem-r"))
            .unwrap();
        assert_eq!(item.rule_version, "content-policy-2026.10");
        assert_eq!(rules().len(), 3);
    }
}
