//! M017：SQLite 老库的侧车行必须迁进 `identity_session_meta`。
//!
//! 真实建表 + 真实 `migrate`，不构造假存储：先按旧 schema 写入一行
//! `identity_session_token`，再跑完整迁移，最后断言新表有同 sid 的侧车。

use rusqlite::Connection;
use wxlike_identity::migrate;

const LEGACY_SQL: &str = "\
CREATE TABLE identity_session_token (
    sid        TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL,
    auth_level TEXT NOT NULL DEFAULT 'auth',
    features   TEXT NOT NULL DEFAULT '',
    serial     INTEGER NOT NULL DEFAULT 0,
    issued_at  INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
INSERT INTO identity_session_token(
    sid, user_id, auth_level, features, serial, issued_at, expires_at
) VALUES ('sid-legacy', 'user-legacy', 'root', 'V', 7, 100, 200);
";

#[test]
fn m017_backfills_legacy_sidecar_rows() {
    let conn = Connection::open_in_memory().expect("memory db");
    conn.execute_batch(LEGACY_SQL).expect("legacy schema");

    migrate(&conn).expect("migrate");
    migrate(&conn).expect("migrate is idempotent");

    let row: (String, String, i64, i64, i64) = conn
        .query_row(
            "SELECT user_id, auth_level, serial, issued_at, expires_at \
             FROM identity_session_meta WHERE sid = 'sid-legacy'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .expect("backfilled meta row");
    assert_eq!(
        row,
        ("user-legacy".to_string(), "root".to_string(), 7, 100, 200)
    );

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM identity_session_meta WHERE sid = 'sid-legacy'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(count, 1, "second migrate must not duplicate the row");
}
