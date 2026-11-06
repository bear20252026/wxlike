//! 群的角色闸门（#108 越权面）与可见性、解散后写入。
//!
//! 契约的第三条不变式是"群内越权失败"：owner/admin/member 的判定全在服务端，
//! 前端传什么角色都不作数。这里逐格核对矩阵，并钉住两条容易被"顺手优化"掉的
//! 边界——**已解散的群不接受任何写入**、**私有群对非成员不可见**。

use rusqlite::Connection;
use wxlike_relations::{group, RelError};

mod common;
use common::{db, make_group};

/// 建一个 owner + admin + member + outsider 的群。
fn setup(conn: &Connection) -> String {
    let gid = make_group(conn, "owner", &["mem1", "mem2"], group::Visibility::Public);
    group::set_role(conn, "owner", &gid, "mem2", group::Role::Admin, 1_500).expect("promote");
    gid
}

#[test]
fn role_matrix_is_decided_server_side() {
    let conn = db();
    let gid = setup(&conn);

    // member 不能拉人、不能踢人、不能改角色。
    assert!(matches!(
        group::add_members(&conn, "mem1", &gid, &["x".to_string()], 2_000),
        Err(RelError::Forbidden(_))
    ));
    assert!(matches!(
        group::kick(&conn, "mem1", &gid, "mem2", 2_000),
        Err(RelError::Forbidden(_))
    ));
    assert!(matches!(
        group::set_role(&conn, "mem1", &gid, "mem2", group::Role::Member, 2_000),
        Err(RelError::Forbidden(_))
    ));

    // admin 能拉人、只能踢 member；动不了另一个 admin，也不能授予角色。
    assert!(group::add_members(&conn, "mem2", &gid, &["invited".to_string()], 2_100).is_ok());
    assert!(group::kick(&conn, "mem2", &gid, "mem1", 2_200).is_ok());
    assert!(matches!(
        group::kick(&conn, "mem2", &gid, "mem2b", 2_300),
        Err(RelError::NotFound(_))
    ));
    assert!(matches!(
        group::set_role(&conn, "mem2", &gid, "mem1", group::Role::Admin, 2_400),
        Err(RelError::Forbidden(_))
    ));

    // 非成员一切都做不了，而且报的是"不是成员"，不是权限不足。
    assert!(matches!(
        group::add_members(&conn, "ghost", &gid, &["mem1".to_string()], 2_500),
        Err(RelError::Forbidden(_))
    ));
}

#[test]
fn owner_only_actions() {
    let conn = db();
    let gid = setup(&conn);

    // 只有 owner 能改可见性 / 解散。
    assert!(matches!(
        group::set_visibility(&conn, "mem2", &gid, group::Visibility::Private, 3_000),
        Err(RelError::Forbidden(_))
    ));
    assert!(matches!(
        group::dissolve(&conn, "mem2", &gid, 3_000),
        Err(RelError::Forbidden(_))
    ));
    // owner 不能直接退群（否则群无人负责）。
    assert!(matches!(
        group::leave(&conn, "owner", &gid, 3_100),
        Err(RelError::Conflict(_))
    ));
    // owner 也不能把自己降级。
    assert!(matches!(
        group::set_role(&conn, "owner", &gid, "owner", group::Role::Member, 3_200),
        Err(RelError::Conflict(_))
    ));
}

#[test]
fn owner_transfer_moves_responsibility_not_just_a_label() {
    let conn = db();
    let gid = setup(&conn);
    group::set_role(&conn, "owner", &gid, "mem2", group::Role::Owner, 4_000).expect("transfer");

    let g = group::groups_of(&conn, "mem2")
        .unwrap()
        .into_iter()
        .find(|g| g.id == gid)
        .expect("new owner still sees the group");
    assert_eq!(g.owner_uid, "mem2", "转让必须同时改 groups.owner_uid");
    assert_eq!(
        group::role_of(&conn, &gid, "owner").unwrap(),
        Some(group::Role::Admin),
        "原 owner 降为 admin"
    );
    // 原 owner 从此不再有专属权限。
    assert!(matches!(
        group::dissolve(&conn, "owner", &gid, 4_100),
        Err(RelError::Forbidden(_))
    ));
    assert!(group::set_visibility(&conn, "mem2", &gid, group::Visibility::Private, 4_200).is_ok());
}

#[test]
fn visibility_controls_strangers_only() {
    let conn = db();
    let gid = make_group(&conn, "owner", &["mem1"], group::Visibility::Private);

    assert!(
        !group::may_see(&conn, "outsider", &gid).unwrap(),
        "私有群对陌生人不可见"
    );
    assert!(group::may_see(&conn, "mem1", &gid).unwrap(), "成员恒可见");

    group::set_visibility(&conn, "owner", &gid, group::Visibility::Public, 2_000)
        .expect("to public");
    assert!(group::may_see(&conn, "outsider", &gid).unwrap());

    // 改回私有：陌生人立刻又看不见。
    group::set_visibility(&conn, "owner", &gid, group::Visibility::Private, 2_100)
        .expect("to private");
    assert!(!group::may_see(&conn, "outsider", &gid).unwrap());
}

