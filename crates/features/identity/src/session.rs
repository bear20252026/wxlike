//! G02-01 账号会话：签发、鉴权、登出。
//!
//! 与平台既有 `sessions` 表同形（`token` 存摘要、`sid` 供设备页寻址），本模块
//! 补的是**规则**：设备必须活跃、受众必须匹配（管理面与用户面令牌不通用）、
//! 登出要连带作废刷新链并给出缓存清理事件。
//!
//! 令牌是 32 字节随机数，库里只有 `sha256` 摘要；`Display`/日志里永远不出现
//! 明文令牌。

use rusqlite::{params, Connection, OptionalExtension as _};

use crate::{digest, random_hex, Audience, IdError, Result, ACCESS_TTL_MS, TOKEN_BYTES};

/// `sid` 字节数（12B → 24 hex，够用且能进 URL）。
pub const SID_BYTES: usize = 12;
/// 过期清扫批量上限，避免一次登出把整个表扫穿。
pub const PURGE_LIMIT: i64 = 500;
/// 过期判定的提前量：**只适用于秒粒度的签名令牌**（见 `refresh::verify_ticket`）。
///
/// 上游 `auth_token.Authenticate` 的 `expires.Before(now.Add(1s))` 是
/// `tokenLayout.Expires` 用 **unix 秒** 编码的直接后果：一枚"还剩 0.4 秒"的令牌
/// 在下一秒的任何一次比对里都会翻转结论，所以干脆把判定点前移一秒。
/// 本平台的会话/刷新行存的是毫秒，不存在这个舍入问题，**所以 `session::authenticate`
/// 不套用这个提前量**——套了只会让临期会话凭空少一秒寿命（`tests/
/// session_device_lifecycle.rs` 的边界断言就是被它抓出来的）。
pub const EXPIRY_SKEW_SECS: i64 = 1;

/// 认证级别（借 `server/auth/auth.go` 的 `Level`）。
///
/// 顺序即强弱，对应上游 `iota * 10` 的 0/10/20/30：判定一律用 `>=`，
/// 不要用 `==`——上游 `token.Authenticate` 检查的就是
/// `auth.Level(tl.AuthLevel) > auth.LevelRoot` 这种范围式合法性判定。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AuthLevel {
    /// 未认证。
    None,
    /// 匿名/轻认证（临时凭据）。
    Anon,
    /// 完整认证用户。
    Auth,
    /// 超管（本平台用于管理面受众）。
    Root,
}

impl AuthLevel {
    /// `ParseAuthLevel`：认不出来就是 `None`，绝不猜"大概是普通用户"。
    pub fn parse(raw: &str) -> Self {
        match raw {
            "anon" | "ANON" => Self::Anon,
            "auth" | "AUTH" => Self::Auth,
            "root" | "ROOT" => Self::Root,
            _ => Self::None,
        }
    }

    /// `MarshalText` 的等价物：`None` 是空串（上游同一条）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Anon => "anon",
            Self::Auth => "auth",
            Self::Root => "root",
        }
    }

    /// 受众要求的最低认证级别：管理面要 `Root`，用户面要 `Auth`。
    ///
    /// `Anon` 与 `None` 因此都进不了任何一面——上游 `selectAccessMode` 的
    /// `LevelNone → ModeNone` 是同一条纪律（认不出来的级别一律无权）。
    pub fn required_for(audience: crate::Audience) -> Self {
        match audience {
            crate::Audience::User => Self::Auth,
            crate::Audience::Admin => Self::Root,
        }
    }

    /// 签发时按受众写定的级别（与 [`AuthLevel::required_for`] 同表，
    /// 分成两个函数是为了让"要求"与"授予"以后能各自收紧而互不影响）。
    pub fn granted_for(audience: crate::Audience) -> Self {
        Self::required_for(audience)
    }
}

/// 令牌特性位（借 `server/auth/auth.go` 的 `Feature`：`V` = 凭据已验证，
/// `L` = **不得用于永久认证一个会话**）。
///
/// `L` 是这次移植里唯一有真实安全后果的一格：上游用它承载"临时认证"
/// （重置密码流程里凭一封邮件拿到的短期凭据），特点是**能问、不能坐**——
/// 拿它开出来的会话不许轮换出刷新链，也不许长期留在会话表里。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Features(u16);

