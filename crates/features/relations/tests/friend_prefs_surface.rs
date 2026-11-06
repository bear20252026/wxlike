//! 好友申请状态机 + 私有偏好 + 跨域闸门（#108 的失败面与越权面）。
//!
//! 覆盖四类：参数错误 / 空结果 / 越权 / 重复提交。私有偏好那一组的重点是
//! **备注与星标不参与任何权限判定**——把好友备注改成"老板"不会因此得到任何
//! 权限，星标也不会让对方更容易收到消息。

use wxlike_relations::port::RelationsPort as _;
use wxlike_relations::{friend, group, port, prefs, RelError};

mod common;
use common::{befriend, db};

fn created(out: friend::SendOutcome) -> String {
    match out {
        friend::SendOutcome::Created(req) => req.id,
        other => panic!("expected created, got {other:?}"),
    }
}

#[test]
fn state_machine_transitions_are_one_way() {
    let conn = db();
    let id = created(friend::send(&conn, "u1", "u2", "你好", 1_000).unwrap());

    // pending -> accepted，之后再撤销/拒绝都读成"已决"。
    let acc = friend::accept(&conn, "u2", &id, 1_100).unwrap();
    assert_eq!(acc.status, friend::Status::Accepted);
    for (name, res) in [
        (
            "accept twice",
            friend::accept(&conn, "u2", &id, 1_200).map(|_| ()),
        ),
        (
            "reject after accept",
            friend::reject(&conn, "u2", &id, 1_200).map(|_| ()),
        ),
        (
            "cancel after accept",
            friend::cancel(&conn, "u1", &id, 1_200).map(|_| ()),
        ),
    ] {
        assert!(
            matches!(res, Err(RelError::Conflict(_))),
            "{name} 应是 conflict: {res:?}"
        );
    }
    // 已接受后重复 send 走"已是好友"，不是错误。
    assert_eq!(
        friend::send(&conn, "u1", "u2", "", 1_300).unwrap(),
        friend::SendOutcome::AlreadyFriends
    );
}

#[test]
fn every_actor_must_be_the_right_one() {
    let conn = db();
    let id = created(friend::send(&conn, "u1", "u2", "", 1_000).unwrap());

    // 第三人既不能接受也不能拒绝，也不能替别人撤回。
    assert!(matches!(
        friend::accept(&conn, "u3", &id, 2_000),
        Err(RelError::Forbidden(_))
    ));
    assert!(matches!(
        friend::reject(&conn, "u3", &id, 2_000),
        Err(RelError::Forbidden(_))
    ));
    assert!(matches!(
        friend::cancel(&conn, "u3", &id, 2_000),
        Err(RelError::Forbidden(_))
    ));
    // 收件人也不能替发申请的人撤回。
    assert!(matches!(
        friend::cancel(&conn, "u2", &id, 2_000),
        Err(RelError::Forbidden(_))
    ));
    // 发申请的人自己不能接受。
    assert!(matches!(
        friend::accept(&conn, "u1", &id, 2_000),
        Err(RelError::Forbidden(_))
    ));
    // 越权尝试不改变任何状态。
    assert_eq!(
        friend::incoming(&conn, "u2", 50).unwrap()[0].status,
        friend::Status::Pending
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM friends", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn unknown_ids_are_not_found_and_lists_are_empty_not_errors() {
    let conn = db();
    for (name, res) in [
        (
            "accept",
            friend::accept(&conn, "u2", "ghost", 1_000).map(|_| ()),
        ),
        (
            "reject",
            friend::reject(&conn, "u2", "ghost", 1_000).map(|_| ()),
        ),
        (
            "cancel",
            friend::cancel(&conn, "u1", "ghost", 1_000).map(|_| ()),
        ),
    ] {
        assert!(
            matches!(res, Err(RelError::NotFound(_))),
            "{name} 未知 id 应 not found: {res:?}"
        );
    }
    assert!(friend::incoming(&conn, "nobody", 50).unwrap().is_empty());
    assert!(friend::outgoing(&conn, "nobody", 50).unwrap().is_empty());
    assert!(friend::friend_ids(&conn, "nobody").unwrap().is_empty());
    assert!(!friend::are_friends(&conn, "nobody", "ghost").unwrap());
    assert!(!friend::is_blocked(&conn, "nobody", "ghost").unwrap());
    assert!(!friend::blocked_either(&conn, "nobody", "ghost").unwrap());
}

#[test]
fn input_validation_rejects_before_touching_the_database() {
    let conn = db();
    // 自申请、自拉黑。
    assert!(matches!(
        friend::send(&conn, "u1", "u1", "", 1_000),
        Err(RelError::BadInput(_))
    ));
    assert!(matches!(
        friend::block(&conn, "u1", "u1", 1_000),
        Err(RelError::BadInput(_))
    ));
    // 超长与含控制字符的附言。
    let long = "x".repeat(wxlike_relations::MAX_MESSAGE_CHARS + 1);
    assert!(matches!(
        friend::send(&conn, "u1", "u2", &long, 1_000),
        Err(RelError::BadInput(_))
    ));
    assert!(matches!(
        friend::send(&conn, "u1", "u2", "a\0b", 1_000),
        Err(RelError::BadInput(_))
    ));
    // 空附言是合法的（只限长，不限非空）。
    assert!(friend::send(&conn, "u1", "u2", "", 1_000).is_ok());
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM friend_requests WHERE from_uid='u1' AND to_uid='u2'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1,
        "校验失败的四种写法都不该留下行"
    );
}

