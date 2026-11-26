//! G02-01 账号会话：签发、鉴权、登出。
//!
//! 与平台既有 `sessions` 表同形（`token` 存摘要、`sid` 供设备页寻址），本模块
//! 补的是**规则**：设备必须活跃、受众必须匹配（管理面与用户面令牌不通用）、
//! 登出要连带作废刷新链并给出缓存清理事件。
//!
//! 令牌是 32 字节随机数，库里只有 `sha256` 摘要；`Display`/日志里永远不出现
//! 明文令牌。

use rusqlite::{params, Connection};

use crate::sqlport::{ColKind, SqlExec, SqlParam, SqliteExec};
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

wxlike_core::auth_level_ops!(AuthLevel);

impl AuthLevel {
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
    current_serial_with(&SqliteExec(conn))
}

/// 端口版：同一份 SQL 在 SQLite / PG 上执行。
pub fn current_serial_with<E: SqlExec + ?Sized>(exec: &E) -> Result<i64> {
    let row = exec.query(
        "SELECT serial FROM identity_token_serial WHERE id = 1",
        &[],
        &[crate::sqlport::ColKind::Int],
    )?;
    match row.and_then(|mut row| row.pop()) {
        Some(crate::sqlport::SqlValue::Int(v)) => Ok(v),
        Some(_) => Err(IdError::Corrupt("token serial column")),
        None => Ok(0),
    }
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
    token_record_with(&SqliteExec(conn), sid)
}

