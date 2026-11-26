//! G02-09 用户治理状态机：suspend / unsuspend / disabled / 临时封禁到期。
//!
//! # 状态模型
//!
//! 存储只有两个标志位 + 一个到期时间（`identity_user_governance`）：
//!
//! * `suspended`：封禁标志（临时或永久）；
//! * `disabled`：停用标志（独立于封禁的管理处置，`set_disabled`）；
//! * `suspended_until_ms`：临时封禁到期；`None` = 永久。
//!
//! **有效状态**由 [`UserState::effective`] 惰性导出（不依赖后台清扫）：
//!
//! | 条件 | 有效状态 |
//! |---|---|
//! | `disabled` | `disabled` |
//! | `suspended` 且（永久 或 `now < until`） | `suspended` |
//! | `suspended` 且 `now >= until` | `expired`（按未封禁放行） |
//! | 其余 | `active` |
//!
//! 到期是**惰性判定**：`ensure_user_active`/`user_view` 读到的就是有效状态，
//! 封禁一到期 API 立即恢复，不等任何定时任务。[`settle_expired`] 把到期的行
//! 物理结算回 active 并留痕（供平台定时作业调用）。
//!
//! # 幂等与回执
//!
//! 同操作重复提交：状态已到位 → `already=true`、`sessions_revoked=0`、不再改
//! 状态；但**平台镜像与事件照常重放**（收敛用），审计也照落（谁又提交了一次）。
//! 改期（已封禁但 `until_ms` 变了）视为**新处置**：`already=false`，重新撤销
//! 会话——封禁期延长必须把现存会话再踢一遍。

use rusqlite::{params, Connection};

use crate::sqlport::{ColKind, SqlExec, SqlParam, SqlValue, SqliteExec};
use serde::{Deserialize, Serialize};

use super::{
    append_audit, append_audit_with, authenticate_operator_with, check_actor, check_reason,
    check_target, reference, require_user_with, AuditDraft, GovernCommand, GovernEffect,
    GovernanceEvent, PlatformGovernancePort,
};
use crate::{check_label, IdError, Result};

/// 用户治理状态（对外展示与闸门判定的唯一口径）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserState {
    Active,
    Suspended,
    Disabled,
    /// 临时封禁已到期：存储标志还在，但有效状态按未封禁处理。
    Expired,
}

impl UserState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Suspended => "suspended",
            Self::Disabled => "disabled",
            Self::Expired => "expired",
        }
    }

    /// 能否放行用户面请求（登录闸门/会话鉴权后调用）。
    pub fn allows_access(self) -> bool {
        matches!(self, Self::Active | Self::Expired)
    }

    /// 由存储标志 + 当前时间导出有效状态。
    pub fn effective(suspended: bool, disabled: bool, until_ms: Option<i64>, now: i64) -> Self {
        if disabled {
            return Self::Disabled;
        }
        if !suspended {
            return Self::Active;
        }
        match until_ms {
            Some(until) if now >= until => Self::Expired,
            _ => Self::Suspended,
        }
    }
}

/// 治理行的存储形状（内部）。
#[derive(Clone, Debug)]
struct GovRow {
    suspended: bool,
    disabled: bool,
    until_ms: Option<i64>,
}

impl GovRow {
    fn state(&self, now: i64) -> UserState {
        UserState::effective(self.suspended, self.disabled, self.until_ms, now)
    }

    /// 镜像到平台 users 表的**有效**标志：到期的临时封禁按未封禁镜像，
    /// 平台不能把已到期用户继续当封禁（读路径惰性判定同口径）。
    fn mirror_flags(&self, now: i64) -> (bool, bool) {
        let suspended = self.state(now) == UserState::Suspended;
        (suspended, self.disabled)
    }
}

/// 无治理行 = 两个标志都 false。
fn flags_of(row: Option<&GovRow>, now: i64) -> (bool, bool) {
    row.map(|r| r.mirror_flags(now)).unwrap_or((false, false))
}

fn load(conn: &Connection, user_id: &str) -> Result<Option<GovRow>> {
    load_with(&SqliteExec(conn), user_id)
}