impl Features {
    /// `FeatureValidated`。
    pub const VALIDATED: u16 = 1;
    /// `FeatureNoLogin`。
    pub const NO_LOGIN: u16 = 1 << 1;
    const ALPHABET: [(u16, char); 2] = [(Self::VALIDATED, 'V'), (Self::NO_LOGIN, 'L')];

    pub const fn from_bits(bits: u16) -> Self {
        Features(bits)
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub const fn empty() -> Self {
        Features(0)
    }

    pub fn contains(self, flag: u16) -> bool {
        self.0 & flag != 0
    }

    pub fn with(self, flag: u16) -> Self {
        Features(self.0 | flag)
    }

    /// `MarshalText`：按 `V`、`L` 的顺序拼字母，空集是空串。
    pub fn to_flags(self) -> String {
        Self::ALPHABET
            .iter()
            .filter(|(bit, _)| self.0 & bit != 0)
            .map(|(_, c)| *c)
            .collect()
    }

    /// `UnmarshalText`：上游对未知字符**也**是报错的（与 `Feature` 的 Loop 分支
    /// 不同，那边只覆盖 V/L 两个字母）；这里同样拒绝任何表外字符，
    /// 宁可让一行读不出来，也不把"看不懂的特性"当成"没有特性"。
    pub fn parse(raw: &str) -> Result<Self> {
        let mut acc = 0u16;
        for ch in raw.chars() {
            match Self::ALPHABET.iter().find(|(_, c)| *c == ch) {
                Some((bit, _)) => acc |= bit,
                None => return Err(IdError::Corrupt("features")),
            }
        }
        Ok(Features(acc))
    }
}

/// 一次登录的产物。**只在这里**出现明文令牌，调用方负责送达客户端。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionGrant {
    pub sid: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub audience: Audience,
}

impl SessionGrant {
    /// 日志/审计用的脱敏描述：不含任何凭据。
    pub fn audit_label(&self) -> String {
        format!("sid={} audience={}", self.sid, self.audience.as_str())
    }

    /// 剩余寿命（上游 `auth.Rec.Lifetime = auth.Duration(time.Until(expires))`）。
    ///
    /// 已过期的算 0，不给负数：负寿命在上游是"这枚令牌压根不该签"的信号
    /// （`GenSecret` 里 `rec.Lifetime < 0 → ErrExpired`），本平台在签发时就拦掉了，
    /// 读侧再出现负数只能是时钟回拨，按"没有剩余"处理最安全。
    pub fn time_left(&self, now: i64) -> i64 {
        self.expires_at.saturating_sub(now).max(0)
    }
}

/// 鉴权通过后的调用者上下文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthContext {
    pub sid: String,
    pub user_id: String,
    pub device_id: String,
    pub audience: Audience,
}

/// 设备管理页的一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    pub sid: String,
    pub device_id: String,
    pub device_name: String,
    pub ip: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub last_seen: i64,
    pub audience: Audience,
}

/// 登出后要清掉的缓存域（由调用方投递到事件总线，本 crate 不发 IO）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheEvent {
    pub scope: &'static str,
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogoutOutcome {
    pub sid: String,
    pub user_id: String,
    pub device_id: String,
    pub refresh_revoked: usize,
    pub cache_events: Vec<CacheEvent>,
}

/// 一行的**原文**读法，`audience` 的判定留给 [`decode`]。
struct RawSession {
    sid: Option<String>,
    device_id: String,
    device_name: String,
    ip: String,
    created_at: i64,
    expires_at: i64,
    last_seen: i64,
    audience: String,
}

fn raw_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawSession> {
    Ok(RawSession {
        sid: r.get(0)?,
        device_id: r.get(1)?,
        device_name: r.get(2)?,
        ip: r.get(3)?,
        created_at: r.get(4)?,
        expires_at: r.get(5)?,
        last_seen: r.get(6)?,
        audience: r.get(7)?,
    })
}

