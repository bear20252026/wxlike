//! G02-01 设备注册表与生命周期。
//!
//! 设备是**账号下的一等实体**，不是会话的附属字段：换设备要能追溯、被移除的
//! 设备要立刻失去 WS 与 API（会话与刷新链一起撤销），重新登录必须走完整认证，
//! 不能靠"再连一次"复活。`identity_key` 是 E2EE 设备身份公钥（G02-03 消费），
//! 在这里只做变更检测：同一 `device_id` 换身份密钥必须走轮换流程。

use rusqlite::{params, Connection, OptionalExtension as _};

use crate::{check_label, now_ms, IdError, Result};

/// 单账号活跃设备上限：超出后先回收已撤销的历史行，仍超则拒绝新设备。
pub const MAX_DEVICES_PER_USER: i64 = 32;
/// 设备标识长度区间（客户端生成的随机 hex）。
pub const DEVICE_ID_MIN: usize = 8;
pub const DEVICE_ID_MAX: usize = 64;
/// E2EE 设备身份公钥（base64）长度上限。
pub const MAX_IDENTITY_KEY_LEN: usize = 128;
/// 允许的平台取值。**封闭集合**：写错的平台名会让推送/密钥分发走错分支。
pub const PLATFORMS: [&str; 4] = ["ios", "android", "desktop", "web"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    Unverified,
    Trusted,
    Revoked,
}

