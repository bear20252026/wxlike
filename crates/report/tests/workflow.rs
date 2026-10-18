//! 举报工单状态机契约：合法路径、非法路径、留痕、SLA。

use wxlike_report::{Action, Reason, ReportError, State, Ticket, DEFAULT_SLA_MS};

const T0: u64 = 1_700_000_000_000;

fn ticket() -> Ticket {
    Ticket::open("t1", "alice", "message", "m1", Reason::Spam, "刷屏", T0).unwrap()
}

#[test]
fn happy_path_open_triage_action_close() {
    let mut t = ticket();
    t.triage("mod1", "受理", T0 + 1).unwrap();
    t.act("mod1", Action::RemoveContent, "确认违规，删除", T0 + 2)
        .unwrap();
    t.close("mod1", "已通知举报人", T0 + 3).unwrap();

    assert_eq!(t.state, State::Closed);
    assert_eq!(t.handler.as_deref(), Some("mod1"));
    assert_eq!(t.action, Some(Action::RemoveContent));
    assert_eq!(t.history.len(), 3);
    assert_eq!(t.history[0].from, State::Open);
    assert_eq!(t.history[0].to, State::Triaged);
    assert_eq!(t.updated_at, T0 + 3);
}

#[test]
fn cannot_act_before_triage() {
    let mut t = ticket();
    let err = t.act("mod1", Action::Warn, "直接处罚", T0 + 1).unwrap_err();
    assert_eq!(
        err,
        ReportError::IllegalTransition {
            from: State::Open,
            to: State::Actioned
        }
    );
    // 未发生任何流转
    assert_eq!(t.state, State::Open);
    assert!(t.history.is_empty());
    assert_eq!(t.action, None);
}

#[test]
fn terminal_states_are_absorbing() {
    let mut t = ticket();
    t.reject("mod1", "举报不成立", T0 + 1).unwrap();
    assert_eq!(t.state, State::Rejected);

    for attempt in [
        t.triage("mod1", "重开", T0 + 2),
        t.act("mod1", Action::Ban, "重判", T0 + 2),
        t.close("mod1", "结案", T0 + 2),
        t.reject("mod1", "再驳", T0 + 2),
    ] {
        assert!(attempt.is_err(), "终态必须拒绝一切流转");
    }
    assert_eq!(t.history.len(), 1);
}

#[test]
fn triaged_can_close_without_action() {
    // 分诊后判定不违规：Triaged → Closed（无处置动作）
    let mut t = ticket();
    t.triage("mod1", "看看", T0 + 1).unwrap();
    t.close("mod1", "不违规", T0 + 2).unwrap();
    assert_eq!(t.state, State::Closed);
    assert_eq!(t.action, None);
}

#[test]
fn reject_allowed_from_open_and_triaged_only() {
    let mut open = ticket();
    open.reject("mod1", "明显误报", T0 + 1).unwrap();

    let mut actioned = ticket();
    actioned.triage("mod1", "受理", T0 + 1).unwrap();
    actioned.act("mod1", Action::Warn, "警告", T0 + 2).unwrap();
    assert!(actioned.reject("mod1", "想改成驳回", T0 + 3).is_err());
}

#[test]
fn mute_requires_positive_duration() {
    let mut t = ticket();
    t.triage("mod1", "受理", T0 + 1).unwrap();
    assert!(t
        .act("mod1", Action::Mute { duration_ms: 0 }, "0 秒禁言", T0 + 2)
        .is_err());
    t.act(
        "mod1",
        Action::Mute {
            duration_ms: 60 * 60 * 1000,
        },
        "禁言 1 小时",
        T0 + 2,
    )
    .unwrap();
    assert_eq!(
        t.action,
        Some(Action::Mute {
            duration_ms: 60 * 60 * 1000
        })
    );
}

#[test]
fn bad_input_is_refused_at_creation() {
    assert_eq!(
        Ticket::open("t", "", "message", "m", Reason::Spam, "", T0).unwrap_err(),
        ReportError::EmptyActor
    );
    assert_eq!(
        Ticket::open("t", "a", "planet", "m", Reason::Spam, "", T0).unwrap_err(),
        ReportError::BadTargetKind
    );
    assert_eq!(
        Ticket::open("t", "a", "message", "m", Reason::Spam, &"x".repeat(600), T0).unwrap_err(),
        ReportError::NoteTooLong
    );
    // 512 字符以内正常受理
    assert!(Ticket::open("t", "a", "user", "u", Reason::Other, "ok", T0).is_ok());
}

#[test]
fn sla_overdue_only_for_open_states() {
    let mut t = ticket();
    assert!(!t.is_overdue(T0 + DEFAULT_SLA_MS, DEFAULT_SLA_MS));
    assert!(t.is_overdue(T0 + DEFAULT_SLA_MS + 1, DEFAULT_SLA_MS));

    t.reject("mod1", "误报", T0 + 10).unwrap();
    assert!(!t.is_overdue(T0 + DEFAULT_SLA_MS * 10, DEFAULT_SLA_MS));
}

#[test]
fn state_roundtrips_through_strings() {
    for s in [
        State::Open,
        State::Triaged,
        State::Actioned,
        State::Closed,
        State::Rejected,
    ] {
        assert_eq!(State::parse(s.as_str()), Some(s));
    }
    for r in [
        Reason::Spam,
        Reason::Abuse,
        Reason::Porn,
        Reason::Fraud,
        Reason::Other,
    ] {
        assert_eq!(Reason::parse(r.as_str()), Some(r));
    }
    assert_eq!(State::parse("nope"), None);
}