/// `audience` 读不懂必须报错，不能折成 `User`。
///
/// `authenticate()` 早就按 `ok_or(Unauthorized)` 处理同一个字段了——列表这条路
/// 却折默认值，结果是**"会话列表显示的受众"和"真正生效的受众"可以不是同一回事**：
/// 一条管理面会话能在"我的登录设备"里被显示成普通登录。#103 要求管理令牌与用户
/// 令牌分开受众，展示面撒谎同样是失效。
fn decode(raw: RawSession) -> Result<SessionRow> {
    Ok(SessionRow {
        sid: raw.sid.unwrap_or_default(),
        device_id: raw.device_id,
        device_name: raw.device_name,
        ip: raw.ip,
        created_at: raw.created_at,
        expires_at: raw.expires_at,
        last_seen: raw.last_seen,
        audience: Audience::parse(&raw.audience).ok_or(IdError::Corrupt("audience"))?,
    })
}

/// 签发会话：先确认设备活跃，再落会话与刷新链。
pub fn open(
    conn: &Connection,
    user_id: &str,
    device_id: &str,
    audience: Audience,
    ttl_ms: i64,
    ip: &str,
    now: i64,
) -> Result<SessionGrant> {
    open_with(
        conn,
        user_id,
        device_id,
        audience,
        ttl_ms,
        ip,
        now,
        Features::empty(),
    )
}

/// 令牌侧车的一行（借 `tokenLayout` 的字段：级别、特性位、序列号）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenRecord {
    pub sid: String,
    pub user_id: String,
    pub auth_level: AuthLevel,
    pub features: Features,
    pub serial: i64,
    pub issued_at: i64,
    pub expires_at: i64,
}

