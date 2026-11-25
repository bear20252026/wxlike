//! **仓储层**（`qr_state`）：把扫码登录状态机接到平台存储端口。
//!
//! 状态机与 SQL 都留在 identity 的 `qr_login`，此处只做 `StorageConnection`
//! → `QrExec` 的适配，因此 SQLite 与 PG 共用同一份状态机代码，不存在第二份
//! 二维码 SQL。

use crate::storage::sql_port::StorageSqlExec;
use crate::storage::StorageConnection;
use wxlike_identity::qr_login::{self, PollState, ScanView, Ticket};
use wxlike_identity::IdError;

pub(in crate::domain) fn create(conn: &StorageConnection<'_>, now: i64) -> Result<Ticket, IdError> {
    qr_login::create_with(&StorageSqlExec(conn), now)
}

pub(in crate::domain) fn bind(
    conn: &StorageConnection<'_>,
    id: &str,
    verifier: &str,
    prefs: &crate::domain::qr_login::TicketPrefs,
    now: i64,
) -> Result<(), IdError> {
    qr_login::bind_with(
        &StorageSqlExec(conn),
        id,
        verifier,
        &prefs.device_id,
        &prefs.platform,
        &prefs.label,
        "",
        now,
    )
}

pub(in crate::domain) fn scan(
    conn: &StorageConnection<'_>,
    secret: &str,
    scanner_user: &str,
    now: i64,
) -> Result<ScanView, IdError> {
    qr_login::scan_with(&StorageSqlExec(conn), secret, scanner_user, now)
}

pub(in crate::domain) fn approve(
    conn: &StorageConnection<'_>,
    id: &str,
    scanner_user: &str,
    now: i64,
) -> Result<(), IdError> {
    qr_login::approve_with(&StorageSqlExec(conn), id, scanner_user, now)
}

pub(in crate::domain) fn reject(
    conn: &StorageConnection<'_>,
    id: &str,
    scanner_user: &str,
    now: i64,
) -> Result<(), IdError> {
    qr_login::reject_with(&StorageSqlExec(conn), id, scanner_user, now)
}

pub(in crate::domain) fn poll(
    conn: &StorageConnection<'_>,
    id: &str,
    verifier: &str,
    now: i64,
) -> Result<PollState, IdError> {
    qr_login::poll_with(&StorageSqlExec(conn), id, verifier, now)
}

/// 平台自有的创建者登记：与 `repo::qr_login::register_creator` 同一行，
/// 但错误类型是 `IdError`，可以放进同一个 `with_tx` 闭包。
pub(in crate::domain) fn register_creator(
    conn: &StorageConnection<'_>,
    ticket_id: &str,
    creator: &str,
    verifier: &str,
    now: i64,
) -> Result<(), IdError> {
    conn.execute(
        "INSERT INTO qr_ticket_creator(ticket_id, creator, verifier, created_at) \
         VALUES(?1, ?2, ?3, ?4)",
        crate::storage::params![ticket_id, creator, verifier, now],
    )
    .map_err(|e| IdError::Storage(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{approve, bind, create, poll, scan};
    use crate::storage::StorageProfile;
    use crate::Db;
    use wxlike_identity::qr_login::PollState;

    const VERIFIER: &str = "0123456789abcdef0123456789abcdef0123456789ab";

    #[test]
    #[ignore = "requires WXLIKE_TEST_PG_URL and M014 applied"]
    fn postgres_qr_state_moves_from_pending_to_approved() {
        let Ok(url) = std::env::var("WXLIKE_TEST_PG_URL") else {
            return;
        };
        let db = Db::open_profile(StorageProfile::Postgres { url }, None).expect("pg db");
        let conn = db.acquire();
        let ticket = create(&conn, 1_000).expect("create");
        let prefs = crate::domain::qr_login::TicketPrefs {
            device_id: "pg-device-1".into(),
            platform: "desktop".into(),
            label: "PG Desk".into(),
        };
        bind(&conn, &ticket.id, VERIFIER, &prefs, 1_000).expect("bind");
        assert_eq!(
            poll(&conn, &ticket.id, VERIFIER, 1_100).expect("poll"),
            PollState::Pending
        );
        let view = scan(&conn, &ticket.secret, "pg-user", 1_200).expect("scan");
        assert_eq!(view.label, "PG Desk");
        approve(&conn, &ticket.id, "pg-user", 1_300).expect("approve");
        assert_eq!(
            poll(&conn, &ticket.id, VERIFIER, 1_400).expect("poll"),
            PollState::Approved
        );
    }
}
