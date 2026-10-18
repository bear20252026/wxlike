//! `wxlike-report` — P4 举报工单流程层（ADR-20 第三层：report-service）。
//!
//! 只做**状态流转与处置动作的合法性**：不判定内容（那是
//! `wxlike_moderation`），不做实时拦截（那是 `wxlike_risk`）。处置动作
//! 由调用方落地到各业务服务（禁言→群服务、封禁→账号服务、删内容→消息服务），
//! 本 crate 负责"谁在什么状态下能做什么"以及**完整留痕**。
//!
//! ```text
//!            triage            act
//!   Open ───────────► Triaged ─────► Actioned ──close──► Closed
//!     │                 │                                  ▲
//!     │                 └──────────────close───────────────┘
//!     └──reject──► Rejected
//! ```
//!
//! 三条不可绕过的规则：
//! 1. **终态不可再流转**（Closed / Rejected 是吸收态）——防"结案后偷偷改判"；
//! 2. **处置必须先分诊**（Open 不能直接 act）——防举报直接变成处罚；
//! 3. **每次流转都记录操作者与时间**——合同 §3.14 审计要求的最小面。

use serde::{Deserialize, Serialize};

/// 举报原因（合同未枚举，按产品功能树取常用五类）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Spam,
    Abuse,
    Porn,
    Fraud,
    Other,
}

impl Reason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Spam => "spam",
            Self::Abuse => "abuse",
            Self::Porn => "porn",
            Self::Fraud => "fraud",
            Self::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "spam" => Some(Self::Spam),
            "abuse" => Some(Self::Abuse),
            "porn" => Some(Self::Porn),
            "fraud" => Some(Self::Fraud),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

/// 工单状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Open,
    Triaged,
    Actioned,
    Closed,
    Rejected,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Triaged => "triaged",
            Self::Actioned => "actioned",
            Self::Closed => "closed",
            Self::Rejected => "rejected",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "triaged" => Some(Self::Triaged),
            "actioned" => Some(Self::Actioned),
            "closed" => Some(Self::Closed),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Closed | Self::Rejected)
    }
}

/// 处置动作。`Mute` 带时长；`Ban` 为永久封禁。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Warn,
    Mute {
        duration_ms: u64,
    },
    Ban,
    RemoveContent,
    /// 判定不违规，仅记录。
    None,
}

