//! 坏数据不许伪装成好消息（relations-v1 的"失败封闭"面）。
//!
//! main 上有四处"读不懂就折一个看着还行的值"：
//!
//! * `friend_requests.status` 折成 `pending`——按 id 取行时，一条坏行被当作
//!   待处理申请继续往下走，`send()` 因此返回 `Duplicate`，用户永远无法重新申请；
//! * `group_meta.visibility` 折成 `public`——**坏掉的私有群对陌生人敞开**；
//! * `group_members.role` 折成"不是成员"——损坏行与缺席行看起来一模一样；
//! * `reopen()`/`cancel()` 忽略条件 UPDATE 的命中行数——没写进任何行也报成功。
//!
//! 前两条的方向都是**向外敞开**，所以必须报错。这些用例直接写原文造出坏行，
//! 断言公开 API 一定失败，而不是"表现正常"。

use rusqlite::Connection;
use wxlike_relations::port::RelationsPort as _;
use wxlike_relations::{friend, group, RelError};

mod common;
use common::db;

/// 造一条 status 列是任意原文的申请行。
fn request_with_status(conn: &Connection, id: &str, from: &str, to: &str, status: &str) {
    conn.execute(
        "INSERT INTO friend_requests(id, from_uid, to_uid, message, status, created_at) \
         VALUES(?1, ?2, ?3, '', ?4, 1000)",
        rusqlite::params![id, from, to, status],
    )
    .expect("insert raw request");
}

fn group_with_visibility(conn: &Connection, id: &str, owner: &str, visibility: &str) {
    conn.execute(
        "INSERT INTO groups(id, name, owner_uid, created_at) VALUES(?1, '群', ?2, 1000)",
        rusqlite::params![id, owner],
    )
    .expect("insert group");
    conn.execute(
        "INSERT INTO group_members(group_id, user_id, role, my_remark, joined_at) \
         VALUES(?1, ?2, 'owner', '', 1000)",
        rusqlite::params![id, owner],
    )
    .expect("insert owner member");
    if visibility != "__none__" {
        conn.execute(
            "INSERT INTO group_meta(group_id, visibility) VALUES(?1, ?2)",
            rusqlite::params![id, visibility],
        )
        .expect("insert meta");
    }
}

/// 读按 id 取行的路径（`accept`/`send` 都走 `load`/`find_between`）。
///
/// 注意"待处理列表"这一路本来就安全：`incoming`/`outgoing` 的 SQL 里写死了
/// `status = 'pending'`，坏值根本选不出来。真正会撒谎的是**按 id 取整行**的
/// 那几条——它们不看 status 条件，全靠闭包里的解析，所以折默认值正是折在那里。
#[test]
fn unknown_request_status_is_never_read_as_pending() {
    let conn = db();
    // 例如别的写入方留下的 `'accepteed'`：它绝不是"待处理"。
    request_with_status(&conn, "r1", "u1", "u2", "accepteed");

    // 列表：SQL 过滤已经挡住了，这里只是把这条边界钉住。
    assert!(friend::incoming(&conn, "u2", 50).unwrap().is_empty());
    assert!(friend::outgoing(&conn, "u1", 50).unwrap().is_empty());

    // 按 id 取行：必须报损坏，而不是折成 pending 后继续往下走。
    let err = friend::accept(&conn, "u2", "r1", 2_000).expect_err("坏状态不能当 pending");
    assert!(
        err.to_string().contains("friend_requests.status"),
        "错误要指认列，实际: {err}"
    );
    assert!(matches!(err, RelError::Corrupt(_)));
}

#[test]
fn send_reports_corrupt_row_instead_of_duplicate() {
    let conn = db();
    request_with_status(&conn, "r1", "u1", "u2", "done");
    // 旧写法：折成 pending -> 返回 Duplicate，用户看到"你已经申请过了"，
    // 而那一行既不是 pending 也永远不会再被处理。
    assert!(matches!(
        friend::send(&conn, "u1", "u2", "", 2_000),
        Err(RelError::Corrupt(_))
    ));
}

#[test]
fn well_known_statuses_still_round_trip() {
    let conn = db();
    for status in ["pending", "accepted", "rejected", "cancelled"] {
        // UNIQUE(from_uid, to_uid) 只允许一行，所以每个状态各用一对账号。
        let id = format!("r-{status}");
        request_with_status(&conn, &id, &format!("s-{status}"), "peer", status);
        let by_id = friend::accept(&conn, "peer", &id, 2_000);
        match status {
            "pending" => assert!(by_id.is_ok(), "pending 必须可接受"),
            // 其它三种状态读得出来，并且都被认成"已决"，不是损坏、不是 pending。
            _ => assert!(
                matches!(by_id, Err(RelError::Conflict(_))),
                "{status} 应读成已决: {by_id:?}"
            ),
        }
    }
    assert!(matches!(
        friend::accept(&conn, "peer", "r-unknown", 2_000),
        Err(RelError::NotFound(_))
    ));
}

