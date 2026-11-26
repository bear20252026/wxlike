//! G02-09 账户治理（管理员 audience）：封禁 / 解封 / 撤销设备 / 元数据视图。
//!
//! # 边界（与 `crates/admin/src/identity.rs::HttpPort` 逐字对应）
//!
//! adminx 只发命令、不复制域算法。四个管理端点的真实动作在这里：
//!
//! | admin 端点 | 域函数 |
//! |---|---|
//! | `POST /api/admin/identity/users/{id}/suspend` | [`user::suspend`] |
//! | `POST /api/admin/identity/users/{id}/unsuspend` | [`user::unsuspend`] |
//! | `POST /api/admin/identity/devices/{id}/revoke` | [`device::revoke_device`] |
//! | `POST /api/admin/identity/groups/{id}/govern` | `wxlike_relations::governance::govern` |
//!
//! 回执 [`GovernEffect`] 的 `reference/before/after/sessions_revoked/already`
//! 与 admin 的 `Effect` 同形；`before/after` 只是**状态摘要**，绝不带消息正文。
//!
//! # 三条纪律
//!
//! 1. **audience 分离**：每个写命令都先用 `authenticate_operator` 走
//!    `session::authenticate(.., Audience::Admin)`——普通用户令牌进不来
//!    （复用 G02-01 的受众/认证级别判定，服务端再次鉴权）。
//! 2. **幂等**：同操作重复提交命中 `already=true`，不产生二次状态变更、不再
//!    次撤销会话；但**镜像与事件会重放**，让平台侧向域状态收敛（见
//!    [`PlatformGovernancePort`]）。
//! 3. **域发事件、平台执行**：封禁即时撤销会话（删 `sessions`/刷新链/step-up），
//!    进程内效果（断 WS、清缓存、设备密钥作废、users 表镜像）由
//!    [`PlatformGovernancePort`] 交平台进程执行——本 crate 不发 IO、不写 admin
//!    审计表，只落自己的 `append_audit` 回执并把事件交回调用方。
//!
//! # ADR-28
//!
//! 本模块是**纯同步**域 API（`&Connection` 进、`Result` 出），没有 async 体，
//! 不新增 tokio 依赖。平台 HTTP 面挂载时把整段同步调用包进 `spawn_blocking`
//! （G00 集成面，见 `integration-requests/G02/G02-09-governance.yaml`）。

pub mod device;
pub mod user;
pub mod view;

use rusqlite::Connection;

use crate::sqlport::{ColKind, SqlExec, SqlParam, SqliteExec};
use serde::{Deserialize, Serialize};

use crate::{IdError, Result};

pub use device::{revoke_device, revoke_device_tx};
pub use user::{
    add_label, add_label_with, ensure_user_active, ensure_user_active_with, remove_label,
    remove_label_with, set_disabled, settle_expired, suspend, unsuspend, user_state, UserState,
};
pub use view::{user_view, user_view_with, AdminUserView, NoCounts, METADATA_FIELDS};

/// 处置理由上限（与 admin `audit::REASON_MAX_CHARS` 同口径）。
pub const REASON_MAX_CHARS: usize = 512;
/// 目标 id / 操作者名长度上限（与 admin `upstream::check_target` 同口径）。
pub const TARGET_MAX_LEN: usize = 64;

/// 一次治理命令。字段与 admin `identity::Command` 对应（`verb` 仅群治理用，
/// 由 relations 域消费；`new_owner` 是群转让的扩展字段，见集成请求）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernCommand {
    pub target: String,
    pub reason: String,
    /// 封禁到期时间（毫秒）。`None` = 永久。
    #[serde(default)]
    pub until_ms: Option<i64>,
    pub actor: String,
    #[serde(default)]
    pub trace: String,
    /// 群治理动词（`freeze`/`unfreeze`/`transfer`/`dissolve`）。
    #[serde(default)]
    pub verb: Option<String>,
    /// 群转让的新群主（仅 `transfer` 用；adminx 当前未透传，见集成请求）。
    #[serde(default)]
    pub new_owner: Option<String>,
}

/// 治理回执。`reference/before/after/sessions_revoked/already` 与 admin
/// `identity::Effect` 逐字对应；`events` 只交平台端口投递，不进 HTTP 回执。
#[derive(Clone, Debug, Serialize)]
pub struct GovernEffect {
    pub reference: String,
    pub before: serde_json::Value,
    pub after: serde_json::Value,
    pub sessions_revoked: u64,
    pub already: bool,
    #[serde(skip)]
    pub events: Vec<GovernanceEvent>,
}

/// 域发出的治理事件（平台进程消费：断 WS、清缓存、作废设备密钥、镜像 users）。
/// 全是元数据，不含任何消息正文。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GovernanceEvent {
    /// 用户治理状态变化（封禁/解封/停用/到期）。
    UserStateChanged {
        user_id: String,
        state: String,
        suspended_until_ms: Option<i64>,
    },
    /// 本次命令连带撤销的会话数（封禁验收面：非零）。
    SessionsRevoked { user_id: String, count: u64 },
    /// 设备被管理端撤销。
    DeviceRevoked { user_id: String, device_id: String },
    /// 缓存清理（scope: user/session/device）。
    CacheClear { scope: String, key: String },
}