/// 端口版：会话侧车行。
pub fn token_record_with<E: SqlExec + ?Sized>(exec: &E, sid: &str) -> Result<Option<TokenRecord>> {
    if sid.is_empty() {
        return Ok(None);
    }
    use crate::sqlport::{ColKind, SqlParam};
    let Some(row) = exec.query(
        "SELECT sid, user_id, auth_level, features, serial, issued_at, expires_at \
         FROM identity_session_meta WHERE sid = ?1",
        &[SqlParam::Text(sid.to_string())],
        &[
            ColKind::Text,
            ColKind::Text,
            ColKind::Text,
            ColKind::Text,
            ColKind::Int,
            ColKind::Int,
            ColKind::Int,
        ],
    )?
    else {
        return Ok(None);
    };
    let mut it = row.into_iter();
    let sid = text(it.next())?;
    let user_id = text(it.next())?;
    let level = text(it.next())?;
    let features = text(it.next())?;
    let serial = int(it.next())?;
    let issued_at = int(it.next())?;
    let expires_at = int(it.next())?;
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

fn text(value: Option<crate::sqlport::SqlValue>) -> Result<String> {
    match value {
        Some(crate::sqlport::SqlValue::Text(v)) => Ok(v),
        _ => Err(IdError::Corrupt("session row column")),
    }
}

fn int(value: Option<crate::sqlport::SqlValue>) -> Result<i64> {
    match value {
        Some(crate::sqlport::SqlValue::Int(v)) => Ok(v),
        _ => Err(IdError::Corrupt("session row column")),
    }
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
    let tx = conn.unchecked_transaction()?;
    let grant = open_tx_with(
        &SqliteExec(&tx),
        user_id,
        device_id,
        audience,
        ttl_ms,
        ip,
        now,
        features,
    )?;
    tx.commit()?;
    Ok(grant)
}

/// 端口版 `open_with`：**调用方负责事务**。平台 `Db::with_tx` 把
/// SQLite/PG 的 `sessions` 访问行与 `identity_session_meta` 侧车行放在
/// 同一个事务里，任一失败整体回滚。
#[allow(clippy::too_many_arguments)]
pub fn open_tx_with<E: SqlExec + ?Sized>(
    exec: &E,
    user_id: &str,
    device_id: &str,
    audience: Audience,
    ttl_ms: i64,
    ip: &str,
    now: i64,
    features: Features,
) -> Result<SessionGrant> {
    require_active_device_with(exec, user_id, device_id)?;
    let ttl_ms = normalize_ttl(ttl_ms)?;
    let sid = random_hex(SID_BYTES);
    let access = random_hex(TOKEN_BYTES);
    let expires_at = now.saturating_add(ttl_ms);
    write_session_rows_with(
        exec, user_id, device_id, ip, now, expires_at, &sid, &access, audience, features,
    )?;
    let refresh =
        crate::refresh::issue_default_with(exec, user_id, &sid, device_id, audience, now)?;
    Ok(SessionGrant {
        sid,
        access_token: access,
        refresh_token: refresh,
        expires_at,
        audience,
    })
}

/// `ttl_ms` 的两条上游规则：`0` 用缺省，负数视为已过期。
fn normalize_ttl(ttl_ms: i64) -> Result<i64> {
    match ttl_ms {
        0 => Ok(ACCESS_TTL_MS),
        n if n < 0 => Err(IdError::Expired),
        n => Ok(n),
    }
}

#[allow(clippy::too_many_arguments)]
fn write_session_rows_with<E: SqlExec + ?Sized>(
    exec: &E,
    user_id: &str,
    device_id: &str,
    ip: &str,
    now: i64,
    expires_at: i64,
    sid: &str,
    access: &str,
    audience: Audience,
    features: Features,
) -> Result<()> {
    exec.exec(
        "INSERT INTO sessions(token, user_id, created_at, expires_at, sid, device_id, \
         device_name, ip, last_seen, audience) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, '', ?7, ?3, ?8)",
        &[
            SqlParam::Text(digest(access)),
            SqlParam::Text(user_id.to_string()),
            SqlParam::Int(now),
            SqlParam::Int(expires_at),
            SqlParam::Text(sid.to_string()),
            SqlParam::Text(device_id.to_string()),
            SqlParam::Text(ip.to_string()),
            SqlParam::Text(audience.as_str().to_string()),
        ],
    )?;
    let serial = current_serial_with(exec)?;
    exec.exec(
        "INSERT INTO identity_session_meta(sid, user_id, auth_level, features, serial, \
         issued_at, expires_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        &[
            SqlParam::Text(sid.to_string()),
            SqlParam::Text(user_id.to_string()),
            SqlParam::Text(AuthLevel::granted_for(audience).as_str().to_string()),
            SqlParam::Text(features.to_flags()),
            SqlParam::Int(serial),
            SqlParam::Int(now),
            SqlParam::Int(expires_at),
        ],
    )?;
    Ok(())
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

/// 端口版默认签发：调用方负责事务，适合扫码兑换这类"开会话 + 改其它表"的流程。
pub fn open_default_with<E: SqlExec + ?Sized>(
    exec: &E,
    user_id: &str,
    device_id: &str,
    audience: Audience,
    ip: &str,
    now: i64,
) -> Result<SessionGrant> {
    open_tx_with(
        exec,
        user_id,
        device_id,
        audience,
        ACCESS_TTL_MS,
        ip,
        now,
        Features::empty(),
    )
}

/// 用访问令牌鉴权。**受众必须显式给出**：管理面拿用户令牌进不来，反之亦然。
pub fn authenticate(
    conn: &Connection,
    access_token: &str,
    audience: Audience,
    now: i64,
) -> Result<AuthContext> {
    authenticate_with(&SqliteExec(conn), access_token, audience, now)
}

/// 端口版鉴权：同一组规则在 SQLite / PG 上执行。
pub fn authenticate_with<E: SqlExec + ?Sized>(
    exec: &E,
    access_token: &str,
    audience: Audience,
    now: i64,
) -> Result<AuthContext> {
    use crate::sqlport::{ColKind, SqlParam};
    let Some(row) = exec.query(
        "SELECT user_id, sid, device_id, expires_at, audience FROM sessions WHERE token = ?1",
        &[SqlParam::Text(digest(access_token))],
        &[
            ColKind::Text,
            ColKind::OptText,
            ColKind::Text,
            ColKind::Int,
            ColKind::Text,
        ],
    )?
    else {
        return Err(IdError::Unauthorized("invalid or expired token"));
    };
    let mut it = row.into_iter();
    let user_id = text(it.next())?;
    let sid = match it.next() {
        Some(crate::sqlport::SqlValue::Text(v)) => Some(v),
        Some(crate::sqlport::SqlValue::Null) => None,
        _ => return Err(IdError::Corrupt("session sid column")),
    };
    let device_id = text(it.next())?;
    let expires_at = int(it.next())?;
    let stored_audience = text(it.next())?;
    if expires_at <= now {
        exec.exec(
            "DELETE FROM sessions WHERE token = ?1",
            &[SqlParam::Text(digest(access_token))],
        )?;
        return Err(IdError::Unauthorized("invalid or expired token"));
    }
    let stored = Audience::parse(&stored_audience).ok_or(IdError::Unauthorized("bad audience"))?;
    if stored != audience {
        return Err(IdError::Unauthorized("wrong audience"));
    }
    check_token_sidecar(exec, sid.as_deref().unwrap_or_default(), audience)?;
    if !device_id.is_empty() {
        require_active_device_with(exec, &user_id, &device_id)?;
    }
    Ok(AuthContext {
        sid: sid.unwrap_or_default(),
        user_id,
        device_id,
        audience,
    })
}

/// 令牌侧车两查：序列号与认证级别。分支顺序与错误同形，见旧实现注释。
fn check_token_sidecar<E: SqlExec + ?Sized>(exec: &E, sid: &str, audience: Audience) -> Result<()> {
    let Some(record) = token_record_with(exec, sid)? else {
        return Ok(());
    };
    if record.serial != current_serial_with(exec)? {
        return Err(IdError::Unauthorized("invalid or expired token"));
    }
    if record.auth_level < AuthLevel::required_for(audience) {
        return Err(IdError::Unauthorized("wrong audience"));
    }
    Ok(())
}

/// 端口版设备闸门：设备行不存在是 NotFound，存在但已撤销/不属于该用户是
/// Forbidden——三种结果与 `device::get` + `is_active` 的旧判定逐条对应。
fn require_active_device_with<E: SqlExec + ?Sized>(
    exec: &E,
    user_id: &str,
    device_id: &str,
) -> Result<()> {
    use crate::sqlport::{ColKind, SqlParam};
    let Some(row) = exec.query(
        "SELECT user_id, revoked_at FROM identity_device WHERE device_id = ?1",
        &[SqlParam::Text(device_id.to_string())],
        &[ColKind::Text, ColKind::OptInt],
    )?
    else {
        return Err(IdError::NotFound("device"));
    };
    let mut it = row.into_iter();
    let stored_user = text(it.next())?;
    let revoked = match it.next() {
        Some(crate::sqlport::SqlValue::Int(v)) => Some(v),
        Some(crate::sqlport::SqlValue::Null) => None,
        _ => return Err(IdError::Corrupt("device revoked_at column")),
    };
    if stored_user != user_id || revoked.is_some() {
        return Err(IdError::Forbidden("device revoked"));
    }
    Ok(())
}

/// 心跳：更新最近活跃时间（失败不阻断请求，鉴权已经过了）。
pub fn touch(conn: &Connection, sid: &str, now: i64) -> Result<()> {
    touch_with(&SqliteExec(conn), sid, now)
}

/// 端口版 `touch`。
pub fn touch_with<E: SqlExec + ?Sized>(exec: &E, sid: &str, now: i64) -> Result<()> {
    exec.exec(
        "UPDATE sessions SET last_seen = ?2 WHERE sid = ?1",
        &[SqlParam::Text(sid.to_string()), SqlParam::Int(now)],
    )?;
    Ok(())
}

/// 账号的有效会话，最近活跃在前；顺手清掉过期行。
pub fn list(conn: &Connection, user_id: &str, now: i64) -> Result<Vec<SessionRow>> {
    list_with(&SqliteExec(conn), user_id, now)
}

/// 端口版 `list`。
pub fn list_with<E: SqlExec + ?Sized>(
    exec: &E,
    user_id: &str,
    now: i64,
) -> Result<Vec<SessionRow>> {
    exec.exec(
        "DELETE FROM sessions WHERE user_id = ?1 AND expires_at <= ?2",
        &[SqlParam::Text(user_id.to_string()), SqlParam::Int(now)],
    )?;
    let rows = exec.query_many(
        "SELECT sid, device_id, device_name, ip, created_at, expires_at, last_seen, audience \
         FROM sessions WHERE user_id = ?1 ORDER BY last_seen DESC, created_at DESC",
        &[SqlParam::Text(user_id.to_string())],
        &[
            ColKind::OptText,
            ColKind::Text,
            ColKind::Text,
            ColKind::Text,
            ColKind::Int,
            ColKind::Int,
            ColKind::Int,
            ColKind::Text,
        ],
    )?;
    rows.into_iter().map(decode_values).collect()
}

fn decode_values(row: Vec<crate::sqlport::SqlValue>) -> Result<SessionRow> {
    let mut it = row.into_iter();
    let sid = match it.next() {
        Some(crate::sqlport::SqlValue::Text(v)) => v,
        Some(crate::sqlport::SqlValue::Null) => String::new(),
        _ => return Err(IdError::Corrupt("session sid column")),
    };
    Ok(SessionRow {
        sid,
        device_id: text(it.next())?,
        device_name: text(it.next())?,
        ip: text(it.next())?,
        created_at: int(it.next())?,
        expires_at: int(it.next())?,
        last_seen: int(it.next())?,
        audience: Audience::parse(&text(it.next())?).ok_or(IdError::Corrupt("audience"))?,
    })
}

/// 登出指定会话：删会话 + 作废整条刷新链 + 给出缓存清理事件。
pub fn logout(conn: &Connection, user_id: &str, sid: &str, now: i64) -> Result<LogoutOutcome> {
    let tx = conn.unchecked_transaction()?;
    let outcome = logout_with(&SqliteExec(&tx), user_id, sid, now)?;
    tx.commit()?;
    Ok(outcome)
}

/// 端口版 `logout`：**调用方负责事务**（删会话/侧车/刷新链/step-up 必须一起成功）。
pub fn logout_with<E: SqlExec + ?Sized>(
    exec: &E,
    user_id: &str,
    sid: &str,
    now: i64,
) -> Result<LogoutOutcome> {
    let Some(values) = exec.query(
        "SELECT device_id FROM sessions WHERE user_id = ?1 AND sid = ?2",
        &[
            SqlParam::Text(user_id.to_string()),
            SqlParam::Text(sid.to_string()),
        ],
        &[ColKind::Text],
    )?
    else {
        return Err(IdError::NotFound("session"));
    };
    let device_id = values
        .into_iter()
        .next()
        .ok_or(IdError::Corrupt("sessions.row width"))?
        .into_text()?;
    exec.exec(
        "DELETE FROM sessions WHERE user_id = ?1 AND sid = ?2",
        &[
            SqlParam::Text(user_id.to_string()),
            SqlParam::Text(sid.to_string()),
        ],
    )?;
    // 侧车行一并删掉：留着的话同一 sid 复用时（理论上不会，但主键冲突比
    // 残留鉴权数据好排查）会撞 PRIMARY KEY，而更糟的是它继续参与序列号判定。
    exec.exec(
        "DELETE FROM identity_session_meta WHERE sid = ?1",
        &[SqlParam::Text(sid.to_string())],
    )?;
    let refresh_revoked = crate::refresh::revoke_sid_with(exec, sid, now)?;
    exec.exec(
        "DELETE FROM identity_step_up WHERE user_id = ?1 AND sid = ?2",
        &[
            SqlParam::Text(user_id.to_string()),
            SqlParam::Text(sid.to_string()),
        ],
    )?;
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
    let revoked = logout_all_with(&SqliteExec(&tx), user_id, now)?;
    tx.commit()?;
    Ok(revoked)
}

/// 端口版 `logout_all`：**调用方负责事务**，四张表必须一起成功。
pub fn logout_all_with<E: SqlExec + ?Sized>(exec: &E, user_id: &str, now: i64) -> Result<usize> {
    let revoked = exec.exec(
        "DELETE FROM sessions WHERE user_id = ?1",
        &[SqlParam::Text(user_id.to_string())],
    )?;
    exec.exec(
        "DELETE FROM identity_session_meta WHERE user_id = ?1",
        &[SqlParam::Text(user_id.to_string())],
    )?;
    crate::refresh::revoke_user_with(exec, user_id, now)?;
    exec.exec(
        "DELETE FROM identity_step_up WHERE user_id = ?1",
        &[SqlParam::Text(user_id.to_string())],
    )?;
    Ok(revoked)
}

/// 清扫过期会话（定时任务；返回删除行数）。
pub fn purge_expired(conn: &Connection, now: i64) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let removed = purge_expired_with(&SqliteExec(&tx), now)?;
    tx.commit()?;
    Ok(removed)
}

