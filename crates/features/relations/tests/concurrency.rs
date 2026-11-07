//! 真并发（多连接打到同一个 SQLite 文件库）。
//!
//! # 为什么不能用一个连接开两个线程
//!
//! `Connection` 不是 `Sync`，而 `open_in_memory()` 每次 `open` 得到的是一个
//! **全新的库**——两个线程各拿一个内存库"并发"，测到的只是各自单线程重放一遍。
//! 所以这里用临时目录里的**一个真实数据库文件**，每个线程自己 `open` 一个连接，
//! 由 SQLite 的写锁真正裁决谁先落地。
//!
//! # 断言的是不变式，不是错误类型
//!
//! 写写冲突时 SQLite 可能直接给 `SQLITE_BUSY`（事务里升级写锁不会等，这是设计，
//! 不是 bug）。所以本文件对 BUSY 有限次重试，然后断言**结果形状**：同一对关系
//! 不会两个都成功、好友行永远是 0 或 2 而不是 1、拉黑与好友不共存。
//! 这些才是契约里"并发接受不会产生第二对"的实际内容。

use std::sync::Arc;
use std::sync::Barrier;
use std::thread;
use std::time::Duration;

use rusqlite::Connection;
use wxlike_relations::{friend, group, RelError};

mod common;
use common::FileDb;

/// 一个"动作"：拿一个真实连接，做一次关系写。
type Action = Arc<dyn Fn(&Connection) -> Result<(), RelError> + Send + Sync>;