/// 平台联动端口（**G00 集成面**，本 crate 只定义边界、不实现）。
///
/// 域命令在落库后调用它：把治理状态镜像到平台 `users` 表
/// （`suspended/disabled/suspended_until_ms` 补列由 G00 执行），并投递进程内
/// 即时效果。幂等命中也会重放这两个调用，保证"后台封了、服务还通"不会残留。
pub trait PlatformGovernancePort {
    /// 镜像 users 表治理字段。`suspended/disabled` 是存储标志位，
    /// `until_ms` 是封禁到期（`None` = 永久或已解封）。
    fn mirror_user_state(
        &self,
        user_id: &str,
        suspended: bool,
        disabled: bool,
        until_ms: Option<i64>,
    ) -> Result<()>;

    /// 投递治理事件（断 WS、清缓存、设备密钥作废等进程内效果）。
    fn dispatch(&self, events: &[GovernanceEvent]);
}

/// 跨域元数据计数端口（**G00 集成面**）。群数来自 relations、举报数来自平台
/// `reports` 表——identity 不直接读别的域的表，计数由平台装配时注入。
pub trait UserMetadataPort {
    /// 用户参与的未解散群数。
    fn group_count(&self, user_id: &str) -> Result<i64>;
    /// 针对该用户的举报工单数。
    fn report_count(&self, user_id: &str) -> Result<i64>;
}

/// 治理域建表（幂等）。由 [`crate::migrate`] 调用；正式迁移编号归 G00。
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

const SCHEMA: &str = r#"
-- G02-09 用户治理状态（identity 域自有；平台 users 表的镜像列由 G00 补）。
-- suspended/disabled 是存储标志位；临时封禁到期按 suspended_until_ms 惰性判定。
CREATE TABLE IF NOT EXISTS identity_user_governance (
    user_id            TEXT PRIMARY KEY,
    suspended          INTEGER NOT NULL DEFAULT 0,
    disabled           INTEGER NOT NULL DEFAULT 0,
    suspended_until_ms INTEGER,
    reason             TEXT NOT NULL DEFAULT '',
    actor              TEXT NOT NULL DEFAULT '',
    updated_at         INTEGER NOT NULL
);

-- G02-09 治理回执/审计（域自有，append-only；不写 admin 的审计表）。
CREATE TABLE IF NOT EXISTS identity_governance_audit (
    seq              INTEGER PRIMARY KEY AUTOINCREMENT,
    action           TEXT NOT NULL,
    target           TEXT NOT NULL,
    actor            TEXT NOT NULL,
    reason           TEXT NOT NULL,
    trace            TEXT NOT NULL DEFAULT '',
    before_json      TEXT NOT NULL DEFAULT '{}',
    after_json       TEXT NOT NULL DEFAULT '{}',
    sessions_revoked INTEGER NOT NULL DEFAULT 0,
    already          INTEGER NOT NULL DEFAULT 0,
    at               INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_identity_gov_audit_target
    ON identity_governance_audit(target, seq);

-- G02-09 管理标签（运营给用户打的元数据标签，进 AdminUserView.labels）。
CREATE TABLE IF NOT EXISTS identity_governance_label (
    user_id    TEXT NOT NULL,
    label      TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, label)
);
"#;

/// 管理操作者鉴权：只认 **admin audience** 的会话令牌（复用 G02-01）。
/// 普通用户令牌、过期/撤销令牌、级别不足一律 `Unauthorized`。
pub(crate) fn authenticate_operator(
    conn: &Connection,
    operator_token: &str,
    now: i64,
) -> Result<crate::session::AuthContext> {
    authenticate_operator_with(&SqliteExec(conn), operator_token, now)
}

/// 端口版管理端鉴权：只认 admin audience 的会话令牌。
pub(crate) fn authenticate_operator_with<E: SqlExec + ?Sized>(
    exec: &E,
    operator_token: &str,
    now: i64,
) -> Result<crate::session::AuthContext> {
    crate::session::authenticate_with(exec, operator_token, crate::Audience::Admin, now)
}

/// 目标 id 形状校验（路径段安全：只允许 ASCII 字母数字与 `- _ .`）。
pub(crate) fn check_target(raw: &str) -> Result<String> {
    let t = raw.trim();
    if t.is_empty() || t.len() > TARGET_MAX_LEN {
        return Err(IdError::BadInput("target id"));
    }
    if !t
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(IdError::BadInput("target id"));
    }
    Ok(t.to_string())
}

