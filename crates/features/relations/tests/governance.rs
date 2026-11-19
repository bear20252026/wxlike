//! G02-09 群治理的验收面（真实 SQLite）。
//!
//! 钉住卡面"群治理四动作"：freeze / unfreeze / transfer / dissolve，
//! 以及幂等重复提交、解散终态封闭、理由留痕、冻结真实生效（成员只读）。
//!
//! audience 鉴权由 identity 域在 HTTP 面统一做（admin 令牌），本域收到的是
//! 已鉴权命令——与 adminx 经 `HttpPort` 转发的形状一致。

use wxlike_relations::governance::{self, GroupAction, GroupGovernCommand, GroupGovernanceEvent};
use wxlike_relations::{group, RelError};

mod common;
use common::{db, make_group};

fn cmd(group_id: &str, action: GroupAction) -> GroupGovernCommand {
    GroupGovernCommand {
        group_id: group_id.into(),
        action,
        reason: "群体违规".into(),
        actor: "ops".into(),
        trace: "trace-x".into(),
        new_owner: None,
    }
}

#[test]
fn freeze_blocks_member_writes_and_unfreeze_restores() {
    let conn = db();
    let id = make_group(&conn, "u1", &["u2", "u3"], group::Visibility::Public);

    let e = governance::govern(&conn, &cmd(&id, GroupAction::Freeze), 1_100).unwrap();
    assert!(!e.already);
    assert_eq!(e.before["state"], "active");
    assert_eq!(e.after["state"], "frozen");

    // 冻结真实生效：成员只读，拉人/踢人等写操作被权限闸门挡住。
    let g = group::groups_of(&conn, "u1")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let acs = group::member_acs(&conn, &id, "u2", &g).unwrap();
    assert!(acs.is_reader() && !acs.is_writer(), "冻结后成员只读");
    assert!(!group::may_invite(&conn, "u1", &id, "u9").unwrap());

    // 解冻恢复写权限。
    let un = governance::govern(&conn, &cmd(&id, GroupAction::Unfreeze), 1_200).unwrap();
    assert!(!un.already);
    assert_eq!(un.after["state"], "active");
    let g = group::groups_of(&conn, "u1")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert!(group::member_acs(&conn, &id, "u2", &g).unwrap().is_writer());
}

#[test]
fn freeze_and_unfreeze_are_idempotent() {
    let conn = db();
    let id = make_group(&conn, "u1", &["u2"], group::Visibility::Public);
    governance::govern(&conn, &cmd(&id, GroupAction::Freeze), 1_100).unwrap();
    let again = governance::govern(&conn, &cmd(&id, GroupAction::Freeze), 1_200).unwrap();
    assert!(again.already);
    governance::govern(&conn, &cmd(&id, GroupAction::Unfreeze), 1_300).unwrap();
    let again = governance::govern(&conn, &cmd(&id, GroupAction::Unfreeze), 1_400).unwrap();
    assert!(again.already);
}

#[test]
fn transfer_changes_owner_and_demotes_old() {
    let conn = db();
    let id = make_group(&conn, "u1", &["u2", "u3"], group::Visibility::Public);
    let mut c = cmd(&id, GroupAction::Transfer);
    c.new_owner = Some("u2".into());

    let e = governance::govern(&conn, &c, 1_100).unwrap();
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
    // 事件含转让双方。
    assert!(e.events.iter().any(|ev| matches!(
        ev,
        GroupGovernanceEvent::GroupTransferred { old_owner, new_owner, .. }
            if old_owner == "u1" && new_owner == "u2"
    )));
    // 幂等：再转给同一群主 already。
    assert!(governance::govern(&conn, &c, 1_200).unwrap().already);
}

#[test]
fn transfer_requires_new_owner_and_existing_member() {
    let conn = db();
    let id = make_group(&conn, "u1", &["u2"], group::Visibility::Public);
    let mut c = cmd(&id, GroupAction::Transfer);
    assert!(matches!(
        governance::govern(&conn, &c, 1_100),
        Err(RelError::BadInput(_))
    ));
    c.new_owner = Some("u9".into()); // 非成员
    assert!(matches!(
        governance::govern(&conn, &c, 1_100),
        Err(RelError::NotFound(_))
    ));
    // 群主未变。
    assert_eq!(
        group::role_of(&conn, &id, "u1").unwrap(),
        Some(group::Role::Owner)
    );
}

