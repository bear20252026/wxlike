//! 后台的准入模型：**操作目录 + 角色能力 + MFA 门**（D3）。
//!
//! 形状借用本仓库自己的 `crates/cmd::Policy`（同语言同问题，不是外部代码）：
//! 1. 默认拒绝——目录里没有的操作一律不存在；
//! 2. 能力分级 `Read < Write < Destructive`，角色按级授予，不写在 handler 里；
//! 3. **拒绝要可读**——消息里带"该用哪个角色 / 该开哪个开关"，运维看完能自己纠正。
//!
//! 与 `cmd::Policy` 的唯一新增维度是 MFA：
//! **任何高于 Read 的操作都必须由"已过 TOTP"的会话发起**。
//! 这条闸门在 [`authorize`] 一处生效（`adminx` 的写面只调它一次），不散落各 handler——
//! 散落必漏。
//!
//! 没做双人复核（maker-checker）：ADR-15 把它和 MFA 并列为敏感操作的可选加固，
//! 但它需要一张待复核队列与第二个确认人，属于独立一张卡，见 `docs/adminx.md`
//! 的"没做"清单。这里把 `Destructive` 单独分级的意义就是为它留好接法。

use std::fmt;

use serde::{Deserialize, Serialize};
use wxlike_report::Action;

/// 操作危险等级——**直接复用 `wxlike_cmd::Capability`**，不在这里定义第二份。
///
/// 为什么这件事重要：权限模型有两个定义时，"什么算破坏性"可以给出两个答案，
/// 而且编译器不会拦。这里只做 `pub use`，后台与 agent 侧永远看到同一组等级。
pub use wxlike_cmd::Capability;

/// 后台角色。能力是**累积**的：高级角色自动能做低级的所有事。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// 看板 / 检索 / 工单只读。审计员与值班同学。
    Viewer,
    /// 可处置工单与放行挂起内容，但不能改全局开关、不能处罚。
    Moderator,
    /// 全量：处罚（含 `ban`）、审核模式切换。
    Superadmin,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Moderator => "moderator",
            Role::Superadmin => "superadmin",
        }
    }

    /// 解析角色名。未知值返回 `None`，由调用方报错——不许默默当 viewer。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "viewer" => Some(Role::Viewer),
            "moderator" => Some(Role::Moderator),
            "superadmin" => Some(Role::Superadmin),
            _ => None,
        }
    }

    /// 能做出这个级别操作的最小角色。拒绝消息要用它来指路——
    /// "需要 destructive 权限"对运维没用，"用 --role superadmin 建号"才有用。
    pub fn minimal_for(need: Capability) -> Self {
        match need {
            Capability::Read => Role::Viewer,
            Capability::Write => Role::Moderator,
            Capability::Destructive => Role::Superadmin,
        }
    }

    pub fn allows(self, need: Capability) -> bool {
        match self {
            Role::Viewer => matches!(need, Capability::Read),
            Role::Moderator => matches!(need, Capability::Read | Capability::Write),
            Role::Superadmin => true,
        }
    }
}

/// HTTP 方法（转发用）。只有这三种，与平台管理端点的实际形状一致。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Post,
    Put,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Post => "POST",
            Method::Put => "PUT",
        }
    }
}

/// 操作的载荷形状。写成枚举而不是 `serde_json::Value`：
/// 目录里每条操作的参数都是编译期确定的，非法参数在进目录之前就该被 serde 拒掉。
#[derive(Clone, Debug)]
pub enum OpPayload {
    Empty,
    Note { note: String },
    Act { action: Action, note: String },
    Mode { mode: ModerationMode },
}

/// 审核模式。值与平台 `wxlike_moderation::Mode` 的 serde 表示逐字对齐。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModerationMode {
    PreReview,
    PostReview,
}

impl ModerationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ModerationMode::PreReview => "pre_review",
            ModerationMode::PostReview => "post_review",
        }
    }
}

/// 目录里的一条操作。
#[derive(Clone, Copy, Debug)]
pub struct Op {
    pub id: &'static str,
    pub capability: Capability,
    pub method: Method,
    /// 平台端点路径模板，`{id}` 会被换成目标对象 id。
    pub path: &'static str,
    /// 是否需要路径里的目标 id。
    pub needs_target: bool,
    pub needs_note: bool,
    pub payload: &'static str,
    pub description: &'static str,
}