#[test]
fn dissolved_group_refuses_every_write() {
    let conn = db();
    let gid = setup(&conn);
    let notified = group::dissolve(&conn, "owner", &gid, 5_000).expect("dissolve");
    assert_eq!(notified.len(), 3, "解散要交出待通知的成员名单（含 owner）");

    // 契约：解散是软删，之后所有写入一律 conflict。逐条写命令验证，
    // 而不是只验证 dissolve 自己——漏掉任何一条都是一条改到死群的后门。
    let cases: Vec<(&str, Result<(), RelError>)> = vec![
        (
            "add_members",
            group::add_members(&conn, "owner", &gid, &["new".to_string()], 6_000).map(|_| ()),
        ),
        ("kick", group::kick(&conn, "owner", &gid, "mem1", 6_000)),
        (
            "set_role",
            group::set_role(&conn, "owner", &gid, "mem1", group::Role::Admin, 6_000),
        ),
        ("leave", group::leave(&conn, "mem1", &gid, 6_000)),
        (
            "set_visibility",
            group::set_visibility(&conn, "owner", &gid, group::Visibility::Public, 6_000)
                .map(|_| ()),
        ),
        (
            "dissolve again",
            group::dissolve(&conn, "owner", &gid, 6_000).map(|_| ()),
        ),
    ];
    for (name, res) in cases {
        assert!(
            matches!(res, Err(RelError::Conflict(_))),
            "已解散的群 {name} 必须是 conflict，实际 {res:?}"
        );
    }

    // 解散不删成员行（留审计），但群从各人的列表里消失，陌生人也不再能看见。
    let rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM group_members WHERE group_id=?1",
            rusqlite::params![gid.clone()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 3, "软删：成员行保留供审计对账");
    assert!(
        group::groups_of(&conn, "mem1").unwrap().is_empty(),
        "解散后不出现在群列表"
    );
    assert!(group::common_groups(&conn, "owner", "mem1")
        .unwrap()
        .is_empty());
    assert!(
        !group::may_see(&conn, "outsider", &gid).unwrap(),
        "解散的公开群也不再敞开"
    );
}

#[test]
fn empty_results_are_ok_not_errors() {
    let conn = db();
    // 空结果一律是"空"，不能报 not found：前端据此渲染空态。
    assert!(group::groups_of(&conn, "nobody").unwrap().is_empty());
    assert!(group::common_groups(&conn, "nobody", "nobody2")
        .unwrap()
        .is_empty());
    assert!(group::members(&conn, "ghost-group").unwrap().is_empty());
    assert_eq!(
        group::role_of(&conn, "ghost-group", "nobody").unwrap(),
        None
    );

    let gid = make_group(&conn, "owner", &[], group::Visibility::Public);
    assert_eq!(
        group::members(&conn, &gid).unwrap(),
        vec!["owner".to_string()]
    );
}

#[test]
fn bad_input_is_rejected_before_any_write() {
    let conn = db();
    assert!(matches!(
        group::create(&conn, "owner", "", &[], group::Visibility::Public, 1_000),
        Err(RelError::BadInput(_))
    ));
    let long = "x".repeat(wxlike_relations::MAX_TEXT_CHARS + 1);
    assert!(matches!(
        group::create(&conn, "owner", &long, &[], group::Visibility::Public, 1_000),
        Err(RelError::BadInput(_))
    ));
    // 控制字符（换行/ANSI）不能进群名。
    assert!(matches!(
        group::create(
            &conn,
            "owner",
            "a\nb",
            &[],
            group::Visibility::Public,
            1_000
        ),
        Err(RelError::BadInput(_))
    ));
    // 未知群：写命令一律 not found（require_active 先读群）。
    assert!(matches!(
        group::add_members(&conn, "owner", "ghost", &["u".to_string()], 1_000),
        Err(RelError::NotFound(_))
    ));
    assert!(
        group::groups_of(&conn, "owner").unwrap().is_empty(),
        "失败不许留下半行"
    );
}

#[test]
fn member_cap_is_enforced_on_both_paths() {
    let conn = db();
    // create 路径：直接超限就报错，不产生群行。
    let many: Vec<String> = (0..wxlike_relations::MAX_GROUP_MEMBERS + 1)
        .map(|i| format!("u{i}"))
        .collect();
    assert!(matches!(
        group::create(
            &conn,
            "owner",
            "大群",
            &many,
            group::Visibility::Public,
            1_000
        ),
        Err(RelError::BadInput(_))
    ));
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM groups", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0,
        "超限的建群不能留下任何行"
    );

    // add_members 路径：先把群填满到上限附近再越界。
    let gid = make_group(&conn, "owner", &[], group::Visibility::Public);
    let fill: Vec<String> = (1..wxlike_relations::MAX_GROUP_MEMBERS)
        .map(|i| format!("m{i}"))
        .collect();
    group::add_members(&conn, "owner", &gid, &fill, 2_000).expect("fill");
    let over: Vec<String> = vec!["over".to_string(), "over2".to_string()];
    assert!(matches!(
        group::add_members(&conn, "owner", &gid, &over, 2_100),
        Err(RelError::BadInput(_))
    ));
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM group_members WHERE group_id=?1",
            rusqlite::params![gid.clone()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, wxlike_relations::MAX_GROUP_MEMBERS as i64);
}

#[test]
fn private_group_membership_never_leaks_through_common_groups() {
    let conn = db();
    let gid = make_group(&conn, "a", &["b"], group::Visibility::Private);
    // 共同群是交集查询：不是两边都入群就一行也不返回，且不报错。
    assert_eq!(
        group::common_groups(&conn, "a", "b")
            .unwrap()
            .iter()
            .map(|g| g.id.clone())
            .collect::<Vec<_>>(),
        vec![gid.clone()]
    );
    assert!(group::common_groups(&conn, "a", "stranger")
        .unwrap()
        .is_empty());
    // 陌生人对这个群连"是否存在"都问不到。
    assert!(!group::may_see(&conn, "stranger", &gid).unwrap());
}