/// 端口版：读治理行（无行 = 从未处置）。
fn load_with<E: SqlExec + ?Sized>(exec: &E, user_id: &str) -> Result<Option<GovRow>> {
    let Some(row) = exec.query(
        "SELECT suspended, disabled, suspended_until_ms FROM identity_user_governance \
         WHERE user_id = ?1",
        &[SqlParam::Text(user_id.to_string())],
        &[ColKind::Int, ColKind::Int, ColKind::OptInt],
    )?
    else {
        return Ok(None);
    };
    let mut it = row.into_iter();
    let suspended = sql_int(it.next())? != 0;
    let disabled = sql_int(it.next())? != 0;
    let until_ms = match it.next() {
        Some(SqlValue::Int(v)) => Some(v),
        Some(SqlValue::Null) => None,
        _ => return Err(IdError::Corrupt("governance until column")),
    };
    Ok(Some(GovRow {
        suspended,
        disabled,
        until_ms,
    }))
}

fn sql_int(value: Option<SqlValue>) -> Result<i64> {
    match value {
        Some(SqlValue::Int(v)) => Ok(v),
        _ => Err(IdError::Corrupt("governance row column")),
    }
}

/// 读用户有效治理状态（无行 = active）。
pub fn user_state(conn: &Connection, user_id: &str, now: i64) -> Result<UserState> {
    user_state_with(&SqliteExec(conn), user_id, now)
}

/// 端口版有效状态。
pub fn user_state_with<E: SqlExec + ?Sized>(
    exec: &E,
    user_id: &str,
    now: i64,
) -> Result<UserState> {
    Ok(load_with(exec, user_id)?
        .map(|r| r.state(now))
        .unwrap_or(UserState::Active))
}

/// 登录/签发会话前的治理闸门：被封禁/停用一律 `Forbidden`。
/// 平台登录入口在口令校验后调用它（G00 集成面）；临时封禁到期自动放行。
pub fn ensure_user_active(conn: &Connection, user_id: &str, now: i64) -> Result<()> {
    ensure_user_active_with(&SqliteExec(conn), user_id, now)
}

/// 端口版登录闸门：会话签发与认证咽喉共用同一份有效状态判定。
pub fn ensure_user_active_with<E: SqlExec + ?Sized>(
    exec: &E,
    user_id: &str,
    now: i64,
) -> Result<()> {
    let state = user_state_with(exec, user_id, now)?;
    if state.allows_access() {
        Ok(())
    } else {
        Err(IdError::Forbidden("account not active"))
    }
}

/// 状态摘要（回执 before/after 与审计共用；只有元数据）。
fn summary(state: UserState, until_ms: Option<i64>) -> serde_json::Value {
    serde_json::json!({
        "state": state.as_str(),
        "suspended_until_ms": until_ms,
    })
}

/// 事务内完成的治理结果：状态、审计序号与回执都已就绪，只差平台镜像和事件。
/// 平台 `Db::with_tx` 提交后调用 [`GovernCommit::finish`]，副作用不会跑在提交前。
pub struct GovernCommit {
    target: String,
    suspended: bool,
    disabled: bool,
    until_ms: Option<i64>,
    effect: GovernEffect,
}

impl GovernCommit {
    /// 提交后发布：先镜像 `users` 补列，再派发进程内事件。
    pub fn finish(self, port: &dyn PlatformGovernancePort) -> Result<GovernEffect> {
        port.mirror_user_state(&self.target, self.suspended, self.disabled, self.until_ms)?;
        port.dispatch(&self.effect.events);
        Ok(self.effect)
    }
}

/// 封禁。`until_ms = None` 为永久；临时封禁到期后自动恢复（惰性判定）。
///
/// 即时撤销该用户**全部**会话（sessions + 令牌侧车 + 刷新链 + step-up），
/// 与 `session::logout_all` 同口径；回执 `sessions_revoked` 即撤销条数。
/// 即时撤销全部会话：sessions 行删掉即拿到撤销条数；侧车/刷新链/step-up
/// 按 user_id 直接清，不依赖会话行还在（与 session::logout_all 同口径）。
/// suspend 与 disable 共用这一份——撤销口径只有一处定义。
fn revoke_all_sessions_with<E: SqlExec + ?Sized>(
    exec: &E,
    target: &str,
    now: i64,
) -> Result<usize> {
    let revoked = exec.exec(
        "DELETE FROM sessions WHERE user_id = ?1",
        &[SqlParam::Text(target.to_string())],
    )?;
    exec.exec(
        "DELETE FROM identity_session_token WHERE user_id = ?1",
        &[SqlParam::Text(target.to_string())],
    )?;
    crate::refresh::revoke_user_with(exec, target, now)?;
    exec.exec(
        "DELETE FROM identity_step_up WHERE user_id = ?1",
        &[SqlParam::Text(target.to_string())],
    )?;
    Ok(revoked)
}

