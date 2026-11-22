//! **仓储层**（`recovery`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use rusqlite::{params, Connection, OptionalExtension as _};

pub(in crate::domain) fn insert_captcha(
    c: &Connection,
    id: &str,
    answer_hash: &str,
    now: i64,
    expires_at: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO captcha(id, answer_hash, created_at, expires_at, used)
         VALUES(?1, ?2, ?3, ?4, 0)",
        params![id, answer_hash, now, expires_at],
    )?;
    Ok(())
}

pub(in crate::domain) struct CaptchaRow {
    pub(crate) answer_hash: String,
    pub(crate) expires_at: i64,
    pub(crate) used: i64,
}

pub(in crate::domain) fn captcha_row(c: &Connection, id: &str) -> Result<Option<CaptchaRow>> {
    Ok(c.query_row(
        "SELECT answer_hash, expires_at, used FROM captcha WHERE id = ?1",
        params![id],
        |r| {
            Ok(CaptchaRow {
                answer_hash: r.get(0)?,
                expires_at: r.get(1)?,
                used: r.get(2)?,
            })
        },
    )
    .optional()?)
}

pub(in crate::domain) fn consume_captcha(c: &Connection, id: &str) -> Result<()> {
    c.execute("UPDATE captcha SET used = 1 WHERE id = ?1", params![id])?;
    Ok(())
}

pub(in crate::domain) fn upsert_recovery_code(
    c: &Connection,
    user: &str,
    code_hash: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO recovery_codes(user_id, code_hash, created_at, used_at)
         VALUES(?1, ?2, ?3, NULL)
         ON CONFLICT(user_id) DO UPDATE SET
           code_hash = excluded.code_hash,
           created_at = excluded.created_at,
           used_at = NULL",
        params![user, code_hash, now],
    )?;
    Ok(())
}

pub(in crate::domain) struct RecoveryRow {
    pub(crate) code_hash: String,
    pub(crate) created_at: i64,
    pub(crate) used_at: Option<i64>,
}

pub(in crate::domain) fn recovery_row(c: &Connection, user: &str) -> Result<Option<RecoveryRow>> {
    Ok(c.query_row(
        "SELECT code_hash, created_at, used_at FROM recovery_codes WHERE user_id = ?1",
        params![user],
        |r| {
            Ok(RecoveryRow {
                code_hash: r.get(0)?,
                created_at: r.get(1)?,
                used_at: r.get(2)?,
            })
        },
    )
    .optional()?)
}

pub(in crate::domain) fn update_password(c: &Connection, user: &str, phc: &str) -> Result<()> {
    c.execute(
        "UPDATE users SET pw_algo = 'argon2id', pw_salt = '', pw_hash = ?2, pw_rounds = 0
         WHERE id = ?1",
        params![user, phc],
    )?;
    Ok(())
}

pub(in crate::domain) fn delete_sessions(c: &Connection, user: &str) -> Result<()> {
    c.execute("DELETE FROM sessions WHERE user_id = ?1", params![user])?;
    Ok(())
}

pub(in crate::domain) fn mark_recovery_used(c: &Connection, user: &str, now: i64) -> Result<()> {
    c.execute(
        "UPDATE recovery_codes SET used_at = ?2 WHERE user_id = ?1",
        params![user, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn recovery_used_at(
    c: &Connection,
    user: &str,
) -> Result<Option<Option<i64>>> {
    Ok(c.query_row(
        "SELECT used_at FROM recovery_codes WHERE user_id = ?1",
        params![user],
        |r| r.get::<_, Option<i64>>(0),
    )
    .optional()?)
}
