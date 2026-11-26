//! M018 的 SQLite 侧：访问会话删除后侧车必须跟着走，且同 sid 还有别的
//! token 时不能提前删。

use rusqlite::Connection;
use wxlike_identity::migrate;

fn insert_session(conn: &Connection, token: &str, sid: &str) {
    conn.execute(
        "INSERT INTO sessions(
            token, user_id, created_at, expires_at, sid, device_id,
            device_name, ip, last_seen, audience
         ) VALUES(?1, 'u1', 100, 200, ?2, 'd1', '', '127.0.0.1', 100, 'user')",
        rusqlite::params![token, sid],
    )
    .expect("insert session");
}

fn insert_meta(conn: &Connection, sid: &str) {
    conn.execute(
        "INSERT INTO identity_session_meta(
            sid, user_id, auth_level, features, serial, issued_at, expires_at
         ) VALUES(?1, 'u1', 'auth', '', 0, 100, 200)",
        rusqlite::params![sid],
    )
    .expect("insert meta");
}

fn meta_count(conn: &Connection, sid: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM identity_session_meta WHERE sid = ?1",
        rusqlite::params![sid],
        |r| r.get(0),
    )
    .expect("count meta")
}

#[test]
fn deleting_the_last_session_token_removes_the_sidecar() {
    let conn = Connection::open_in_memory().expect("memory db");
    migrate(&conn).expect("migrate");

    insert_session(&conn, "t1", "sid-1");
    insert_meta(&conn, "sid-1");
    conn.execute("DELETE FROM sessions WHERE token = 't1'", [])
        .expect("delete session");

    assert_eq!(meta_count(&conn, "sid-1"), 0);
}

#[test]
fn a_shared_sid_keeps_the_sidecar_until_the_last_token_goes() {
    let conn = Connection::open_in_memory().expect("memory db");
    migrate(&conn).expect("migrate");

    insert_session(&conn, "t1", "sid-shared");
    insert_session(&conn, "t2", "sid-shared");
    insert_meta(&conn, "sid-shared");

    conn.execute("DELETE FROM sessions WHERE token = 't1'", [])
        .expect("delete first");
    assert_eq!(meta_count(&conn, "sid-shared"), 1);

    conn.execute("DELETE FROM sessions WHERE token = 't2'", [])
        .expect("delete second");
    assert_eq!(meta_count(&conn, "sid-shared"), 0);
}