/// 治理处置的公共收尾：事件构造 + 平台镜像 + 回执。suspend/disable/enable
/// 的事件形状完全一致（状态变更 + 会话撤销 + 缓存清理），只有 action 名不同。
#[allow(clippy::too_many_arguments)]
fn commit_of(
    target: &str,
    after_state: UserState,
    after_until: Option<i64>,
    after_row: Option<&GovRow>,
    now: i64,
    action: &str,
    seq: i64,
    before: serde_json::Value,
    after: serde_json::Value,
    sessions_revoked: usize,
    already: bool,
) -> GovernCommit {
    let mut events = vec![GovernanceEvent::UserStateChanged {
        user_id: target.to_string(),
        state: after_state.as_str().to_string(),
        suspended_until_ms: after_until,
    }];
    if sessions_revoked > 0 {
        events.push(GovernanceEvent::SessionsRevoked {
            user_id: target.to_string(),
            count: sessions_revoked as u64,
        });
    }
    events.push(GovernanceEvent::CacheClear {
        scope: "user".to_string(),
        key: target.to_string(),
    });
    let (suspended, disabled) = flags_of(after_row, now);
    GovernCommit {
        target: target.to_string(),
        suspended,
        disabled,
        until_ms: after_until,
        effect: GovernEffect {
            reference: reference(action, seq),
            before,
            after,
            sessions_revoked: sessions_revoked as u64,
            already,
            events,
        },
    }
}

/// 三个处置入口共用的前置：校验字段、鉴权、读变更前状态。
struct Prep {
    target: String,
    reason: String,
    actor: String,
    before_row: Option<GovRow>,
    before_state: UserState,
    before_until: Option<i64>,
}

fn prepare_with<E: SqlExec + ?Sized>(
    exec: &E,
    operator_token: &str,
    cmd: &GovernCommand,
    now: i64,
) -> Result<Prep> {
    let target = check_target(&cmd.target)?;
    let reason = check_reason(&cmd.reason)?;
    let actor = check_actor(&cmd.actor)?;
    authenticate_operator_with(exec, operator_token, now)?;
    require_user_with(exec, &target)?;
    let before_row = load_with(exec, &target)?;
    let before_state = state_of(&before_row, now);
    let before_until = before_row.as_ref().and_then(|r| r.until_ms);
    Ok(Prep {
        target,
        reason,
        actor,
        before_row,
        before_state,
        before_until,
    })
}

fn state_of(row: &Option<GovRow>, now: i64) -> UserState {
    row.as_ref()
        .map(|r| r.state(now))
        .unwrap_or(UserState::Active)
}

/// 事务内收尾：重读状态 → 审计。提交与镜像由调用方接着做。
struct WriteOutcome {
    after_row: Option<GovRow>,
    after_state: UserState,
    after_until: Option<i64>,
    before: serde_json::Value,
    after: serde_json::Value,
    seq: i64,
}

fn close_tx_with<E: SqlExec + ?Sized>(
    exec: &E,
    prep: &Prep,
    action: &str,
    cmd: &GovernCommand,
    sessions_revoked: usize,
    already: bool,
    now: i64,
) -> Result<WriteOutcome> {
    let after_row = load_with(exec, &prep.target)?;
    let after_state = state_of(&after_row, now);
    let after_until = after_row.as_ref().and_then(|r| r.until_ms);
    let before = summary(prep.before_state, prep.before_until);
    let after = summary(after_state, after_until);
    let seq = append_audit_with(
        exec,
        &AuditDraft {
            action,
            target: &prep.target,
            actor: &prep.actor,
            reason: &prep.reason,
            trace: &cmd.trace,
            before: &before,
            after: &after,
            sessions_revoked: sessions_revoked as u64,
            already,
            at: now,
        },
    )?;
    Ok(WriteOutcome {
        after_row,
        after_state,
        after_until,
        before,
        after,
        seq,
    })
}