#[test]
fn block_is_a_two_way_gate_and_leaves_no_trace_behind() {
    let conn = db();
    befriend(&conn, "u1", "u2");
    let pending = created(friend::send(&conn, "u3", "u1", "", 2_000).unwrap());

    friend::block(&conn, "u1", "u2", 2_100).unwrap();
    // 双向都发不出申请，且错误只说 blocked，不说"你们已经是好友过"。
    for (from, to) in [("u1", "u2"), ("u2", "u1")] {
        let err = friend::send(&conn, from, to, "", 2_200).unwrap_err();
        assert!(
            matches!(err, RelError::Forbidden("blocked")),
            "{from}->{to} 应统一是 blocked: {err}"
        );
        assert!(!err.to_string().contains("friend"), "错误不能泄漏历史关系");
    }
    // 与本次拉黑无关的第三人申请不受影响。
    assert!(friend::accept(&conn, "u1", &pending, 2_300).is_ok());
    // 拉黑拆干净：两行好友都没了。
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM friends WHERE user_id='u1' AND friend_id='u2'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    // 解除拉黑不恢复关系，也不自动重新申请。
    friend::unblock(&conn, "u1", "u2", 2_400).unwrap();
    assert!(!friend::are_friends(&conn, "u1", "u2").unwrap());
    let err = friend::unblock(&conn, "u1", "u2", 2_500)
        .expect_err("重复解除拉黑要说清楚：本来就没有这条黑名单");
    assert!(matches!(err, RelError::NotFound(_)), "实际: {err}");
}