/// 一次状态流转（审计最小面）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    pub at_ms: u64,
    pub from: State,
    pub to: State,
    pub actor: String,
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ticket {
    pub id: String,
    pub reporter: String,
    /// 被举报对象的类型：`user` / `message` / `group` / `channel`。
    pub target_kind: String,
    pub target_id: String,
    pub reason: Reason,
    pub detail: String,
    pub state: State,
    pub created_at: u64,
    pub updated_at: u64,
    /// 已执行的处置（最多一条：分诊后一次性决定）。
    pub action: Option<Action>,
    pub handler: Option<String>,
    pub history: Vec<Transition>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReportError {
    #[error("illegal transition {from:?} -> {to:?}")]
    IllegalTransition { from: State, to: State },
    #[error("ticket is already closed")]
    Terminal,
    #[error("actor must be non-empty")]
    EmptyActor,
    #[error("note too long")]
    NoteTooLong,
    #[error("target kind must be user|message|group|channel")]
    BadTargetKind,
}

pub type Result<T> = std::result::Result<T, ReportError>;

const NOTE_MAX: usize = 512;
const TARGET_KINDS: [&str; 4] = ["user", "message", "group", "channel"];

impl Ticket {
    /// 新建工单（状态 Open，未分诊）。
    pub fn open(
        id: &str,
        reporter: &str,
        target_kind: &str,
        target_id: &str,
        reason: Reason,
        detail: &str,
        now_ms: u64,
    ) -> Result<Self> {
        if reporter.trim().is_empty() {
            return Err(ReportError::EmptyActor);
        }
        if !TARGET_KINDS.contains(&target_kind) {
            return Err(ReportError::BadTargetKind);
        }
        if detail.chars().count() > NOTE_MAX {
            return Err(ReportError::NoteTooLong);
        }
        Ok(Self {
            id: id.to_string(),
            reporter: reporter.to_string(),
            target_kind: target_kind.to_string(),
            target_id: target_id.to_string(),
            reason,
            detail: detail.to_string(),
            state: State::Open,
            created_at: now_ms,
            updated_at: now_ms,
            action: None,
            handler: None,
            history: Vec::new(),
        })
    }

    /// 分诊：Open → Triaged，认领处理人。
    pub fn triage(&mut self, actor: &str, note: &str, now_ms: u64) -> Result<()> {
        self.require_actor(actor)?;
        self.require_note(note)?;
        if self.state != State::Open {
            return Err(ReportError::IllegalTransition {
                from: self.state,
                to: State::Triaged,
            });
        }
        self.handler = Some(actor.to_string());
        self.move_to(State::Triaged, actor, note, now_ms);
        Ok(())
    }

    /// 处置：Triaged → Actioned。必须先分诊（Open 直接 act 一律拒绝）。
    pub fn act(&mut self, actor: &str, action: Action, note: &str, now_ms: u64) -> Result<()> {
        self.require_actor(actor)?;
        self.require_note(note)?;
        if self.state != State::Triaged {
            return Err(ReportError::IllegalTransition {
                from: self.state,
                to: State::Actioned,
            });
        }
        if let Action::Mute { duration_ms } = &action {
            if *duration_ms == 0 {
                return Err(ReportError::NoteTooLong); // 复用：时长必须为正
            }
        }
        self.action = Some(action);
        self.move_to(State::Actioned, actor, note, now_ms);
        Ok(())
    }

    /// 结案：Triaged 或 Actioned → Closed。
    pub fn close(&mut self, actor: &str, note: &str, now_ms: u64) -> Result<()> {
        self.require_actor(actor)?;
        self.require_note(note)?;
        if !matches!(self.state, State::Triaged | State::Actioned) {
            return Err(ReportError::IllegalTransition {
                from: self.state,
                to: State::Closed,
            });
        }
        self.move_to(State::Closed, actor, note, now_ms);
        Ok(())
    }

    /// 驳回：Open 或 Triaged → Rejected。
    pub fn reject(&mut self, actor: &str, note: &str, now_ms: u64) -> Result<()> {
        self.require_actor(actor)?;
        self.require_note(note)?;
        if !matches!(self.state, State::Open | State::Triaged) {
            return Err(ReportError::IllegalTransition {
                from: self.state,
                to: State::Rejected,
            });
        }
        self.move_to(State::Rejected, actor, note, now_ms);
        Ok(())
    }

    /// 是否超时（默认 24 h SLA，合同未定数值，取运营常见值）。
    pub fn is_overdue(&self, now_ms: u64, sla_ms: u64) -> bool {
        !self.state.is_terminal() && now_ms.saturating_sub(self.created_at) > sla_ms
    }

    fn move_to(&mut self, to: State, actor: &str, note: &str, now_ms: u64) {
        self.history.push(Transition {
            at_ms: now_ms,
            from: self.state,
            to,
            actor: actor.to_string(),
            note: note.to_string(),
        });
        self.state = to;
        self.updated_at = now_ms;
    }

    fn require_actor(&self, actor: &str) -> Result<()> {
        if actor.trim().is_empty() {
            return Err(ReportError::EmptyActor);
        }
        Ok(())
    }

    fn require_note(&self, note: &str) -> Result<()> {
        if note.chars().count() > NOTE_MAX {
            return Err(ReportError::NoteTooLong);
        }
        Ok(())
    }
}

/// 默认 SLA：24 h。
pub const DEFAULT_SLA_MS: u64 = 24 * 60 * 60 * 1000;