/// 当前全局令牌序列号。表里没有行 = 从没轮转过 = 0。
pub fn current_serial(conn: &Connection) -> Result<i64> {
    let row: Option<i64> = conn
        .query_row(
            "SELECT serial FROM identity_token_serial WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(row.unwrap_or(0))
}

/// 轮转序列号：**一次调用作废此前签发的全部令牌**（上游 `serial_num` 的用途：
/// 签名密钥泄露、或怀疑令牌被批量重放时用）。
///
/// 与"逐个撤销"的区别是它不需要知道有哪些会话：新签发的带新序列号，
/// 老的在 [`authenticate`] 里当场失效——等价于上游换掉 `serial_num` 重启服务。
pub fn rotate_serial(conn: &Connection, now: i64) -> Result<i64> {
    let next = current_serial(conn)?.saturating_add(1);
    conn.execute(
        "INSERT INTO identity_token_serial(id, serial, rotated_at) VALUES(1, ?1, ?2) \
         ON CONFLICT(id) DO UPDATE SET serial = ?1, rotated_at = ?2",
        params![next, now],
    )?;
    Ok(next)
}

/// 读会话的令牌侧车。没有行 = 这条会话不是本 crate 签发的（平台历史数据），
/// 调用方按"没有额外约束"处理——与 `sessions.audience` 列缺省同一条兼容逻辑。
///
/// 有行但 `auth_level`/`features` 读不懂 → [`IdError::Corrupt`]：
/// 绝不能折成 `Auth`/空集，那等于一行坏数据自动获得权限。
pub fn token_record(conn: &Connection, sid: &str) -> Result<Option<TokenRecord>> {
    if sid.is_empty() {
        return Ok(None);
    }
    let raw: Option<(String, String, String, String, i64, i64, i64)> = conn
        .query_row(
            "SELECT sid, user_id, auth_level, features, serial, issued_at, expires_at \
             FROM identity_session_token WHERE sid = ?1",
            params![sid],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .optional()?;
    let Some((sid, user_id, level, features, serial, issued_at, expires_at)) = raw else {
        return Ok(None);
    };
    Ok(Some(TokenRecord {
        sid,
        user_id,
        auth_level: parse_level(&level)?,
        features: Features::parse(&features)?,
        serial,
        issued_at,
        expires_at,
    }))
}

/// `ParseAuthLevel` 的严格版：表外值报错而不是回 `LevelNone`。
///
/// 上游可以宽松，因为它只拿这个函数解析配置文件里写死的三个词；
/// 这里解析的是**库里的一行**，`"roo"` 折成 `None` 与折成 `Root` 之间的差别
/// 就是"少一个字母的拼写错误能不能提权"。空串是 `None`（上游 `MarshalText` 的
/// 逆运算），不是错误。
fn parse_level(raw: &str) -> Result<AuthLevel> {
    match raw {
        "" => Ok(AuthLevel::None),
        "anon" | "ANON" | "auth" | "AUTH" | "root" | "ROOT" => Ok(AuthLevel::parse(raw)),
        _ => Err(IdError::Corrupt("auth_level")),
    }
}

/// 带特性位的签发入口（`Features::NO_LOGIN` 造"临时认证"会话，见 [`Features`]）。
#[allow(clippy::too_many_arguments)]
pub fn open_with(
    conn: &Connection,
    user_id: &str,
    device_id: &str,
    audience: Audience,
    ttl_ms: i64,
    ip: &str,
    now: i64,
    features: Features,
) -> Result<SessionGrant> {
    require_active_device(conn, user_id, device_id)?;
    // 上游 `GenSecret` 的两条 lifetime 规则原样搬过来：
    // `0` = 没表态 → 用配置的缺省；负数 = 已经死了 → 拒绝签发，
    // 而不是签一枚注定过期的令牌再指望调用方看出来。
    let ttl_ms = match ttl_ms {
        0 => ACCESS_TTL_MS,
        n if n < 0 => return Err(IdError::Expired),
        n => n,
    };
    let sid = random_hex(SID_BYTES);
    let access = random_hex(TOKEN_BYTES);
    let expires_at = now.saturating_add(ttl_ms);
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO sessions(token, user_id, created_at, expires_at, sid, device_id, \
         device_name, ip, last_seen, audience) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, '', ?7, ?3, ?8)",
        params![
            digest(&access),
            user_id,
            now,
            expires_at,
            sid,
            device_id,
            ip,
            audience.as_str()
        ],
    )?;
    tx.execute(
        "INSERT INTO identity_session_token(sid, user_id, auth_level, features, serial, \
         issued_at, expires_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            sid,
            user_id,
            AuthLevel::granted_for(audience).as_str(),
            features.to_flags(),
            current_serial(&tx)?,
            now,
            expires_at
        ],
    )?;
    let refresh = crate::refresh::issue_default(&tx, user_id, &sid, device_id, audience, now)?;
    tx.commit()?;
    Ok(SessionGrant {
        sid,
        access_token: access,
        refresh_token: refresh,
        expires_at,
        audience,
    })
}

/// 临时认证会话（`L` 位）：能过鉴权，**不许**拿去换刷新链。
///
/// 上游用它承载"凭一封邮件/一条短信短暂证明你是谁"的那一步：
/// `FeatureNoLogin` 的注释就是 *"the token should not be used to permanently
/// authenticate a session"*。本平台同款用途是重置密码/改绑手机后的确认页。
pub fn open_temp(
    conn: &Connection,
    user_id: &str,
    device_id: &str,
    audience: Audience,
    ttl_ms: i64,
    ip: &str,
    now: i64,
) -> Result<SessionGrant> {
    open_with(
        conn,
        user_id,
        device_id,
        audience,
        ttl_ms,
        ip,
        now,
        Features::empty().with(Features::NO_LOGIN),
    )
}

/// 默认 2 小时有效期的便捷入口。
pub fn open_default(
    conn: &Connection,
    user_id: &str,
    device_id: &str,
    audience: Audience,
    ip: &str,
    now: i64,
) -> Result<SessionGrant> {
    open(conn, user_id, device_id, audience, ACCESS_TTL_MS, ip, now)
}

fn require_active_device(conn: &Connection, user_id: &str, device_id: &str) -> Result<()> {
    match crate::device::get(conn, device_id)? {
        Some(d) if d.user_id == user_id && d.is_active() => Ok(()),
        Some(_) => Err(IdError::Forbidden("device revoked")),
        None => Err(IdError::NotFound("device")),
    }
}