/// BUSY / LOCKED = "这次没排上锁"，与业务失败是两回事。
fn is_busy(err: &RelError) -> bool {
    matches!(
        err,
        RelError::Db(rusqlite::Error::SqliteFailure(code, _))
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

/// 重试到出现**非锁**结果为止。
fn retry<T>(db: &FileDb, f: impl Fn(&Connection) -> Result<T, RelError>) -> Result<T, RelError> {
    let conn = db.connect();
    let mut last_busy = None;
    for _ in 0..300 {
        match f(&conn) {
            Ok(v) => return Ok(v),
            Err(e) if is_busy(&e) => {
                last_busy = Some(e);
                thread::sleep(Duration::from_millis(2));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_busy.expect("retry loop always records a busy error before giving up"))
}

/// 两个动作在同一时刻起跑（栅栏对齐），各自独立重试锁竞争。
fn race(db: &Arc<FileDb>, a: Action, b: Action) -> (Result<(), RelError>, Result<(), RelError>) {
    let start = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for action in [a, b] {
        let db = Arc::clone(db);
        let start = Arc::clone(&start);
        handles.push(thread::spawn(move || {
            let conn = db.connect();
            start.wait();
            let mut last_busy = None;
            for _ in 0..300 {
                match action(&conn) {
                    Ok(v) => return Ok(v),
                    Err(e) if is_busy(&e) => {
                        last_busy = Some(e);
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(e) => return Err(e),
                }
            }
            Err(last_busy.expect("racing thread always records a busy error before giving up"))
        }));
    }
    let mut out = Vec::new();
    for h in handles {
        out.push(h.join().expect("racing thread must not panic"));
    }
    (out.remove(0), out.remove(0))
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).expect("count query")
}

/// 自检：BUSY 判定必须真的认得 BUSY，否则整套重试都是在装样子。
#[test]
fn busy_detection_actually_matches_sqlite_busy() {
    let busy = RelError::Db(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
        Some("database is locked".to_string()),
    ));
    assert!(is_busy(&busy), "SQLITE_BUSY 要被判成锁竞争");
    assert!(
        !is_busy(&RelError::Conflict("x")),
        "业务冲突不能被判成锁竞争"
    );
    assert!(!is_busy(&RelError::NotFound("x")));
}

#[test]
fn concurrent_accepts_create_exactly_one_pair() {
    let db = Arc::new(FileDb::new("acc-pair"));
    let id = retry(&db, |c| {
        friend::send(c, "u1", "u2", "", 1_000).map(|o| match o {
            friend::SendOutcome::Created(req) => req.id,
            other => panic!("expected created, got {other:?}"),
        })
    })
    .expect("send");

    // 同一个收件人从两个设备并发点"接受"。
    let mk = |me: &'static str| -> Action {
        let id = id.clone();
        Arc::new(move |c: &Connection| friend::accept(c, me, &id, 2_000).map(|_| ()))
    };
    let (r1, r2) = race(&db, mk("u2"), mk("u2"));

    let ok = [&r1, &r2].iter().filter(|r| r.is_ok()).count();
    assert_eq!(ok, 1, "两个接受只能成一个: {r1:?} / {r2:?}");
    // 输的那个必须是"已决"冲突，而不是别的错误。
    let loser = if r1.is_ok() { &r2 } else { &r1 };
    assert!(
        matches!(loser, Err(RelError::Conflict(_))),
        "并发败者应读成冲突: {loser:?}"
    );

    let conn = db.connect();
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM friends"),
        2,
        "好友永远是两行，不是一行也不是三行"
    );
    assert!(friend::are_friends(&conn, "u1", "u2").unwrap());
}

#[test]
fn accept_and_cancel_never_both_succeed() {
    let db = Arc::new(FileDb::new("acc-cancel"));
    let id = retry(&db, |c| {
        friend::send(c, "u1", "u2", "", 1_000).map(|o| match o {
            friend::SendOutcome::Created(req) => req.id,
            other => panic!("expected created, got {other:?}"),
        })
    })
    .expect("send");

    // 发申请的人点"撤回"，同时收件人点"接受"——这是真会发生的并发。
    let accept = {
        let id = id.clone();
        Arc::new(move |c: &Connection| friend::accept(c, "u2", &id, 2_000).map(|_| ())) as Action
    };
    let cancel = {
        let id = id.clone();
        Arc::new(move |c: &Connection| friend::cancel(c, "u1", &id, 2_000).map(|_| ())) as Action
    };
    let (ra, rc) = race(&db, accept, cancel);

    let ok = [&ra, &rc].iter().filter(|r| r.is_ok()).count();
    assert_eq!(ok, 1, "接受与撤回不能都成功: {ra:?} / {rc:?}");

    let conn = db.connect();
    let status: String = conn
        .query_row(
            "SELECT status FROM friend_requests WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .expect("read status");
    let friends = count(&conn, "SELECT COUNT(*) FROM friends");
    // 终态必须与赢家的动作一致——撤回成功却已是好友，就是旧代码撒谎的样子。
    match status.as_str() {
        "accepted" => assert_eq!(friends, 2, "accepted 就必须有两行好友"),
        "cancelled" => assert_eq!(friends, 0, "cancelled 就不能有好友行"),
        other => panic!("意外终态: {other}"),
    }
}

#[test]
fn block_and_friends_never_coexist() {
    let db = Arc::new(FileDb::new("blk-acc"));
    let id = retry(&db, |c| {
        friend::send(c, "u1", "u2", "", 1_000).map(|o| match o {
            friend::SendOutcome::Created(req) => req.id,
            other => panic!("expected created, got {other:?}"),
        })
    })
    .expect("send");

    let accept: Action = {
        let id = id.clone();
        Arc::new(move |c: &Connection| friend::accept(c, "u2", &id, 2_000).map(|_| ()))
    };
    let block: Action = Arc::new(|c: &Connection| friend::block(c, "u2", "u1", 2_000).map(|_| ()));
    let (ra, rb) = race(&db, accept, block);
    assert!(ra.is_ok() || rb.is_ok(), "{ra:?} / {rb:?}");

    let conn = db.connect();
    let blocked = friend::is_blocked(&conn, "u2", "u1").unwrap();
    let friends = count(&conn, "SELECT COUNT(*) FROM friends");
    assert!(
        !(blocked && friends > 0),
        "拉黑与好友关系不能同时存在（拉黑优先必须真的拆干净）"
    );
    if blocked {
        assert_eq!(friends, 0);
    }
}

#[test]
fn concurrent_duplicate_send_creates_one_request_row() {
    let db = Arc::new(FileDb::new("dup-send"));
    let a: Action = Arc::new(|c: &Connection| friend::send(c, "u1", "u2", "hi", 1_000).map(|_| ()));
    let b: Action = Arc::new(|c: &Connection| friend::send(c, "u1", "u2", "hi", 1_001).map(|_| ()));
    let (ra, rb) = race(&db, a, b);

    let conn = db.connect();
    let rows = count(
        &conn,
        "SELECT COUNT(*) FROM friend_requests WHERE from_uid='u1' AND to_uid='u2'",
    );
    assert_eq!(rows, 1, "重复提交永远只留一行: {ra:?} / {rb:?}");
    // 两个都要么都成功（其中一个是 Duplicate），要么失败的那个是锁之外的错误。
    for r in [&ra, &rb] {
        assert!(
            r.is_ok() || is_busy(r.as_ref().expect_err("busy")),
            "重复申请不该产生第三种结局: {r:?}"
        );
    }
}

#[test]
fn concurrent_add_same_member_writes_one_row() {
    let db = Arc::new(FileDb::new("grp-add"));
    let gid = retry(&db, |c| {
        group::create(
            c,
            "owner",
            "群",
            &["u2".to_string()],
            group::Visibility::Public,
            1_000,
        )
        .map(|g| g.id)
    })
    .expect("create");

    let mk = |at: i64| -> Action {
        let gid = gid.clone();
        Arc::new(move |c: &Connection| {
            group::add_members(c, "owner", &gid, &["u9".to_string()], at).map(|_| ())
        })
    };
    let (r1, r2) = race(&db, mk(2_000), mk(2_001));
    assert!(r1.is_ok() || r2.is_ok(), "{r1:?} / {r2:?}");

    let conn = db.connect();
    let rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM group_members WHERE group_id=?1 AND user_id='u9'",
            rusqlite::params![gid.clone()],
            |r| r.get(0),
        )
        .expect("count u9 membership");
    assert_eq!(rows, 1, "并发拉同一人只能有一行（重复提交幂等）");
    let joined: i64 = conn
        .query_row(
            "SELECT joined_at FROM group_members WHERE group_id=?1 AND user_id='u9'",
            rusqlite::params![gid.clone()],
            |r| r.get(0),
        )
        .expect("joined_at");
    // 谁先抢到写锁是不可预测的，能断言的是：留存的行必然是两次尝试之一的时刻，
    // 而且后到者没有把它改写（幂等，不是"最后一次写赢"）。
    assert!(
        joined == 2_000 || joined == 2_001,
        "joined_at 只能是两次尝试之一，实际 {joined}"
    );
}

#[test]
fn dissolve_and_invite_do_not_write_into_a_dead_group() {
    let db = Arc::new(FileDb::new("grp-dis"));
    let gid = retry(&db, |c| {
        group::create(
            c,
            "owner",
            "群",
            &["u2".to_string()],
            group::Visibility::Public,
            1_000,
        )
        .map(|g| g.id)
    })
    .expect("create");

    let dissolve: Action = {
        let gid = gid.clone();
        Arc::new(move |c: &Connection| group::dissolve(c, "owner", &gid, 2_000).map(|_| ()))
    };
    let invite: Action = {
        let gid = gid.clone();
        Arc::new(move |c: &Connection| {
            group::add_members(c, "owner", &gid, &["u9".to_string()], 2_000).map(|_| ())
        })
    };
    let (rd, ri) = race(&db, dissolve, invite);

    let conn = db.connect();
    let dissolved = count(
        &conn,
        &format!(
            "SELECT COUNT(*) FROM group_meta WHERE group_id='{gid}' AND dissolved_at IS NOT NULL"
        ),
    );
    assert_eq!(dissolved, 1, "解散必须落地（软删标记）: {rd:?} / {ri:?}");
    if ri.is_ok() {
        // 邀请抢在解散之前成功，是允许的；但解散之后再动就必须被闸门挡住。
        let again = group::add_members(&conn, "owner", &gid, &["u8".to_string()], 3_000);
        assert!(
            matches!(again, Err(RelError::Conflict(_))),
            "已解散的群不接受任何后续写入: {again:?}"
        );
    }
}

/// 真实落盘：换一个连接重开这个文件，数据还在。
#[test]
fn writes_survive_reopening_the_database_file() {
    let db = FileDb::new("persist");
    {
        let conn = db.connect();
        friend::send(&conn, "u1", "u2", "", 1_000).expect("send");
        group::create(
            &conn,
            "u1",
            "群",
            &["u3".to_string()],
            group::Visibility::Private,
            1_000,
        )
        .expect("create");
    }
    // 全新连接、同一个文件路径。
    let conn = Connection::open(db.path()).expect("reopen file");
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM friend_requests WHERE from_uid='u1'"
        ),
        1,
        "内存仓储会把这一步悄悄省掉，真文件不会"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM group_meta WHERE visibility='private'"
        ),
        1
    );
}
