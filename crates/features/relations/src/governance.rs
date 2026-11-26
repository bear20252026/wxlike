//! G02-09 群治理（管理员 audience）：freeze / unfreeze / transfer / dissolve。
//!
//! 对应 `POST /api/admin/identity/groups/{id}/govern`（admin
//! `identity::HttpPort::govern_group`，`action` 即这里的 [`GroupAction`]）。
//!
//! # 与用户自助群操作的边界
//!
//! G02-02 的 `group::*` 是**成员自助**（owner 才能解散/转让）；这里是**管理端
//! 治理**：操作者是后台运营账号，凭 admin audience 令牌进来（audience 校验由
//! identity 域在 HTTP 面统一做，本 crate 收到的已是鉴权后的命令——与 adminx
//! 经 `HttpPort` 转发的形状一致）。治理动作不受"必须是群主"限制，但每一步都
//! 服务端校验目标状态，且全部幂等、全部留痕（`relations_audit`）。
//!
//! # 状态机
//!
//! ```text
//! active --freeze--> frozen --unfreeze--> active
//! active|frozen --dissolve--> dissolved（终态，不可逆）
//! active|frozen --transfer--> active|frozen（换群主，不改冻结/解散态）
//! ```
//!
//! 冻结 = 成员只读（复用 `group_meta.frozen_at`，权限闸门在 `group::given_of_member`）；
//! 解散 = 软删（`group_meta.dissolved_at`），成员行保留供审计。
//!
//! # ADR-28
//!
//! 纯同步域 API（`&Connection` 进、`Result` 出），无 async 体、不新增 tokio。

use rusqlite::{params, Connection, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use crate::exec::{RelExec, RelKind, RelParam, RelValue};
use crate::{audit, group, RelError, Result};

/// 处置理由上限（与 identity 治理域同口径）。
pub const REASON_MAX_CHARS: usize = 512;
/// 目标 id 长度上限。
pub const TARGET_MAX_LEN: usize = 64;

/// 群治理动词（封闭集合，与 admin `g14.rs` 的校验逐字一致）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupAction {
    Freeze,
    Unfreeze,
    Transfer,
    Dissolve,
}

impl GroupAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Freeze => "freeze",
            Self::Unfreeze => "unfreeze",
            Self::Transfer => "transfer",
            Self::Dissolve => "dissolve",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "freeze" => Some(Self::Freeze),
            "unfreeze" => Some(Self::Unfreeze),
            "transfer" => Some(Self::Transfer),
            "dissolve" => Some(Self::Dissolve),
            _ => None,
        }
    }
}

/// 一次群治理命令（identity `GovernCommand` 的群侧投影）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupGovernCommand {
    pub group_id: String,
    pub action: GroupAction,
    pub reason: String,
    pub actor: String,
    pub trace: String,
    /// 仅 `transfer` 用：新群主 user_id。
    pub new_owner: Option<String>,
}

/// 群治理事件（平台进程消费：刷新群成员缓存、通知成员）。全是元数据。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GroupGovernanceEvent {
    GroupStateChanged {
        group_id: String,
        state: String,
    },
    GroupTransferred {
        group_id: String,
        old_owner: String,
        new_owner: String,
    },
    GroupDissolved {
        group_id: String,
        /// 需要通知的成员（含原群主）。
        members: Vec<String>,
    },
    CacheClear {
        scope: String,
        key: String,
    },
}

/// 群治理回执（与 identity `GovernEffect` / admin `Effect` 同形）。
#[derive(Clone, Debug, Serialize)]
pub struct GroupGovernEffect {
    pub reference: String,
    pub before: serde_json::Value,
    pub after: serde_json::Value,
    pub sessions_revoked: u64,
    pub already: bool,
    #[serde(skip)]
    pub events: Vec<GroupGovernanceEvent>,
}

/// 治理域建表 + group_meta 补列（幂等）。由 [`crate::migrate`] 调用。
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(GOV_SCHEMA)?;
    ensure_group_meta_frozen(conn)
}

/// 老库补 `frozen_at` 列（存在才补；与 `ensure_friends_starred` 同手法）。
fn ensure_group_meta_frozen(conn: &Connection) -> Result<()> {
    let mut have = Vec::new();
    {
        let mut stmt = conn.prepare("PRAGMA table_info(group_meta)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        for name in rows {
            have.push(name?);
        }
    }
    if !have.iter().any(|name| name == "frozen_at") {
        conn.execute_batch("ALTER TABLE group_meta ADD COLUMN frozen_at INTEGER;")?;
    }
    Ok(())
}