/// 用访问令牌鉴权。**受众必须显式给出**：管理面拿用户令牌进不来，反之亦然。
pub fn authenticate(
    conn: &Connection,
    access_token: &str,
    audience: Audience,
    now: i64,
) -> Result<AuthContext> {
    let found: Option<(String, Option<String>, String, i64, String)> = conn
        .query_row(
            "SELECT user_id, sid, device_id, expires_at, audience FROM sessions WHERE token = ?1",
            params![digest(access_token)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((user_id, sid, device_id, expires_at, stored_audience)) = found else {
        return Err(IdError::Unauthorized("invalid or expired token"));
    };
    if expires_at <= now {
        conn.execute(
            "DELETE FROM sessions WHERE token = ?1",
            params![digest(access_token)],
        )?;
        return Err(IdError::Unauthorized("invalid or expired token"));
    }
    let stored = Audience::parse(&stored_audience).ok_or(IdError::Unauthorized("bad audience"))?;
    if stored != audience {
        return Err(IdError::Unauthorized("wrong audience"));
    }
    // 令牌侧车两查：序列号与认证级别。
    //
    // 上游 `auth_token.Authenticate` 的分支顺序是"长度 → 签名 → 级别合法 →
    // 序列号 → 过期"，并且**签名不符和序列号不符回的是同一个 `ErrFailed`**：
    // 客户端分不清"这枚不是我拿到的那枚"与"这枚被整体轮转作废了"。这里刻意
    // 同形——两个分支都回与"令牌不存在"完全一样的 `invalid or expired token`，
    // 免得鉴权错误变成一台轮转状态探测仪。
    // 级别不足回 `wrong audience`：那是调用方拿错面在问，不是对服务端的探测。
    if let Some(record) = token_record(conn, sid.as_deref().unwrap_or_default())? {
        if record.serial != current_serial(conn)? {
            return Err(IdError::Unauthorized("invalid or expired token"));
        }
        if record.auth_level < AuthLevel::required_for(audience) {
            return Err(IdError::Unauthorized("wrong audience"));
        }
    }
    if !device_id.is_empty() {
        require_active_device(conn, &user_id, &device_id)?;
    }
    Ok(AuthContext {
        sid: sid.unwrap_or_default(),
        user_id,
        device_id,
        audience,
    })
}

/// 心跳：更新最近活跃时间（失败不阻断请求，鉴权已经过了）。
pub fn touch(conn: &Connection, sid: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET last_seen = ?2 WHERE sid = ?1",
        params![sid, now],
    )?;
    Ok(())
}

/// 账号的有效会话，最近活跃在前；顺手清掉过期行。
pub fn list(conn: &Connection, user_id: &str, now: i64) -> Result<Vec<SessionRow>> {
    conn.execute(
        "DELETE FROM sessions WHERE user_id = ?1 AND expires_at <= ?2",
        params![user_id, now],
    )?;
    let mut stmt = conn.prepare(
        "SELECT sid, device_id, device_name, ip, created_at, expires_at, last_seen, audience \
         FROM sessions WHERE user_id = ?1 ORDER BY last_seen DESC, created_at DESC",
    )?;
    let rows = stmt.query_map(params![user_id], raw_of)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(decode)
        .collect()
}

/// 登出指定会话：删会话 + 作废整条刷新链 + 给出缓存清理事件。
pub fn logout(conn: &Connection, user_id: &str, sid: &str, now: i64) -> Result<LogoutOutcome> {
    let tx = conn.unchecked_transaction()?;
    let device_id: Option<String> = tx
        .query_row(
            "SELECT device_id FROM sessions WHERE user_id = ?1 AND sid = ?2",
            params![user_id, sid],
            |r| r.get(0),
        )
        .optional()?;
    let Some(device_id) = device_id else {
        return Err(IdError::NotFound("session"));
    };
    tx.execute(
        "DELETE FROM sessions WHERE user_id = ?1 AND sid = ?2",
        params![user_id, sid],
    )?;
    // 侧车行一并删掉：留着的话同一 sid 复用时（理论上不会，但主键冲突比
    // 残留鉴权数据好排查）会撞 PRIMARY KEY，而更糟的是它继续参与序列号判定。
    tx.execute(
        "DELETE FROM identity_session_token WHERE sid = ?1",
        params![sid],
    )?;
    let refresh_revoked = crate::refresh::revoke_sid(&tx, sid, now)?;
    tx.execute(
        "DELETE FROM identity_step_up WHERE user_id = ?1 AND sid = ?2",
        params![user_id, sid],
    )?;
    tx.commit()?;
    Ok(LogoutOutcome {
        sid: sid.to_string(),
        user_id: user_id.to_string(),
        device_id,
        refresh_revoked,
        cache_events: vec![
            CacheEvent {
                scope: "session",
                key: sid.to_string(),
            },
            CacheEvent {
                scope: "user",
                key: user_id.to_string(),
            },
        ],
    })
}

/// 登出全部会话（改密 / 注销 / "退出所有设备"）。返回被撤销的会话数。
pub fn logout_all(conn: &Connection, user_id: &str, now: i64) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let revoked = tx.execute("DELETE FROM sessions WHERE user_id = ?1", params![user_id])?;
    tx.execute(
        "DELETE FROM identity_session_token WHERE user_id = ?1",
        params![user_id],
    )?;
    crate::refresh::revoke_user(&tx, user_id, now)?;
    tx.execute(
        "DELETE FROM identity_step_up WHERE user_id = ?1",
        params![user_id],
    )?;
    tx.commit()?;
    Ok(revoked)
}