pub fn suspend(
    conn: &Connection,
    operator_token: &str,
    cmd: &GovernCommand,
    port: &dyn PlatformGovernancePort,
    now: i64,
) -> Result<GovernEffect> {
    let tx = conn.unchecked_transaction()?;
    let pending = suspend_tx(&SqliteExec(&tx), operator_token, cmd, now)?;
    tx.commit()?;
    pending.finish(port)
}

/// 端口版封禁：调用方负责事务边界，提交后再调用 [`GovernCommit::finish`]。
pub fn suspend_tx<E: SqlExec + ?Sized>(
    exec: &E,
    operator_token: &str,
    cmd: &GovernCommand,
    now: i64,
) -> Result<GovernCommit> {
    let prep = prepare_with(exec, operator_token, cmd, now)?;
    if let Some(until) = cmd.until_ms {
        if until <= now {
            return Err(IdError::BadInput("until_ms must be in the future"));
        }
    }
    // 幂等命中：已经在**生效的**封禁中且到期时间没变。已到期（标志未结算）
    // 的行不算命中——重新封禁是新的处置，要再踢一遍会话。
    let already = prep
        .before_row
        .as_ref()
        .map(|r| r.suspended && r.state(now) == UserState::Suspended && r.until_ms == cmd.until_ms)
        .unwrap_or(false);

    let mut sessions_revoked = 0usize;
    if !already {
        exec.exec(
            "INSERT INTO identity_user_governance(user_id, suspended, disabled, \
             suspended_until_ms, reason, actor, updated_at) \
             VALUES(?1, 1, COALESCE((SELECT disabled FROM identity_user_governance \
             WHERE user_id = ?1), 0), ?2, ?3, ?4, ?5) \
             ON CONFLICT(user_id) DO UPDATE SET suspended = 1, suspended_until_ms = ?2, \
             reason = ?3, actor = ?4, updated_at = ?5",
            &[
                SqlParam::Text(prep.target.to_string()),
                match cmd.until_ms {
                    Some(v) => SqlParam::Int(v),
                    None => SqlParam::Null,
                },
                SqlParam::Text(prep.reason.to_string()),
                SqlParam::Text(prep.actor.to_string()),
                SqlParam::Int(now),
            ],
        )?;
        sessions_revoked = revoke_all_sessions_with(exec, &prep.target, now)?;
    }
    let out = close_tx_with(exec, &prep, "suspend", cmd, sessions_revoked, already, now)?;
    // 镜像与事件**幂等命中也重放**：平台侧据此收敛（"后台封了、服务还通"
    // 的残留就是靠重放消掉的）。操作者上下文只用于鉴权，审计落 cmd.actor。
    Ok(commit_of(
        &prep.target,
        out.after_state,
        out.after_until,
        out.after_row.as_ref(),
        now,
        "suspend",
        out.seq,
        out.before,
        out.after,
        sessions_revoked,
        already,
    ))
}

/// 解封（可逆且本身留痕）。永久/临时封禁都清；`disabled` 是独立处置，不受影响。
pub fn unsuspend(
    conn: &Connection,
    operator_token: &str,
    cmd: &GovernCommand,
    port: &dyn PlatformGovernancePort,
    now: i64,
) -> Result<GovernEffect> {
    let tx = conn.unchecked_transaction()?;
    let pending = unsuspend_tx(&SqliteExec(&tx), operator_token, cmd, now)?;
    tx.commit()?;
    pending.finish(port)
}

/// 端口版解封：调用方负责事务边界，提交后再调用 [`GovernCommit::finish`]。
pub fn unsuspend_tx<E: SqlExec + ?Sized>(
    exec: &E,
    operator_token: &str,
    cmd: &GovernCommand,
    now: i64,
) -> Result<GovernCommit> {
    let prep = prepare_with(exec, operator_token, cmd, now)?;
    // 幂等：本来就没在封禁（含已到期）→ already。
    let already = !prep
        .before_row
        .as_ref()
        .map(|r| r.suspended)
        .unwrap_or(false);

    if !already {
        exec.exec(
            "UPDATE identity_user_governance SET suspended = 0, suspended_until_ms = NULL, \
             reason = ?2, actor = ?3, updated_at = ?4 WHERE user_id = ?1",
            &[
                SqlParam::Text(prep.target.to_string()),
                SqlParam::Text(prep.reason.to_string()),
                SqlParam::Text(prep.actor.to_string()),
                SqlParam::Int(now),
            ],
        )?;
    }
    let out = close_tx_with(exec, &prep, "unsuspend", cmd, 0, already, now)?;
    Ok(commit_of(
        &prep.target,
        out.after_state,
        out.after_until,
        out.after_row.as_ref(),
        now,
        "unsuspend",
        out.seq,
        out.before,
        out.after,
        0,
        already,
    ))
}