fn check_target(raw: &str) -> Result<String> {
    let t = raw.trim();
    if t.is_empty() || t.len() > TARGET_MAX_LEN {
        return Err(RelError::BadInput("target id"));
    }
    if !t
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(RelError::BadInput("target id"));
    }
    Ok(t.to_string())
}

fn check_reason(raw: &str) -> Result<String> {
    let t = raw.trim();
    if t.is_empty() {
        return Err(RelError::BadInput("reason required"));
    }
    if t.chars().count() > REASON_MAX_CHARS {
        return Err(RelError::BadInput("reason too long"));
    }
    Ok(t.to_string())
}

fn check_actor(raw: &str) -> Result<String> {
    let t = raw.trim();
    if t.is_empty() || t.chars().count() > TARGET_MAX_LEN {
        return Err(RelError::BadInput("actor"));
    }
    Ok(t.to_string())
}

/// 群当前状态摘要（元数据）。
fn state_of(g: &group::Group) -> &'static str {
    if g.dissolved {
        "dissolved"
    } else if g.frozen {
        "frozen"
    } else {
        "active"
    }
}

fn summary(g: &group::Group) -> serde_json::Value {
    serde_json::json!({
        "group_id": g.id,
        "state": state_of(g),
        "owner_uid": g.owner_uid,
    })
}

/// `groups LEFT JOIN group_meta` 的原始行（按 SELECT 列序展开）。
type GroupRawRow = (
    String,         // g.id
    String,         // g.name
    String,         // g.owner_uid
    i64,            // g.created_at
    Option<String>, // m.visibility
    Option<i64>,    // m.dissolved_at
    Option<i64>,    // m.frozen_at
);

/// 读群（含已解散/已冻结；治理要能对已冻结的群解冻）。
fn load(conn: &Connection, group_id: &str) -> Result<group::Group> {
    load_with(conn, group_id)
}

/// 端口版读群（含已解散/已冻结；治理要能对已冻结的群解冻）。
fn load_with<E: RelExec + ?Sized>(exec: &E, group_id: &str) -> Result<group::Group> {
    let kinds = [
        RelKind::Text,
        RelKind::Text,
        RelKind::Text,
        RelKind::Int,
        RelKind::Text,
        RelKind::Int,
        RelKind::Int,
    ];
    let sql = "SELECT g.id, g.name, g.owner_uid, g.created_at, m.visibility, m.dissolved_at, \
         m.frozen_at FROM groups g LEFT JOIN group_meta m ON m.group_id = g.id WHERE g.id = ?1";
    let raw = exec.query(sql, &[RelParam::Text(group_id.to_string())], &kinds)?;
    let Some(mut raw) = raw else {
        return Err(RelError::NotFound("group"));
    };
    let id = take_text(&mut raw)?;
    let name = take_text(&mut raw)?;
    let owner_uid = take_text(&mut raw)?;
    let created_at = take_int(&mut raw)?;
    let visibility = take_opt_text(&mut raw)?;
    let dissolved_at = take_opt_int(&mut raw)?;
    let frozen_at = take_opt_int(&mut raw)?;
    let visibility = match visibility.as_deref() {
        None => group::Visibility::Public,
        Some(v) => group::Visibility::parse(v).ok_or(RelError::Corrupt("group_meta.visibility"))?,
    };
    Ok(group::Group {
        id,
        name,
        owner_uid,
        visibility,
        created_at,
        dissolved: dissolved_at.is_some(),
        frozen: frozen_at.is_some(),
    })
}

fn take_text(values: &mut Vec<RelValue>) -> Result<String> {
    if values.is_empty() {
        return Err(RelError::Corrupt("governance row width"));
    }
    values.remove(0).into_text()
}

fn take_int(values: &mut Vec<RelValue>) -> Result<i64> {
    if values.is_empty() {
        return Err(RelError::Corrupt("governance row width"));
    }
    values.remove(0).into_int()
}

fn take_opt_text(values: &mut Vec<RelValue>) -> Result<Option<String>> {
    if values.is_empty() {
        return Err(RelError::Corrupt("governance row width"));
    }
    values.remove(0).opt_text()
}

fn take_opt_int(values: &mut Vec<RelValue>) -> Result<Option<i64>> {
    if values.is_empty() {
        return Err(RelError::Corrupt("governance row width"));
    }
    values.remove(0).opt_int()
}