/// **唯一目录**。加一条操作 = 在这里加一行 + 在 handler 里接一个分支。
pub const OPS: &[Op] = &[
    Op {
        id: "report.triage",
        capability: Capability::Write,
        method: Method::Post,
        path: "/api/admin/reports/{id}/triage",
        needs_target: true,
        needs_note: true,
        payload: "note",
        description: "认领工单：open → triaged",
    },
    Op {
        id: "report.act",
        capability: Capability::Destructive,
        method: Method::Post,
        path: "/api/admin/reports/{id}/act",
        needs_target: true,
        needs_note: true,
        payload: "action+note",
        description: "执行处置（warn/mute/ban/remove_content/none）——先分诊后处置",
    },
    Op {
        id: "report.close",
        capability: Capability::Write,
        method: Method::Post,
        path: "/api/admin/reports/{id}/close",
        needs_target: true,
        needs_note: true,
        payload: "note",
        description: "结案：triaged|actioned → closed",
    },
    Op {
        id: "report.reject",
        capability: Capability::Write,
        method: Method::Post,
        path: "/api/admin/reports/{id}/reject",
        needs_target: true,
        needs_note: true,
        payload: "note",
        description: "驳回：open|triaged → rejected",
    },
    Op {
        id: "moderation.release",
        capability: Capability::Write,
        method: Method::Post,
        path: "/api/admin/moderation/{id}/release",
        needs_target: true,
        needs_note: false,
        payload: "empty",
        description: "放行挂起内容（平台会返回原文供投递）",
    },
    Op {
        id: "moderation.reject",
        capability: Capability::Destructive,
        method: Method::Post,
        path: "/api/admin/moderation/{id}/reject",
        needs_target: true,
        needs_note: false,
        payload: "empty",
        description: "丢弃挂起内容（不可恢复）",
    },
    Op {
        id: "moderation.mode",
        capability: Capability::Destructive,
        method: Method::Put,
        path: "/api/admin/moderation/mode",
        needs_target: false,
        needs_note: false,
        payload: "mode",
        description: "全站切换先审后发/先发后审（影响所有发送方）",
    },
    Op {
        id: "wallet.reconcile",
        capability: Capability::Write,
        method: Method::Post,
        path: "/api/admin/reconcile",
        needs_target: false,
        needs_note: false,
        payload: "empty",
        description: "跑一次全量对账（只核对，不改余额）",
    },
];

pub fn get(id: &str) -> Option<&'static Op> {
    OPS.iter().find(|o| o.id == id)
}

/// 拒绝原因。每条都带足够的信息让运维自己修。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Deny {
    /// 目录里没有这个操作（拼错或版本不匹配）。
    UnknownOp(String),
    /// 角色能力不足。
    RoleLacks {
        op: String,
        role: &'static str,
        need: Capability,
    },
    /// D3 的硬门：没过 MFA 就没有任何写。
    NeedsMfa { op: String, need: Capability },
    /// 缺目标 id（report/moderation 类操作必须指名对象）。
    MissingTarget(String),
    /// 缺备注（审计追责要能说清"为什么这么判"）。
    MissingNote(String),
}

impl fmt::Display for Deny {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Deny::UnknownOp(id) => write!(
                f,
                "未知操作 {id:?}（可用 `GET /api/ops` 看完整目录，共 {} 条）",
                OPS.len()
            ),
            Deny::RoleLacks { op, role, need } => {
                let (need_str, fix_role) = (need.as_str(), Role::minimal_for(*need).as_str());
                write!(
                    f,
                    "{op} 需要 {need_str} 级权限，当前角色 {role} 没有（让 superadmin 执行：\
                     wxlike-admin account add --name <新账号> --role {fix_role}）"
                )
            }
            Deny::NeedsMfa { op, need } => write!(
                f,
                "{op} 是 {need} 级操作，必须用**已过 TOTP** 的会话执行。当前会话没验过码：\
                 登录时带上 6 位动态码（`POST /api/login` 的 `totp` 字段），\
                 没绑过就先 `POST /api/me/totp/enroll` 再 `POST /api/me/totp/confirm`",
                need = need.as_str()
            ),
            Deny::MissingTarget(op) => write!(f, "{op} 必须指名对象：路径里带 /{{id}}"),
            Deny::MissingNote(op) => {
                write!(f, "{op} 必须写 note（合同 §3.14：处置结论要能说清依据）")
            }
        }
    }
}

impl std::error::Error for Deny {}