impl Trust {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::Trusted => "trusted",
            Self::Revoked => "revoked",
        }
    }

    fn parse(raw: &str) -> Self {
        match raw {
            "trusted" => Self::Trusted,
            "revoked" => Self::Revoked,
            _ => Self::Unverified,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceRow {
    pub device_id: String,
    pub user_id: String,
    pub platform: String,
    pub label: String,
    pub os_version: String,
    pub app_version: String,
    pub identity_key: String,
    pub trust: Trust,
    pub created_at: i64,
    pub last_seen: i64,
    pub revoked_at: Option<i64>,
}

impl DeviceRow {
    pub fn is_active(&self) -> bool {
        self.trust != Trust::Revoked
    }
}

/// 客户端上报的设备信息。
#[derive(Clone, Debug, Default)]
pub struct Registration {
    pub platform: String,
    pub label: String,
    pub os_version: String,
    pub app_version: String,
    pub identity_key: String,
}

/// 移除设备的后果：哪些凭据被一起作废。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RevokeOutcome {
    pub device_id: String,
    pub sessions: usize,
    pub refresh: usize,
}

fn valid_device_id(raw: &str) -> bool {
    let len = raw.len();
    (DEVICE_ID_MIN..=DEVICE_ID_MAX).contains(&len)
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn row_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<DeviceRow> {
    Ok(DeviceRow {
        device_id: r.get(0)?,
        user_id: r.get(1)?,
        platform: r.get(2)?,
        label: r.get(3)?,
        os_version: r.get(4)?,
        app_version: r.get(5)?,
        identity_key: r.get(6)?,
        trust: Trust::parse(&r.get::<_, String>(7)?),
        created_at: r.get(8)?,
        last_seen: r.get(9)?,
        revoked_at: r.get(10)?,
    })
}

const SELECT_ROW: &str = "SELECT device_id, user_id, platform, label, os_version, app_version, \
     identity_key, trust, created_at, last_seen, revoked_at FROM identity_device";

/// 注册或刷新一台设备。重复调用同一台设备是幂等的（只更新元数据）。
pub fn register(
    conn: &Connection,
    user_id: &str,
    device_id: &str,
    reg: &Registration,
    now: i64,
) -> Result<DeviceRow> {
    if !valid_device_id(device_id) {
        return Err(IdError::BadInput("device_id"));
    }
    if !PLATFORMS.contains(&reg.platform.as_str()) {
        return Err(IdError::BadInput("platform"));
    }
    let label = check_label(&reg.label, "label")?;
    let os = check_label(&reg.os_version, "os_version")?;
    let app = check_label(&reg.app_version, "app_version")?;
    if reg.identity_key.len() > MAX_IDENTITY_KEY_LEN {
        return Err(IdError::BadInput("identity_key"));
    }
    if let Some(existing) = get(conn, device_id)? {
        return refresh_existing(conn, existing, reg, &label, &os, &app, now);
    }
    admit(conn, user_id, device_id, reg, &label, &os, &app, now)
}

/// 已存在的设备：撤销的拒绝复活，换了身份密钥的拒绝静默覆盖。
fn refresh_existing(
    conn: &Connection,
    existing: DeviceRow,
    reg: &Registration,
    label: &str,
    os: &str,
    app: &str,
    now: i64,
) -> Result<DeviceRow> {
    if !existing.is_active() {
        return Err(IdError::Forbidden("device revoked"));
    }
    if !existing.identity_key.is_empty()
        && !reg.identity_key.is_empty()
        && existing.identity_key != reg.identity_key
    {
        return Err(IdError::Conflict("device identity key changed"));
    }
    let key = if reg.identity_key.is_empty() {
        existing.identity_key.clone()
    } else {
        reg.identity_key.clone()
    };
    conn.execute(
        "UPDATE identity_device SET platform = ?2, label = ?3, os_version = ?4, \
         app_version = ?5, identity_key = ?6, last_seen = ?7 WHERE device_id = ?1",
        params![existing.device_id, reg.platform, label, os, app, key, now],
    )?;
    get(conn, &existing.device_id)?.ok_or(IdError::NotFound("device"))
}

/// 新设备入册；先回收历史撤销行，仍超限则拒绝。
fn admit(
    conn: &Connection,
    user_id: &str,
    device_id: &str,
    reg: &Registration,
    label: &str,
    os: &str,
    app: &str,
    now: i64,
) -> Result<DeviceRow> {
    let active: i64 = conn.query_row(
        "SELECT COUNT(*) FROM identity_device WHERE user_id = ?1 AND revoked_at IS NULL",
        params![user_id],
        |r| r.get(0),
    )?;
    if active >= MAX_DEVICES_PER_USER {
        conn.execute(
            "DELETE FROM identity_device WHERE device_id = (
                 SELECT device_id FROM identity_device
                 WHERE user_id = ?1 AND revoked_at IS NOT NULL
                 ORDER BY revoked_at ASC LIMIT 1)",
            params![user_id],
        )?;
    }
    let still: i64 = conn.query_row(
        "SELECT COUNT(*) FROM identity_device WHERE user_id = ?1 AND revoked_at IS NULL",
        params![user_id],
        |r| r.get(0),
    )?;
    if still >= MAX_DEVICES_PER_USER {
        return Err(IdError::Conflict("device limit reached"));
    }
    conn.execute(
        "INSERT INTO identity_device(device_id, user_id, platform, label, os_version, \
         app_version, identity_key, trust, created_at, last_seen, revoked_at) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 'unverified', ?8, ?8, NULL)",
        params![
            device_id,
            user_id,
            reg.platform,
            label,
            os,
            app,
            reg.identity_key,
            now
        ],
    )?;
    get(conn, device_id)?.ok_or(IdError::NotFound("device"))
}

/// 单台设备（含已撤销）。
pub fn get(conn: &Connection, device_id: &str) -> Result<Option<DeviceRow>> {
    let sql = format!("{SELECT_ROW} WHERE device_id = ?1");
    Ok(conn
        .query_row(&sql, params![device_id], row_of)
        .optional()?)
}

/// 该账号的设备列表，最近活跃在前。默认不含已撤销。
pub fn list(conn: &Connection, user_id: &str, include_revoked: bool) -> Result<Vec<DeviceRow>> {
    let sql = if include_revoked {
        format!("{SELECT_ROW} WHERE user_id = ?1 ORDER BY last_seen DESC")
    } else {
        format!("{SELECT_ROW} WHERE user_id = ?1 AND revoked_at IS NULL ORDER BY last_seen DESC")
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![user_id], row_of)?;
    let out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(out)
}

/// 心跳：更新最近活跃时间。设备已撤销时返回错误，调用方据此断开连接。
pub fn touch(conn: &Connection, user_id: &str, device_id: &str, now: i64) -> Result<()> {
    let n = conn.execute(
        "UPDATE identity_device SET last_seen = ?3 \
         WHERE device_id = ?1 AND user_id = ?2 AND revoked_at IS NULL",
        params![device_id, user_id, now],
    )?;
    if n == 0 {
        return Err(IdError::Unauthorized("device not active"));
    }
    Ok(())
}

/// 设备验证结果（G02-03 写入，G02-07 读）。
pub fn set_trust(conn: &Connection, user_id: &str, device_id: &str, trust: Trust) -> Result<()> {
    if trust == Trust::Revoked {
        return Err(IdError::BadInput("use revoke()"));
    }
    let n = conn.execute(
        "UPDATE identity_device SET trust = ?3 \
         WHERE device_id = ?1 AND user_id = ?2 AND revoked_at IS NULL",
        params![device_id, user_id, trust.as_str()],
    )?;
    if n == 0 {
        return Err(IdError::NotFound("device"));
    }
    Ok(())
}

/// 移除设备：会话与刷新链**同一事务**内作废，随后调用方广播"该设备下线"事件。
pub fn revoke(
    conn: &Connection,
    user_id: &str,
    device_id: &str,
    now: i64,
) -> Result<RevokeOutcome> {
    let tx = conn.unchecked_transaction()?;
    let found = tx
        .query_row(
            "SELECT revoked_at FROM identity_device WHERE device_id = ?1 AND user_id = ?2",
            params![device_id, user_id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?;
    let Some(revoked_at) = found else {
        return Err(IdError::NotFound("device"));
    };
    if revoked_at.is_some() {
        // 幂等：重复移除返回同一结论，不再动别的设备。
        return Ok(RevokeOutcome {
            device_id: device_id.to_string(),
            sessions: 0,
            refresh: 0,
        });
    }
    tx.execute(
        "UPDATE identity_device SET trust = 'revoked', revoked_at = ?3 \
         WHERE device_id = ?1 AND user_id = ?2",
        params![device_id, user_id, now],
    )?;
    let sessions = tx.execute(
        "DELETE FROM sessions WHERE user_id = ?1 AND device_id = ?2",
        params![user_id, device_id],
    )?;
    let refresh = crate::refresh::revoke_device(&tx, user_id, device_id, now)?;
    tx.commit()?;
    Ok(RevokeOutcome {
        device_id: device_id.to_string(),
        sessions,
        refresh,
    })
}

/// 当前活跃设备数（限额与运营视图共用）。
pub fn active_count(conn: &Connection, user_id: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM identity_device WHERE user_id = ?1 AND revoked_at IS NULL",
        params![user_id],
        |r| r.get(0),
    )?)
}

/// 给设备打心跳时的"现在"，抽出来便于调用方注入固定时钟。
pub fn touch_now(conn: &Connection, user_id: &str, device_id: &str) -> Result<()> {
    touch(conn, user_id, device_id, now_ms())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::db;

    fn reg(platform: &str, key: &str) -> Registration {
        Registration {
            platform: platform.into(),
            label: "Pixel".into(),
            os_version: "15".into(),
            app_version: "2.0.0".into(),
            identity_key: key.into(),
        }
    }

    #[test]
    fn register_is_idempotent_and_keeps_trust() {
        let conn = db();
        let a = register(&conn, "u1", "dev-aaaa1111", &reg("android", "keyA"), 1_000).unwrap();
        assert_eq!(a.trust, Trust::Unverified);
        set_trust(&conn, "u1", "dev-aaaa1111", Trust::Trusted).unwrap();
        let b = register(&conn, "u1", "dev-aaaa1111", &reg("android", "keyA"), 2_000).unwrap();
        assert_eq!(b.created_at, 1_000);
        assert_eq!(b.last_seen, 2_000);
        assert_eq!(b.trust, Trust::Trusted, "重复注册不得重置信任状态");
        assert_eq!(list(&conn, "u1", false).unwrap().len(), 1);
    }

    #[test]
    fn rejects_bad_input() {
        let conn = db();
        assert!(matches!(
            register(&conn, "u1", "short", &reg("android", ""), 1),
            Err(IdError::BadInput("device_id"))
        ));
        assert!(matches!(
            register(&conn, "u1", "dev-aaaa1111", &reg("symbian", ""), 1),
            Err(IdError::BadInput("platform"))
        ));
        let long = Registration {
            platform: "ios".into(),
            label: "x".repeat(crate::MAX_LABEL_CHARS + 1),
            ..Default::default()
        };
        assert!(matches!(
            register(&conn, "u1", "dev-aaaa1111", &long, 1),
            Err(IdError::BadInput("label"))
        ));
    }

    #[test]
    fn identity_key_change_needs_rotation() {
        let conn = db();
        register(&conn, "u1", "dev-aaaa1111", &reg("ios", "keyA"), 1).unwrap();
        let err = register(&conn, "u1", "dev-aaaa1111", &reg("ios", "keyB"), 2).unwrap_err();
        assert!(
            matches!(err, IdError::Conflict(_)),
            "静默换密钥 = 服务端可伪装设备"
        );
    }

    #[test]
    fn revoked_device_cannot_come_back() {
        let conn = db();
        register(&conn, "u1", "dev-aaaa1111", &reg("android", "keyA"), 1).unwrap();
        let out = revoke(&conn, "u1", "dev-aaaa1111", 5).unwrap();
        assert_eq!(out.device_id, "dev-aaaa1111");
        assert!(matches!(
            register(&conn, "u1", "dev-aaaa1111", &reg("android", "keyA"), 6),
            Err(IdError::Forbidden(_))
        ));
        assert!(matches!(
            touch(&conn, "u1", "dev-aaaa1111", 7),
            Err(IdError::Unauthorized(_))
        ));
        // 幂等：再撤一次不报错，也不返回第二次的连带计数。
        let again = revoke(&conn, "u1", "dev-aaaa1111", 8).unwrap();
        assert_eq!(again.sessions, 0);
    }

    #[test]
    fn revoke_is_scoped_to_owner() {
        let conn = db();
        register(&conn, "u1", "dev-aaaa1111", &reg("android", ""), 1).unwrap();
        assert!(matches!(
            revoke(&conn, "u2", "dev-aaaa1111", 2),
            Err(IdError::NotFound(_))
        ));
    }

    #[test]
    fn empty_list_and_limit() {
        let conn = db();
        assert!(list(&conn, "nobody", false).unwrap().is_empty());
        for i in 0..MAX_DEVICES_PER_USER {
            let id = format!("dev-{i:08}");
            register(&conn, "u1", &id, &reg("web", ""), i).unwrap();
        }
        assert!(matches!(
            register(&conn, "u1", "dev-overflow", &reg("web", ""), 99),
            Err(IdError::Conflict(_))
        ));
        // 回收一台后又能进一台。
        revoke(&conn, "u1", "dev-00000000", 100).unwrap();
        register(&conn, "u1", "dev-overflow", &reg("web", ""), 101).unwrap();
        assert_eq!(active_count(&conn, "u1").unwrap(), MAX_DEVICES_PER_USER);
    }
}