/// 群治理入口。audience 鉴权由 identity 域在 HTTP 面完成（本函数收到的是已鉴权
/// 命令）；这里做目标/理由/状态校验、幂等写入、留痕与事件产出。
pub fn govern(conn: &Connection, cmd: &GroupGovernCommand, now: i64) -> Result<GroupGovernEffect> {
    let group_id = check_target(&cmd.group_id)?;
    let reason = check_reason(&cmd.reason)?;
    let actor = check_actor(&cmd.actor)?;
    let before_group = load(conn, &group_id)?;
    let before = summary(&before_group);

    let (after_group, already, mut events) = match cmd.action {
        GroupAction::Freeze => freeze(conn, &before_group, &actor, now)?,
        GroupAction::Unfreeze => unfreeze(conn, &before_group, &actor)?,
        GroupAction::Transfer => {
            let new_owner = cmd
                .new_owner
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or(RelError::BadInput("transfer requires new_owner"))?;
            let new_owner = check_target(new_owner)?;
            transfer(conn, &before_group, &new_owner, &actor, now)?
        }
        GroupAction::Dissolve => dissolve(conn, &before_group, &actor, now)?,
    };
    let after = summary(&after_group);

    // 留痕：每个动作（含幂等命中）都落 relations_audit，kind 带治理前缀。
    let kind = format!("group.govern.{}", cmd.action.as_str());
    audit(conn, &actor, &group_id, &kind, now)?;
    // 治理理由单独落一条可复核记录（relations_audit 只有 kind，理由进 detail 表）。
    let seq = append_reason(
        conn,
        &group_id,
        cmd.action.as_str(),
        &actor,
        &reason,
        &cmd.trace,
        now,
    )?;

    events.push(GroupGovernanceEvent::CacheClear {
        scope: "group".to_string(),
        key: group_id.clone(),
    });
    let reference = format!("gov:group.{}:{seq}", cmd.action.as_str());
    Ok(GroupGovernEffect {
        reference,
        before,
        after,
        sessions_revoked: 0,
        already,
        events,
    })
}

fn freeze(
    conn: &Connection,
    g: &group::Group,
    actor: &str,
    now: i64,
) -> Result<(group::Group, bool, Vec<GroupGovernanceEvent>)> {
    if g.dissolved {
        return Err(RelError::Conflict("group dissolved"));
    }
    let already = g.frozen;
    if !already {
        upsert_meta(conn, g, Some(now), None, actor)?;
    }
    let after = load(conn, &g.id)?;
    let events = vec![GroupGovernanceEvent::GroupStateChanged {
        group_id: g.id.clone(),
        state: state_of(&after).to_string(),
    }];
    Ok((after, already, events))
}

fn unfreeze(
    conn: &Connection,
    g: &group::Group,
    actor: &str,
) -> Result<(group::Group, bool, Vec<GroupGovernanceEvent>)> {
    if g.dissolved {
        return Err(RelError::Conflict("group dissolved"));
    }
    let already = !g.frozen;
    if !already {
        upsert_meta(conn, g, None, None, actor)?;
    }
    let after = load(conn, &g.id)?;
    let events = vec![GroupGovernanceEvent::GroupStateChanged {
        group_id: g.id.clone(),
        state: state_of(&after).to_string(),
    }];
    Ok((after, already, events))
}

fn transfer(
    conn: &Connection,
    g: &group::Group,
    new_owner: &str,
    _actor: &str,
    now: i64,
) -> Result<(group::Group, bool, Vec<GroupGovernanceEvent>)> {
    if g.dissolved {
        return Err(RelError::Conflict("group dissolved"));
    }
    if new_owner == g.owner_uid {
        // 幂等：已经是这个群主。
        let after = load(conn, &g.id)?;
        return Ok((after, true, Vec::new()));
    }
    // 新群主必须是现有成员（治理不凭空造成员；先拉人再转让）。
    if group::role_of(conn, &g.id, new_owner)?.is_none() {
        return Err(RelError::NotFound("member"));
    }
    let old_owner = g.owner_uid.clone();
    let tx = conn.unchecked_transaction()?;
    // 新群主升 owner，原群主降 admin（与 group::set_role 的转让同口径）。
    tx.execute(
        "UPDATE group_members SET role = 'owner' WHERE group_id = ?1 AND user_id = ?2",
        params![g.id, new_owner],
    )?;
    tx.execute(
        "UPDATE group_members SET role = 'admin' WHERE group_id = ?1 AND user_id = ?2",
        params![g.id, old_owner],
    )?;
    tx.execute(
        "UPDATE groups SET owner_uid = ?2 WHERE id = ?1",
        params![g.id, new_owner],
    )?;
    // 位行同步：新群主给 O，原群主收回 O（避免两个 owner 的位残留）。
    // 只改已存在的显式位行；没有显式行的成员走角色缺省位，角色已更新即可。
    set_owner_bit(&tx, &g.id, new_owner, true, now)?;
    set_owner_bit(&tx, &g.id, &old_owner, false, now)?;
    tx.commit()?;
    let after = load(conn, &g.id)?;
    let events = vec![GroupGovernanceEvent::GroupTransferred {
        group_id: g.id.clone(),
        old_owner,
        new_owner: new_owner.to_string(),
    }];
    Ok((after, false, events))
}

