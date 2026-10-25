//! G02-06 协议同意记录（ConsentReceipt）。
//!
//! **勾选不是认证。** 这里只记录"谁在什么时候同意了哪个版本的哪份文件"，
//! 不签发任何会话；调用顺序必须是"先验证身份成功，再落回执"，否则等于让
//! 一次勾选变成登录。
//!
//! 回执按 `(user, doc, version)` 唯一：重复提交是幂等的（返回同一条），
//! 升级版本会产生新的一条，历史版本保留——这是"同意时间/版本"要能审计的前提。

use rusqlite::{params, Connection};

use crate::{random_hex, IdError, Result};

/// 需要留痕的协议文件。
pub const DOCS: [&str; 3] = ["terms", "privacy", "community"];
/// 允许的用途：注册、登录、显式同意页。
pub const PURPOSES: [&str; 3] = ["signup", "login", "settings"];
/// 版本号最大长度。
pub const MAX_VERSION_CHARS: usize = 32;
/// 语言标记最大长度。
pub const MAX_LOCALE_CHARS: usize = 16;
/// 回执 id 的随机字节数。
pub const RECEIPT_ID_BYTES: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsentReceipt {
    pub id: String,
    pub user_id: String,
    pub doc: String,
    pub version: String,
    pub purpose: String,
    pub locale: String,
    pub agreed_at: i64,
}

fn validate(doc: &str, purpose: &str, version: &str, locale: &str) -> Result<()> {
    if !DOCS.contains(&doc) {
        return Err(IdError::BadInput("doc"));
    }
    if !PURPOSES.contains(&purpose) {
        return Err(IdError::BadInput("purpose"));
    }
    if version.is_empty() || version.chars().count() > MAX_VERSION_CHARS {
        return Err(IdError::BadInput("version"));
    }
    if version.chars().any(|c| !(c.is_ascii_digit() || c == '.')) {
        return Err(IdError::BadInput("version"));
    }
    if locale.chars().count() > MAX_LOCALE_CHARS || locale.chars().any(|c| c.is_control()) {
        return Err(IdError::BadInput("locale"));
    }
    Ok(())
}

/// 落一条回执。重复调用同一 `(doc, version)` 返回已有记录（幂等）。
pub fn record(
    conn: &Connection,
    user_id: &str,
    doc: &str,
    version: &str,
    purpose: &str,
    locale: &str,
    ip_hash: &str,
    now: i64,
) -> Result<ConsentReceipt> {
    validate(doc, purpose, version, locale)?;
    conn.execute(
        "INSERT OR IGNORE INTO identity_consent(id, user_id, doc, version, purpose, locale, \
         agreed_at, ip_hash) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            random_hex(RECEIPT_ID_BYTES),
            user_id,
            doc,
            version,
            purpose,
            locale,
            now,
            ip_hash
        ],
    )?;
    latest_of(conn, user_id, doc, Some(version))?.ok_or(IdError::NotFound("consent receipt"))
}

/// 该用户对某份文件的最新一条回执。
pub fn latest(conn: &Connection, user_id: &str, doc: &str) -> Result<Option<ConsentReceipt>> {
    if !DOCS.contains(&doc) {
        return Err(IdError::BadInput("doc"));
    }
    latest_of(conn, user_id, doc, None)
}

fn latest_of(
    conn: &Connection,
    user_id: &str,
    doc: &str,
    version: Option<&str>,
) -> Result<Option<ConsentReceipt>> {
    let sql = match version {
        Some(_) => {
            "SELECT id, user_id, doc, version, purpose, locale, agreed_at FROM identity_consent \
             WHERE user_id = ?1 AND doc = ?2 AND version = ?3"
        }
        None => {
            "SELECT id, user_id, doc, version, purpose, locale, agreed_at FROM identity_consent \
             WHERE user_id = ?1 AND doc = ?2 ORDER BY agreed_at DESC LIMIT 1"
        }
    };
    let mut stmt = conn.prepare(sql)?;
    let mut rows = match version {
        Some(v) => stmt.query(params![user_id, doc, v])?,
        None => stmt.query(params![user_id, doc])?,
    };
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some(ConsentReceipt {
        id: row.get(0)?,
        user_id: row.get(1)?,
        doc: row.get(2)?,
        version: row.get(3)?,
        purpose: row.get(4)?,
        locale: row.get(5)?,
        agreed_at: row.get(6)?,
    }))
}