/// 停用/启用（`disabled` 标志，独立于封禁）。停用同样即时撤销全部会话。
pub fn set_disabled(
    conn: &Connection,
    operator_token: &str,
    cmd: &GovernCommand,
    disabled: bool,
    port: &dyn PlatformGovernancePort,
    now: i64,
) -> Result<GovernEffect> {
    let tx = conn.unchecked_transaction()?;
    let pending = set_disabled_tx(&SqliteExec(&tx), operator_token, cmd, disabled, now)?;
    tx.commit()?;
    pending.finish(port)
}

/// 端口版停用/启用：调用方负责事务边界，提交后再调用 [`GovernCommit::finish`]。
pub fn set_disabled_tx<E: SqlExec + ?Sized>(
    exec: &E,
    operator_token: &str,
    cmd: &GovernCommand,
    disabled: bool,
    now: i64,
) -> Result<GovernCommit> {
    let prep = prepare_with(exec, operator_token, cmd, now)?;
    let already = prep
        .before_row
        .as_ref()
        .map(|r| r.disabled == disabled)
        .unwrap_or(!disabled);

    let mut sessions_revoked = 0usize;
    if !already {
        exec.exec(
            "INSERT INTO identity_user_governance(user_id, suspended, disabled, \
             suspended_until_ms, reason, actor, updated_at) \
             VALUES(?1, COALESCE((SELECT suspended FROM identity_user_governance \
             WHERE user_id = ?1), 0), ?2, (SELECT suspended_until_ms FROM \
             identity_user_governance WHERE user_id = ?1), ?3, ?4, ?5) \
             ON CONFLICT(user_id) DO UPDATE SET disabled = ?2, reason = ?3, actor = ?4, \
             updated_at = ?5",
            &[
                SqlParam::Text(prep.target.to_string()),
                SqlParam::Int(disabled as i64),
                SqlParam::Text(prep.reason.to_string()),
                SqlParam::Text(prep.actor.to_string()),
                SqlParam::Int(now),
            ],
        )?;
        if disabled {
            sessions_revoked = revoke_all_sessions_with(exec, &prep.target, now)?;
        }
    }
    let action = if disabled { "disable" } else { "enable" };
    let out = close_tx_with(exec, &prep, action, cmd, sessions_revoked, already, now)?;
    Ok(commit_of(
        &prep.target,
        out.after_state,
        out.after_until,
        out.after_row.as_ref(),
        now,
        action,
        out.seq,
        out.before,
        out.after,
        sessions_revoked,
        already,
    ))
}

/// 结算到期的临时封禁（平台定时作业调用；读路径的惰性判定不依赖它）。
/// 返回结算的用户数；每个被结算的用户落一条 `suspend.expire` 审计并镜像平台。
pub fn settle_expired(
    conn: &Connection,
    port: &dyn PlatformGovernancePort,
    now: i64,
) -> Result<usize> {
    let expired = expired_suspensions(conn, now)?;
    for user_id in &expired {
        let tx = conn.unchecked_transaction()?;
        if !clear_expiry(&tx, user_id, now)? {
            continue;
        }
        let after_row = load(&tx, user_id)?;
        let after_state = after_row
            .as_ref()
            .map(|r| r.state(now))
            .unwrap_or(UserState::Active);
        let after_until = after_row.as_ref().and_then(|r| r.until_ms);
        audit_expiry(&tx, user_id, &after_state, after_until, now)?;
        tx.commit()?;
        let (suspended, disabled) = flags_of(after_row.as_ref(), now);
        port.mirror_user_state(user_id, suspended, disabled, after_until)?;
        port.dispatch(&[
            GovernanceEvent::UserStateChanged {
                user_id: user_id.clone(),
                state: after_state.as_str().to_string(),
                suspended_until_ms: after_until,
            },
            GovernanceEvent::CacheClear {
                scope: "user".to_string(),
                key: user_id.clone(),
            },
        ]);
    }
    Ok(expired.len())
}