#[test]
fn pending_request_is_idempotent_under_repeat() {
    let conn = db();
    let first = friend::send(&conn, "u1", "u2", "one", 1_000).unwrap();
    let id = created(first);
    // 同一对账号反复点"添加"只有一行，返回 Duplicate（不是错误，避免客户端重试风暴）。
    for at in [1_100, 1_200, 1_300] {
        match friend::send(&conn, "u1", "u2", "again", at).unwrap() {
            friend::SendOutcome::Duplicate(req) => assert_eq!(req.id, id),
            other => panic!("重复申请应返回 Duplicate: {other:?}"),
        }
    }
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM friend_requests", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    // 附言仍是第一次那份：Duplicate 不覆盖已经提交给对方的内容。
    assert_eq!(
        conn.query_row(
            "SELECT message FROM friend_requests WHERE id=?1",
            rusqlite::params![id.clone()],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "one"
    );
}

#[test]
fn prefs_never_grant_or_change_permissions() {
    let conn = db();
    befriend(&conn, "u1", "u2");
    let gid = group::create(
        &conn,
        "u1",
        "群",
        &["u2".to_string()],
        group::Visibility::Public,
        3_000,
    )
    .unwrap()
    .id;

    let port = port::SqlRelations { conn: &conn };
    let before = port.may_message("u1", "u2").unwrap();

    // 备注写"老板"、星标、标签——都不改任何闸门。
    prefs::set_remark(&conn, "u1", "u2", "老板").unwrap();
    prefs::set_star(&conn, "u1", "u2", true).unwrap();
    prefs::set_tags(&conn, "u1", "u2", &["VIP".into()]).unwrap();
    assert_eq!(port.may_message("u1", "u2").unwrap(), before);
    assert_eq!(
        group::role_of(&conn, &gid, "u2").unwrap(),
        Some(group::Role::Member),
        "偏好不参与群角色判定"
    );

    // 反过来：被拉黑之后，再怎么改偏好也进不来。
    friend::block(&conn, "u2", "u1", 3_100).unwrap();
    assert!(matches!(
        port.may_message("u1", "u2"),
        Ok(port::MessageGate::Blocked)
    ));
    assert!(
        prefs::set_remark(&conn, "u1", "u2", "still").is_err(),
        "关系已经没了，偏好写不进去"
    );
}

#[test]
fn prefs_are_one_sided_and_validated() {
    let conn = db();
    befriend(&conn, "u1", "u2");
    prefs::set_remark(&conn, "u1", "u2", "同事").unwrap();
    prefs::set_star(&conn, "u1", "u2", true).unwrap();

    // 对方那一行完全不受影响。
    let theirs = prefs::get(&conn, "u2", "u1").unwrap().unwrap();
    assert_eq!(theirs.remark, "");
    assert!(!theirs.starred);

    // 陌生人是 not found，而不是"建一条偏好"。
    assert!(matches!(
        prefs::set_remark(&conn, "u1", "stranger", "x"),
        Err(RelError::NotFound(_))
    ));
    assert!(matches!(
        prefs::set_star(&conn, "stranger", "u1", true),
        Err(RelError::NotFound(_))
    ));
    // 空串备注 = 清除，不是 BadInput。
    prefs::set_remark(&conn, "u1", "u2", "").unwrap();
    assert_eq!(prefs::get(&conn, "u1", "u2").unwrap().unwrap().remark, "");
    assert!(prefs::get(&conn, "u1", "ghost").unwrap().is_none());
}

#[test]
fn tag_replace_is_atomic_and_private() {
    let conn = db();
    befriend(&conn, "u1", "u2");
    prefs::set_tags(&conn, "u1", "u2", &["a".into(), "b".into(), "c".into()]).unwrap();
    // 整体替换：第二次调用不留旧标签。
    prefs::set_tags(&conn, "u1", "u2", &["z".into()]).unwrap();
    assert_eq!(
        prefs::get(&conn, "u1", "u2").unwrap().unwrap().tags,
        vec!["z".to_string()]
    );
    // 标签也是单方面的。
    assert!(prefs::get(&conn, "u2", "u1")
        .unwrap()
        .unwrap()
        .tags
        .is_empty());
    // 校验：空标签、超长、数量超限，全部在写库之前拒绝。
    for bad in [
        vec!["".to_string()],
        vec!["x".repeat(wxlike_relations::MAX_TEXT_CHARS + 1)],
        (0..=prefs::MAX_TAGS + 1).map(|i| format!("t{i}")).collect(),
    ] {
        assert!(
            matches!(
                prefs::set_tags(&conn, "u1", "u2", &bad),
                Err(RelError::BadInput(_))
            ),
            "{bad:?} 应被拒"
        );
    }
    // 上一组失败之后，原来的 `z` 还在（失败不许清空）。
    assert_eq!(
        prefs::get(&conn, "u1", "u2").unwrap().unwrap().tags,
        vec!["z".to_string()]
    );
}

#[test]
fn message_gate_covers_every_relation_shape() {
    let conn = db();
    let port = port::SqlRelations { conn: &conn };

    // 自己发给自己 = 多端同步，恒允许。
    assert_eq!(
        port.may_message("u1", "u1").unwrap(),
        port::MessageGate::Allowed
    );
    // 陌生人 = 请求箱，不建会话。
    assert_eq!(
        port.may_message("u1", "u9").unwrap(),
        port::MessageGate::StrangerRequest
    );
    befriend(&conn, "u1", "u2");
    assert_eq!(
        port.may_message("u1", "u2").unwrap(),
        port::MessageGate::Allowed
    );
    // 单向拉黑就够，且两个方向都拒。
    friend::block(&conn, "u2", "u1", 4_000).unwrap();
    assert_eq!(
        port.may_message("u1", "u2").unwrap(),
        port::MessageGate::Blocked
    );
    assert_eq!(
        port.may_message("u2", "u1").unwrap(),
        port::MessageGate::Blocked
    );
    // 被拉黑的一方问"我们是不是共同群"——闸门不给任何额外线索。
    assert!(port.is_blocked("u2", "u1").unwrap());
    assert!(!port.is_blocked("u1", "u2").unwrap());
    assert!(!port.are_friends("u1", "u2").unwrap());
}
