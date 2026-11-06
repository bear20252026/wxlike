//! 坏行不许折成"看着还行"的默认值（身份域的失败封闭面）。
//!
//! main 上这里有三处折返，全部与"是否已经授权"有关：
//!
//! * `sessions.audience` 折成 `User`——会话列表会撒谎；
//! * `identity_refresh.audience` 折成 `User`——**轮换会把降级写进新链，从此永久**；
//! * `identity_qr.state` 折成 `Pending`——`refresh()` 只把
//!   `consumed/rejected/expired` 当终态，坏值被当成还没用过的码。
//!
//! 第三处最要命：用户明确**拒绝**过一次的那台桌面，理论上可以再刷一张新码、
//! 再被批准、再登录一次。"不"必须算数，所以这里断言的是"必须报错"。

use rusqlite::Connection;
use wxlike_identity::{device, qr_login, refresh, session, Audience, IdError};

mod common;
use common::db;

const DEV: &str = "device-corrupt1";

fn corrupt_session_audience(conn: &Connection, raw: &str) -> String {
    device::register(
        conn,
        "u1",
        DEV,
        &device::Registration {
            platform: "web".to_string(),
            ..Default::default()
        },
        1_000,
    )
    .expect("register");
    let grant = session::open_default(conn, "u1", DEV, Audience::User, "ip", 1_000).expect("open");
    conn.execute(
        "UPDATE sessions SET audience = ?2 WHERE sid = ?1",
        rusqlite::params![grant.sid, raw],
    )
    .expect("corrupt audience");
    grant.sid
}

#[test]
fn session_list_refuses_to_label_an_unreadable_audience_as_user() {
    let conn = db();
    // 展示面必须报错。折成 User 的话，一条管理面会话会在"我的登录设备"里
    // 被显示成普通登录，用户据此决定撤销谁——列表撒谎就是误导撤销。
    let _ = corrupt_session_audience(&conn, "userr");
    let err = session::list(&conn, "u1", 1_500).expect_err("坏受众不能折成 User");
    assert!(matches!(err, IdError::Corrupt(_)), "实际: {err}");
    // 而鉴权路径早就 fail-closed 了：两条路的口径必须一致。
    let rows_ok = {
        let c2 = db();
        device::register(
            &c2,
            "u1",
            DEV,
            &device::Registration {
                platform: "web".to_string(),
                ..Default::default()
            },
            1_000,
        )
        .unwrap();
        let g = session::open_default(&c2, "u1", DEV, Audience::Admin, "ip", 1_000).unwrap();
        c2.execute(
            "UPDATE sessions SET audience = '' WHERE sid = ?1",
            rusqlite::params![g.sid],
        )
        .unwrap();
        matches!(
            session::authenticate(&c2, &g.access_token, Audience::Admin, 1_500),
            Err(IdError::Unauthorized(_))
        )
    };
    assert!(rows_ok, "鉴权面遇到坏受众同样必须拒绝，不能默认放行");
}

#[test]
fn refresh_chain_corrupt_audience_cannot_be_demoted_to_user() {
    let conn = db();
    device::register(
        &conn,
        "u1",
        DEV,
        &device::Registration {
            platform: "desktop".to_string(),
            ..Default::default()
        },
        1_000,
    )
    .expect("register");
    let grant =
        session::open_default(&conn, "u1", DEV, Audience::Admin, "ip", 1_000).expect("open");
    conn.execute(
        "UPDATE identity_refresh SET audience = 'adminn' WHERE sid = ?1",
        rusqlite::params![grant.sid],
    )
    .expect("corrupt chain audience");

    // 轮换会带着 audience 去写**下一代**令牌：折成 User 之后这条链永远回不到
    // 管理面，而且没有任何地方记录它曾经是什么。必须当场失败。
    let err = refresh::rotate(&conn, &grant.refresh_token, 2_000)
        .expect_err("坏受众不能折成 User 后继续轮换");
    assert!(matches!(err, IdError::Corrupt(_)), "实际: {err}");

    // 链视图同样不许撒谎，而且不许留下半次轮换：链仍然只有第一代那一条。
    let err = refresh::chain_of(&conn, &grant.sid).expect_err("链视图不能折成 User");
    assert!(matches!(err, IdError::Corrupt(_)), "实际: {err}");
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM identity_refresh", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1, "失败的轮换不许产生第二代令牌");
}

