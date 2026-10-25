//! 好友私有偏好：备注、星标、标签。
//!
//! 这三样都是**每个用户自己的一套**，存在 `friends` / `friend_tag` 的行上，
//! 对方永远看不到，也不参与任何权限判定——备注叫"老板"不会让对方变成老板。
//! 跨端同步走的是同一张表，所以换设备后看到的是同一份偏好。

use rusqlite::{params, Connection, OptionalExtension as _};

use crate::{check_text, friend, RelError, Result, MAX_TEXT_CHARS};

/// 单个好友的标签数量上限。
pub const MAX_TAGS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FriendPref {
    pub friend_id: String,
    pub remark: String,
    pub starred: bool,
    pub tags: Vec<String>,
}

fn require_friend(conn: &Connection, me: &str, friend_id: &str) -> Result<()> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM friends WHERE user_id = ?1 AND friend_id = ?2",
        params![me, friend_id],
        |r| r.get(0),
    )?;
    if n == 0 {
        return Err(RelError::NotFound("friendship"));
    }
    Ok(())
}

/// 设置备注（空串 = 清除备注）。
pub fn set_remark(conn: &Connection, me: &str, friend_id: &str, remark: &str) -> Result<()> {
    require_friend(conn, me, friend_id)?;
    let remark = check_text(remark, MAX_TEXT_CHARS, "remark")?;
    conn.execute(
        "UPDATE friends SET remark = ?3 WHERE user_id = ?1 AND friend_id = ?2",
        params![me, friend_id, remark],
    )?;
    Ok(())
}

/// 星标好友（置顶常用联系人）。
pub fn set_star(conn: &Connection, me: &str, friend_id: &str, starred: bool) -> Result<()> {
    require_friend(conn, me, friend_id)?;
    conn.execute(
        "UPDATE friends SET starred = ?3 WHERE user_id = ?1 AND friend_id = ?2",
        params![me, friend_id, i64::from(starred)],
    )?;
    Ok(())
}

/// 整体替换标签集合：去重、限数量、限长度。
pub fn set_tags(conn: &Connection, me: &str, friend_id: &str, tags: &[String]) -> Result<()> {
    require_friend(conn, me, friend_id)?;
    let mut clean: Vec<String> = Vec::new();
    for tag in tags {
        let tag = check_text(tag, MAX_TEXT_CHARS, "tag")?;
        if tag.is_empty() {
            return Err(RelError::BadInput("tag"));
        }
        if !clean.contains(&tag) {
            clean.push(tag);
        }
    }
    if clean.len() > MAX_TAGS {
        return Err(RelError::BadInput("too many tags"));
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM friend_tag WHERE user_id = ?1 AND friend_id = ?2",
        params![me, friend_id],
    )?;
    let now = crate::now_ms();
    for tag in &clean {
        tx.execute(
            "INSERT INTO friend_tag(user_id, friend_id, tag, created_at) VALUES(?1, ?2, ?3, ?4)",
            params![me, friend_id, tag, now],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn tags_of(conn: &Connection, me: &str, friend_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT tag FROM friend_tag WHERE user_id = ?1 AND friend_id = ?2 ORDER BY tag ASC",
    )?;
    let rows = stmt.query_map(params![me, friend_id], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// 单个好友的私有偏好。
pub fn get(conn: &Connection, me: &str, friend_id: &str) -> Result<Option<FriendPref>> {
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT remark, starred FROM friends WHERE user_id = ?1 AND friend_id = ?2",
            params![me, friend_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((remark, starred)) = row else {
        return Ok(None);
    };
    Ok(Some(FriendPref {
        friend_id: friend_id.to_string(),
        remark,
        starred: starred != 0,
        tags: tags_of(conn, me, friend_id)?,
    }))
}

/// 全部好友的私有偏好（通讯录页一次取齐，避免 N+1）。
pub fn list(conn: &Connection, me: &str) -> Result<Vec<FriendPref>> {
    let mut out = Vec::new();
    for friend_id in friend::friend_ids(conn, me)? {
        if let Some(pref) = get(conn, me, &friend_id)? {
            out.push(pref);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::db;

    fn befriend(conn: &Connection, a: &str, b: &str) {
        friend::send(conn, a, b, "", 1_000).unwrap();
        friend::send(conn, b, a, "", 1_100).unwrap();
    }

    #[test]
    fn prefs_are_per_user_and_private() {
        let conn = db();
        befriend(&conn, "u1", "u2");
        set_remark(&conn, "u1", "u2", "老板").unwrap();
        set_star(&conn, "u1", "u2", true).unwrap();
        set_tags(&conn, "u1", "u2", &["同事".into(), "北京".into()]).unwrap();

        let mine = get(&conn, "u1", "u2").unwrap().unwrap();
        assert_eq!(mine.remark, "老板");
        assert!(mine.starred);
        assert_eq!(mine.tags, vec!["北京".to_string(), "同事".to_string()]);

        let theirs = get(&conn, "u2", "u1").unwrap().unwrap();
        assert_eq!(theirs.remark, "", "备注是单方面的");
        assert!(!theirs.starred);
        assert!(theirs.tags.is_empty());
    }

    #[test]
    fn tags_replace_and_dedupe() {
        let conn = db();
        befriend(&conn, "u1", "u2");
        set_tags(&conn, "u1", "u2", &["a".into(), "a".into(), "b".into()]).unwrap();
        assert_eq!(get(&conn, "u1", "u2").unwrap().unwrap().tags.len(), 2);
        set_tags(&conn, "u1", "u2", &[]).unwrap();
        assert!(get(&conn, "u1", "u2").unwrap().unwrap().tags.is_empty());
    }

    #[test]
    fn rejects_non_friends_and_bad_input() {
        let conn = db();
        assert!(matches!(
            set_remark(&conn, "u1", "u2", "x"),
            Err(RelError::NotFound(_))
        ));
        befriend(&conn, "u1", "u2");
        assert!(matches!(
            set_tags(&conn, "u1", "u2", &["".into()]),
            Err(RelError::BadInput(_))
        ));
        let many: Vec<String> = (0..MAX_TAGS + 1).map(|i| format!("t{i}")).collect();
        assert!(matches!(
            set_tags(&conn, "u1", "u2", &many),
            Err(RelError::BadInput(_))
        ));
        let long = "x".repeat(MAX_TEXT_CHARS + 1);
        assert!(matches!(
            set_remark(&conn, "u1", "u2", &long),
            Err(RelError::BadInput(_))
        ));
    }

    #[test]
    fn list_returns_every_friend() {
        let conn = db();
        befriend(&conn, "u1", "u2");
        befriend(&conn, "u1", "u3");
        set_star(&conn, "u1", "u3", true).unwrap();
        let all = list(&conn, "u1").unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all.iter().filter(|p| p.starred).count(), 1);
    }
}