/// 一次写操作的准入判定。**顺序是刻意的**：先报"操作不存在"再报"没权限"，
/// 避免用错误消息探测目录（与 `cmd::Policy::authorize` 同一考量）。
pub fn authorize(
    op_id: &str,
    role: Role,
    mfa: bool,
    has_target: bool,
) -> Result<&'static Op, Deny> {
    let op = get(op_id).ok_or_else(|| Deny::UnknownOp(op_id.to_string()))?;
    if !role.allows(op.capability) {
        return Err(Deny::RoleLacks {
            op: op.id.to_string(),
            role: role.as_str(),
            need: op.capability,
        });
    }
    // MFA 门只管高于 Read 的操作。`NeedsMfa` 放在角色之后：
    // 角色不够时先说角色，因为换会话也解决不了权限问题。
    if op.capability > Capability::Read && !mfa {
        return Err(Deny::NeedsMfa {
            op: op.id.to_string(),
            need: op.capability,
        });
    }
    if op.needs_target && !has_target {
        return Err(Deny::MissingTarget(op.id.to_string()));
    }
    Ok(op)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_are_unique_and_paths_are_platform_endpoints() {
        let mut seen = std::collections::HashSet::new();
        for op in OPS {
            assert!(seen.insert(op.id), "操作 id 重复: {}", op.id);
            assert!(op.path.starts_with("/api/admin/"), "{}", op.path);
            assert!(!op.description.is_empty());
            if op.needs_target {
                assert!(op.path.contains("{id}"), "{} 需要 {{id}}", op.id);
            }
        }
    }

    #[test]
    fn viewer_can_only_read() {
        assert!(Role::Viewer.allows(Capability::Read));
        assert!(!Role::Viewer.allows(Capability::Write));
        assert!(!Role::Viewer.allows(Capability::Destructive));
        assert!(Role::Moderator.allows(Capability::Write));
        assert!(!Role::Moderator.allows(Capability::Destructive));
        assert!(Role::Superadmin.allows(Capability::Destructive));
        // 能力是有序的：Destructive 必然也是 Write，不存在"能 ban 不能 triage"
        assert!(Capability::Read < Capability::Write);
        assert!(Capability::Write < Capability::Destructive);
    }

    #[test]
    fn unknown_op_is_reported_before_permissions() {
        let err = authorize("report.not_in_catalog", Role::Superadmin, true, true).unwrap_err();
        assert!(matches!(err, Deny::UnknownOp(_)), "{err:?}");
        // 消息里要给出"怎么列目录"
        assert!(err.to_string().contains("/api/ops"));
    }

    #[test]
    fn role_denial_names_the_role_and_the_missing_level() {
        let err = authorize("report.triage", Role::Viewer, true, true).unwrap_err();
        assert!(matches!(err, Deny::RoleLacks { .. }), "{err:?}");
        let msg = err.to_string();
        assert!(msg.contains("viewer") && msg.contains("write"), "{msg}");
        // 角色不够时不该把原因说成 MFA（换会话也解决不了权限）
        assert!(!msg.contains("TOTP"), "{msg}");
    }

    /// 指路必须给**角色名**，不是能力名：`--role destructive` 是一条建不出号的命令。
    #[test]
    fn role_denial_points_at_the_role_that_actually_grants_it() {
        for (op_id, want_role) in [
            ("report.triage", "moderator"),
            ("report.act", "superadmin"),
            ("moderation.mode", "superadmin"),
        ] {
            let need = get(op_id).unwrap().capability;
            let err = authorize(op_id, Role::Viewer, true, true).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains(&format!("--role {want_role}")),
                "{op_id}: {msg}"
            );
            assert!(
                !msg.contains(&format!("--role {}", need.as_str())),
                "{op_id}: 不该把能力名当角色名: {msg}"
            );
            assert_eq!(Role::minimal_for(need).as_str(), want_role);
        }
    }

    #[test]
    fn every_write_needs_mfa_even_for_superadmin() {
        // D3 验收面：没有 MFA 的管理员不能执行任何写操作
        for op in OPS {
            let target = op.needs_target;
            let err = authorize(op.id, Role::Superadmin, false, target).unwrap_err();
            assert!(
                matches!(err, Deny::NeedsMfa { .. }),
                "{} 在无 MFA 会话下必须被拒，实际 {err:?}",
                op.id
            );
            assert!(err.to_string().contains("totp"), "{err}");
            // 带上 MFA 就放行（角色已是 superadmin）
            assert!(authorize(op.id, Role::Superadmin, true, target).is_ok());
        }
    }

    #[test]
    fn moderator_can_triage_but_not_punish_or_switch_mode() {
        assert!(authorize("report.triage", Role::Moderator, true, true).is_ok());
        assert!(authorize("report.close", Role::Moderator, true, true).is_ok());
        assert!(authorize("wallet.reconcile", Role::Moderator, true, false).is_ok());
        for op in ["report.act", "moderation.reject", "moderation.mode"] {
            let err = authorize(op, Role::Moderator, true, true).unwrap_err();
            assert!(matches!(err, Deny::RoleLacks { .. }), "{op}: {err:?}");
        }
    }

    #[test]
    fn target_is_mandatory_for_object_scoped_ops() {
        let err = authorize("report.triage", Role::Moderator, true, false).unwrap_err();
        assert!(matches!(err, Deny::MissingTarget(_)), "{err:?}");
        assert!(err.to_string().contains("{id}"), "{err}");
        // 全局操作不需要目标
        assert!(authorize("wallet.reconcile", Role::Moderator, true, false).is_ok());
    }

    #[test]
    fn role_and_mode_parse_reject_unknown_words() {
        assert_eq!(Role::parse("  SuperAdmin "), Some(Role::Superadmin));
        assert_eq!(Role::parse("root"), None);
        assert_eq!(Role::parse("admin"), None, "平台里有群 admin 角色，别混");
        // 模式值必须与平台 serde 一致
        assert_eq!(
            serde_json::to_string(&ModerationMode::PreReview).unwrap(),
            "\"pre_review\""
        );
        assert_eq!(
            serde_json::to_string(&ModerationMode::PostReview).unwrap(),
            "\"post_review\""
        );
        assert!(serde_json::from_str::<ModerationMode>("\"pre-review\"").is_err());
    }
}