/// 清扫过期会话（定时任务；返回删除行数）。
pub fn purge_expired(conn: &Connection, now: i64) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let removed = tx.execute(
        "DELETE FROM sessions WHERE rowid IN (
             SELECT rowid FROM sessions WHERE expires_at <= ?1 LIMIT ?2)",
        params![now, PURGE_LIMIT],
    )?;
    // 只留"会话已删、侧车还在"的状态会让 `token_record` 返回一条指向不存在的
    // 会话的行；同一批量上限内一起清，两张表不会长期错位。
    tx.execute(
        "DELETE FROM identity_session_token WHERE rowid IN (
             SELECT rowid FROM identity_session_token WHERE expires_at <= ?1 LIMIT ?2)",
        params![now, PURGE_LIMIT],
    )?;
    tx.commit()?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::db;

    fn device(conn: &Connection, user: &str, id: &str, now: i64) {
        let reg = crate::device::Registration {
            platform: "android".into(),
            ..Default::default()
        };
        crate::device::register(conn, user, id, &reg, now).unwrap();
    }

    #[test]
    fn open_authenticate_logout_roundtrip() {
        let conn = db();
        device(&conn, "u1", "dev-aaaa1111", 1_000);
        let grant = open_default(
            &conn,
            "u1",
            "dev-aaaa1111",
            Audience::User,
            "1.2.3.4",
            1_000,
        )
        .expect("grant");
        assert_eq!(grant.audience, Audience::User);
        assert!(!grant.audit_label().contains(&grant.access_token));

        let ctx = authenticate(&conn, &grant.access_token, Audience::User, 1_100).unwrap();
        assert_eq!(ctx.user_id, "u1");
        assert_eq!(ctx.sid, grant.sid);
        assert_eq!(ctx.device_id, "dev-aaaa1111");

        let out = logout(&conn, "u1", &grant.sid, 1_200).unwrap();
        assert_eq!(out.refresh_revoked, 1);
        assert!(out.cache_events.iter().any(|e| e.scope == "session"));
        assert!(matches!(
            authenticate(&conn, &grant.access_token, Audience::User, 1_300),
            Err(IdError::Unauthorized(_))
        ));
    }

    #[test]
    fn audience_is_not_interchangeable() {
        let conn = db();
        device(&conn, "u1", "dev-aaaa1111", 1_000);
        let user_grant =
            open_default(&conn, "u1", "dev-aaaa1111", Audience::User, "", 1_000).unwrap();
        assert!(matches!(
            authenticate(&conn, &user_grant.access_token, Audience::Admin, 1_100),
            Err(IdError::Unauthorized("wrong audience"))
        ));
        let admin_grant =
            open_default(&conn, "u1", "dev-aaaa1111", Audience::Admin, "", 1_000).unwrap();
        assert!(matches!(
            authenticate(&conn, &admin_grant.access_token, Audience::User, 1_100),
            Err(IdError::Unauthorized("wrong audience"))
        ));
    }

    #[test]
    fn expired_token_is_rejected_and_purged() {
        let conn = db();
        device(&conn, "u1", "dev-aaaa1111", 1_000);
        let grant = open(&conn, "u1", "dev-aaaa1111", Audience::User, 10, "", 1_000).unwrap();
        assert!(matches!(
            authenticate(&conn, &grant.access_token, Audience::User, 2_000),
            Err(IdError::Unauthorized(_))
        ));
        assert!(list(&conn, "u1", 2_000).unwrap().is_empty());
    }

    #[test]
    fn revoked_device_loses_api_access() {
        let conn = db();
        device(&conn, "u1", "dev-aaaa1111", 1_000);
        let grant = open_default(&conn, "u1", "dev-aaaa1111", Audience::User, "", 1_000).unwrap();
        let out = crate::device::revoke(&conn, "u1", "dev-aaaa1111", 1_100).unwrap();
        assert_eq!(out.sessions, 1);
        assert!(matches!(
            authenticate(&conn, &grant.access_token, Audience::User, 1_200),
            Err(IdError::Unauthorized(_))
        ));
    }

    #[test]
    fn unknown_device_cannot_open() {
        let conn = db();
        assert!(matches!(
            open_default(&conn, "u1", "dev-missing0", Audience::User, "", 1_000),
            Err(IdError::NotFound(_))
        ));
    }

    #[test]
    fn serial_rotation_invalidates_every_issued_token() {
        // 上游 `serial_num`：换一次序列号 = 作废之前签发的**全部**令牌。
        let conn = db();
        device(&conn, "u1", "dev-aaaa1111", 1_000);
        let a = open_default(&conn, "u1", "dev-aaaa1111", Audience::User, "", 1_000).unwrap();
        let b = open_default(&conn, "u1", "dev-aaaa1111", Audience::User, "", 1_001).unwrap();
        assert_eq!(current_serial(&conn).unwrap(), 0);
        assert_eq!(rotate_serial(&conn, 1_002).unwrap(), 1);
        for grant in [a, b] {
            assert!(matches!(
                authenticate(&conn, &grant.access_token, Audience::User, 1_100),
                // 与"令牌不存在"完全同形：轮转状态不可被探测。
                Err(IdError::Unauthorized("invalid or expired token"))
            ));
        }
        let fresh = open_default(&conn, "u1", "dev-aaaa1111", Audience::User, "", 1_100).unwrap();
        authenticate(&conn, &fresh.access_token, Audience::User, 1_101).unwrap();
        let _ = logout(&conn, "u1", &fresh.sid, 1_102);
    }

    #[test]
    fn admin_audience_needs_root_level_and_is_not_reachable_with_user_token() {
        let conn = db();
        device(&conn, "u1", "dev-aaaa1111", 1_000);
        let user = open_default(&conn, "u1", "dev-aaaa1111", Audience::User, "", 1_000).unwrap();
        let rec = token_record(&conn, &user.sid)
            .unwrap()
            .expect("sidecar row");
        assert_eq!(rec.auth_level, AuthLevel::Auth);
        // 手工把级别降到 anon（模拟一行被写坏/被降级签发的会话）。
        conn.execute(
            "UPDATE identity_session_token SET auth_level = 'anon' WHERE sid = ?1",
            params![user.sid],
        )
        .unwrap();
        assert!(matches!(
            authenticate(&conn, &user.access_token, Audience::Admin, 1_100),
            Err(IdError::Unauthorized("wrong audience"))
        ));
        // 用户面同样要求 Auth：anon 侧车一律挡在门外。
        assert!(matches!(
            authenticate(&conn, &user.access_token, Audience::User, 1_100),
            Err(IdError::Unauthorized(_))
        ));
        let admin = open_default(&conn, "u1", "dev-aaaa1111", Audience::Admin, "", 1_100).unwrap();
        let rec = token_record(&conn, &admin.sid)
            .unwrap()
            .expect("sidecar row");
        assert_eq!(rec.auth_level, AuthLevel::Root);
        authenticate(&conn, &admin.access_token, Audience::Admin, 1_101).unwrap();
    }

    #[test]
    fn lifetime_zero_falls_back_and_negative_is_refused() {
        let conn = db();
        device(&conn, "u1", "dev-aaaa1111", 1_000);
        // 0 = 没表态 → 用 ACCESS_TTL_MS（上游 GenSecret 的同一条）。
        let dflt = open(&conn, "u1", "dev-aaaa1111", Audience::User, 0, "", 1_000).unwrap();
        assert_eq!(dflt.expires_at, 1_000 + ACCESS_TTL_MS);
        // 负数 = 签一枚已经死的令牌 → 直接拒。
        assert!(matches!(
            open(&conn, "u1", "dev-aaaa1111", Audience::User, -1, "", 1_000),
            Err(IdError::Expired)
        ));
    }

    #[test]
    fn temp_session_carries_no_login_flag_and_cannot_rotate() {
        let conn = db();
        device(&conn, "u1", "dev-aaaa1111", 1_000);
        let temp = open_temp(
            &conn,
            "u1",
            "dev-aaaa1111",
            Audience::User,
            60_000,
            "",
            1_000,
        )
        .unwrap();
        let rec = token_record(&conn, &temp.sid)
            .unwrap()
            .expect("sidecar row");
        assert!(rec.features.contains(Features::NO_LOGIN));
        assert_eq!(rec.features.to_flags(), "L");
        // 临时认证能过鉴权（它得撑住确认页），但换不出新会话。
        authenticate(&conn, &temp.access_token, Audience::User, 1_100).unwrap();
        assert!(matches!(
            crate::refresh::rotate(&conn, &temp.refresh_token, 1_100),
            Err(IdError::Unauthorized(_))
        ));
    }

    #[test]
    fn corrupt_sidecar_fields_fail_closed() {
        let conn = db();
        device(&conn, "u1", "dev-aaaa1111", 1_000);
        let grant = open_default(&conn, "u1", "dev-aaaa1111", Audience::User, "", 1_000).unwrap();
        conn.execute(
            "UPDATE identity_session_token SET auth_level = 'roo' WHERE sid = ?1",
            params![grant.sid],
        )
        .unwrap();
        assert!(matches!(
            token_record(&conn, &grant.sid),
            Err(IdError::Corrupt("auth_level"))
        ));
        conn.execute(
            "UPDATE identity_session_token SET auth_level = 'auth', features = 'VLX' WHERE sid = ?1",
            params![grant.sid],
        )
        .unwrap();
        assert!(matches!(
            token_record(&conn, &grant.sid),
            Err(IdError::Corrupt("features"))
        ));
    }

    #[test]
    fn feature_flags_round_trip_and_reject_unknown_letters() {
        assert_eq!(Features::empty().to_flags(), "");
        assert_eq!(Features::parse("").unwrap(), Features::empty());
        let both = Features::empty()
            .with(Features::VALIDATED)
            .with(Features::NO_LOGIN);
        assert_eq!(both.to_flags(), "VL");
        assert_eq!(Features::parse("LV").unwrap(), both, "字母顺序不表示优先级");
        assert!(matches!(
            Features::parse("V?"),
            Err(IdError::Corrupt("features"))
        ));
        assert!(!Features::empty().contains(Features::NO_LOGIN));
    }

    #[test]
    fn logout_all_revokes_every_session() {
        let conn = db();
        device(&conn, "u1", "dev-aaaa1111", 1_000);
        device(&conn, "u1", "dev-bbbb2222", 1_000);
        let a = open_default(&conn, "u1", "dev-aaaa1111", Audience::User, "", 1_000).unwrap();
        let b = open_default(&conn, "u1", "dev-bbbb2222", Audience::User, "", 1_000).unwrap();
        assert_eq!(logout_all(&conn, "u1", 1_100).unwrap(), 2);
        assert!(authenticate(&conn, &a.access_token, Audience::User, 1_200).is_err());
        assert!(authenticate(&conn, &b.access_token, Audience::User, 1_200).is_err());
    }
}