#[test]
fn well_known_audiences_still_round_trip() {
    let conn = db();
    device::register(
        &conn,
        "u1",
        DEV,
        &device::Registration {
            platform: "ios".to_string(),
            ..Default::default()
        },
        1_000,
    )
    .unwrap();
    for aud in [Audience::User, Audience::Admin] {
        let g = session::open_default(&conn, "u1", DEV, aud, "ip", 1_100).unwrap();
        let rows = session::list(&conn, "u1", 1_200).unwrap();
        assert!(
            rows.iter().any(|r| r.audience == aud),
            "{aud:?} 必须能如实读回来"
        );
        let rotated = refresh::rotate(&conn, &g.refresh_token, 1_300).unwrap();
        assert_eq!(rotated.audience, aud, "轮换必须原样带上受众");
    }
}

#[test]
fn qr_state_that_cannot_be_read_is_not_pending() {
    let conn = db();
    let t = qr_login::create(&conn, 1_000).expect("create");
    let verifier = "v".repeat(48);
    qr_login::bind(&conn, &t.id, &verifier, DEV, "desktop", "本机", "ip", 1_000).expect("bind");

    // 把它改成一条读不懂的原文（模拟别的写入方 / 半途迁移）。
    conn.execute(
        "UPDATE identity_qr SET state = 'consum' WHERE id = ?1",
        rusqlite::params![t.id],
    )
    .expect("corrupt state");

    let err = qr_login::poll(&conn, &t.id, &verifier, 1_500)
        .expect_err("读不懂的状态不能当 pending 继续轮询");
    assert!(matches!(err, IdError::Corrupt(_)), "实际: {err}");
    let err = qr_login::refresh(&conn, &t.id, &verifier, 1_500)
        .expect_err("换码路径尤其不许把坏值当未完成");
    assert!(matches!(err, IdError::Corrupt(_)), "实际: {err}");
    // 换码失败不许产生第二张码。
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM identity_qr", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "失败的换码不许留下新 challenge");
}

#[test]
fn a_rejected_qr_stays_rejected() {
    // 这条不是坏数据，而是同一道闸门的正常侧：用户点"拒绝"之后，
    // 桌面不能靠"换一张码"把这次拒绝冲掉。
    let conn = db();
    let t = qr_login::create(&conn, 1_000).unwrap();
    let verifier = "a".repeat(48);
    qr_login::bind(&conn, &t.id, &verifier, DEV, "desktop", "本机", "ip", 1_000).unwrap();
    qr_login::scan(&conn, &t.secret, "phone-user", 1_100).unwrap();
    qr_login::reject(&conn, &t.id, "phone-user", 1_200).unwrap();

    let err =
        qr_login::refresh(&conn, &t.id, &verifier, 1_300).expect_err("已拒绝的码不许换码重来");
    assert!(matches!(err, IdError::Conflict(_)), "实际: {err}");
    assert!(matches!(
        qr_login::approve(&conn, &t.id, "phone-user", 1_300),
        Err(IdError::Conflict(_))
    ));
    assert!(matches!(
        qr_login::exchange(&conn, &t.id, &verifier, Audience::User, "ip", 1_300),
        Err(IdError::Conflict(_)),
    ));
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0,
        "拒绝之后不能存在任何由它产生的会话"
    );
}

#[test]
fn unknown_qr_state_strings_are_all_rejected_on_read() {
    let conn = db();
    for bad in ["", "PENDING", "used", "pending ", "审核中"] {
        let t = qr_login::create(&conn, 1_000).unwrap();
        let verifier = "z".repeat(50);
        qr_login::bind(
            &conn,
            &t.id,
            &verifier,
            &format!("device-{bad}"),
            "web",
            "l",
            "ip",
            1_000,
        )
        .ok();
        conn.execute(
            "UPDATE identity_qr SET state = ?2 WHERE id = ?1",
            rusqlite::params![t.id, bad],
        )
        .unwrap();
        let err = qr_login::poll(&conn, &t.id, &verifier, 1_500)
            .err()
            .unwrap_or_else(|| panic!("状态 {bad:?} 必须报错，不该被读成 pending"));
        assert!(
            matches!(
                err,
                IdError::Corrupt(_) | IdError::Unauthorized(_) | IdError::BadInput(_)
            ),
            "状态 {bad:?} 给出了意外的错误: {err}"
        );
    }
}