fn dissolve(
    conn: &Connection,
    g: &group::Group,
    actor: &str,
    now: i64,
) -> Result<(group::Group, bool, Vec<GroupGovernanceEvent>)> {
    let already = g.dissolved;
    let members = group::members(conn, &g.id)?;
    if !already {
        upsert_meta(conn, g, g.frozen.then_some(now), Some(now), actor)?;
    }
    let after = load(conn, &g.id)?;
    let events = vec![GroupGovernanceEvent::GroupDissolved {
        group_id: g.id.clone(),
        members,
    }];
    Ok((after, already, events))
}

/// 增/删某成员显式位行里的 `O` 位（只动已存在的行；无行 = 走角色缺省，不造行）。
fn set_owner_bit(
    conn: &Connection,
    group_id: &str,
    user: &str,
    give: bool,
    now: i64,
) -> Result<()> {
    let given_raw: Option<String> = conn
        .query_row(
            "SELECT mode_given FROM group_member_acs WHERE group_id = ?1 AND user_id = ?2",
            params![group_id, user],
            |r| r.get(0),
        )
        .optional()?;
    let Some(given_raw) = given_raw else {
        return Ok(());
    };
    let mut given = crate::port::Acs::parse(&given_raw)
        .map_err(|_| RelError::Corrupt("group_member_acs.mode_given"))?;
    if given.is_defined() {
        given = if give {
            given.with(crate::port::Acs::OWNER)
        } else {
            given.without(crate::port::Acs::OWNER)
        };
        conn.execute(
            "UPDATE group_member_acs SET mode_given = ?3, updated_at = ?4 \
             WHERE group_id = ?1 AND user_id = ?2",
            params![group_id, user, given.masked().to_flags(), now],
        )?;
    }
    Ok(())
}

/// 写 group_meta 的冻结/解散标记（保留可见性；幂等 upsert）。
/// `frozen_at`：`Some(t)` 置冻结、`None` 清冻结；`dissolved_at` 同理。
fn upsert_meta(
    conn: &Connection,
    g: &group::Group,
    frozen_at: Option<i64>,
    dissolved_at: Option<i64>,
    actor: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO group_meta(group_id, visibility, dissolved_at, dissolved_by, frozen_at) \
         VALUES(?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(group_id) DO UPDATE SET dissolved_at = ?3, \
         dissolved_by = CASE WHEN ?3 IS NOT NULL THEN ?4 ELSE dissolved_by END, frozen_at = ?5",
        params![g.id, g.visibility.as_str(), dissolved_at, actor, frozen_at],
    )?;
    Ok(())
}