/// 端口版 `purge_expired`：**调用方负责事务**。
///
/// 用主键 `token` / `sid` 的子查询限量删除，不依赖 `rowid`（PG 视图没有）。
pub fn purge_expired_with<E: SqlExec + ?Sized>(exec: &E, now: i64) -> Result<usize> {
    let removed = exec.exec(
        "DELETE FROM sessions WHERE token IN ( \
             SELECT token FROM sessions WHERE expires_at <= ?1 LIMIT ?2)",
        &[SqlParam::Int(now), SqlParam::Int(PURGE_LIMIT)],
    )?;
    // 只留"会话已删、侧车还在"的状态会让 `token_record` 返回一条指向不存在的
    // 会话的行；同一批量上限内一起清，两张表不会长期错位。
    exec.exec(
        "DELETE FROM identity_session_meta WHERE sid IN ( \
             SELECT sid FROM identity_session_meta WHERE expires_at <= ?1 LIMIT ?2)",
        &[SqlParam::Int(now), SqlParam::Int(PURGE_LIMIT)],
    )?;
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
            "UPDATE identity_session_meta SET auth_level = 'anon' WHERE sid = ?1",
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
            "UPDATE identity_session_meta SET auth_level = 'roo' WHERE sid = ?1",
            params![grant.sid],
        )
        .unwrap();
        assert!(matches!(
            token_record(&conn, &grant.sid),
            Err(IdError::Corrupt("auth_level"))
        ));
        conn.execute(
            "UPDATE identity_session_meta SET auth_level = 'auth', features = 'VLX' WHERE sid = ?1",
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