/// 是否已同意 `required` 或更新版本（点分数字比较，`1.10 > 1.9`）。
pub fn has_accepted(conn: &Connection, user_id: &str, doc: &str, required: &str) -> Result<bool> {
    let mut stmt =
        conn.prepare("SELECT version FROM identity_consent WHERE user_id = ?1 AND doc = ?2")?;
    let rows = stmt.query_map(params![user_id, doc], |r| r.get::<_, String>(0))?;
    let mut accepted = false;
    for version in rows {
        if version_at_least(&version?, required)? {
            accepted = true;
        }
    }
    Ok(accepted)
}

/// 点分数字版本比较：逐段比，短的一方补 0。
pub fn version_at_least(have: &str, required: &str) -> Result<bool> {
    let a = parse_version(have)?;
    let b = parse_version(required)?;
    let len = a.len().max(b.len());
    for i in 0..len {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return Ok(x > y);
        }
    }
    Ok(true)
}

fn parse_version(raw: &str) -> Result<Vec<u64>> {
    let mut out = Vec::new();
    for part in raw.split('.') {
        let n: u64 = part
            .parse()
            .map_err(|_| IdError::BadInput("version segment"))?;
        out.push(n);
    }
    if out.is_empty() {
        return Err(IdError::BadInput("version"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::db;

    #[test]
    fn record_is_idempotent() {
        let conn = db();
        let a = record(
            &conn, "u1", "terms", "1.0", "signup", "zh-CN", "iphash", 1_000,
        )
        .unwrap();
        let b = record(
            &conn, "u1", "terms", "1.0", "login", "zh-CN", "iphash", 2_000,
        )
        .unwrap();
        assert_eq!(a.id, b.id, "同一版本重复提交必须返回同一条");
        assert_eq!(a.agreed_at, 1_000, "首次同意时间不被覆盖");
        assert_eq!(latest(&conn, "u1", "terms").unwrap().unwrap().id, a.id);
    }

    #[test]
    fn version_upgrade_keeps_history_and_gates() {
        let conn = db();
        record(&conn, "u1", "privacy", "1.0", "signup", "zh-CN", "", 1_000).unwrap();
        assert!(has_accepted(&conn, "u1", "privacy", "1.0").unwrap());
        assert!(!has_accepted(&conn, "u1", "privacy", "1.1").unwrap());
        record(&conn, "u1", "privacy", "1.1", "login", "zh-CN", "", 2_000).unwrap();
        assert!(has_accepted(&conn, "u1", "privacy", "1.1").unwrap());
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM identity_consent", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 2, "历史版本必须保留");
        assert!(latest(&conn, "u1", "privacy").unwrap().unwrap().version == "1.1");
    }

    #[test]
    fn dotted_compare_is_numeric_not_lexicographic() {
        assert!(version_at_least("1.10", "1.9").unwrap());
        assert!(!version_at_least("1.9", "1.10").unwrap());
        assert!(version_at_least("2", "1.99").unwrap());
        assert!(version_at_least("1.0.0", "1").unwrap());
    }

    #[test]
    fn empty_and_bad_input() {
        let conn = db();
        assert!(latest(&conn, "u1", "terms").unwrap().is_none());
        assert!(matches!(
            record(&conn, "u1", "eula", "1.0", "signup", "", "", 1),
            Err(IdError::BadInput("doc"))
        ));
        assert!(matches!(
            record(&conn, "u1", "terms", "v1", "signup", "", "", 1),
            Err(IdError::BadInput("version"))
        ));
        assert!(matches!(
            record(&conn, "u1", "terms", "1.0", "browse", "", "", 1),
            Err(IdError::BadInput("purpose"))
        ));
    }
}
