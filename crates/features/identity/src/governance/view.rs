//! G02-09 管理端用户元数据视图（`AdminUserView`）。
//!
//! **白名单投影**：只返回 [`METADATA_FIELDS`] 里的字段，与 admin
//! `identity::METADATA_FIELDS` 逐字一致。结构体上根本没有"消息正文"字段——
//! 从源头就不给，比 admin 侧 `project()` 再丢一遍更早一层（双保险的第一层）。
//!
//! 跨域计数（群数/举报数）经 [`UserMetadataPort`] 注入：identity 不直接读
//! relations / 平台 reports 表。测试与未装配场景可用 [`NoCounts`]（计 0）。

use rusqlite::{params, Connection, OptionalExtension as _};
use serde::Serialize;

use super::{check_target, user::user_state, UserMetadataPort};
use crate::Result;

/// 后台允许看到的用户元数据字段（与 admin `identity::METADATA_FIELDS` 逐字一致）。
pub const METADATA_FIELDS: &[&str] = &[
    "user_id",
    "display_name",
    "state",
    "created_at",
    "last_seen_ms",
    "device_count",
    "group_count",
    "report_count",
    "suspended_until_ms",
    "labels",
];

/// 管理端用户视图。**只有元数据**：没有任何消息正文/密文/会话密钥字段。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AdminUserView {
    pub user_id: String,
    pub display_name: String,
    pub state: String,
    pub created_at: i64,
    pub last_seen_ms: i64,
    pub device_count: i64,
    pub group_count: i64,
    pub report_count: i64,
    pub suspended_until_ms: Option<i64>,
    pub labels: Vec<String>,
}

/// 未装配跨域计数时的占位实现（计 0）。生产装配由 G00 注入真实端口。
pub struct NoCounts;

impl UserMetadataPort for NoCounts {
    fn group_count(&self, _user_id: &str) -> Result<i64> {
        Ok(0)
    }
    fn report_count(&self, _user_id: &str) -> Result<i64> {
        Ok(0)
    }
}

/// 投影一个用户的管理视图。需要 **admin audience** 令牌（服务端再次鉴权）。
///
/// `display_name/created_at` 读平台 users 表（只读）；identity 独立库无 users
/// 表时回落空名/0（治理状态仍以 user_id 为键）。`last_seen_ms` 取会话表最近
/// 活跃；`device_count` 只数活跃设备。
pub fn user_view(
    conn: &Connection,
    operator_token: &str,
    user_id: &str,
    counts: &dyn UserMetadataPort,
    now: i64,
) -> Result<AdminUserView> {
    super::authenticate_operator(conn, operator_token, now)?;
    let target = check_target(user_id)?;
    super::require_user(conn, &target)?;

    let state = user_state(conn, &target, now)?;
    let until_ms: Option<i64> = conn
        .query_row(
            "SELECT suspended_until_ms FROM identity_user_governance WHERE user_id = ?1",
            params![target],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    // 到期未结算：对外不展示一个已经失效的封禁期限。
    let until_ms = if state == super::UserState::Expired {
        None
    } else {
        until_ms
    };

    let (display_name, created_at) = profile_of(conn, &target)?;
    let last_seen_ms: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(last_seen), 0) FROM sessions WHERE user_id = ?1",
            params![target],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let device_count = crate::device::active_count(conn, &target)?;
    let labels = labels_of(conn, &target)?;

    Ok(AdminUserView {
        user_id: target.clone(),
        display_name,
        state: state.as_str().to_string(),
        created_at,
        last_seen_ms,
        device_count,
        group_count: counts.group_count(&target)?,
        report_count: counts.report_count(&target)?,
        suspended_until_ms: until_ms,
        labels,
    })
}

/// 读平台 users 表的展示名与注册时间（只读；无表/无行回落空名/0）。
fn profile_of(conn: &Connection, user_id: &str) -> Result<(String, i64)> {
    let has_users: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'users'",
        [],
        |r| r.get::<_, i64>(0),
    )? > 0;
    if !has_users {
        return Ok((String::new(), 0));
    }
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT name, created_at FROM users WHERE id = ?1",
            params![user_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.unwrap_or((String::new(), 0)))
}

fn labels_of(conn: &Connection, user_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT label FROM identity_governance_label WHERE user_id = ?1 ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map(params![user_id], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::db;
    use crate::{device::Registration, Audience, IdError};

    struct FixedCounts;
    impl UserMetadataPort for FixedCounts {
        fn group_count(&self, _user_id: &str) -> Result<i64> {
            Ok(3)
        }
        fn report_count(&self, _user_id: &str) -> Result<i64> {
            Ok(2)
        }
    }

    fn admin_token(conn: &Connection) -> String {
        let reg = Registration {
            platform: "desktop".into(),
            ..Default::default()
        };
        crate::device::register(conn, "ops", "dev-ops00001", &reg, 1_000).unwrap();
        crate::session::open_default(conn, "ops", "dev-ops00001", Audience::Admin, "", 1_000)
            .unwrap()
            .access_token
    }

    #[test]
    fn view_serializes_exactly_the_whitelist() {
        let conn = db();
        let token = admin_token(&conn);
        let view = user_view(&conn, &token, "u1", &FixedCounts, 1_000).unwrap();
        let value = serde_json::to_value(&view).unwrap();
        let obj = value.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(|s| s.as_str()).collect();
        keys.sort_unstable();
        let mut want: Vec<&str> = METADATA_FIELDS.to_vec();
        want.sort_unstable();
        assert_eq!(keys, want, "视图字段必须与白名单逐字一致");
        assert_eq!(view.group_count, 3);
        assert_eq!(view.report_count, 2);
    }

    #[test]
    fn view_never_carries_message_content() {
        let conn = db();
        let token = admin_token(&conn);
        // 即便库里别处有正文，视图结构体也没有承载它的字段。
        let view = user_view(&conn, &token, "u1", &NoCounts, 1_000).unwrap();
        let text = serde_json::to_string(&view).unwrap();
        for field in ["message", "ciphertext", "session_key", "content", "body"] {
            assert!(!text.contains(field), "视图出现正文相关字段 {field}: {text}");
        }
    }

    #[test]
    fn user_token_cannot_read_admin_view() {
        let conn = db();
        let reg = Registration {
            platform: "android".into(),
            ..Default::default()
        };
        crate::device::register(&conn, "u1", "dev-aaaa1111", &reg, 1_000).unwrap();
        let user = crate::session::open_default(
            &conn,
            "u1",
            "dev-aaaa1111",
            Audience::User,
            "",
            1_000,
        )
        .unwrap();
        let err = user_view(&conn, &user.access_token, "u1", &NoCounts, 1_100).unwrap_err();
        assert!(matches!(err, IdError::Unauthorized(_)));
    }
}
