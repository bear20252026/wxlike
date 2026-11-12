//! 身份域真并发：多个连接打到同一个 SQLite 文件库。
//!
//! 这里关心的不是"能不能同时跑"，而是**同时跑之后库里只剩一个赢家**：
//! 刷新令牌只能被消费一次（第二次按重放处理，整条链作废）、一张二维码只能换出
//! 一条会话、一次 step-up 只能被用一个用途、一个 device_id 只能属于一个人。
//! 这些靠的都是条件 UPDATE 的抢占，所以必须用真实 SQL 才能测出来。
//!
//! BUSY / LOCKED 是 SQLite 的锁竞争结果，不是业务失败，因此对它有限次重试，
//! 断言只落在终态上。

use std::sync::Arc;
use std::sync::Barrier;
use std::thread;

use rusqlite::Connection;
use wxlike_core::test_support;
use wxlike_identity::{
    device, login_challenge as lc, qr_login, refresh, session, step_up, Audience, IdError,
};

mod common;
use common::{file_db, is_busy, setup, FileDb};

type Action = Arc<dyn Fn(&Connection) -> Result<(), IdError> + Send + Sync>;

/// 注册一台设备（`session::open` 要求设备行存在且属于该用户）。
fn ensure_device(db: &FileDb, who: &str, id: &str) {
    setup(db, move |c| {
        device::register(
            c,
            who,
            id,
            &device::Registration {
                platform: "desktop".to_string(),
                ..Default::default()
            },
            1_000,
        )
        .map(|_| ())
    });
}

/// N 个动作同时起跑，各自独立重试锁竞争。
fn race(db: &Arc<FileDb>, actions: Vec<Action>) -> Vec<Result<(), IdError>> {
    let start = Arc::new(Barrier::new(actions.len()));
    let mut handles = Vec::new();
    for action in actions {
        let db = Arc::clone(db);
        let start = Arc::clone(&start);
        handles.push(thread::spawn(move || {
            let conn = db.connect().expect("connect file sqlite");
            start.wait();
            test_support::retry(&conn, is_busy, |c| action(c))
        }));
    }
    handles
        .into_iter()
        .map(|h| h.join().expect("racing thread must not panic"))
        .collect()
}

fn rows(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).expect("count query")
}

const DEV1: &str = "device-conc-0001";
const VERIFIER: &str = "verifier-abcdefghijklmnopqrstuvwxyzABCDEFGHIJ01234567";
const PEPPER: &[u8] = b"pepper-for-concurrency";

#[test]
fn concurrent_rotation_lets_one_through_and_treats_the_other_as_replay() {
    let db = Arc::new(file_db("rot"));
    ensure_device(&db, "u1", DEV1);
    let tokens = setup(&db, |c| {
        session::open_default(c, "u1", DEV1, Audience::User, "ip", 1_000)
            .map(|g| (g.refresh_token, g.sid))
    });
    let (rt, sid) = (tokens.0, tokens.1);

    // 同一个刷新令牌并发刷两次：常见于弱网重试，两个请求都带着同一枚令牌。
    let mk = |at: i64| -> Action {
        let rt = rt.clone();
        Arc::new(move |c: &Connection| refresh::rotate(c, &rt, at).map(|_| ()))
    };
    let out = race(&db, vec![mk(2_000), mk(2_001)]);
    let ok = out.iter().filter(|r| r.is_ok()).count();
    assert_eq!(ok, 1, "同一枚刷新令牌只能被消费一次: {out:?}");
    let loser = if out[0].is_ok() { &out[1] } else { &out[0] };
    assert!(
        matches!(loser, Err(IdError::Unauthorized(_))),
        "第二次必须按重放拒绝: {loser:?}"
    );

    // 重放的后果是整条链作废——包括赢的那一次刚签出来的新令牌。
    let revoked: i64 = setup(&db, {
        let sid = sid.clone();
        move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM identity_refresh WHERE sid = ?1 AND revoked_at IS NOT NULL",
                rusqlite::params![sid.clone()],
                |r| r.get::<_, i64>(0),
            )
            .map_err(IdError::Db)
        }
    });
    assert!(revoked >= 1, "重放要作废整条链，实际作废 {revoked} 行");
    let alive: i64 = setup(&db, move |c| {
        c.query_row(
            "SELECT COUNT(*) FROM identity_refresh WHERE sid = ?1 AND revoked_at IS NULL",
            rusqlite::params![sid.clone()],
            |r| r.get::<_, i64>(0),
        )
        .map_err(IdError::Db)
    });
    assert_eq!(alive, 0, "链上不许留下任何可用令牌");
}

#[test]
fn concurrent_exchange_produces_exactly_one_session() {
    let db = Arc::new(file_db("qr"));
    let id = setup(&db, |c| {
        let t = qr_login::create(c, 1_000)?;
        qr_login::bind(c, &t.id, VERIFIER, DEV1, "desktop", "本机", "ip", 1_000)?;
        qr_login::scan(c, &t.secret, "phone-1", 1_100)?;
        qr_login::approve(c, &t.id, "phone-1", 1_200)?;
        Ok(t.id)
    });

    let mk = |at: i64| -> Action {
        let id = id.clone();
        Arc::new(move |c: &Connection| {
            qr_login::exchange(c, &id, VERIFIER, Audience::User, "ip", at).map(|_| ())
        })
    };
    let out = race(&db, vec![mk(2_000), mk(2_001), mk(2_002)]);
    assert_eq!(
        out.iter().filter(|r| r.is_ok()).count(),
        1,
        "三台桌面同时兑换只有一台能成: {out:?}"
    );
    let conn = db.connect().expect("connect file sqlite");
    assert_eq!(rows(&conn, "SELECT COUNT(*) FROM sessions"), 1);
    assert_eq!(
        rows(
            &conn,
            "SELECT COUNT(*) FROM identity_qr WHERE state = 'consumed'"
        ),
        1
    );
}