#[test]
fn corrupt_visibility_cannot_make_a_private_group_public() {
    let conn = db();
    group_with_visibility(&conn, "g1", "owner1", "privat");
    // 非成员来问"我能看到这个群吗"。折成 public 就等于把群敞开。
    let err = group::may_see(&conn, "outsider", "g1").expect_err("坏可见性必须失败");
    assert!(err.to_string().contains("group_meta.visibility"));
    assert!(matches!(err, RelError::Corrupt(_)));
}

#[test]
fn corrupt_visibility_propagates_through_every_read_path() {
    let conn = db();
    group_with_visibility(&conn, "g1", "owner1", "");
    let stranger = wxlike_relations::port::SqlRelations { conn: &conn };
    let err = group::groups_of(&conn, "owner1").expect_err("成员自己的群列表也不能撒谎");
    assert!(matches!(err, RelError::Corrupt(_)));
    let err = group::common_groups(&conn, "owner1", "owner1").expect_err("共同群同理");
    assert!(matches!(err, RelError::Corrupt(_)));
    // 跨域端口同样不许把坏群当公开群交出去。
    let err = stranger
        .shared_groups("owner1", "owner1")
        .expect_err("端口不能替坏群做默认值");
    assert!(matches!(err, RelError::Corrupt(_)));
}

#[test]
fn missing_group_meta_row_is_still_readable_as_public() {
    let conn = db();
    // 引入 group_meta 之前建出来的存量群：`NULL`（整行缺失）与"存在但读不懂"
    // 必须区别对待，否则一次上线就把全部历史群打成 500。
    group_with_visibility(&conn, "g1", "owner1", "__none__");
    assert!(group::may_see(&conn, "outsider", "g1").expect("legacy group visible"));
    let mine = group::groups_of(&conn, "owner1").expect("legacy group must still load");
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].visibility, group::Visibility::Public);
    assert!(!mine[0].dissolved);
}

#[test]
fn corrupt_role_is_not_mistaken_for_absence() {
    let conn = db();
    group_with_visibility(&conn, "g1", "owner1", "private");
    conn.execute(
        "INSERT INTO group_members(group_id, user_id, role, my_remark, joined_at) \
         VALUES('g1', 'u9', 'membre', '', 1000)",
        [],
    )
    .expect("insert corrupt role");

    let err = group::role_of(&conn, "g1", "u9").expect_err("坏角色不能读成 None");
    assert!(err.to_string().contains("group_members.role"));
    // 非成员才是 None。
    assert_eq!(group::role_of(&conn, "g1", "nobody").unwrap(), None);
}

#[test]
fn corrupt_role_blocks_writes_rather_than_falling_to_forbidden_guess() {
    let conn = db();
    group_with_visibility(&conn, "g1", "owner1", "public");
    conn.execute(
        "UPDATE group_members SET role = 'onwer' WHERE user_id = 'owner1'",
        [],
    )
    .expect("corrupt owner role");
    // 该账号确实有这一行，只是读不懂：必须报损坏，而不是"你不是成员"。
    assert!(matches!(
        group::kick(&conn, "owner1", "g1", "x", 2_000),
        Err(RelError::Corrupt(_))
    ));
}

#[test]
fn audit_rows_survive_and_name_the_actor() {
    let conn = db();
    friend::send(&conn, "u1", "u2", "", 1_000).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM relations_audit WHERE actor='u1' AND kind='friend.request'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "每次关系写入都要留一条可追溯审计");
}

/// 申请行是 `accepted`，但好友两行残缺（半途失败或被别的域清过）。
///
/// 这时 `send()` 走 `reopen` 分支：条件 UPDATE 只匹配 `rejected|cancelled`，
/// 命中 0 行。旧写法忽略行数，照样返回 `Created`——前端显示"已发送"，收件人
/// 的列表里却永远不会出现这条申请。
#[test]
fn reopen_that_writes_nothing_cannot_report_success() {
    let conn = db();
    let id = {
        let out = friend::send(&conn, "u1", "u2", "first", 1_000).unwrap();
        match out {
            friend::SendOutcome::Created(req) => req.id,
            other => panic!("expected created, got {other:?}"),
        }
    };
    friend::accept(&conn, "u2", &id, 1_100).unwrap();
    assert!(friend::are_friends(&conn, "u1", "u2").unwrap());

    // 人为制造"已接受但没有好友行"的残缺状态。
    conn.execute("DELETE FROM friends", []).unwrap();
    assert!(!friend::are_friends(&conn, "u1", "u2").unwrap());

    let err = friend::send(&conn, "u1", "u2", "again", 1_200)
        .expect_err("写不进任何行就不能说申请已发出");
    assert!(matches!(err, RelError::Conflict(_)), "实际: {err}");

    // 库里那条申请仍然是 accepted，没有被谎报成 pending。
    let status: String = conn
        .query_row(
            "SELECT status FROM friend_requests WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "accepted");
    assert!(friend::incoming(&conn, "u2", 50).unwrap().is_empty());
}