#[test]
fn dissolve_is_terminal_and_notifies_members() {
    let conn = db();
    let id = make_group(&conn, "u1", &["u2", "u3"], group::Visibility::Public);
    let e = governance::govern(&conn, &cmd(&id, GroupAction::Dissolve), 1_100).unwrap();
    assert!(!e.already);
    assert_eq!(e.after["state"], "dissolved");
    assert!(e.events.iter().any(|ev| matches!(
        ev,
        GroupGovernanceEvent::GroupDissolved { members, .. } if members.len() == 3
    )));
    // 幂等。
    assert!(
        governance::govern(&conn, &cmd(&id, GroupAction::Dissolve), 1_200)
            .unwrap()
            .already
    );
    // 终态封闭：不能再冻结/解冻/转让。
    for action in [
        GroupAction::Freeze,
        GroupAction::Unfreeze,
        GroupAction::Transfer,
    ] {
        let mut c = cmd(&id, action);
        c.new_owner = Some("u2".into());
        assert!(matches!(
            governance::govern(&conn, &c, 1_300),
            Err(RelError::Conflict(_))
        ));
    }
    // 成员自助写也被解散闸门挡住。
    assert!(matches!(
        group::add_members(&conn, "u1", &id, &["u9".into()], 1_400),
        Err(RelError::Conflict(_))
    ));
}

#[test]
fn frozen_group_can_be_dissolved_and_stays_dissolved() {
    let conn = db();
    let id = make_group(&conn, "u1", &["u2"], group::Visibility::Public);
    governance::govern(&conn, &cmd(&id, GroupAction::Freeze), 1_100).unwrap();
    let e = governance::govern(&conn, &cmd(&id, GroupAction::Dissolve), 1_200).unwrap();
    assert_eq!(e.after["state"], "dissolved");
    // 解散后解冻被拒（终态优先）。
    assert!(matches!(
        governance::govern(&conn, &cmd(&id, GroupAction::Unfreeze), 1_300),
        Err(RelError::Conflict(_))
    ));
}

#[test]
fn every_action_leaves_a_reviewable_reason() {
    let conn = db();
    let id = make_group(&conn, "u1", &["u2"], group::Visibility::Public);
    governance::govern(&conn, &cmd(&id, GroupAction::Freeze), 1_100).unwrap();
    let mut c = cmd(&id, GroupAction::Transfer);
    c.new_owner = Some("u2".into());
    c.reason = "原群主失联，移交运营".into();
    governance::govern(&conn, &c, 1_200).unwrap();

    let rows: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare(
                "SELECT action, reason FROM group_governance_audit WHERE group_id = ?1 ORDER BY seq",
            )
            .unwrap();
        let rows = stmt
            .query_map([&id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
    };
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|(a, r)| a == "freeze" && r == "群体违规"));
    assert!(rows
        .iter()
        .any(|(a, r)| a == "transfer" && r.contains("移交运营")));
    // relations_audit 也有治理 kind 行。
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM relations_audit WHERE kind LIKE 'group.govern.%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 2);
}

#[test]
fn bad_target_and_empty_reason_are_rejected() {
    let conn = db();
    let id = make_group(&conn, "u1", &["u2"], group::Visibility::Public);
    let mut c = cmd(&id, GroupAction::Freeze);
    c.reason = "  ".into();
    assert!(matches!(
        governance::govern(&conn, &c, 1_100),
        Err(RelError::BadInput(_))
    ));
    let mut traversal = cmd("../../etc/passwd", GroupAction::Freeze);
    traversal.reason = "x".into();
    assert!(matches!(
        governance::govern(&conn, &traversal, 1_100),
        Err(RelError::BadInput(_))
    ));
    assert!(matches!(
        governance::govern(&conn, &cmd("missing", GroupAction::Freeze), 1_100),
        Err(RelError::NotFound(_))
    ));
}

#[test]
fn unknown_verb_is_not_parseable() {
    assert_eq!(GroupAction::parse("freeze"), Some(GroupAction::Freeze));
    assert_eq!(GroupAction::parse("unfreeze"), Some(GroupAction::Unfreeze));
    assert_eq!(GroupAction::parse("transfer"), Some(GroupAction::Transfer));
    assert_eq!(GroupAction::parse("dissolve"), Some(GroupAction::Dissolve));
    assert_eq!(GroupAction::parse("nuke"), None);
}