/// 治理理由留痕（可复核"凭什么"）。独立小表，append-only；返回自增 seq。
fn append_reason(
    conn: &Connection,
    group_id: &str,
    action: &str,
    actor: &str,
    reason: &str,
    trace: &str,
    now: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO group_governance_audit(group_id, action, actor, reason, trace, at) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![group_id, action, actor, reason, trace, now],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 治理域自有审计表（理由/trace 可复核；与 relations_audit 的 kind 行互补）。
const GOV_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS group_governance_audit (
    seq      INTEGER PRIMARY KEY AUTOINCREMENT,
    group_id TEXT NOT NULL,
    action   TEXT NOT NULL,
    actor    TEXT NOT NULL,
    reason   TEXT NOT NULL,
    trace    TEXT NOT NULL DEFAULT '',
    at       INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_group_gov_audit_group
    ON group_governance_audit(group_id, seq);
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::db;

    fn seed(conn: &Connection) -> String {
        let g = group::create(
            conn,
            "u1",
            "研发群",
            &["u2".to_string(), "u3".to_string()],
            group::Visibility::Public,
            1_000,
        )
        .unwrap();
        g.id
    }

    fn cmd(group_id: &str, action: GroupAction) -> GroupGovernCommand {
        GroupGovernCommand {
            group_id: group_id.into(),
            action,
            reason: "群体违规".into(),
            actor: "ops".into(),
            trace: "t1".into(),
            new_owner: None,
        }
    }

    #[test]
    fn action_parse_is_closed_set() {
        assert_eq!(GroupAction::parse("freeze"), Some(GroupAction::Freeze));
        assert_eq!(GroupAction::parse("dissolve"), Some(GroupAction::Dissolve));
        assert_eq!(GroupAction::parse("nuke"), None);
    }

    #[test]
    fn freeze_then_unfreeze_roundtrip_and_idempotent() {
        let conn = db();
        migrate(&conn).unwrap();
        let id = seed(&conn);
        let e = govern(&conn, &cmd(&id, GroupAction::Freeze), 1_100).unwrap();
        assert!(!e.already);
        assert_eq!(e.after["state"], "frozen");
        // 冻结后成员只读。
        let g = load(&conn, &id).unwrap();
        let acs = group::member_acs(&conn, &id, "u2", &g).unwrap();
        assert!(acs.is_reader() && !acs.is_writer());
        // 幂等：再冻一次 already。
        let again = govern(&conn, &cmd(&id, GroupAction::Freeze), 1_200).unwrap();
        assert!(again.already);
        // 解冻恢复。
        let un = govern(&conn, &cmd(&id, GroupAction::Unfreeze), 1_300).unwrap();
        assert!(!un.already);
        assert_eq!(un.after["state"], "active");
        let g = load(&conn, &id).unwrap();
        assert!(group::member_acs(&conn, &id, "u2", &g).unwrap().is_writer());
        // 幂等：再解一次 already。
        assert!(
            govern(&conn, &cmd(&id, GroupAction::Unfreeze), 1_400)
                .unwrap()
                .already
        );
    }

    #[test]
    fn transfer_changes_owner_and_is_idempotent() {
        let conn = db();
        migrate(&conn).unwrap();
        let id = seed(&conn);
        let mut c = cmd(&id, GroupAction::Transfer);
        c.new_owner = Some("u2".into());
        let e = govern(&conn, &c, 1_100).unwrap();
        assert!(!e.already);
        assert_eq!(e.after["owner_uid"], "u2");
        assert_eq!(
            group::role_of(&conn, &id, "u2").unwrap(),
            Some(group::Role::Owner)
        );
        assert_eq!(
            group::role_of(&conn, &id, "u1").unwrap(),
            Some(group::Role::Admin)
        );
        // 幂等：转给同一个群主 already。
        assert!(govern(&conn, &c, 1_200).unwrap().already);
    }

    #[test]
    fn transfer_requires_member_and_new_owner() {
        let conn = db();
        migrate(&conn).unwrap();
        let id = seed(&conn);
        let mut c = cmd(&id, GroupAction::Transfer);
        assert!(matches!(
            govern(&conn, &c, 1_100),
            Err(RelError::BadInput(_))
        ));
        c.new_owner = Some("u9".into()); // 非成员
        assert!(matches!(
            govern(&conn, &c, 1_100),
            Err(RelError::NotFound(_))
        ));
    }

    #[test]
    fn dissolve_is_terminal_and_idempotent() {
        let conn = db();
        migrate(&conn).unwrap();
        let id = seed(&conn);
        let e = govern(&conn, &cmd(&id, GroupAction::Dissolve), 1_100).unwrap();
        assert!(!e.already);
        assert_eq!(e.after["state"], "dissolved");
        assert!(
            govern(&conn, &cmd(&id, GroupAction::Dissolve), 1_200)
                .unwrap()
                .already
        );
        // 解散后不能再冻结/解冻/转让。
        assert!(matches!(
            govern(&conn, &cmd(&id, GroupAction::Freeze), 1_300),
            Err(RelError::Conflict(_))
        ));
        assert!(matches!(
            govern(&conn, &cmd(&id, GroupAction::Unfreeze), 1_300),
            Err(RelError::Conflict(_))
        ));
    }

    #[test]
    fn reason_is_required_and_audited() {
        let conn = db();
        migrate(&conn).unwrap();
        let id = seed(&conn);
        let mut c = cmd(&id, GroupAction::Freeze);
        c.reason = "  ".into();
        assert!(matches!(
            govern(&conn, &c, 1_100),
            Err(RelError::BadInput(_))
        ));
        c.reason = "群体违规".into();
        govern(&conn, &c, 1_100).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM group_governance_audit WHERE group_id = ?1 AND reason = '群体违规'",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn unknown_group_is_not_found() {
        let conn = db();
        migrate(&conn).unwrap();
        assert!(matches!(
            govern(&conn, &cmd("missing", GroupAction::Freeze), 1_100),
            Err(RelError::NotFound(_))
        ));
    }
}
