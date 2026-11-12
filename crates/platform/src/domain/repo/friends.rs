//! **仓储层**（`friends`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use rusqlite::params;

/// Appends one address-book change for `user` about `subject`.
pub(in crate::domain) fn log(
    c: &rusqlite::Connection,
    user: &str,
    subject: &str,
    kind: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO contact_log(user_id, subject_id, kind, changed_at) VALUES(?1, ?2, ?3, ?4)",
        params![user, subject, kind, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn log_tx(
    tx: &rusqlite::Transaction<'_>,
    user: &str,
    subject: &str,
    kind: &str,
    now: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO contact_log(user_id, subject_id, kind, changed_at) VALUES(?1, ?2, ?3, ?4)",
        params![user, subject, kind, now],
    )?;
    Ok(())
}