/// 处置理由必填且限长（封禁/解封都要能复核依据）。
pub(crate) fn check_reason(raw: &str) -> Result<String> {
    let t = raw.trim();
    if t.is_empty() {
        return Err(IdError::BadInput("reason required"));
    }
    if t.chars().count() > REASON_MAX_CHARS {
        return Err(IdError::BadInput("reason too long"));
    }
    Ok(t.to_string())
}

/// 操作者名必填（审计要落到具体的人，不是匿名 "admin"）。
pub(crate) fn check_actor(raw: &str) -> Result<String> {
    let t = raw.trim();
    if t.is_empty() || t.chars().count() > TARGET_MAX_LEN {
        return Err(IdError::BadInput("actor"));
    }
    Ok(t.to_string())
}

/// 目标用户必须存在（读平台 users 表，只读不写）。
///
/// `users` 表由平台建（与 `sessions` 同口径）：identity 独立测试库没有这张表，
/// 此时跳过存在性校验（治理状态以 user_id 为键，不依赖 users 行）。平台库里
/// 表一定在，校验照常生效——封一个不存在的账号返回 NotFound。
pub(crate) fn require_user(conn: &Connection, user_id: &str) -> Result<()> {
    require_user_with(&SqliteExec(conn), user_id)
}

/// 端口版目标存在性校验：平台 `users` 表不存在时跳过（identity 独立测试库）。
pub(crate) fn require_user_with<E: SqlExec + ?Sized>(exec: &E, user_id: &str) -> Result<()> {
    if !exec.table_exists("users")? {
        return Ok(());
    }
    let row = exec.query(
        "SELECT COUNT(*) FROM users WHERE id = ?1",
        &[SqlParam::Text(user_id.to_string())],
        &[ColKind::Int],
    )?;
    match row.and_then(|mut row| row.pop()) {
        Some(crate::sqlport::SqlValue::Int(0)) => Err(IdError::NotFound("user")),
        Some(crate::sqlport::SqlValue::Int(_)) => Ok(()),
        _ => Err(IdError::Corrupt("users count column")),
    }
}

/// 一条治理审计草稿（字段打包，避免超长参数列表）。
pub(crate) struct AuditDraft<'a> {
    pub action: &'a str,
    pub target: &'a str,
    pub actor: &'a str,
    pub reason: &'a str,
    pub trace: &'a str,
    pub before: &'a serde_json::Value,
    pub after: &'a serde_json::Value,
    pub sessions_revoked: u64,
    pub already: bool,
    pub at: i64,
}

/// 落一条治理回执，返回自增 seq（回执 reference 用它关联审计行）。
pub(crate) fn append_audit(conn: &Connection, draft: &AuditDraft<'_>) -> Result<i64> {
    append_audit_with(&SqliteExec(conn), draft)
}

/// 端口版审计写入：INSERT 不写方言，自增 seq 由端口补 `RETURNING`。
pub(crate) fn append_audit_with<E: SqlExec + ?Sized>(
    exec: &E,
    draft: &AuditDraft<'_>,
) -> Result<i64> {
    exec.insert_returning(
        "INSERT INTO identity_governance_audit(action, target, actor, reason, trace, \
         before_json, after_json, sessions_revoked, already, at) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        &[
            SqlParam::Text(draft.action.to_string()),
            SqlParam::Text(draft.target.to_string()),
            SqlParam::Text(draft.actor.to_string()),
            SqlParam::Text(draft.reason.to_string()),
            SqlParam::Text(draft.trace.to_string()),
            SqlParam::Text(serde_json::to_string(draft.before)?),
            SqlParam::Text(serde_json::to_string(draft.after)?),
            SqlParam::Int(draft.sessions_revoked as i64),
            SqlParam::Int(i64::from(draft.already)),
            SqlParam::Int(draft.at),
        ],
    )
}

/// 回执 reference：`gov:<action>:<audit seq>`，可据此回查审计行。
pub(crate) fn reference(action: &str, seq: i64) -> String {
    format!("gov:{action}:{seq}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_shape_is_path_safe() {
        assert_eq!(check_target("u_1001").unwrap(), "u_1001");
        assert!(check_target("").is_err());
        assert!(check_target("../../etc/passwd").is_err());
        assert!(check_target("a/b").is_err());
        assert!(check_target(&"x".repeat(65)).is_err());
    }

    #[test]
    fn reason_is_required_and_bounded() {
        assert!(check_reason("   ").is_err());
        assert_eq!(check_reason("  冒充客服  ").unwrap(), "冒充客服");
        assert!(check_reason(&"x".repeat(REASON_MAX_CHARS + 1)).is_err());
        assert!(check_reason(&"x".repeat(REASON_MAX_CHARS)).is_ok());
    }

    #[test]
    fn actor_is_required() {
        assert!(check_actor("").is_err());
        assert_eq!(check_actor(" ops ").unwrap(), "ops");
    }

    #[test]
    fn reference_links_audit_seq() {
        assert_eq!(reference("suspend", 7), "gov:suspend:7");
    }
}