#[test]
fn concurrent_step_up_consume_is_single_use() {
    let db = Arc::new(file_db("step"));
    ensure_device(&db, "u1", DEV1);
    let sid = setup(&db, |c| {
        session::open_default(c, "u1", DEV1, Audience::User, "ip", 1_000).map(|g| g.sid)
    });
    setup(&db, |c| {
        step_up::grant_default(c, "u1", &sid, "password.change", 1_100)
    });

    let mk = || -> Action {
        let sid = sid.clone();
        Arc::new(move |c: &Connection| step_up::consume(c, "u1", &sid, "password.change", 2_000))
    };
    let out = race(&db, vec![mk(), mk(), mk()]);
    assert_eq!(
        out.iter().filter(|r| r.is_ok()).count(),
        1,
        "一次 step-up 只够一次敏感操作，否则改密可以连做三次: {out:?}"
    );
    // 换个用途也不给过：授权是按 (user, sid, purpose) 存的。
    // setup 会把非锁错误直接 panic，所以要错误本身就不能走它。
    let conn = db.connect().expect("connect file sqlite");
    let other = step_up::consume(&conn, "u1", &sid, "account.delete", 2_100);
    assert!(
        matches!(other, Err(IdError::Unauthorized(_))),
        "实际: {other:?}"
    );
}

#[test]
fn concurrent_verify_of_the_same_code_consumes_once() {
    let db = Arc::new(file_db("chal"));
    let (cid, code) = setup(&db, |c| {
        let issued = lc::issue(
            c,
            "13800001111",
            "+86",
            "login",
            DEV1,
            "7.7.7.1",
            PEPPER,
            lc::DEFAULT_LIMITS,
            1_000,
        )?;
        Ok((issued.id, issued.code))
    });

    let mk = |at: i64| -> Action {
        let cid = cid.clone();
        let code = code.clone();
        Arc::new(move |c: &Connection| lc::verify(c, &cid, &code, PEPPER, at).map(|_| ()))
    };
    let out = race(&db, vec![mk(2_000), mk(2_001)]);
    let ok = out.iter().filter(|r| r.is_ok()).count();
    assert!(ok <= 1, "同一个验证码不能被两次确认登录: {out:?}");
    let conn = db.connect().expect("connect file sqlite");
    let consumed = rows(
        &conn,
        "SELECT COUNT(*) FROM identity_challenge WHERE consumed_at IS NOT NULL",
    );
    assert_eq!(consumed, 1, "无论几个请求，消费标记只落一次");
}

#[test]
fn same_device_id_cannot_be_claimed_by_two_accounts() {
    let db = Arc::new(file_db("devid"));
    let reg = || device::Registration {
        platform: "web".to_string(),
        ..Default::default()
    };
    let mk = |who: &'static str| -> Action {
        Arc::new(move |c: &Connection| device::register(c, who, DEV1, &reg(), 2_000).map(|_| ()))
    };
    let out = race(&db, vec![mk("alice"), mk("bob")]);
    assert_eq!(out.iter().filter(|r| r.is_ok()).count(), 1, "{out:?}");

    let conn = db.connect().expect("connect file sqlite");
    assert_eq!(
        rows(&conn, "SELECT COUNT(*) FROM identity_device"),
        1,
        "一个 id 只能有一行"
    );
    let owner: String = conn
        .query_row(
            "SELECT user_id FROM identity_device WHERE device_id = ?1",
            rusqlite::params![DEV1],
            |r| r.get(0),
        )
        .expect("owner");
    assert!(
        owner == "alice" || owner == "bob",
        "归属者应是赢家，实际 {owner}"
    );
    // 后到者既没抢走设备，也没法用这台设备开会话。
    let denied = setup(&db, |c| {
        let other = if owner == "alice" { "bob" } else { "alice" };
        session::open_default(c, other, DEV1, Audience::User, "ip", 3_000)
            .err()
            .ok_or(IdError::Conflict("loser unexpectedly got a session"))
    });
    assert!(
        matches!(denied, IdError::Forbidden(_) | IdError::NotFound(_)),
        "实际: {denied}"
    );
}

/// 换一个全新连接重开这个文件：会话、令牌摘要、设备归属都还在。
///
/// 这条是"不许用内存 mock 交差"的直接验证——内存仓储在这里必然失败。
#[test]
fn session_state_survives_reopening_the_database_file() {
    let db = file_db("persist");
    ensure_device(&db, "u1", DEV1);
    let access = setup(&db, |c| {
        session::open_default(c, "u1", DEV1, Audience::Admin, "ip", 1_000).map(|g| g.access_token)
    });
    {
        let conn = db.connect().expect("connect file sqlite");
        assert!(session::authenticate(&conn, &access, Audience::Admin, 1_100).is_ok());
    }
    // 全新的进程内连接（等价于进程重启后重开库）。
    let conn = Connection::open(db.path()).expect("reopen file");
    let ctx = session::authenticate(&conn, &access, Audience::Admin, 1_200)
        .expect("重开库后会话必须仍然有效");
    assert_eq!(ctx.user_id, "u1");
    assert_eq!(rows(&conn, "SELECT COUNT(*) FROM sessions"), 1);
    // 库里只有摘要，没有令牌原文。
    let leak: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE token = ?1",
            rusqlite::params![access],
            |r| r.get(0),
        )
        .expect("plaintext probe");
    assert_eq!(leak, 0, "落库的必须是摘要，不能是可用的令牌原文");
}