fn expired_suspensions(conn: &Connection, now: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT user_id FROM identity_user_governance \
         WHERE suspended = 1 AND suspended_until_ms IS NOT NULL AND suspended_until_ms <= ?1",
    )?;
    let rows = stmt.query_map(params![now], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// 解封一行；返回是否真的改到了（并发下可能已被别人结算）。
fn clear_expiry(tx: &Connection, user_id: &str, now: i64) -> Result<bool> {
    let n = tx.execute(
        "UPDATE identity_user_governance SET suspended = 0, suspended_until_ms = NULL, \
         updated_at = ?2 WHERE user_id = ?1 AND suspended = 1 \
         AND suspended_until_ms IS NOT NULL AND suspended_until_ms <= ?2",
        params![user_id, now],
    )?;
    Ok(n > 0)
}

fn audit_expiry(
    tx: &Connection,
    user_id: &str,
    after_state: &UserState,
    after_until: Option<i64>,
    now: i64,
) -> Result<()> {
    let before = summary(UserState::Suspended, None);
    let after = summary(*after_state, after_until);
    append_audit(
        tx,
        &AuditDraft {
            action: "suspend.expire",
            target: user_id,
            actor: "system",
            reason: "temporary suspension expired",
            trace: "",
            before: &before,
            after: &after,
            sessions_revoked: 0,
            already: false,
            at: now,
        },
    )
    .map(|_| ())
}

/// 给用户打管理标签（幂等：同标签重复打不产生第二行）。
pub fn add_label(
    conn: &Connection,
    operator_token: &str,
    user_id: &str,
    label: &str,
    now: i64,
) -> Result<()> {
    add_label_with(&SqliteExec(conn), operator_token, user_id, label, now)
}

/// 端口版打标签：一条语句，事务由调用方决定。
pub fn add_label_with<E: SqlExec + ?Sized>(
    exec: &E,
    operator_token: &str,
    user_id: &str,
    label: &str,
    now: i64,
) -> Result<()> {
    super::authenticate_operator_with(exec, operator_token, now)?;
    let target = check_target(user_id)?;
    super::require_user_with(exec, &target)?;
    let label = check_label(label, "label")?;
    if label.is_empty() {
        return Err(IdError::BadInput("label"));
    }
    exec.exec(
        "INSERT INTO identity_governance_label(user_id, label, created_at) \
         VALUES(?1, ?2, ?3) ON CONFLICT DO NOTHING",
        &[
            SqlParam::Text(target),
            SqlParam::Text(label),
            SqlParam::Int(now),
        ],
    )?;
    Ok(())
}

/// 移除管理标签（幂等：不存在也不报错）。
pub fn remove_label(
    conn: &Connection,
    operator_token: &str,
    user_id: &str,
    label: &str,
    now: i64,
) -> Result<()> {
    remove_label_with(&SqliteExec(conn), operator_token, user_id, label, now)
}

/// 端口版移除标签：一条语句，事务由调用方决定。
pub fn remove_label_with<E: SqlExec + ?Sized>(
    exec: &E,
    operator_token: &str,
    user_id: &str,
    label: &str,
    now: i64,
) -> Result<()> {
    super::authenticate_operator_with(exec, operator_token, now)?;
    let target = check_target(user_id)?;
    exec.exec(
        "DELETE FROM identity_governance_label WHERE user_id = ?1 AND label = ?2",
        &[SqlParam::Text(target), SqlParam::Text(label.to_string())],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::db;

    #[test]
    fn effective_state_matrix() {
        assert_eq!(
            UserState::effective(false, false, None, 100),
            UserState::Active
        );
        assert_eq!(
            UserState::effective(true, false, None, 100),
            UserState::Suspended
        );
        assert_eq!(
            UserState::effective(true, false, Some(200), 100),
            UserState::Suspended
        );
        assert_eq!(
            UserState::effective(true, false, Some(200), 200),
            UserState::Expired
        );
        assert_eq!(
            UserState::effective(true, true, Some(200), 100),
            UserState::Disabled,
            "停用优先于封禁"
        );
        assert!(UserState::Expired.allows_access());
        assert!(!UserState::Suspended.allows_access());
        assert!(!UserState::Disabled.allows_access());
    }

    #[test]
    fn no_row_means_active() {
        let conn = db();
        assert_eq!(user_state(&conn, "nobody", 1).unwrap(), UserState::Active);
        assert!(ensure_user_active(&conn, "nobody", 1).is_ok());
    }
}
