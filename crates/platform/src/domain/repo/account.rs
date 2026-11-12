//! **仓储层**（`account`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::{PlatformError, Result};
use rusqlite::{params, Connection, OptionalExtension as _};
use serde_json::Value;
use std::collections::BTreeMap;
use wxlike_identity::preferences::{self, Job, JobKind, JobStatus};

use crate::domain::account::{json_of, DELETED_NAME_PREFIX, PURGE_DELETES, PURGE_MASKS};

/// 已注销标记。`None` = 没注销（或没有这个用户——调用方都得当成"不许清理"）。
pub(in crate::domain) fn deleted_at(conn: &Connection, user_id: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT deleted_at FROM users WHERE id = ?1",
            params![user_id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten())
}

/// 有没有还在排队/执行中的注销作业。用的就是 `preferences::list` 这条
/// 既有查询，不另开一条 SQL——判"是否已在注销"的口径只能有一处。
pub(in crate::domain) fn open_delete_job(conn: &Connection, user_id: &str) -> Result<Option<Job>> {
    Ok(preferences::list(conn, user_id)?.into_iter().find(|j| {
        j.kind == JobKind::Delete && matches!(j.status, JobStatus::Queued | JobStatus::Running)
    }))
}

/// 钱包余额。用 SQL 的 COALESCE 表达"没有钱包行就是 0"，不在 Rust 里
/// `unwrap_or(0)`——那种写法会把"读库失败"也读成 0（然后真的去删钱包）。
pub(in crate::domain) fn wallet_balance(conn: &Connection, user_id: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE((SELECT balance FROM wallets WHERE user_id = ?1), 0)",
        params![user_id],
        |r| r.get(0),
    )?)
}

/// 一条只带一个用户 id 参数的查询 → JSON 数组，列名由调用方给。
pub(in crate::domain) fn rows_json(
    conn: &Connection,
    sql: &str,
    user_id: &str,
    cols: &[&str],
) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![user_id], |r| {
        let mut obj = serde_json::Map::new();
        for (i, name) in cols.iter().enumerate() {
            let v: rusqlite::types::Value = r.get(i)?;
            obj.insert((*name).to_string(), json_of(v));
        }
        Ok(Value::Object(obj))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// 逐域执行 DELETE，返回每个域清掉的行数（写进作业的摘要）。
pub(in crate::domain) fn purge_rows(
    conn: &Connection,
    user_id: &str,
) -> Result<Vec<(&'static str, usize)>> {
    let mut by_domain: BTreeMap<&'static str, usize> = BTreeMap::new();
    for (domain, sql) in PURGE_DELETES {
        let n = conn.execute(sql, params![user_id])?;
        *by_domain.entry(domain).or_insert(0) += n;
    }
    Ok(by_domain.into_iter().collect())
}

pub(in crate::domain) fn mask_rows(conn: &Connection, user_id: &str) -> Result<()> {
    for (_, sql) in PURGE_MASKS {
        conn.execute(sql, params![user_id])?;
    }
    Ok(())
}

/// 匿名化用户行本身：display name、头像、签名都不是"已注销账号"该留的东西，
/// 口令材料全部清零（连摘要都不留，库副本泄露出去也换不回任何凭证）。
///
/// 行留着而不是 DELETE：`groups.owner_uid` / `communities.owner_uid` /
/// `threads.created_by` 三条外键没有 CASCADE，硬删会把无关的域顶坏。
pub(in crate::domain) fn anonymize_user(conn: &Connection, user_id: &str) -> Result<()> {
    let anon_name = format!("{DELETED_NAME_PREFIX}{user_id}");
    let n = conn.execute(
        "UPDATE users SET name = ?2, avatar = '', signature = '', pw_salt = '', \
         pw_hash = '', pw_rounds = 0, pw_algo = 'deleted', totp_secret = NULL, \
         totp_enabled = 0 WHERE id = ?1",
        params![user_id, anon_name],
    )?;
    if n != 1 {
        // 用户行都不在，这次"清理成功"就是假的。
        return Err(PlatformError::NotFound);
    }
    Ok(())
}
