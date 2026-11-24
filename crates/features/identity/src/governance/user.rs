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

use rusqlite::{params, Connection, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use super::{
    append_audit, authenticate_operator, check_actor, check_reason, check_target, reference,
    require_user, AuditDraft, GovernCommand, GovernEffect, GovernanceEvent, PlatformGovernancePort,
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

fn row_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<GovRow> {
    Ok(GovRow {
        suspended: r.get::<_, i64>(0)? != 0,
        disabled: r.get::<_, i64>(1)? != 0,
        until_ms: r.get(2)?,
    })
}

fn load(conn: &Connection, user_id: &str) -> Result<Option<GovRow>> {
    Ok(conn
        .query_row(
            "SELECT suspended, disabled, suspended_until_ms FROM identity_user_governance \
             WHERE user_id = ?1",
            params![user_id],
            row_of,
        )
        .optional()?)
}

/// 读用户有效治理状态（无行 = active）。
pub fn user_state(conn: &Connection, user_id: &str, now: i64) -> Result<UserState> {
    Ok(load(conn, user_id)?
        .map(|r| r.state(now))
        .unwrap_or(UserState::Active))
}

/// 登录/签发会话前的治理闸门：被封禁/停用一律 `Forbidden`。
/// 平台登录入口在口令校验后调用它（G00 集成面）；临时封禁到期自动放行。
pub fn ensure_user_active(conn: &Connection, user_id: &str, now: i64) -> Result<()> {
    let state = user_state(conn, user_id, now)?;
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

/// 封禁。`until_ms = None` 为永久；临时封禁到期后自动恢复（惰性判定）。
///
/// 即时撤销该用户**全部**会话（sessions + 令牌侧车 + 刷新链 + step-up），
/// 与 `session::logout_all` 同口径；回执 `sessions_revoked` 即撤销条数。
/// 即时撤销全部会话：sessions 行删掉即拿到撤销条数；侧车/刷新链/step-up
/// 按 user_id 直接清，不依赖会话行还在（与 session::logout_all 同口径）。
/// suspend 与 disable 共用这一份——撤销口径只有一处定义。
fn revoke_all_sessions(tx: &Connection, target: &str, now: i64) -> Result<usize> {
    let revoked = tx.execute("DELETE FROM sessions WHERE user_id = ?1", params![target])?;
    tx.execute(
        "DELETE FROM identity_session_token WHERE user_id = ?1",
        params![target],
    )?;
    crate::refresh::revoke_user(tx, target, now)?;
    tx.execute(
        "DELETE FROM identity_step_up WHERE user_id = ?1",
        params![target],
    )?;
    Ok(revoked)
}

/// 治理处置的公共收尾：事件构造 + 平台镜像 + 回执。suspend/disable/enable
/// 的事件形状完全一致（状态变更 + 会话撤销 + 缓存清理），只有 action 名不同。
#[allow(clippy::too_many_arguments)]
fn finish_govern(
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
    port: &dyn PlatformGovernancePort,
) -> Result<GovernEffect> {
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
    port.mirror_user_state(target, suspended, disabled, after_until)?;
    port.dispatch(&events);
    Ok(GovernEffect {
        reference: reference(action, seq),
        before,
        after,
        sessions_revoked: sessions_revoked as u64,
        already,
        events,
    })
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

fn prepare(conn: &Connection, operator_token: &str, cmd: &GovernCommand, now: i64) -> Result<Prep> {
    let target = check_target(&cmd.target)?;
    let reason = check_reason(&cmd.reason)?;
    let actor = check_actor(&cmd.actor)?;
    authenticate_operator(conn, operator_token, now)?;
    require_user(conn, &target)?;
    let before_row = load(conn, &target)?;
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

fn close_tx(
    tx: &Connection,
    prep: &Prep,
    action: &str,
    cmd: &GovernCommand,
    sessions_revoked: usize,
    already: bool,
    now: i64,
) -> Result<WriteOutcome> {
    let after_row = load(tx, &prep.target)?;
    let after_state = state_of(&after_row, now);
    let after_until = after_row.as_ref().and_then(|r| r.until_ms);
    let before = summary(prep.before_state, prep.before_until);
    let after = summary(after_state, after_until);
    let seq = append_audit(
        tx,
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
    let prep = prepare(conn, operator_token, cmd, now)?;
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

    let tx = conn.unchecked_transaction()?;
    let mut sessions_revoked = 0usize;
    if !already {
        tx.execute(
            "INSERT INTO identity_user_governance(user_id, suspended, disabled, \
             suspended_until_ms, reason, actor, updated_at) \
             VALUES(?1, 1, COALESCE((SELECT disabled FROM identity_user_governance \
             WHERE user_id = ?1), 0), ?2, ?3, ?4, ?5) \
             ON CONFLICT(user_id) DO UPDATE SET suspended = 1, suspended_until_ms = ?2, \
             reason = ?3, actor = ?4, updated_at = ?5",
            params![prep.target, cmd.until_ms, prep.reason, prep.actor, now],
        )?;
        sessions_revoked = revoke_all_sessions(&tx, &prep.target, now)?;
    }
    let out = close_tx(&tx, &prep, "suspend", cmd, sessions_revoked, already, now)?;
    tx.commit()?;

    // 镜像与事件**幂等命中也重放**：平台侧据此收敛（"后台封了、服务还通"
    // 的残留就是靠重放消掉的）。操作者上下文只用于鉴权，审计落 cmd.actor。
    finish_govern(
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
        port,
    )
}

/// 解封（可逆且本身留痕）。永久/临时封禁都清；`disabled` 是独立处置，不受影响。
pub fn unsuspend(
    conn: &Connection,
    operator_token: &str,
    cmd: &GovernCommand,
    port: &dyn PlatformGovernancePort,
    now: i64,
) -> Result<GovernEffect> {
    let prep = prepare(conn, operator_token, cmd, now)?;
    // 幂等：本来就没在封禁（含已到期）→ already。
    let already = !prep
        .before_row
        .as_ref()
        .map(|r| r.suspended)
        .unwrap_or(false);

    let tx = conn.unchecked_transaction()?;
    if !already {
        tx.execute(
            "UPDATE identity_user_governance SET suspended = 0, suspended_until_ms = NULL, \
             reason = ?2, actor = ?3, updated_at = ?4 WHERE user_id = ?1",
            params![prep.target, prep.reason, prep.actor, now],
        )?;
    }
    let out = close_tx(&tx, &prep, "unsuspend", cmd, 0, already, now)?;
    tx.commit()?;

    finish_govern(
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
        port,
    )
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
    let prep = prepare(conn, operator_token, cmd, now)?;
    let already = prep
        .before_row
        .as_ref()
        .map(|r| r.disabled == disabled)
        .unwrap_or(!disabled);

    let tx = conn.unchecked_transaction()?;
    let mut sessions_revoked = 0usize;
    if !already {
        tx.execute(
            "INSERT INTO identity_user_governance(user_id, suspended, disabled, \
             suspended_until_ms, reason, actor, updated_at) \
             VALUES(?1, COALESCE((SELECT suspended FROM identity_user_governance \
             WHERE user_id = ?1), 0), ?2, (SELECT suspended_until_ms FROM \
             identity_user_governance WHERE user_id = ?1), ?3, ?4, ?5) \
             ON CONFLICT(user_id) DO UPDATE SET disabled = ?2, reason = ?3, actor = ?4, \
             updated_at = ?5",
            params![prep.target, disabled, prep.reason, prep.actor, now],
        )?;
        if disabled {
            sessions_revoked = revoke_all_sessions(&tx, &prep.target, now)?;
        }
    }
    let action = if disabled { "disable" } else { "enable" };
    let out = close_tx(&tx, &prep, action, cmd, sessions_revoked, already, now)?;
    tx.commit()?;

    finish_govern(
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
        port,
    )
}

/// 结算到期的临时封禁（平台定时作业调用；读路径的惰性判定不依赖它）。
/// 返回结算的用户数；每个被结算的用户落一条 `suspend.expire` 审计并镜像平台。
pub fn settle_expired(
    conn: &Connection,
    port: &dyn PlatformGovernancePort,
    now: i64,
) -> Result<usize> {
    let expired: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT user_id FROM identity_user_governance \
             WHERE suspended = 1 AND suspended_until_ms IS NOT NULL AND suspended_until_ms <= ?1",
        )?;
        let rows = stmt.query_map(params![now], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for user_id in &expired {
        let tx = conn.unchecked_transaction()?;
        let n = tx.execute(
            "UPDATE identity_user_governance SET suspended = 0, suspended_until_ms = NULL, \
             updated_at = ?2 WHERE user_id = ?1 AND suspended = 1 \
             AND suspended_until_ms IS NOT NULL AND suspended_until_ms <= ?2",
            params![user_id, now],
        )?;
        if n == 0 {
            continue;
        }
        let after_row = load(&tx, user_id)?;
        let after_state = after_row
            .as_ref()
            .map(|r| r.state(now))
            .unwrap_or(UserState::Active);
        let after_until = after_row.as_ref().and_then(|r| r.until_ms);
        let before = summary(UserState::Suspended, None);
        let after = summary(after_state, after_until);
        append_audit(
            &tx,
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
        )?;
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

/// 给用户打管理标签（幂等：同标签重复打不产生第二行）。
pub fn add_label(
    conn: &Connection,
    operator_token: &str,
    user_id: &str,
    label: &str,
    now: i64,
) -> Result<()> {
    authenticate_operator(conn, operator_token, now)?;
    let target = check_target(user_id)?;
    require_user(conn, &target)?;
    let label = check_label(label, "label")?;
    if label.is_empty() {
        return Err(IdError::BadInput("label"));
    }
    conn.execute(
        "INSERT OR IGNORE INTO identity_governance_label(user_id, label, created_at) \
         VALUES(?1, ?2, ?3)",
        params![target, label, now],
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
    authenticate_operator(conn, operator_token, now)?;
    let target = check_target(user_id)?;
    conn.execute(
        "DELETE FROM identity_governance_label WHERE user_id = ?1 AND label = ?2",
        params![target, label],
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
